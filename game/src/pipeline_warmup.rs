//! Render-pipeline warm-up at load.
//!
//! Pipelines compile synchronously (main.rs: async compilation skipped draws and caused black frames), so the first
//! use of every effect (muzzle flash, impacts, decals, trails, explosions) used to stall a frame for 30-140 ms.
//! Unity compiles a shader's D3D bytecode on first use in well under a frame, so the original has no such hitch.
//! Here every effect prefab is instanced once, right after the map and player exist, in front of the player camera
//! (world + viewmodel layers, so both camera views specialise their pipelines) behind an unlit black cover. The cover
//! and the effects are removed after WARM_SECS; their sounds are dropped. RESTRAFE_NO_WARMUP=1 disables it.

use crate::player::fpc::PlayerCamera;
use crate::vfx::{SpawnOpts, Vfx};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;

const WARM_SECS: f32 = 0.6;
/// Give up waiting for the projectile models after this long.
const WARM_MAX_SECS: f32 = 6.0;
const WARM_FRAMES: u32 = 12;

#[derive(Resource, Default)]
struct Warm {
    started: bool,
    done: bool,
    t: f32,
    frames: u32,
    roots: Vec<Entity>,
    cover: Option<Entity>,
    /// Projectile models (glb scenes, ready asynchronously) and the frames they have been fully visible.
    models: Vec<Entity>,
    models_shown: u32,
}

pub struct PipelineWarmupPlugin;

impl Plugin for PipelineWarmupPlugin {
    fn build(&self, app: &mut App) {
        if std::env::var_os("RESTRAFE_NO_WARMUP").is_some() {
            return;
        }
        app.init_resource::<Warm>().add_systems(Update, warmup);
        if std::env::var_os("RESTRAFE_DEBUG_WARMUP").is_some() {
            app.add_systems(Last, (debug_new_meshes, debug_added_meshes));
        }
    }
}

fn prefab_keys() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("assets/data/vfx")
        .map(|d| d.filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(|s| format!("GameObject/{s}.prefab"))).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// (key, prefab json, subtree path): weapon muzzle flashes / ready effects that live inside the weapon prefab
/// (fx.rs "weaponfx:<weapon>:<path>"), and the player's taser-stun effect (health.rs).
fn sub_prefabs() -> Vec<(String, String, String)> {
    let mut v = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir("assets/data/weapons").map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
    if files.is_empty() {
        files = crate::owned::weapon_names().into_iter().map(|n| std::path::PathBuf::from(format!("assets/data/weapons/{n}.json"))).collect();
    }
    files.sort();
    for f in files {
        let (Some(stem), Some(text)) = (f.file_stem().and_then(|s| s.to_str()).map(str::to_string), crate::owned::data_text(&f.to_string_lossy())) else { continue };
        for field in ["\"muzzleFlash\": \"ref:", "\"muzzleFlash2\": \"ref:", "\"readyVfx\": \"ref:"] {
            for (i, _) in text.match_indices(field) {
                let rest = &text[i + field.len()..];
                let Some(end) = rest.find('"') else { continue };
                let path = rest[..end].split("::").next().unwrap_or("").to_string();
                v.push((format!("weaponfx:{stem}:{path}"), format!("assets/data/weapons/{stem}.json"), path));
            }
        }
    }
    v.push((crate::health::STUN_VFX_KEY.to_string(), "assets/data/player/PlayerIK.json".into(), "PlayerIK/Graphics/VFX_TaserStunned_00".into()));
    v
}

fn warmup(
    mut w: ResMut<Warm>,
    time: Res<Time>,
    rack: Option<Res<crate::map::WeaponRack>>,
    cams: Query<(Entity, &GlobalTransform), With<PlayerCamera>>,
    assets: Res<AssetServer>,
    pending: Query<(), With<crate::unity_pbr::HiddenUntilReady>>,
    kids: Query<&Children>,
    mut vis: Query<&mut Visibility>,
    settings: Res<crate::settings::Settings>,
    mut vfx: Vfx,
) {
    if w.done {
        return;
    }
    let Ok((cam, cam_tf)) = cams.single() else { return };
    if !w.started {
        if rack.is_none() {
            return;
        }
        w.started = true;
        let (meshes, mats) = vfx.assets_mut();
        let (mesh, mat) = (meshes.add(Rectangle::new(1.0, 1.0)), mats.add(StandardMaterial { base_color: Color::BLACK, unlit: true, ..default() }));
        // Unlit black cover 0.05 m in front of the camera (near plane 0.01) on the world and viewmodel layers.
        let cover = vfx
            .commands
            .spawn((
                Name::new("pipeline-warmup-cover"),
                Mesh3d(mesh),
                MeshMaterial3d(mat),
                Transform::from_xyz(0.0, 0.0, -0.05),
                ChildOf(cam),
                NotShadowCaster,
                NotShadowReceiver,
                crate::weapons::markers::world_and_viewmodel(),
            ))
            .id();
        w.cover = Some(cover);
        // A shadow-casting point light (perspective cube shadow views, like the flashlight / weapon spot lights) so the
        // shadow pipelines of every map mesh type compile now too; removed with the effects.
        let sh = vfx
            .commands
            .spawn((
                Name::new("pipeline-warmup-shadow-light"),
                PointLight { intensity: 1.0, range: 200.0, shadow_maps_enabled: true, ..default() },
                Transform::from_translation(cam_tf.translation()),
            ))
            .id();
        w.roots.push(sh);
        let fwd = cam_tf.forward().as_vec3();
        let (pos, rot) = (cam_tf.translation() + fwd * 1.0, cam_tf.rotation());
        let mut keys = prefab_keys();
        // Effects that are subtrees of other prefabs, registered under the keys their spawners use.
        for (key, file, path) in sub_prefabs() {
            if !vfx.is_registered(&key) {
                if let Ok(mut def) = crate::vfx::def::PrefabDef::load_sub(&file, Some(&path)) {
                    if let Some(o) = def.objects.first_mut() {
                        o.active = true; // e.g. VFX_TaserStunned_00 is inactive in the prefab (SetActive(true) shows it)
                    }
                    vfx.register(&key, def);
                }
            }
            keys.push(key);
        }
        // fx.rs RenderObject ray lines (in-memory LineRenderer prefab, Sprites/Default).
        let ray = format!("{}grenade", crate::fx::RAY_LINE);
        if !vfx.is_registered(&ray) {
            vfx.register(&ray, crate::fx::ray_line_def(&ray));
        }
        keys.push(ray);
        for k in &keys {
            let opts = SpawnOpts { layers: Some(crate::weapons::markers::world_and_viewmodel()), line: Some((pos, pos + fwd * 0.5)), ..default() };
            if let Some(r) = vfx.spawn(k, pos, rot, opts) {
                w.roots.push(r);
            }
        }
        // Decals spawned by fx.rs (blood splatter, grenade scorch, damage zone): same materials and components.
        for m in crate::fx::warmup_decal_materials(&assets) {
            let (meshes, mats) = vfx.assets_mut();
            let (mesh, mat) = (meshes.add(Rectangle::new(0.5, 0.5)), mats.add(m));
            let d = vfx.commands.spawn((Name::new("pipeline-warmup-decal"), crate::transparent_sort::RenderQueue(3001), Mesh3d(mesh), MeshMaterial3d(mat), Transform::from_translation(pos).with_rotation(rot), NotShadowCaster)).id();
            w.roots.push(d);
        }
        // Projectile models (launcher.rs projectile_visuals: glb scene + PrefabRender), e.g. the hand grenade's
        // alpha-blended radius sphere that only shows in flight.
        let mut files: Vec<_> = std::fs::read_dir("assets/models/projectiles").map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
        files.sort();
        for f in files {
            let Some(stem) = f.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
            let scene = assets.load(GltfAssetLabel::Scene(0).from_asset(format!("models/projectiles/{stem}.glb")));
            let mut m = vfx.commands.spawn((Name::new("pipeline-warmup-model"), bevy::world_serialization::WorldAssetRoot(scene), Transform::from_translation(pos).with_rotation(rot).with_scale(Vec3::splat(0.2)), Visibility::Hidden));
            let json = format!("assets/data/projectiles/{stem}.json");
            if crate::owned::data_exists(&json) {
                m.insert((crate::unity_pbr::PrefabRender(json), crate::unity_pbr::HiddenUntilReady));
            }
            let id = m.id();
            w.models.push(id);
            w.roots.push(id);
        }
        vfx.drop_pending_sounds();
        if std::env::var_os("RESTRAFE_DEBUG_WARMUP").is_some() {
            println!("PIPEWARM start prefabs={} spawned={}", keys.len(), w.roots.len());
        }
        return;
    }
    vfx.drop_pending_sounds();
    w.t += time.delta_secs();
    w.frames += 1;
    // QualitySettings.softParticles (Ultra) re-keys the soft-capable particle materials: compile the other variant too
    // (flipped halfway, restored at the end), so a quality change in the menu does not hitch on the next effects.
    if w.frames == WARM_FRAMES / 2 {
        flip_soft(&mut vfx, None);
    }
    // Once every projectile model is ready, show all of its parts (inactive prefab objects too) for a few frames.
    if w.models.iter().all(|m| !pending.contains(*m)) {
        let mut stack = w.models.clone();
        while let Some(e) = stack.pop() {
            if let Ok(mut v) = vis.get_mut(e) {
                if *v != Visibility::Visible {
                    *v = Visibility::Visible;
                }
            }
            if let Ok(c) = kids.get(e) {
                stack.extend(c.iter());
            }
        }
        w.models_shown += 1;
    }
    if (w.t < WARM_SECS || w.frames < WARM_FRAMES || w.models_shown < 4) && w.t < WARM_MAX_SECS {
        return;
    }
    flip_soft(&mut vfx, Some(settings.quality_setting >= 3));
    for r in std::mem::take(&mut w.roots) {
        vfx.commands.entity(r).try_despawn();
    }
    if let Some(c) = w.cover.take() {
        vfx.commands.entity(c).try_despawn();
    }
    w.done = true;
    if std::env::var_os("RESTRAFE_DEBUG_WARMUP").is_some() {
        println!("PIPEWARM done t={:.2} frames={} models_shown={}", w.t, w.frames, w.models_shown);
    }
}

/// Soft-particle key of every soft-capable particle material: toggled (`None`) or set to the quality's value.
fn flip_soft(vfx: &mut Vfx, to: Option<bool>) {
    let pm = vfx.particle_mats_mut();
    let ids: Vec<_> = pm.iter().filter(|(_, m)| m.soft_variant && to.is_none_or(|t| m.key.soft != t)).map(|(id, _)| id).collect();
    for id in ids {
        if let Some(mut m) = pm.get_mut(id) {
            m.key.soft = to.unwrap_or(!m.key.soft);
        }
    }
}

/// RESTRAFE_DEBUG_WARMUP=1: after the warm-up, every mesh entity that becomes visible for the first time
/// ("NEWVIS frame=<n> <name> <- <parent name>"), to find pipelines that still compile in play.
fn debug_new_meshes(
    w: Res<Warm>,
    q: Query<(Entity, &bevy::camera::visibility::ViewVisibility, Option<&Name>, Option<&ChildOf>), (With<Mesh3d>, Changed<bevy::camera::visibility::ViewVisibility>)>,
    names: Query<&Name>,
    mut seen: Local<bevy::platform::collections::HashSet<Entity>>,
    mut frame: Local<u32>,
) {
    *frame += 1;
    for (e, v, n, p) in &q {
        if !v.get() || !seen.insert(e) || !w.done {
            continue;
        }
        let pn = p.and_then(|p| names.get(p.parent()).ok()).map(|n| n.as_str().to_string()).unwrap_or_default();
        println!("NEWVIS frame={} {} <- {}", *frame, n.map(|n| n.as_str()).unwrap_or("?"), pn);
    }
}

/// RESTRAFE_DEBUG_WARMUP=1: mesh entities spawned after the warm-up ("NEWMESH frame=<n> <name> <- <parent>"), also those
/// only a shadow view sees.
fn debug_added_meshes(w: Res<Warm>, q: Query<(Option<&Name>, Option<&ChildOf>), Added<Mesh3d>>, names: Query<&Name>, mut frame: Local<u32>) {
    *frame += 1;
    if !w.done {
        return;
    }
    for (n, p) in &q {
        let pn = p.and_then(|p| names.get(p.parent()).ok()).map(|n| n.as_str().to_string()).unwrap_or_default();
        println!("NEWMESH frame={} {} <- {}", *frame, n.map(|n| n.as_str()).unwrap_or("?"), pn);
    }
}
