//! Port of the game's effect prefabs (Unity ParticleSystem / LineRenderer / bullet-hole decal prefabs).
//! `bevy_hanabi` (GPU) was not used: the prefabs need per-module Unity semantics (MinMaxCurve/Gradient,
//! stretched billboards, sub-modules) that are simpler and more predictable to run on the CPU, and effects
//! are short with tens of particles. See `def.rs` (data), `mat.rs` (materials), `sim.rs` (simulation).
//!
//! Usage: `Vfx::spawn("GameObject/VFX_X.prefab", pos, rot, SpawnOpts::default())`.

pub mod def;
pub mod distortion;
pub mod mat;
pub mod sim;

use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use def::*;
use mat::{MatLib, Family};
use sim::{empty_mesh, Batch, DecalFx, Emitter, FxLight, FxRoot, LineFx, Template, root_begin, root_end, sim_decals, sim_emitters, sim_trails, sim_lights, sim_lines, cleanup_particle_lights};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Resource, Default)]
pub struct VfxDb {
    prefabs: HashMap<String, Option<Arc<PrefabDef>>>,
    mats: MatLib,
    templates: HashMap<String, Option<Arc<Template>>>,
    /// One-shots started by spawned effects (`BulletSound`), played by [`play_fx_sounds`].
    pending_sounds: Vec<crate::weapons::WeaponSound>,
}

impl VfxDb {
    pub fn prefab(&mut self, key: &str) -> Option<Arc<PrefabDef>> {
        if let Some(p) = self.prefabs.get(key) {
            return p.clone();
        }
        let stem = key.rsplit('/').next().unwrap_or(key).trim_end_matches(".prefab");
        let p = match PrefabDef::load(&format!("assets/data/vfx/{stem}.json")) {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                warn!("vfx prefab {key}: {e}");
                None
            }
        };
        self.prefabs.insert(key.to_string(), p.clone());
        p
    }

    /// Unity built-in meshes (`ref-missing:<id>`: 10202 cube, 10206 cylinder, 10207 sphere).
    fn template(&mut self, key: &str) -> Option<Arc<Template>> {
        if let Some(t) = self.templates.get(key) {
            return t.clone();
        }
        let m: Mesh = if key.ends_with(":10207") {
            Sphere::new(0.5).into()
        } else if key.ends_with(":10206") {
            Cylinder::new(0.5, 2.0).into()
        } else {
            Cuboid::new(1.0, 1.0, 1.0).into()
        };
        let t = Template::from_mesh(&m).map(Arc::new);
        self.templates.insert(key.to_string(), t.clone());
        t
    }
}

#[derive(Default, Clone)]
pub struct SpawnOpts {
    /// Parent entity + its world matrix (decals stick to the hit object).
    pub parent: Option<(Entity, Mat4)>,
    pub layers: Option<RenderLayers>,
    /// Endpoints for LineRenderer prefabs (bullet trails).
    pub line: Option<(Vec3, Vec3)>,
    pub scale: Option<f32>,
    /// See `FxRoot::persistent`.
    pub persistent: bool,
    /// Initial `FxRoot::start_color`.
    pub start_color: Option<Vec4>,
    /// `FxRoot::light_intensity`.
    pub light_intensity: Option<f32>,
    /// LineRenderer lifetime override (`Destroy(obj, t)`; default 0.4 s).
    pub life: Option<f32>,
    /// With `layers`: the muzzle-flash loop `if (!Light && tag != "vfx") layer = 8` (Gun.cs:1087): "vfx"-tagged objects
    /// keep their prefab layer.
    pub keep_vfx_tag: bool,
}

#[derive(SystemParam)]
pub struct Vfx<'w, 's> {
    pub commands: Commands<'w, 's>,
    db: ResMut<'w, VfxDb>,
    meshes: ResMut<'w, Assets<Mesh>>,
    mats: ResMut<'w, Assets<StandardMaterial>>,
    pmats: ResMut<'w, Assets<mat::ParticleMat>>,
    images: ResMut<'w, Assets<Image>>,
    assets: Res<'w, AssetServer>,
}

impl Vfx<'_, '_> {
    /// Registers an in-memory effect definition (e.g. a subtree of the player prefab) under `key`.
    pub fn register(&mut self, key: &str, def: PrefabDef) {
        self.db.prefabs.insert(key.to_string(), Some(Arc::new(def)));
    }

    /// Drops the one-shot sounds of effects spawned so far this frame (pipeline warm-up).
    pub fn drop_pending_sounds(&mut self) {
        self.db.pending_sounds.clear();
    }

    pub fn particle_mats_mut(&mut self) -> &mut Assets<mat::ParticleMat> {
        &mut self.pmats
    }

    pub fn assets_mut(&mut self) -> (&mut Assets<Mesh>, &mut Assets<StandardMaterial>) {
        (&mut self.meshes, &mut self.mats)
    }

    pub fn is_registered(&self, key: &str) -> bool {
        self.db.prefabs.contains_key(key)
    }

    /// `Object.Instantiate(prefab, pos, rot)`; the root keeps the prefab's local scale.
    pub fn spawn(&mut self, key: &str, pos: Vec3, rot: Quat, opts: SpawnOpts) -> Option<Entity> {
        // Self-check log: RESTRAFE_FX_LOG=1 prints every effect instance (compare with the C# FX table).
        if std::env::var_os("RESTRAFE_FX_LOG").is_some() {
            let fwd = rot * Vec3::NEG_Z;
            println!(
                "FXSPAWN {key} at=({:.2},{:.2},{:.2}) fwd=({:.2},{:.2},{:.2}) parent={} light={:?} scale={:?} persistent={}",
                pos.x, pos.y, pos.z, fwd.x, fwd.y, fwd.z, opts.parent.is_some(), opts.light_intensity, opts.scale, opts.persistent
            );
        }
        let prefab = self.db.prefab(key)?;
        let root_def = prefab.objects.first()?;
        // Unity `Instantiate(prefab, pos, rot, parent)` keeps the prefab's localScale *relative to the parent*,
        // and `clone.transform.localScale = s` (SpawnOpts::scale) replaces it rather than multiplying.
        // (Without an override the world scale is kept, so impacts parented to scaled map boxes stay undistorted.)
        let mut tf = Transform { translation: pos, rotation: rot, scale: if opts.scale.is_some() { Vec3::ONE } else { root_def.scale } };
        if let Some((_, pm)) = opts.parent {
            tf = Transform::from_matrix(pm.inverse() * tf.to_matrix());
        }
        if let Some(s) = opts.scale {
            tf.scale = Vec3::splat(s);
        }
        let root = self.commands.spawn((Name::new(format!("vfx:{}", prefab.name)), tf, Visibility::default(), FxRoot { age: 0.0, busy: true, max_age: 12.0, extra: Vec::new(), persistent: opts.persistent, start_color: opts.start_color, light_intensity: opts.light_intensity })).id();
        if let Some((p, _)) = opts.parent {
            self.commands.entity(root).try_insert(ChildOf(p));
        }
        let mut ents: Vec<Option<Entity>> = Vec::with_capacity(prefab.objects.len());
        let mut extra: Vec<Entity> = Vec::new();
        let mut max_age = 2.0f32;
        for (n, o) in prefab.objects.iter().enumerate() {
            let node_layers = if opts.keep_vfx_tag && o.vfx_tag {
                Some(RenderLayers::layer(if o.layer == 8 { crate::weapons::markers::VIEWMODEL_LAYER } else { 0 }))
            } else {
                // No override: the prefab's own layer (HeldWeapon objects stay on the viewmodel layer).
                opts.layers.clone().or_else(|| (o.layer == 8).then(|| RenderLayers::layer(crate::weapons::markers::VIEWMODEL_LAYER)))
            };
            let parent_alive = o.parent.map(|p| ents[p].is_some()).unwrap_or(true);
            if !o.active || !parent_alive {
                ents.push(None);
                continue;
            }
            let e = if n == 0 {
                root
            } else {
                let parent = o.parent.and_then(|p| ents[p]).unwrap_or(root);
                self.commands.spawn((Name::new(o.path.clone()), Transform { translation: o.pos, rotation: o.rot, scale: o.scale }, Visibility::default(), ChildOf(parent))).id()
            };
            ents.push(Some(e));
            if let Some(pp) = &o.pp {
                self.commands.entity(e).insert(pp.clone());
            }
            if let Some(s) = &o.sound {
                // BulletSound.Start: PlayOneShot(hitClips[RoundToInt(Random.Range(0, n))]) on the object's AudioSource.
                let k = (crate::unity::random_range_f(0.0, s.clips.len() as f32) as usize).min(s.clips.len() - 1); // int Range(0, n): 0..n-1
                self.db.pending_sounds.push(crate::weapons::WeaponSound {
                    clip: s.clips[k].clone(),
                    volume: s.volume,
                    // The root sits at the spawn position (its prefab localPosition is replaced by Instantiate).
                    at: s.spatial.then_some(if n == 0 { pos } else { pos + rot * o.pos }),
                    pitch: s.pitch,
                    linear_max: s.linear_max,
                    source: None,
                    rolloff: s.rolloff.clone(),
                });
            }
            if let Some(ps) = &o.ps {
                let mat = ps.render.material.as_deref().filter(|_| ps.render.enabled).and_then(|m| self.db.mats.get(m, &mut self.mats, &mut self.pmats, &mut self.images));
                // A system without a usable material still simulates in Unity (it just draws nothing): needed for the
                // Lights-module-only systems (VFX_ChargingRay_03 Light / Light_Hitz, the latter a Death sub-emitter).
                if mat.is_some() || ps.lights.is_some() {
                    let (mut tmpl, mut mesh_src) = (None, None);
                    if ps.render.mode == 4 {
                        let key = ps.render.mesh.as_deref().unwrap_or("ref-missing:10202");
                        if let Some(name) = key.strip_prefix("asset:Mesh/") {
                            let file = format!("vfx/mesh/{}.glb", name.trim_end_matches(".asset"));
                            mesh_src = Some(self.assets.load(GltfAssetLabel::Primitive { mesh: 0, primitive: 0 }.from_asset(file)));
                        } else {
                            tmpl = self.db.template(key);
                        }
                    }
                    let mesh = self.meshes.add(empty_mesh());
                    let mut r = self.commands.spawn((Name::new(format!("vfx-render:{}", o.path)), Mesh3d(mesh.clone()), Transform::IDENTITY, Visibility::Hidden, NoFrustumCulling, NotShadowCaster, NotShadowReceiver));
                    if let Some(m) = &mat {
                        m.insert(&mut r);
                    }
                    if let Some(l) = &node_layers {
                        r.insert(l.clone());
                    }
                    let render = r.id();
                    extra.push(render);
                    let mut emitter = Emitter::new(prefab.clone(), n, root, render, mesh, mat, tmpl, mesh_src);
                    if let Some(tm) = ps.trail.as_ref().and_then(|t| self.db.mats.get(&t.material, &mut self.mats, &mut self.pmats, &mut self.images)) {
                        let tmesh = self.meshes.add(empty_mesh());
                        let mut tr = self.commands.spawn((Name::new(format!("vfx-trail:{}", o.path)), Mesh3d(tmesh.clone()), Transform::IDENTITY, Visibility::Hidden, NoFrustumCulling, NotShadowCaster, NotShadowReceiver));
                        if std::env::var_os("RESTRAFE_DEBUG_TRAIL_RED").is_some() {
                            tr.insert(MeshMaterial3d(self.mats.add(StandardMaterial { base_color: Color::srgb(1.0, 0.0, 0.0), unlit: true, cull_mode: None, ..default() })));
                        } else {
                            tm.insert(&mut tr);
                        }
                        if let Some(l) = &node_layers {
                            tr.insert(l.clone());
                        }
                        let trail_render = tr.id();
                        extra.push(trail_render);
                        emitter.trail_render = Some((trail_render, tmesh));
                    }
                    self.commands.entity(e).insert(emitter);
                    // Longest start lifetime: random modes reach their extreme at r = 0 or 1 (minScalar may exceed scalar).
                    let life = ps.duration + [0.0, 0.5, 1.0].iter().flat_map(|&t| [ps.start_life.eval(t, 0.0), ps.start_life.eval(t, 1.0)]).fold(0.0f32, f32::max) + 1.0;
                    max_age = max_age.max(life.min(10.0));
                }
            }
            if let (Some(line), Some((from, to))) = (&o.line, opts.line) {
                if let Some(mat) = line.material.as_deref().and_then(|m| self.db.mats.get(m, &mut self.mats, &mut self.pmats, &mut self.images)) {
                    let mesh = self.meshes.add(empty_mesh());
                    let mut r = self.commands.spawn((
                        Name::new("vfx-line"),
                        Mesh3d(mesh.clone()),
                        Transform::IDENTITY,
                        Visibility::Visible,
                        NoFrustumCulling,
                        NotShadowCaster,
                        NotShadowReceiver,
                        LineFx { prefab: prefab.clone(), obj: n, root, mesh, from, to, age: 0.0, life: opts.life.unwrap_or(line.life), alpha: line.fade.map(|f| f.0.w).unwrap_or(1.0) },
                    ));
                    mat.insert(&mut r);
                    if let Some(l) = &node_layers {
                        r.insert(l.clone());
                    }
                    extra.push(r.id());
                    max_age = max_age.max(opts.life.unwrap_or(line.life) + 0.05);
                }
            }
            if let Some(d) = &o.decal {
                let base = d.material.as_deref().and_then(|m| self.db.mats.get_std(m, &mut self.mats, &mut self.pmats, &mut self.images));
                if let Some(base) = base {
                    // Own material instance so the tint alpha can fade per decal.
                    let m = match &base.handle {
                        mat::MatHandle::Std(h) => self.mats.get(h).cloned(),
                        mat::MatHandle::Particle(_) => None,
                    };
                    if let Some(mut m) = m {
                        if base.family == Family::Blend || base.family == Family::Opaque || base.family == Family::Multiply {
                            m.alpha_mode = if base.family == Family::Opaque { AlphaMode::Blend } else { m.alpha_mode };
                        }
                        let orig_alpha = m.base_color.alpha();
                        let mh = self.mats.add(m);
                        // Random frame of the atlas (WFX_BulletHoleDecal.OnEnable).
                        let cells = (d.frames.x * d.frames.y) as i32;
                        let k = crate::unity::random_range_i(0, cells.max(1));
                        let (cx, cy) = ((k % d.frames.x as i32) as f32, (k / d.frames.x as i32) as f32);
                        let mut b = Batch::default();
                        let (u0, v0) = (cx / d.frames.x, cy / d.frames.y);
                        b.push_quad(Vec3::ZERO, Vec3::X * 0.5, Vec3::Y * 0.5, [1.0; 4], [u0, v0, u0 + 1.0 / d.frames.x, v0 + 1.0 / d.frames.y]);
                        let mut mesh = empty_mesh();
                        b.write(&mut mesh);
                        let rot = if d.random_rotation { Quat::from_rotation_z(crate::unity::random_range_f(0.0, std::f32::consts::TAU)) } else { Quat::IDENTITY };
                        let mut r = self.commands.spawn((
                            Name::new("vfx-decal"),
                            Mesh3d(self.meshes.add(mesh)),
                            MeshMaterial3d(mh.clone()),
                            Transform::from_rotation(rot),
                            ChildOf(e),
                            DecalFx { root, life: d.lifetime, fadeout: d.lifetime * d.fadeout_percent / 100.0, orig_alpha, material: mh },
                        ));
                        if let Some(l) = &node_layers {
                            r.insert(l.clone());
                        }
                        max_age = max_age.max(d.lifetime + 0.5);
                    }
                }
            }
            let module_light = prefab.objects.iter().any(|x| x.ps.as_ref().and_then(|p| p.lights.as_ref()).is_some_and(|l| l.obj == Some(n)));
            if let Some(l) = o.light.as_ref().filter(|l| !module_light && l.enabled) {
                let l = &crate::vfx::def::LightDef { intensity: opts.light_intensity.unwrap_or(l.intensity), ..l.clone() };
                if std::env::var_os("RESTRAFE_DEBUG_VFX_LIGHTS").is_some() {
                    println!("VFXLIGHT obj {} {} I={} range={} col={:?}", prefab.name, o.path, l.intensity, l.range, l.color);
                }
                if l.intensity > 0.0 && std::env::var_os("RESTRAFE_NO_VFX_LIGHTS").is_none() {
                    let mut r = self.commands.spawn((
                        Name::new("vfx-light"),
                        PointLight { color: Color::srgb(l.color.x, l.color.y, l.color.z), intensity: crate::unity_light::unity_point_lumens(l.intensity), range: l.range.max(1.0), radius: crate::unity_light::unity_light_radius(l.render_mode), shadow_maps_enabled: false, ..default() },
                        Transform::default(),
                        ChildOf(e),
                        crate::weapons::markers::world_and_viewmodel(),
                    ));
                    // Persistent (embedded) effects keep their lights on; one-shot effects flash them briefly.
                    if !opts.persistent {
                        r.insert(FxLight { root, life: 0.08, total: 0.08, intensity: crate::unity_light::unity_point_lumens(l.intensity) });
                    }
                }
            }
        }
        self.commands.entity(root).insert(FxRoot { age: 0.0, busy: true, max_age: max_age + 0.5, extra, persistent: opts.persistent, start_color: opts.start_color, light_intensity: opts.light_intensity });
        Some(root)
    }
}

pub struct VfxPlugin;

impl Plugin for VfxPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((MaterialPlugin::<mat::ParticleMat>::default(), distortion::DistortionPlugin)).init_resource::<VfxDb>().add_observer(despawn_fx_extras).add_systems(Update, (play_fx_sounds, mat::sync_soft_particles)).add_systems(
            PostUpdate,
            (root_begin, sim_emitters, sim_trails, sim_lines, sim_decals, sim_lights, cleanup_particle_lights, root_end).chain().after(TransformSystems::Propagate),
        );
    }
}

/// A despawned effect root (e.g. its parent item was despawned: Unity `SetActive(false)` hides the parented effect)
/// takes its unparented render entities (`FxRoot::extra`: particle meshes, trails, lights) with it.
fn despawn_fx_extras(ev: On<Remove, sim::FxRoot>, roots: Query<&sim::FxRoot>, mut commands: Commands) {
    if let Ok(r) = roots.get(ev.entity) {
        for e in &r.extra {
            commands.entity(*e).try_despawn();
        }
    }
}

fn play_fx_sounds(mut db: ResMut<VfxDb>, mut sounds: MessageWriter<crate::weapons::WeaponSound>) {
    for s in db.pending_sounds.drain(..) {
        sounds.write(s);
    }
}
