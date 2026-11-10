//! Effects embedded in weapon prefabs (child GameObjects with a ParticleSystem / Light), which AssetRipper's glb
//! cannot carry:
//! - ParticleSystems active in the hierarchy with `playOnAwake` (melee blade-tip `trail` ribbons: Katana, Flamberge,
//!   Nizeh, JahvalMahmaerd, BigFattyBro; looping `VFX_SmokeTrail_02_Short` on Bayshore / DF_Cyst / GammaGen2 / Keso;
//!   Prophet's `VFX_PRophet_MuzzleStatic_00`), spawned through `crate::vfx` from the weapon prefab subtree
//!   (`PrefabDef::load_sub`), persistent and parented to the matching glb node.
//! - Point / spot lights active in the hierarchy (DF_GodSword, Phoenix, Repulsar, HandCanon, DF_Cyst, ...), except the
//!   FlashLight / Warden spot lights handled by `flashlight.rs` and lights belonging to a spawned effect subtree.
//! - Objects toggled by gameplay code: [`FxToggle`] (`GameObject.SetActive`, e.g. Taser `readyVfx`) and
//!   [`NodeMaterial`] (`renderer.materials = ...`, e.g. Taser `light` readyMat / chargingMat).
//!
//! glb nodes are in AssetRipper's X-mirrored frame (see markers.rs); the vfx definitions use the port's Unity->Bevy
//! convention (z negated), which differs by a 180 degree turn about Y, so the effect root sits under its node with that
//! rotation. Effect render entities follow the item's viewmodel layer (held) or the world layer.
use super::data::Prefab;
use super::markers::WeaponMarkers;
use super::{Item, WeaponModel};
use crate::vfx::def::PrefabDef;
use crate::vfx::sim::FxRoot;
use crate::vfx::{SpawnOpts, Vfx};
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use std::collections::HashMap;

/// Set on an item once its embedded effects were spawned.
#[derive(Component)]
pub struct AttachedFxDone;

/// Spawned embedded effects of an item: (prefab path of the effect root, vfx root entity).
#[derive(Component, Default)]
pub struct AttachedFx {
    pub roots: Vec<(String, Entity)>,
}

/// `GameObject.SetActive(on)` for the effect subtree at `path` (full prefab path, e.g.
/// `Taser/ElbowPivotPoint/AimStrafePivot/VFX_Taser_MuzzleStatics_00`).
#[derive(Component, Clone, Debug)]
pub struct FxToggle {
    pub path: String,
    pub on: bool,
}

/// Replace the materials of the glb node named `node` by `Material/<mat>.mat`.
#[derive(Component, Clone, Debug)]
pub struct NodeMaterial {
    pub node: String,
    pub mat: String,
}

fn active_in_hierarchy(prefab: &Prefab, path: &str) -> bool {
    let mut p = path;
    loop {
        if prefab.objects.iter().find(|o| o.path == p).is_some_and(|o| !o.active) {
            return false;
        }
        match p.rsplit_once('/') {
            Some((parent, _)) => p = parent,
            None => return true,
        }
    }
}

fn node_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Spawns the effect subtree rooted at `path` of `def`'s prefab under the glb node of the same name.
fn spawn_subtree(vfx: &mut Vfx, item: &Item, model: Entity, path: &str, kids: &Query<&Children>, names: &Query<&Name>, persistent: bool) -> Option<Entity> {
    let node = crate::unity_pbr::find_named(model, node_of(path), kids, names)?;
    let key = format!("weaponfx:{}:{path}", item.def.name);
    if !vfx.is_registered(&key) {
        match PrefabDef::load_sub(&format!("assets/data/weapons/{}.json", item.def.name), Some(path)) {
            Ok(def) => vfx.register(&key, def),
            Err(e) => {
                warn!("attached fx {key}: {e}");
                return None;
            }
        }
    }
    let root = vfx.spawn(&key, Vec3::ZERO, Quat::IDENTITY, SpawnOpts { persistent, ..default() })?;
    // The node already carries the object's own local transform: the root only converts frames.
    vfx.commands.entity(root).insert((Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)), ChildOf(node)));
    Some(root)
}

#[allow(clippy::too_many_arguments)]
fn spawn_attached(
    mut commands: Commands,
    mut vfx: Vfx,
    items: Query<(Entity, &Item), (With<WeaponMarkers>, Without<AttachedFxDone>)>,
    kids: Query<&Children>,
    models: Query<(), With<WeaponModel>>,
    names: Query<&Name>,
    assets: Res<AssetServer>,
) {
    for (e, item) in &items {
        let Some(model) = kids.get(e).ok().and_then(|c| c.iter().find(|c| models.contains(*c))) else { continue };
        let prefab = &item.def.prefab;
        let mut roots: Vec<(String, Entity)> = Vec::new();
        let mut taken: Vec<String> = Vec::new();
        for o in &prefab.objects {
            let Some(ps) = o.comp("ParticleSystem") else { continue };
            if taken.iter().any(|t| o.path.starts_with(&format!("{t}/"))) {
                continue; // part of an effect subtree already spawned
            }
            if !active_in_hierarchy(prefab, &o.path) || ps.get("playOnAwake").and_then(|v| v.as_i64()) == Some(0) {
                continue;
            }
            if let Some(r) = spawn_subtree(&mut vfx, item, model, &o.path, &kids, &names, true) {
                roots.push((o.path.clone(), r));
                taken.push(o.path.clone());
            }
        }
        // Standalone lights (the vfx system only flashes lights briefly).
        for o in &prefab.objects {
            let Some(l) = o.comp("Light") else { continue };
            if taken.iter().any(|t| o.path.starts_with(&format!("{t}/"))) || !active_in_hierarchy(prefab, &o.path) {
                continue;
            }
            let kind = l.get("m_Type").and_then(|v| v.as_i64());
            // Point lights, and spot lights except FlashLight's (toggled by flashlight.rs); e.g. the Warden's Spot Light.
            if l.get("m_Enabled").and_then(|v| v.as_i64()) == Some(0) || !(kind == Some(2) || (kind == Some(0) && !super::flashlight::is_flashlight(&item.def.class))) {
                continue;
            }
            let Some(node) = crate::unity_pbr::find_named(model, node_of(&o.path), &kids, &names) else { continue };
            let c = l.get("m_Color");
            let g = |k: &str| c.and_then(|c| c.get(k)).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let i = l.get("m_Intensity").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let range = l.get("m_Range").and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
            if kind == Some(0) {
                let fl = |k: &str, d: f32| l.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d);
                let outer = (fl("m_SpotAngle", 30.0) * 0.5).to_radians();
                let mut inner = (fl("m_InnerSpotAngle", 21.8) * 0.5).to_radians().min(outer);
                // Built-in pipeline cookie (alpha, linear) replaces the spot falloff: inner = outer (see flashlight.rs).
                let cookie = l.get("m_Cookie").and_then(|v| v.as_str()).and_then(|c| c.strip_prefix("asset:Texture2D/")).map(|c| format!("textures/{c}")).filter(|c| std::path::Path::new("assets").join(c).exists() || crate::owned::Owned::global().is_some_and(|o| o.resolve_texture(c.rsplit('/').next().unwrap_or(c).trim_end_matches(".png")).is_some()));
                if cookie.is_some() {
                    inner = outer;
                }
                let shadows = l.get("m_Shadows").and_then(|s| s.get("m_Type")).and_then(|v| v.as_i64()).unwrap_or(0) != 0;
                let near = l.get("m_Shadows").and_then(|s| s.get("m_NearPlane")).and_then(|v| v.as_f64()).unwrap_or(0.2) as f32;
                // The glb node keeps Unity's +Z as its forward; Bevy spot lights shine along local -Z.
                let mut ec = commands.spawn((
                    Name::new(format!("weapon-spot:{}", o.path)),
                    SpotLight {
                        color: Color::srgb(g("r"), g("g"), g("b")),
                        intensity: crate::unity_light::unity_point_lumens(i),
                        range,
                        radius: crate::unity_light::UNITY_LIGHT_RADIUS,
                        outer_angle: outer,
                        inner_angle: inner,
                        shadow_maps_enabled: shadows,
                        shadow_map_near_z: near,
                        ..default()
                    },
                    Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
                    ChildOf(node),
                    super::markers::world_and_viewmodel(),
                ));
                if let Some(c) = cookie {
                    ec.insert(bevy::light::SpotLightTexture { image: assets.load_builder().with_settings(|s: &mut bevy::image::ImageLoaderSettings| s.is_srgb = false).load(c) });
                }
                continue;
            }
            commands.spawn((
                Name::new(format!("weapon-light:{}", o.path)),
                PointLight {
                    color: Color::srgb(g("r"), g("g"), g("b")),
                    intensity: crate::unity_light::unity_point_lumens(i),
                    range,
                    radius: crate::unity_light::UNITY_LIGHT_RADIUS,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::default(),
                ChildOf(node),
                super::markers::world_and_viewmodel(),
            ));
        }
        commands.entity(e).insert((AttachedFxDone, AttachedFx { roots }));
    }
}

/// `ps.Play()` if it is not playing, on a ParticleSystem inside the weapon prefab (ItemBehaviour.smokeTrail on
/// Gun / LargeRaycastGun fire; playOnAwake 0, non-looping). The burst follows the weapon node.
#[derive(Component, Clone, Debug)]
pub struct FxPlay {
    pub path: String,
}

fn play_attached(
    mut vfx: Vfx,
    mut items: Query<(Entity, &Item, &FxPlay, Option<&mut AttachedFx>), With<AttachedFxDone>>,
    kids: Query<&Children>,
    models: Query<(), With<WeaponModel>>,
    names: Query<&Name>,
    roots: Query<(), With<FxRoot>>,
) {
    for (e, item, p, list) in &mut items {
        vfx.commands.entity(e).remove::<FxPlay>();
        let Some(mut list) = list else { continue };
        let key = format!("play:{}", p.path);
        // isPlaying: the previous burst's root still exists (it is removed once its particles are gone).
        list.roots.retain(|(k, r)| k != &key || roots.contains(*r));
        if list.roots.iter().any(|(k, _)| k == &key) {
            continue;
        }
        let Some(model) = kids.get(e).ok().and_then(|c| c.iter().find(|c| models.contains(*c))) else { continue };
        if let Some(r) = spawn_subtree(&mut vfx, item, model, &p.path, &kids, &names, false) {
            list.roots.push((key, r));
        }
    }
}

/// [`FxToggle`] requests.
fn toggle_attached(
    mut commands: Commands,
    mut vfx: Vfx,
    mut items: Query<(Entity, &Item, &FxToggle, Option<&mut AttachedFx>), With<AttachedFxDone>>,
    kids: Query<&Children>,
    models: Query<(), With<WeaponModel>>,
    names: Query<&Name>,
    roots: Query<&FxRoot>,
) {
    for (e, item, t, list) in &mut items {
        vfx.commands.entity(e).remove::<FxToggle>();
        let Some(mut list) = list else { continue };
        let existing = list.roots.iter().position(|(p, _)| *p == t.path);
        match (t.on, existing) {
            (true, None) => {
                let Some(model) = kids.get(e).ok().and_then(|c| c.iter().find(|c| models.contains(*c))) else { continue };
                if let Some(r) = spawn_subtree(&mut vfx, item, model, &t.path, &kids, &names, true) {
                    list.roots.push((t.path.clone(), r));
                }
            }
            (false, Some(i)) => {
                let (_, r) = list.roots.remove(i);
                if let Ok(fx) = roots.get(r) {
                    for x in &fx.extra {
                        commands.entity(*x).try_despawn();
                    }
                }
                commands.entity(r).try_despawn();
            }
            _ => {}
        }
    }
}

/// [`NodeMaterial`] requests.
fn node_materials(
    mut commands: Commands,
    mut mp: crate::unity_pbr::UnityMatParams,
    items: Query<(Entity, &NodeMaterial)>,
    kids: Query<&Children>,
    names: Query<&Name>,
    meshes: Query<(), With<Mesh3d>>,
) {
    for (e, r) in &items {
        commands.entity(e).remove::<NodeMaterial>();
        let Some(node) = crate::unity_pbr::find_named(e, &r.node, &kids, &names) else { continue };
        let Some(h) = mp.material(&r.mat) else { continue };
        let targets: Vec<Entity> = std::iter::once(node).chain(kids.get(node).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default()).filter(|x| meshes.contains(*x)).collect();
        for t in targets {
            commands.entity(t).try_remove::<MeshMaterial3d<StandardMaterial>>().try_remove::<MeshMaterial3d<crate::unity_pbr::UnityPbr>>().try_insert(MeshMaterial3d(h.clone()));
        }
    }
}

/// Render layers of the effect render entities follow the item (held = viewmodel layer); orphaned effects (item
/// despawned) are removed with their render entities.
fn sync_attached(
    mut commands: Commands,
    items: Query<(&AttachedFx, Option<&RenderLayers>), With<Item>>,
    roots: Query<&FxRoot>,
    layers: Query<Option<&RenderLayers>>,
    mut known: Local<HashMap<Entity, Vec<Entity>>>,
) {
    let mut alive: Vec<Entity> = Vec::new();
    for (fx, rl) in &items {
        for (_, r) in &fx.roots {
            let Ok(root) = roots.get(*r) else { continue };
            alive.push(*r);
            known.insert(*r, root.extra.clone());
            for x in &root.extra {
                let cur = layers.get(*x).ok().flatten();
                match (rl, cur) {
                    (Some(want), Some(have)) if want == have => {}
                    (Some(want), _) => {
                        commands.entity(*x).try_insert(want.clone());
                    }
                    (None, Some(_)) => {
                        commands.entity(*x).try_remove::<RenderLayers>();
                    }
                    (None, None) => {}
                }
            }
        }
    }
    known.retain(|r, extra| {
        if alive.contains(r) {
            return true;
        }
        for x in extra.iter() {
            commands.entity(*x).try_despawn();
        }
        commands.entity(*r).try_despawn();
        false
    });
}

pub struct AttachedFxPlugin;

impl Plugin for AttachedFxPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (spawn_attached, toggle_attached, play_attached, node_materials, sync_attached).chain());
    }
}
