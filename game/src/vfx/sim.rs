//! CPU particle simulation + mesh generation for Unity ParticleSystems, LineRenderers and bullet-hole
//! decals. Each ParticleSystem object becomes an [`Emitter`] entity (inside the prefab hierarchy) plus a
//! top-level render entity holding one dynamic mesh with world-space vertices (camera-facing quads,
//! stretched billboards or transformed template meshes, vertex-coloured).

use super::def::*;
use super::mat::{srgb, Family, MatHandle, MatInfo};
use crate::player::fpc::PlayerCamera;
use crate::unity::random_range_f;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy_rapier3d::prelude::ReadRapierContext;
use std::sync::Arc;

const GRAVITY: f32 = 9.81;
/// Max particle-collision raycasts per emitter per frame.
const MAX_PARTICLE_RAYS: usize = 24;
const COLLIDE_MASK: u32 = (1 << crate::physics::layer::DEFAULT) | (1 << crate::physics::layer::SHOOT_THROUGH) | (1 << crate::physics::layer::INTERACT_ENVIRONMENT) | (1 << crate::physics::layer::DONT_COLLIDE_WITH_PLAYER);

/// `RESTRAFE_DEBUG_VFX=1` prints live emitters.
fn debug_vfx() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var_os("RESTRAFE_DEBUG_VFX").is_some())
}

fn rnd() -> f32 {
    random_range_f(0.0, 1.0)
}

fn rand_unit() -> Vec3 {
    loop {
        let v = Vec3::new(random_range_f(-1.0, 1.0), random_range_f(-1.0, 1.0), random_range_f(-1.0, 1.0));
        let l = v.length_squared();
        if l > 1e-4 && l <= 1.0 {
            return v / l.sqrt();
        }
    }
}

/// Root of one spawned prefab; despawned (with its extra render entities) once nothing is alive.
#[derive(Component)]
pub struct FxRoot {
    pub age: f32,
    pub busy: bool,
    pub max_age: f32,
    pub extra: Vec<Entity>,
    /// Never expires; looping systems loop forever (effects embedded in the player, e.g. highFallVfx).
    pub persistent: bool,
    /// Runtime `main.startColor` override for newly emitted particles (Unity Color32: clamped to 0..1).
    pub start_color: Option<Vec4>,
    /// Runtime Light.intensity override of every Light in the instance (Gun / DualLauncher muzzle: `lightIntensity`).
    pub light_intensity: Option<f32>,
}

/// Template mesh for mesh-mode particles.
#[derive(Clone)]
pub struct Template {
    pub pos: Vec<Vec3>,
    pub nrm: Vec<Vec3>,
    pub uv: Vec<Vec2>,
    pub idx: Vec<u32>,
}

impl Template {
    pub fn from_mesh(m: &Mesh) -> Option<Template> {
        use bevy::mesh::VertexAttributeValues as V;
        let pos = match m.attribute(Mesh::ATTRIBUTE_POSITION)? {
            V::Float32x3(v) => v.iter().map(|p| Vec3::from(*p)).collect::<Vec<_>>(),
            _ => return None,
        };
        let nrm = match m.attribute(Mesh::ATTRIBUTE_NORMAL) {
            Some(V::Float32x3(v)) => v.iter().map(|p| Vec3::from(*p)).collect(),
            _ => vec![Vec3::Y; pos.len()],
        };
        let uv = match m.attribute(Mesh::ATTRIBUTE_UV_0) {
            Some(V::Float32x2(v)) => v.iter().map(|p| Vec2::from(*p)).collect(),
            _ => vec![Vec2::ZERO; pos.len()],
        };
        let idx = match m.indices() {
            Some(Indices::U32(i)) => i.clone(),
            Some(Indices::U16(i)) => i.iter().map(|x| *x as u32).collect(),
            None => (0..pos.len() as u32).collect(),
        };
        Some(Template { pos, nrm, uv, idx })
    }
}

#[derive(Clone, Copy)]
struct Particle {
    pos: Vec3,
    vel: Vec3,
    age: f32,
    life: f32,
    size: Vec3,
    rot0: Vec3,
    rot: f32,
    /// Accumulated X / Y rotation (Rotation over Lifetime separateAxes), radians.
    rot_xy: Vec2,
    /// +1, or -1 for particles mirrored by `flipRotation`.
    rot_sign: f32,
    /// Last total velocity (velocity + Velocity over Lifetime, x speedModifier) in simulation space: what the
    /// renderer stretches / aligns along and what the *-by-Speed modules read.
    tvel: Vec3,
    color: Vec4,
    /// Per-particle random seeds: [0] lifetime/gravity/frame fallback, [1] size + curves, [2] rotation/uv/clamp,
    /// [3] colour/lights, [4..10] independent per-axis randoms (velocity / force over lifetime Y Z, X),
    /// [10] the StableRandomX vertex stream.
    r: [f32; 11],
    light: Option<Entity>,
    lit: bool,
    id: u32,
    trail: bool,
    /// Noise module size multiplier (1 + noise x sizeAmount), refreshed every update.
    nsize: f32,
}

/// Inherited parent-particle properties for a sub-emitter instance (SubEmitter `properties` bits).
#[derive(Clone, Copy, Default)]
struct Inherit {
    color: Option<Vec4>,
    size: Option<Vec3>,
    rot: Option<f32>,
    life: Option<f32>,
    /// InheritDuration: the parent system's normalised time drives the main-module curves.
    t_norm: Option<f32>,
}

/// One sub-emitter trigger: the sub system runs its own emission (rate over time + bursts over its duration) at
/// `pos` (sub system simulation space), like a short-lived copy of the system.
#[derive(Clone, Copy)]
struct SubInst {
    pos: Vec3,
    time: f32,
    first: bool,
    rate_acc: f32,
    inh: Inherit,
    /// Birth sub-emitters follow their parent particle: (parent object, parent particle id); `seen` is refreshed
    /// each frame while the parent lives.
    key: Option<(usize, u32)>,
    seen: bool,
}

/// Queued sub-emitter trigger (from a parent's Birth / Collision / Death event).
struct SubTrigger {
    root: Entity,
    target: usize,
    world_pos: Vec3,
    inh: Inherit,
    key: Option<(usize, u32)>,
}

#[derive(Component)]
pub struct Emitter {
    pub prefab: Arc<PrefabDef>,
    pub obj: usize,
    pub root: Entity,
    pub render: Entity,
    pub mesh: Handle<Mesh>,
    pub mat: Option<MatInfo>,
    pub tmpl: Option<Arc<Template>>,
    /// glb mesh still loading (becomes `tmpl`).
    pub mesh_src: Option<Handle<Mesh>>,
    particles: Vec<Particle>,
    time: f32,
    first: bool,
    delay: f32,
    emitting: bool,
    rate_acc: f32,
    next_id: u32,
    /// Trails module: particle id -> ribbon.
    trails: std::collections::HashMap<u32, TrailPts>,
    /// Trails module render entity + mesh.
    pub trail_render: Option<(Entity, Handle<Mesh>)>,
    /// Active sub-emitter instances (only on systems referenced by another system's Sub Emitters module).
    sub_insts: Vec<SubInst>,
    /// The renderer has its own copy of a FXVille blood material with _LightStr != 0 (per-renderer light factor).
    own_mat: bool,
}

impl Emitter {
    pub fn new(prefab: Arc<PrefabDef>, obj: usize, root: Entity, render: Entity, mesh: Handle<Mesh>, mat: Option<MatInfo>, tmpl: Option<Arc<Template>>, mesh_src: Option<Handle<Mesh>>) -> Emitter {
        let def = prefab.objects[obj].ps.as_ref().unwrap();
        let delay = def.start_delay.eval(0.0, rnd());
        let emitting = prefab.objects[obj].ps.as_ref().map(|p| !p.is_sub).unwrap_or(true);
        Emitter { prefab, obj, root, render, mesh, mat, tmpl, mesh_src, particles: Vec::new(), time: 0.0, first: true, delay, emitting: emitting, rate_acc: 0.0, next_id: 0, trails: Default::default(), trail_render: None, sub_insts: Vec::new(), own_mat: false }
    }
}

fn sample_shape(s: &ShapeDef) -> (Vec3, Vec3) {
    let arc = s.arc.to_radians();
    let thick = s.thickness.clamp(0.0, 1.0);
    let (mut p, mut d) = match s.kind {
        4 | 7 | 8 | 9 => {
            let phi = rnd() * arc;
            let rf = (1.0 - thick) + thick * rnd().sqrt();
            let a = s.angle.to_radians() * rf;
            (Vec3::new(phi.cos(), phi.sin(), 0.0) * s.radius * rf, Vec3::new(a.sin() * phi.cos(), a.sin() * phi.sin(), a.cos()))
        }
        2 | 3 => {
            let mut d = rand_unit();
            d.z = d.z.abs();
            let rf = (1.0 - thick) + thick * rnd().cbrt();
            (d * s.radius * rf, d)
        }
        5 | 15 | 16 => (Vec3::new(rnd() - 0.5, rnd() - 0.5, rnd() - 0.5), Vec3::Z),
        10 | 11 | 12 => {
            let phi = rnd() * arc;
            let rf = if s.kind == 10 { (1.0 - thick) + thick * rnd().sqrt() } else { 1.0 };
            (Vec3::new(phi.cos(), phi.sin(), 0.0) * s.radius * rf, Vec3::new(phi.cos(), phi.sin(), 0.0))
        }
        _ => {
            let d = rand_unit();
            let rf = (1.0 - thick) + thick * rnd().cbrt();
            (d * s.radius * rf, d)
        }
    };
    p = s.rot * (p * s.scale) + s.pos;
    d = s.rot * d;
    if s.spherical_dir > 0.0 && p.length() > 1e-4 {
        d = d.lerp(p.normalize(), s.spherical_dir);
    }
    if s.random_dir > 0.0 {
        d = d.lerp(rand_unit(), s.random_dir);
    }
    (p, d.normalize_or_zero())
}

fn emit_one(e: &mut Emitter, def: &PsDef, world: &Mat4, rot: Quat, scale: f32, t_norm: f32) {
    if e.particles.len() >= def.max_particles {
        return;
    }
    let r = [rnd(), rnd(), rnd(), rnd(), rnd(), rnd(), rnd(), rnd(), rnd(), rnd(), rnd()];
    let life = def.start_life.eval(t_norm, r[0]).max(0.01);
    let speed = def.start_speed.eval(t_norm, rnd());
    let (pu, du) = match &def.shape {
        Some(s) => sample_shape(s),
        None => (Vec3::ZERO, Vec3::Z),
    };
    let (mut pos, mut dir) = (u2p(pu), u2p(du));
    let sx = def.start_size[0].eval(t_norm, r[1]);
    // 3D Start Size / 3D Start Rotation: each axis is its own MinMaxCurve with its own random value (random modes give
    // non-uniform sizes / independent angles, as in Unity).
    let size = if def.size3d { Vec3::new(sx, def.start_size[1].eval(t_norm, rnd()), def.start_size[2].eval(t_norm, rnd())) } else { Vec3::splat(sx) };
    let rz = def.start_rot[2].eval(t_norm, r[2]);
    let rot0 = if def.rot3d { Vec3::new(def.start_rot[0].eval(t_norm, rnd()), def.start_rot[1].eval(t_norm, rnd()), rz) } else { Vec3::new(0.0, 0.0, rz) };
    // Main flipRotation: a fraction of the particles get mirrored start rotation and angular velocity.
    let rot_sign = if def.flip_rot > 0.0 && rnd() < def.flip_rot { -1.0 } else { 1.0 };
    let rot0 = rot0 * rot_sign;
    let color = def.start_color.eval(t_norm, r[3]);
    let mut vel = dir * speed;
    if def.sim_world {
        pos = world.transform_point3(pos);
        vel = rot * vel * scale;
        dir = rot * dir;
    }
    let _ = dir;
    e.next_id = e.next_id.wrapping_add(1);
    let trail = def.trail.as_ref().is_some_and(|t| t.ratio >= 1.0 || rnd() < t.ratio);
    e.particles.push(Particle { pos, vel, age: 0.0, life, size, rot0, rot: 0.0, rot_xy: Vec2::ZERO, rot_sign, tvel: vel, color, r, light: None, lit: false, id: e.next_id, trail, nsize: 1.0 });
}

/// Per-frame simulation of all emitters + mesh rebuild.
#[allow(clippy::type_complexity)]
pub fn sim_emitters(
    mut commands: Commands,
    rapier: ReadRapierContext,
    time: Res<Time>,
    cam: Query<(&GlobalTransform, Option<&Projection>), With<PlayerCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut q: Query<(&mut Emitter, &GlobalTransform, &Transform)>,
    mut roots: Query<&mut FxRoot>,
    mut vis: Query<&mut Visibility, Without<Emitter>>,
    (probes, tets, mut pmats): (Option<Res<crate::scene_map::MapProbes>>, Option<Res<crate::scene_map::MapProbeTets>>, ResMut<Assets<super::mat::ParticleMat>>),
) {
    let Ok((cam, proj)) = cam.single() else { return };
    let ctx = rapier.single().ok();
    let (cam_pos, cam_rot) = (cam.translation(), cam.rotation());
    // Viewport height per unit of view depth (ParticleSystemRenderer min/maxParticleSize are viewport fractions).
    let view_h = match proj {
        Some(Projection::Perspective(p)) => 2.0 * (p.fov * 0.5).tan(),
        _ => 2.0 * (68f32.to_radians() * 0.5).tan(),
    };
    let dt_real = time.delta_secs().min(0.05);
    // Death sub-emitter triggers: (fx root, object index, world position).
    let mut sub_triggers: Vec<SubTrigger> = Vec::new();
    for (mut e, gt, tf) in &mut q {
        let prefab = e.prefab.clone();
        let def = prefab.objects[e.obj].ps.as_ref().unwrap();
        let root_light = roots.get(e.root).ok().and_then(|r| r.light_intensity);
        let dt = dt_real * def.sim_speed;
        let (gs, grot, gpos) = gt.to_scale_rotation_translation();
        let eff_scale = match def.scaling {
            0 => gs,
            1 => tf.scale,
            // Shape: the hierarchy scale only scales the emitter shape (positions), not particle size or speed.
            _ => gs,
        };
        let sc = if def.scaling == 2 { 1.0 } else { ((eff_scale.x.abs() + eff_scale.y.abs() + eff_scale.z.abs()) / 3.0).max(1e-4) };
        let world = Mat4::from_scale_rotation_translation(eff_scale, grot, gpos);
        let inv_rot = grot.inverse();

        let (persistent, start_col) = roots.get(e.root).map(|r| (r.persistent, r.start_color)).unwrap_or((false, None));
        let n_before = e.particles.len();
        // ---- emission ----
        let mut busy = !e.particles.is_empty();
        if e.delay > 0.0 {
            e.delay -= dt;
            busy = true;
        } else if e.emitting {
            busy = true;
            let prev_t = if e.first { -1.0 } else { e.time };
            e.first = false;
            e.time += dt;
            let now = e.time;
            let mut acc = e.rate_acc;
            emit_span(&mut e, def, &world, grot, sc, prev_t, now, dt, &mut acc, None);
            e.rate_acc = acc;
            if now >= def.duration {
                if def.looping && (now < 10.0 || persistent) {
                    e.time -= def.duration;
                    e.first = true;
                } else {
                    e.emitting = false;
                }
            }
        }

        // ---- sub-emitter instances: each runs this system's emission at its trigger position ----
        if !e.sub_insts.is_empty() {
            busy = true;
            let mut insts = std::mem::take(&mut e.sub_insts);
            for s in &mut insts {
                let prev_t = if s.first { -1.0 } else { s.time };
                s.first = false;
                s.time += dt;
                let at = if def.sim_world { Mat4::from_scale_rotation_translation(eff_scale, grot, s.pos) } else { world };
                let before = e.particles.len();
                let mut acc = s.rate_acc;
                emit_span(&mut e, def, &at, grot, sc, prev_t, s.time, dt, &mut acc, s.inh.t_norm);
                s.rate_acc = acc;
                for p in &mut e.particles[before..] {
                    if !def.sim_world {
                        p.pos += s.pos;
                    }
                    if let Some(c) = s.inh.color {
                        p.color *= c;
                    }
                    if let Some(z) = s.inh.size {
                        p.size *= z;
                    }
                    if let Some(r) = s.inh.rot {
                        p.rot0.z += r;
                    }
                    if let Some(l) = s.inh.life {
                        p.life = (p.life * l).max(0.01);
                    }
                }
                if s.time >= def.duration && def.looping && s.key.is_some() {
                    s.time -= def.duration;
                    s.first = true;
                }
            }
            insts.retain(|s| s.time < def.duration);
            e.sub_insts = insts;
        }
        if let Some(c) = start_col {
            for p in &mut e.particles[n_before..] {
                p.color = c;
            }
        }

        // ---- particle update ----
        let t_emit = (e.time / def.duration).clamp(0.0, 1.0);
        let g_world = Vec3::new(0.0, -GRAVITY, 0.0);
        let to_sim = |v: Vec3, world_space: bool| -> Vec3 {
            // vector given in world (true) or emitter-local (false) axes -> simulation space
            if def.sim_world {
                if world_space { v } else { grot * v }
            } else if world_space {
                inv_rot * v
            } else {
                v
            }
        };
        let e_time = e.time;
        for p in &mut e.particles {
            p.age += dt;
        }
        let root_now = e.root;
        let obj_now = e.obj;
        // Dead particles are removed like Unity's particle buffer does it: the last particle is moved into the dead
        // slot (this is the draw order of Sort Mode None).
        let mut k = 0;
        while k < e.particles.len() {
            let p = e.particles[k];
            if p.age < p.life {
                k += 1;
                continue;
            }
            let wp = if def.sim_world { p.pos } else { world.transform_point3(p.pos) };
            push_subs(&mut sub_triggers, def, 2, root_now, e.obj, &p, wp, e.time / def.duration, false);
            if let Some(l) = p.light {
                commands.entity(l).try_despawn();
            }
            e.particles.swap_remove(k);
        }
        let mut ray_budget = MAX_PARTICLE_RAYS;
        let mut n_lights = e.particles.iter().filter(|p| p.light.is_some()).count();
        let emitter_scale = sc;
        let root_e = e.root;
        for p in &mut e.particles {
            let nt = p.age / p.life;
            // Lights module: one light per particle (ratio chance), capped by maxLights.
            if let Some(ld) = def.lights.as_ref().filter(|_| !p.lit) {
                p.lit = true;
                if ld.obj.is_some() && n_lights < ld.max_lights && p.r[2] < ld.ratio && std::env::var_os("RESTRAFE_NO_VFX_LIGHTS").is_none() {
                    if std::env::var_os("RESTRAFE_DEBUG_VFX_LIGHTS").is_some() {
                        let lo = prefab.objects[ld.obj.unwrap()].light.as_ref().unwrap();
                        println!("VFXLIGHT module {} I={} range={} alpha_int={} size_rng={} int={:?} rng={:?}", prefab.name, lo.intensity, lo.range, ld.alpha_affects_intensity, ld.size_affects_range, ld.intensity, ld.range);
                    }
                    let lo = prefab.objects[ld.obj.unwrap()].light.as_ref().unwrap();
                    let id = commands
                        .spawn((
                            Name::new("vfx-particle-light"),
                            PointLight { color: Color::srgb(lo.color.x, lo.color.y, lo.color.z), intensity: 0.0, range: lo.range.max(1.0), radius: crate::unity_light::unity_light_radius(lo.render_mode), shadow_maps_enabled: false, ..default() },
                            Transform::from_translation(if def.sim_world { p.pos } else { world.transform_point3(p.pos) }),
                            FxParticleLight { root: root_e },
                            crate::weapons::markers::world_and_viewmodel(),
                        ))
                        .id();
                    p.light = Some(id);
                    n_lights += 1;
                }
            }
            let old_pos = p.pos;
            let grav = def.gravity.eval(t_emit, p.r[0]);
            if grav != 0.0 {
                p.vel += to_sim(g_world * grav, true) * dt;
            }
            if let Some(fm) = &def.force {
                // Random modes pick an independent value per axis.
                let f = Vec3::new(fm.x.eval(nt, p.r[7]), fm.y.eval(nt, p.r[8]), -fm.z.eval(nt, p.r[9]));
                p.vel += to_sim(f, fm.world) * dt;
            }
            if let Some((mag, dampen, drag, by_size, by_vel)) = &def.clamp {
                let lim = mag.eval(nt, p.r[2]);
                let sp = p.vel.length();
                // A speed limit of 0 is valid: every moving particle exceeds it and loses `dampen` of its speed
                // (the blood packs use limit 0 + dampen 0.02..0.05 as air drag).
                if lim >= 0.0 && sp > lim {
                    let k = 1.0 - (1.0 - dampen.clamp(0.0, 1.0)).powf(dt * 60.0);
                    p.vel = p.vel.lerp(p.vel / sp * lim, k);
                }
                let mut d = drag.eval(nt, p.r[2]);
                if d > 0.0 && (*by_size || *by_vel) {
                    // Drag-equation form of multiplyDragByParticleSize (x cross-section area pi r^2 of the current size,
                    // r = largest axis / 2) and multiplyDragByParticleVelocity (x speed^2): speed -= drag * dt. The v^2
                    // case is integrated exactly (v / (1 + k v dt)) so large coefficients stay stable at any frame rate.
                    // (All 3 drag users, the Losange impact smoke, set both flags.)
                    if *by_size {
                        let mut s = p.size;
                        if let Some(sl) = &def.size_life {
                            s *= if sl.separate { Vec3::new(sl.x.eval(nt, p.r[1]), sl.y.eval(nt, p.r[1]), sl.z.eval(nt, p.r[1])) } else { Vec3::splat(sl.x.eval(nt, p.r[1])) };
                        }
                        let r = s.abs().max_element() * emitter_scale * 0.5;
                        d *= std::f32::consts::PI * r * r;
                    }
                    let sp = p.vel.length();
                    if sp > 1e-6 {
                        let ns = if *by_vel { sp / (1.0 + d * sp * dt) } else { (sp - d * dt).max(0.0) };
                        p.vel *= ns / sp;
                    }
                } else if d > 0.0 {
                    p.vel *= (1.0 - d * dt).max(0.0);
                }
            }
            let mut total = p.vel;
            if let Some(v) = &def.vel {
                // Random modes (TwoConstants / TwoCurves) pick an independent value per axis.
                let ol = Vec3::new(v.x.eval(nt, p.r[4]), v.y.eval(nt, p.r[5]), -v.z.eval(nt, p.r[6]));
                total += to_sim(ol, v.world);
            }
            if let Some(rm) = &def.radial {
                // Radial velocity: away from the system origin (sim space: local origin, or the emitter position).
                let from = if def.sim_world { p.pos - gpos } else { p.pos };
                total += from.normalize_or_zero() * rm.eval(nt, p.r[4]);
            }
            if let Some(sm) = &def.speed_mod {
                total *= sm.eval(nt, p.r[1]);
            }
            p.tvel = total;
            p.pos += total * dt;
            if let Some(nd) = &def.noise {
                let (dp, drot, dsize) = noise_sample(nd, p.pos, e_time, nt, p.r[3]);
                p.pos += dp * dt;
                p.rot += drot * p.rot_sign * dt;
                p.nsize = dsize;
            }
            // Collision module (world): raycast the particle's step, bounce / dampen / lifetime loss.
            if let (Some(cd), Some(ctx)) = (def.collision.as_ref(), ctx.as_ref()) {
                let step = p.pos - old_pos;
                if ray_budget > 0 && step.length() > 1e-4 && p.age > 0.0 {
                    ray_budget -= 1;
                    let (ow, nw) = if def.sim_world { (old_pos, p.pos) } else { (world.transform_point3(old_pos), world.transform_point3(p.pos)) };
                    let wl = (nw - ow).length();
                    let radius = (p.size.x * 0.5 * cd.radius_scale * emitter_scale).min(0.2);
                    if wl > 1e-5 {
                        let dir = (nw - ow) / wl;
                        if let Some(h) = crate::physics::raycast(ctx, ow, dir, wl + radius, COLLIDE_MASK) {
                            let nrm = h.normal.normalize_or_zero();
                            if nrm != Vec3::ZERO {
                                let hw = h.point + nrm * radius;
                                p.pos = if def.sim_world { hw } else { world.inverse().transform_point3(hw) };
                                let ns = if def.sim_world { nrm } else { inv_rot * nrm };
                                let vn = ns * p.vel.dot(ns);
                                let vt = p.vel - vn;
                                let bounce = cd.bounce.eval(nt, p.r[1]).max(0.0);
                                let damp = cd.dampen.eval(nt, p.r[1]).clamp(0.0, 1.0);
                                p.vel = vt * (1.0 - damp) - vn * bounce;
                                p.age += p.life * cd.lifetime_loss.eval(nt, p.r[1]).clamp(0.0, 1.0);
                                push_subs(&mut sub_triggers, def, 1, root_e, obj_now, p, hw, t_emit, false);
                                let sp = p.vel.length();
                                if sp < cd.min_kill || sp > cd.max_kill {
                                    p.age = p.life;
                                }
                            }
                        }
                    }
                }
            }
            if let (Some(ld), Some(l)) = (def.lights.as_ref(), p.light) {
                let lo = prefab.objects[ld.obj.unwrap()].light.as_ref().unwrap();
                let lo_intensity = root_light.unwrap_or(lo.intensity);
                let mut c = p.color;
                if let Some(cl) = &def.color_life {
                    c *= cl.eval(nt, p.r[3]);
                }
                let wp = if def.sim_world { p.pos } else { world.transform_point3(p.pos) };
                // Unity Lights module: intensity = template * intensityCurve (x particle alpha if alphaAffectsIntensity),
                // range = template * rangeCurve (x particle size if sizeAffectsRange).
                let k = ld.intensity.eval(nt, p.r[3]) * if ld.alpha_affects_intensity { c.w.clamp(0.0, 1.0) } else { 1.0 };
                let mut rg = ld.range.eval(nt, p.r[3]).max(0.01);
                if ld.size_affects_range {
                    let mut s = p.size.x;
                    if let Some(sl) = &def.size_life {
                        s *= sl.x.eval(nt, p.r[1]);
                    }
                    rg *= s.max(0.0);
                }
                let tint = if ld.use_color { Vec3::new(c.x, c.y, c.z) } else { Vec3::ONE };
                commands.entity(l).try_insert((
                    Transform::from_translation(wp),
                    PointLight {
                        color: Color::srgb(lo.color.x * tint.x, lo.color.y * tint.y, lo.color.z * tint.z),
                        intensity: crate::unity_light::unity_point_lumens(lo_intensity * k),
                        radius: crate::unity_light::unity_light_radius(lo.render_mode),
                        range: (lo.range * rg).max(1.0),
                        shadow_maps_enabled: false,
                        ..default()
                    },
                ));
            }
            if let Some(rl) = &def.rot_life {
                p.rot += rl.eval(nt, p.r[2]) * p.rot_sign * dt;
            }
            if let Some((rx, ry)) = &def.rot_life_xy {
                p.rot_xy += Vec2::new(rx.eval(nt, p.r[2]), ry.eval(nt, p.r[2])) * p.rot_sign * dt;
            }
            if let Some((rs, range)) = &def.rot_speed {
                let s = ((p.tvel.length() - range.x) / (range.y - range.x).max(1e-5)).clamp(0.0, 1.0);
                p.rot += rs.eval(s, p.r[2]) * p.rot_sign * dt;
            }
        }
        // Birth sub-emitters: one instance per live particle, following it (removed when the particle dies).
        if def.subs.iter().any(|s| s.kind == 0 && s.idx.is_some()) {
            for p in &e.particles {
                let wp = if def.sim_world { p.pos } else { world.transform_point3(p.pos) };
                push_subs(&mut sub_triggers, def, 0, root_now, obj_now, p, wp, t_emit, true);
            }
        }
        busy |= !e.particles.is_empty();
        if let Ok(mut r) = roots.get_mut(e.root) {
            r.busy |= busy;
        }

        if debug_vfx() && !e.particles.is_empty() {
            println!("VFX {} particles={} mat={} t={:.3} gpos={:.3?} gs={:.4?} cam={:.3?} p0={:.3?}", prefab.objects[e.obj].path, e.particles.len(), e.mat.is_some(), e.time, gpos, gs, cam_pos, e.particles.first().map(|p| if def.sim_world { p.pos } else { world.transform_point3(p.pos) }));
        }
        if e.tmpl.is_none() {
            if let Some(h) = &e.mesh_src {
                if let Some(m) = meshes.get(h) {
                    e.tmpl = Template::from_mesh(m).map(Arc::new);
                    e.mesh_src = None;
                }
            }
        }
        // ---- mesh ----
        let Ok(mut v) = vis.get_mut(e.render) else { continue };
        if e.particles.is_empty() || e.mat.is_none() {
            if *v != Visibility::Hidden {
                *v = Visibility::Hidden;
            }
            continue;
        }
        *v = Visibility::Visible;
        // FXVille blood _LightStr != 0: v4 = 1 + _LightStr * (max(ShadeSH9(n), 0.15 * (sky + equator + ground)) - 1) with the
        // renderer's light-probe SH (Unity: one per renderer, at its bounds centre) and the billboard normal (towards the
        // camera, the same for every particle). Per-emitter material copy, value updated every frame.
        if let (Some(MatHandle::Particle(h)), Some(probes), Some(tets)) = (e.mat.as_ref().map(|m| m.handle.clone()), probes.as_ref(), tets.as_ref()) {
            let ls = pmats.get(&h).map(|m| m.params.p3.z).unwrap_or(0.0);
            if ls != 0.0 {
                let h = if e.own_mat {
                    h
                } else {
                    let Some(copy) = pmats.get(&h).cloned() else { continue };
                    let nh = pmats.add(copy);
                    commands.entity(e.render).try_insert(MeshMaterial3d(nh.clone()));
                    if let Some(m) = e.mat.as_mut() {
                        m.handle = MatHandle::Particle(nh.clone());
                    }
                    e.own_mat = true;
                    nh
                };
                let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                for p in &e.particles {
                    let w = if def.sim_world { p.pos } else { gt.transform_point(p.pos) };
                    lo = lo.min(w);
                    hi = hi.max(w);
                }
                if let Some(sh) = crate::scene_map::light_probe_sh(probes, tets, (lo + hi) * 0.5) {
                    let n = cam_rot * Vec3::Z; // towards the camera; Unity space = z mirrored
                    let e9 = crate::scene_map::shade_sh9(&sh, Vec3::new(n.x, n.y, -n.z));
                    let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
                    let amb = crate::scene_map::MAP_AMBIENT_SRGB.iter().fold(Vec3::ZERO, |a, c| a + Vec3::new(lin(c[0]), lin(c[1]), lin(c[2])));
                    let f = Vec3::ONE + ls * (e9.max(amb * 0.15) - Vec3::ONE);
                    if let Some(mut m) = pmats.get_mut(&h) {
                        m.params.light = f.extend(1.0);
                    }
                }
            }
        }
        let mat = e.mat.clone().unwrap();
        let mut b = Batch::default();
        let rd = &def.render;
        let (fwd, cam_right, cam_up) = (cam_rot * Vec3::NEG_Z, cam_rot * Vec3::X, cam_rot * Vec3::Y);
        // Renderer Sort Mode (draw order = later on top). None keeps the particle buffer order.
        let mut order: Vec<usize> = (0..e.particles.len()).collect();
        if mat.family != Family::Additive && rd.sort != 0 {
            let wp = |p: &Particle| if def.sim_world { p.pos } else { world.transform_point3(p.pos) };
            let key = |p: &Particle| -> f32 {
                match rd.sort {
                    1 => -(wp(p) - cam_pos).length_squared(), // By Distance: far first
                    2 => -p.age,                              // Oldest in Front: youngest first
                    3 => p.age,                               // Youngest in Front: oldest first
                    _ => -(wp(p) - cam_pos).dot(fwd),         // By Depth
                }
            };
            order.sort_by(|&a, &c| key(&e.particles[a]).partial_cmp(&key(&e.particles[c])).unwrap_or(std::cmp::Ordering::Equal));
        }
        for &n in &order {
            let p = &e.particles[n];
            let nt = (p.age / p.life).clamp(0.0, 1.0);
            // Total velocity (incl. Velocity over Lifetime): stretched billboards and Velocity alignment follow it.
            let (wpos, wvel) = if def.sim_world { (p.pos, p.tvel) } else { (world.transform_point3(p.pos), grot * p.tvel) };
            // size
            let mut size = p.size;
            if let Some(sl) = &def.size_life {
                if sl.separate {
                    size *= Vec3::new(sl.x.eval(nt, p.r[1]), sl.y.eval(nt, p.r[1]), sl.z.eval(nt, p.r[1]));
                } else {
                    size *= sl.x.eval(nt, p.r[1]);
                }
            }
            if let Some((ss, range)) = &def.size_speed {
                let s = ((p.tvel.length() - range.x) / (range.y - range.x).max(1e-5)).clamp(0.0, 1.0);
                size *= if ss.separate { Vec3::new(ss.x.eval(s, p.r[1]), ss.y.eval(s, p.r[1]), ss.z.eval(s, p.r[1])) } else { Vec3::splat(ss.x.eval(s, p.r[1])) };
            }
            size *= sc * p.nsize;
            // Renderer min/maxParticleSize (Billboard mode): the on-screen size is clamped to [min, max] x viewport, so a
            // particle right in front of the camera (muzzle flash, rocket trail puff at spawn) covers at most
            // m_MaxParticleSize (default 0.5) of the screen instead of filling it.
            if rd.mode == 0 {
                let depth = (wpos - cam_pos).dot(fwd).max(1e-4);
                let (lo, hi) = (rd.min_size * view_h * depth, rd.max_size * view_h * depth);
                let s = size.x.max(size.y);
                if s > hi && s > 0.0 {
                    size *= hi / s;
                } else if s < lo && s > 0.0 {
                    size *= lo / s;
                }
            }
            // colour
            let mut c = p.color;
            if let Some(cl) = &def.color_life {
                c *= cl.eval(nt, p.r[3]);
            }
            // Lit surface materials (S_Surfaces debris): the dark start colours assume scene lighting; show the texture.
            // Vertex colours: linearised only with ParticleSystemRenderer.m_ApplyActiveColorSpace (legacy renderers pass raw).
            let lc = |v: f32| if rd.apply_cs { srgb(v) } else { v.max(0.0) };
            let col = if mat.family == Family::Opaque { [0.55, 0.55, 0.55, 1.0] } else { [lc(c.x), lc(c.y), lc(c.z), c.w.clamp(0.0, 1.0)] };
            if col[3] <= 0.001 {
                continue;
            }
            // uv
            let (mut cols, mut rows, mut frame) = (1, 1, 0usize);
            if let Some(uv) = &def.uv {
                cols = uv.tiles_x;
                rows = uv.tiles_y;
                let total = (cols * rows) as f32;
                let f = (uv.start.eval(nt, p.r[2]) + uv.frame.eval(nt, p.r[2]) * uv.cycles).rem_euclid(1.0);
                frame = ((f * total) as usize).min((cols * rows - 1) as usize);
            } else if mat.flipbook != (1, 1) {
                cols = mat.flipbook.0;
                rows = mat.flipbook.1;
                frame = ((p.r[0] * (cols * rows) as f32) as usize).min((cols * rows - 1) as usize);
            }
            let (u0, v0) = ((frame as i32 % cols) as f32 / cols as f32, (frame as i32 / cols) as f32 / rows as f32);
            let mut uvr = [u0, v0, u0 + 1.0 / cols as f32, v0 + 1.0 / rows as f32];
            // FXVille Blood custom vertex streams (renderer m_VertexStreams; Unity packs UV + StableRandomX +
            // AgePercent into TEXCOORD0 and Custom1 into TEXCOORD1, missing components read 0). The vertex program
            // (DXBC blob 3) mirrors the UV per particle (uv *= 1 + _Flip * (round(frac(rand * (13, 8))) * 2 - 2),
            // sampled with Repeat) and drops the particle in clip space: clip.y += Custom1.z * (age^2 + age *
            // (_FallOffset + (rand - 0.5) * _FallRandomness)). The camera renders into a render texture, whose D3D
            // projection is y-flipped, so +clip.y is down on screen: a view-space shift of -dy / P[1][1].
            let mut wpos = wpos;
            let mut blood_vtx: Option<([f32; 2], f32)> = None;
            // Debug: RESTRAFE_VFX_NO_CUSTOM=1 drops the custom streams (before/after comparisons).
            static NO_CUSTOM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            let no_custom = *NO_CUSTOM.get_or_init(|| std::env::var_os("RESTRAFE_VFX_NO_CUSTOM").is_some());
            if let Some(bv) = mat.blood.as_ref().filter(|_| !no_custom) {
                let has = |s: u8| rd.streams.contains(&s);
                let rand = if has(0x17) { p.r[10] } else { 0.0 };
                let age = if has(0x15) { nt } else { 0.0 };
                let comps = if has(0x22) { 4 } else if has(0x21) { 3 } else if has(0x20) { 2 } else if has(0x1f) { 1 } else { 0 };
                let mut cv = def.custom1.as_ref().map(|c| c.eval(nt, [p.r[6], p.r[7], p.r[8], p.r[9]])).unwrap_or(Vec4::ZERO);
                for k in comps..4 {
                    cv[k] = 0.0;
                }
                let fl = |m: f32, k: f32| 1.0 + m * (((rand * k).fract()).round() * 2.0 - 2.0);
                let (fu, fv) = (fl(bv.flip.x, 13.0), fl(bv.flip.y, 8.0));
                uvr = [uvr[0] * fu, uvr[1] * fv, uvr[2] * fu, uvr[3] * fv];
                let dy = cv.z * (age * age + age * (bv.fall_offset + (rand - 0.5) * bv.fall_random));
                wpos -= cam_up * dy * view_h * 0.5;
                blood_vtx = Some(([cv.x, cv.y], rand));
            }
            let n_vtx = b.pos.len();
            let angle = p.rot0.z + p.rot;
            // Particle rotation (Unity Euler ZXY: Ry * Rx * Rz) converted to the port's Z-mirrored space: X and Y
            // angles flip sign, Z (the roll about the facing axis) keeps it.
            let prot = if def.rot3d { Quat::from_euler(EulerRot::YXZ, -(p.rot0.y + p.rot_xy.y), -(p.rot0.x + p.rot_xy.x), angle) } else { Quat::from_rotation_z(angle) };
            // Render alignment Velocity: the particle faces along its velocity (Unity LookRotation(velocity); in port
            // space local -Z maps to the direction), then its own rotation is applied on top.
            let vel_frame = || {
                let sp = wvel.length();
                if sp > 1e-6 {
                    let d = wvel / sp;
                    Transform::IDENTITY.looking_to(d, if d.y.abs() > 0.999 { Vec3::Z } else { Vec3::Y }).rotation
                } else {
                    grot
                }
            };

            match rd.mode {
                4 => {
                    let Some(t) = &e.tmpl else { continue };
                    let q = match rd.align {
                        1 => Quat::IDENTITY,
                        0 | 3 => cam_rot,
                        4 => vel_frame(),
                        _ => grot,
                    } * prot;
                    b.push_mesh(t, wpos, q, size, col);
                }
                1 => {
                    // stretched billboard along the velocity
                    let speed = wvel.length();
                    let dir = if speed > 1e-4 { wvel / speed } else { grot * Vec3::NEG_Z };
                    let len = size.x * rd.length_scale.max(0.0) + speed * rd.velocity_scale;
                    let to_cam = (cam_pos - wpos).normalize_or_zero();
                    let side = dir.cross(to_cam).normalize_or_zero();
                    let side = if side == Vec3::ZERO { cam_right } else { side };
                    // The texture's U axis runs along the velocity (head = U 1): every stretched texture in the data is
                    // authored horizontally (blood spray masks fan out to +U, WFX smoke spikes lead with their +U tip).
                    // Renderer pivot (particle-size units, Y = along the stretch for the FXVille blood: 0.25 / 0.375)
                    // shifts the quad forward so the particle sits near the tail.
                    let centre = wpos + dir * (len * rd.pivot.y) - side * (size.x * rd.pivot.x);
                    b.push_quad(centre, dir * len * 0.5, side * size.x * 0.5, col, uvr);
                }
                _ => {
                    let (right, up) = match rd.align {
                        0 | 3 if !def.rot3d => {
                            let (s, c2) = angle.sin_cos();
                            (cam_right * c2 + cam_up * s, cam_up * c2 - cam_right * s)
                        }
                        a => {
                            let base = match a {
                                // View with 3D rotation: the rotation is applied in camera space.
                                0 | 3 => cam_rot,
                                1 => Quat::IDENTITY,
                                4 => vel_frame(),
                                _ => grot,
                            };
                            let q = base * prot;
                            (q * Vec3::X, q * Vec3::Y)
                        }
                    };
                    let (hx, hy) = (right * size.x * 0.5, up * size.y * 0.5);
                    let centre = wpos + hx * (2.0 * rd.pivot.x) + hy * (2.0 * rd.pivot.y);
                    b.push_quad(centre, hx, hy, col, uvr);
                }
            }
            if let Some((c, rand)) = blood_vtx {
                b.extra(n_vtx, c, [1.0, 0.0, 0.0, rand]);
            }
        }
        let _ = fwd;
        if b.pos.is_empty() {
            *v = Visibility::Hidden;
            continue;
        }
        if let Some(mut m) = meshes.get_mut(&e.mesh) {
            b.write_sorted(&mut m, cam_pos, fwd, rd.fudge, rd.order);
        }
    }
    // Sub-emitter triggers: start an instance of the target system at the parent particle (Birth instances are
    // refreshed while their parent lives and dropped with it).
    for (mut e, _, _) in &mut q {
        for s in &mut e.sub_insts {
            s.seen = false;
        }
    }
    for t in sub_triggers {
        if let Ok(mut r) = roots.get_mut(t.root) {
            r.busy = true;
        }
        for (mut e, gt, tf) in &mut q {
            if e.root != t.root || e.obj != t.target {
                continue;
            }
            let prefab = e.prefab.clone();
            let def = prefab.objects[t.target].ps.as_ref().unwrap();
            let pos = if def.sim_world {
                t.world_pos
            } else {
                let (gs, grot, gpos) = gt.to_scale_rotation_translation();
                let eff = if def.scaling == 1 { tf.scale } else { gs };
                Mat4::from_scale_rotation_translation(eff, grot, gpos).inverse().transform_point3(t.world_pos)
            };
            if let Some(s) = t.key.and_then(|k| e.sub_insts.iter_mut().find(|s| s.key == Some(k))) {
                s.pos = pos;
                s.inh = t.inh;
                s.seen = true;
                continue;
            }
            e.sub_insts.push(SubInst { pos, time: 0.0, first: true, rate_acc: 0.0, inh: t.inh, key: t.key, seen: true });
        }
    }
    for (mut e, _, _) in &mut q {
        if e.sub_insts.iter().any(|s| s.key.is_some() && !s.seen) {
            e.sub_insts.retain(|s| s.key.is_none() || s.seen);
        }
    }
}

/// Rate over time + bursts of one emission window (prev_t, now] of a system (or of a sub-emitter instance).
#[allow(clippy::too_many_arguments)]
fn emit_span(e: &mut Emitter, def: &PsDef, world: &Mat4, rot: Quat, sc: f32, prev_t: f32, now: f32, dt: f32, rate_acc: &mut f32, t_override: Option<f32>) {
    let t_norm = t_override.unwrap_or((now / def.duration).clamp(0.0, 1.0));
    *rate_acc += def.rate.eval(t_norm, rnd()) * dt;
    while *rate_acc >= 1.0 {
        *rate_acc -= 1.0;
        emit_one(e, def, world, rot, sc, t_norm);
    }
    // bursts (fire when their time falls in (prev, now])
    for b in &def.bursts {
        // cycleCount 0 = infinite: every repeatInterval until the end of the duration.
        let cycles = if b.cycles == 0 { (((def.duration - b.time) / b.interval.max(1e-3)).floor() as i32 + 1).clamp(1, 10_000) } else { b.cycles };
        for c in 0..cycles {
            let tc = b.time + c as f32 * b.interval;
            if tc > prev_t && tc <= now.max(1e-6) && (b.prob >= 1.0 || rnd() < b.prob) {
                let n = b.count.eval(t_norm, rnd()).round().max(0.0) as i32;
                for _ in 0..n {
                    emit_one(e, def, world, rot, sc, t_norm);
                }
            }
        }
    }
}

/// Queue the `kind` (0 Birth, 1 Collision, 2 Death) sub-emitters of `def` for parent particle `p` at world `wp`,
/// with the inherited properties (Color = current colour, Size = current size, Rotation = current angle,
/// Lifetime = remaining lifetime as a multiplier, Duration = the parent system's normalised time).
#[allow(clippy::too_many_arguments)]
fn push_subs(out: &mut Vec<SubTrigger>, def: &PsDef, kind: i32, root: Entity, obj: usize, p: &Particle, wp: Vec3, sys_t: f32, keyed: bool) {
    for s in def.subs.iter().filter(|s| s.kind == kind) {
        let Some(target) = s.idx else { continue };
        // Birth instances roll their probability once (at the parent's birth), not every frame.
        if !keyed && s.prob < 1.0 && rnd() >= s.prob {
            continue;
        }
        if keyed && s.prob < 1.0 && p.r[9] >= s.prob {
            continue;
        }
        let nt = (p.age / p.life).clamp(0.0, 1.0);
        let mut inh = Inherit::default();
        if s.props & 1 != 0 {
            inh.color = Some(p.color * def.color_life.as_ref().map(|c| c.eval(nt, p.r[3])).unwrap_or(Vec4::ONE));
        }
        if s.props & 2 != 0 {
            let mut z = p.size;
            if let Some(sl) = &def.size_life {
                z *= if sl.separate { Vec3::new(sl.x.eval(nt, p.r[1]), sl.y.eval(nt, p.r[1]), sl.z.eval(nt, p.r[1])) } else { Vec3::splat(sl.x.eval(nt, p.r[1])) };
            }
            inh.size = Some(z);
        }
        if s.props & 4 != 0 {
            inh.rot = Some(p.rot0.z + p.rot);
        }
        if s.props & 8 != 0 {
            inh.life = Some((p.life - p.age).max(0.0));
        }
        if s.props & 16 != 0 {
            inh.t_norm = Some(sys_t.clamp(0.0, 1.0));
        }
        out.push(SubTrigger { root, target, world_pos: wp, inh, key: keyed.then_some((obj, p.id)) });
    }
}

#[derive(Default)]
pub struct Batch {
    pub pos: Vec<[f32; 3]>,
    pub nrm: Vec<[f32; 3]>,
    pub uv: Vec<[f32; 2]>,
    pub col: Vec<[f32; 4]>,
    pub idx: Vec<u32>,
    /// Optional TEXCOORD1 (custom stream) and tangent (w = StableRandomX) per vertex (FXVille blood).
    pub uv1: Vec<[f32; 2]>,
    pub tan: Vec<[f32; 4]>,
}

impl Batch {
    pub fn push_quad(&mut self, c: Vec3, hx: Vec3, hy: Vec3, col: [f32; 4], uv: [f32; 4]) {
        let n = hx.cross(hy).normalize_or_zero().to_array();
        let base = self.pos.len() as u32;
        // top-left, top-right, bottom-right, bottom-left (image space v grows downward)
        for (s, t, u, v) in [(-1.0, 1.0, uv[0], uv[1]), (1.0, 1.0, uv[2], uv[1]), (1.0, -1.0, uv[2], uv[3]), (-1.0, -1.0, uv[0], uv[3])] {
            self.pos.push((c + hx * s + hy * t).to_array());
            self.nrm.push(n);
            self.uv.push([u, v]);
            self.col.push(col);
        }
        self.idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Set the custom-stream attributes of the vertices pushed since `from`. For a billboard quad the tangent's xyz is
    /// its +u edge (Unity's Tangent stream: the particle's right axis); `tan.w` carries StableRandomX.
    pub fn extra(&mut self, from: usize, uv1: [f32; 2], tan: [f32; 4]) {
        self.uv1.resize(from, [0.0; 2]);
        self.tan.resize(from, [1.0, 0.0, 0.0, 0.0]);
        let mut t = tan;
        if self.pos.len() == from + 4 {
            let r = (Vec3::from_array(self.pos[from + 1]) - Vec3::from_array(self.pos[from])).normalize_or_zero();
            if r != Vec3::ZERO {
                t = [r.x, r.y, r.z, tan[3]];
            }
        }
        for _ in from..self.pos.len() {
            self.uv1.push(uv1);
            self.tan.push(t);
        }
    }

    pub fn push_mesh(&mut self, t: &Template, c: Vec3, q: Quat, scale: Vec3, col: [f32; 4]) {
        let base = self.pos.len() as u32;
        for (i, p) in t.pos.iter().enumerate() {
            self.pos.push((c + q * (*p * scale)).to_array());
            self.nrm.push((q * t.nrm[i]).to_array());
            self.uv.push(t.uv[i].to_array());
            self.col.push(col);
        }
        self.idx.extend(t.idx.iter().map(|i| base + i));
    }

    /// [`Self::write`] for a particle renderer, plus Unity's transparent sort key. Unity sorts the transparent queue
    /// by sorting layer, then `sortingOrder`, then the camera distance (perspective: Euclidean, camera position to the
    /// renderer's bounds centre) plus `ParticleSystemRenderer.sortingFudge`, back to front. Bevy sorts Transparent3d
    /// items by the view-space Z of the mesh AABB centre (`RenderMesh::aabb_center`, recomputed whenever the mesh
    /// asset changes, i.e. every frame here). Two vertices that no index references move that AABB centre onto the
    /// camera axis at the Unity key distance (`sortingOrder` dominating via `ORDER_SPAN`), so the item sorts like
    /// Unity's renderer without drawing anything extra (the render entities have `NoFrustumCulling`).
    pub fn write_sorted(mut self, m: &mut Mesh, cam_pos: Vec3, cam_fwd: Vec3, fudge: f32, order: i32) {
        /// Metres of sort distance per `sortingOrder` step (orders in the data: -10..1).
        const ORDER_SPAN: f32 = 2000.0;
        static OLD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        // RESTRAFE_VFX_OLD_SORT=1: plain bounds-centre view depth (before/after comparisons).
        let old = *OLD.get_or_init(|| std::env::var_os("RESTRAFE_VFX_OLD_SORT").is_some());
        if !self.pos.is_empty() && !old {
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for p in &self.pos {
                let v = Vec3::from_array(*p);
                lo = lo.min(v);
                hi = hi.max(v);
            }
            let key = ((lo + hi) * 0.5 - cam_pos).length() + fudge - order as f32 * ORDER_SPAN;
            let s = cam_pos + cam_fwd * key;
            let h = (hi - s).abs().max((lo - s).abs()) + Vec3::splat(1e-3);
            for v in [s - h, s + h] {
                self.pos.push(v.to_array());
                self.nrm.push([0.0, 1.0, 0.0]);
                self.uv.push([0.0, 0.0]);
                self.col.push([0.0; 4]);
            }
        }
        if !self.uv1.is_empty() {
            self.uv1.resize(self.pos.len(), [0.0; 2]);
            self.tan.resize(self.pos.len(), [1.0, 0.0, 0.0, 0.0]);
        }
        self.write(m);
    }

    pub fn write(self, m: &mut Mesh) {
        m.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.pos);
        m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.nrm);
        m.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uv);
        m.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.col);
        if !self.uv1.is_empty() {
            m.insert_attribute(Mesh::ATTRIBUTE_UV_1, self.uv1);
            m.insert_attribute(Mesh::ATTRIBUTE_TANGENT, self.tan);
        } else {
            m.remove_attribute(Mesh::ATTRIBUTE_UV_1);
            m.remove_attribute(Mesh::ATTRIBUTE_TANGENT);
        }
        m.insert_indices(Indices::U32(self.idx));
    }
}

pub fn empty_mesh() -> Mesh {
    let mut m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3]);
    m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 3]);
    m.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]; 3]);
    m.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0f32; 4]; 3]);
    m.insert_indices(Indices::U32(vec![0, 1, 2]));
    m
}

// ---- line renderer (bullet trails) --------------------------------------------------------

#[derive(Component)]
pub struct LineFx {
    pub prefab: Arc<PrefabDef>,
    pub obj: usize,
    pub root: Entity,
    pub mesh: Handle<Mesh>,
    pub from: Vec3,
    pub to: Vec3,
    pub age: f32,
    pub life: f32,
    pub alpha: f32,
}

/// `LineRenderer` as a camera-facing strip; `LineFade` decays the alpha like `Mathf.Lerp(a, 0, dt * speed)`.
pub fn sim_lines(
    time: Res<Time>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut q: Query<&mut LineFx>,
    mut roots: Query<&mut FxRoot>,
) {
    let Ok(cam) = cam.single() else { return };
    let cam_pos = cam.translation();
    let dt = time.delta_secs();
    for mut l in &mut q {
        l.age += dt;
        if let Ok(mut r) = roots.get_mut(l.root) {
            r.busy |= l.age < l.life;
        }
        let prefab = l.prefab.clone();
        let Some(def) = prefab.objects[l.obj].line.as_ref() else { continue };
        if let Some((_, speed)) = def.fade {
            l.alpha += (0.0 - l.alpha) * (dt * speed).clamp(0.0, 1.0);
        }
        if def.shrink > 0.0 {
            l.to = l.to.lerp(l.from, (dt * def.shrink).clamp(0.0, 1.0));
        }
        let axis = l.to - l.from;
        let len = axis.length();
        if len < 1e-4 {
            continue;
        }
        let dir = axis / len;
        let n = 8; // segments (width curve / gradient resolution)
        let mut b = Batch::default();
        let mut prev: Option<(Vec3, Vec3, [f32; 4])> = None;
        for k in 0..=n {
            let u = k as f32 / n as f32;
            let p = l.from + axis * u;
            let side = dir.cross((cam_pos - p).normalize_or_zero()).normalize_or_zero();
            let half = side * def.width * def.width_curve.eval(u).max(0.0) * 0.5;
            let c = match def.fade {
                Some((c, _)) => Vec4::new(c.x, c.y, c.z, l.alpha),
                None => def.gradient.eval(u),
            };
            let col = if def.apply_cs { [srgb(c.x), srgb(c.y), srgb(c.z), c.w] } else { [c.x, c.y, c.z, c.w] };
            if let Some((pp, ph, pc)) = prev {
                let base = b.pos.len() as u32;
                let u0 = (k - 1) as f32 / n as f32;
                for (pt, uu, cc) in [(pp + ph, u0, pc), (p + half, u, col), (p - half, u, col), (pp - ph, u0, pc)] {
                    b.pos.push(pt.to_array());
                    b.nrm.push([0.0, 1.0, 0.0]);
                    b.uv.push([uu, if (pt - p).dot(side) > 0.0 { 0.0 } else { 1.0 }]);
                    b.col.push(cc);
                }
                b.idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
            prev = Some((p, half, col));
        }
        if let Some(mut m) = meshes.get_mut(&l.mesh) {
            b.write(&mut m);
        }
    }
}

// ---- decals / lights ----------------------------------------------------------------------

/// `WFX_BulletHoleDecal`: holds `lifetime`, fades the tint alpha over the last `fadeoutpercent`.
#[derive(Component)]
pub struct DecalFx {
    pub root: Entity,
    pub life: f32,
    pub fadeout: f32,
    pub orig_alpha: f32,
    pub material: Handle<StandardMaterial>,
}

pub fn sim_decals(time: Res<Time>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<&mut DecalFx>, mut roots: Query<&mut FxRoot>) {
    for mut d in &mut q {
        d.life -= time.delta_secs();
        if let Ok(mut r) = roots.get_mut(d.root) {
            r.busy |= d.life > 0.0;
        }
        if d.life <= d.fadeout {
            if let Some(mut m) = mats.get_mut(&d.material) {
                let a = d.orig_alpha * (d.life / d.fadeout.max(1e-4)).clamp(0.0, 1.0);
                m.base_color = m.base_color.with_alpha(a);
            }
        }
    }
}

/// Point light of a prefab `Light` component (muzzle flash / explosion flash), fading over `total`.
#[derive(Component)]
pub struct FxLight {
    pub root: Entity,
    pub life: f32,
    pub total: f32,
    pub intensity: f32,
}

/// Light spawned by a Lights module; removed when its emitter root is gone.
#[derive(Component)]
pub struct FxParticleLight {
    pub root: Entity,
}

pub fn cleanup_particle_lights(mut commands: Commands, q: Query<(Entity, &FxParticleLight)>, roots: Query<(), With<FxRoot>>) {
    for (e, l) in &q {
        if roots.get(l.root).is_err() {
            commands.entity(e).try_despawn();
        }
    }
}

pub fn sim_lights(time: Res<Time>, mut q: Query<(&mut FxLight, &mut PointLight)>, mut roots: Query<&mut FxRoot>) {
    for (mut l, mut pl) in &mut q {
        l.life -= time.delta_secs();
        if let Ok(mut r) = roots.get_mut(l.root) {
            r.busy |= l.life > 0.0;
        }
        pl.intensity = l.intensity * (l.life / l.total).clamp(0.0, 1.0);
    }
}

pub fn root_begin(time: Res<Time>, mut roots: Query<&mut FxRoot>) {
    for mut r in &mut roots {
        r.busy = false;
        r.age += time.delta_secs();
    }
}

pub fn root_end(mut commands: Commands, roots: Query<(Entity, &FxRoot)>) {
    for (e, r) in &roots {
        if !r.persistent && ((!r.busy && r.age > 0.05) || r.age > r.max_age) {
            for x in &r.extra {
                commands.entity(*x).try_despawn();
            }
            commands.entity(e).try_despawn();
        }
    }
}

fn hash3(i: IVec3, c: u32) -> f32 {
    let mut h = (i.x as u32).wrapping_mul(0x8da6_b343) ^ (i.y as u32).wrapping_mul(0xd816_3841) ^ (i.z as u32).wrapping_mul(0xcb1a_b31f) ^ c.wrapping_mul(0x9e37_79b9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    (h & 0xffff) as f32 / 32767.5 - 1.0
}

/// 3D gradient (Perlin) noise, roughly in [-1, 1] (channel `c` selects an independent field).
fn grad_noise(p: Vec3, c: u32) -> f32 {
    const G: [Vec3; 12] = [
        Vec3::new(1.0, 1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(-1.0, -1.0, 0.0),
        Vec3::new(1.0, 0.0, 1.0), Vec3::new(-1.0, 0.0, 1.0), Vec3::new(1.0, 0.0, -1.0), Vec3::new(-1.0, 0.0, -1.0),
        Vec3::new(0.0, 1.0, 1.0), Vec3::new(0.0, -1.0, 1.0), Vec3::new(0.0, 1.0, -1.0), Vec3::new(0.0, -1.0, -1.0),
    ];
    let i = p.floor();
    let f = p - i;
    let u = f * f * f * (f * (f * 6.0 - Vec3::splat(15.0)) + Vec3::splat(10.0));
    let i = i.as_ivec3();
    let g = |x: i32, y: i32, z: i32| {
        let h = ((hash3(i + IVec3::new(x, y, z), c) + 1.0) * 0.5 * 12.0) as usize;
        G[h.min(11)].dot(f - Vec3::new(x as f32, y as f32, z as f32))
    };
    let lx = |y: i32, z: i32| g(0, y, z) + (g(1, y, z) - g(0, y, z)) * u.x;
    let ly = |z: i32| lx(0, z) + (lx(1, z) - lx(0, z)) * u.y;
    ly(0) + (ly(1) - ly(0)) * u.z
}

/// Fixed sampling axes for the lower noise qualities (1D: one coordinate per channel, 2D: two).
const NOISE_AXES: [Vec3; 3] = [Vec3::new(0.577, 0.577, 0.577), Vec3::new(-0.707, 0.0, 0.707), Vec3::new(0.408, -0.816, 0.408)];

/// Noise field value for quality `q` at noise-space point `p` (scroll applied by the caller), 3 channels from
/// field `c0`. High (3D) = curl of a 3-channel gradient-noise potential (divergence free, like Unity's curl noise;
/// derivatives taken in noise space, i.e. the damped, frequency-independent magnitude). Medium (2D) / Low (1D) =
/// independent gradient-noise channels sampled on 2 / 1 projected coordinates (cheaper, less varied field).
fn noise_field(q: i32, p: Vec3, c0: u32) -> Vec3 {
    match q {
        2.. => {
            const E: f32 = 1e-2;
            let d = |c: u32, ax: Vec3| (grad_noise(p + ax * E, c0 + c) - grad_noise(p - ax * E, c0 + c)) / (2.0 * E);
            let (x, y, z) = (Vec3::X, Vec3::Y, Vec3::Z);
            // curl of (psi0, psi1, psi2); x0.275 gives it the RMS of the plain gradient noise (0.27; measured).
            0.275 * Vec3::new(d(2, y) - d(1, z), d(0, z) - d(2, x), d(1, x) - d(0, y))
        }
        1 => Vec3::from_array(std::array::from_fn(|k| {
            let (a, b) = (NOISE_AXES[k], NOISE_AXES[(k + 1) % 3]);
            grad_noise(Vec3::new(p.dot(a), p.dot(b), 0.5), c0 + k as u32)
        })),
        _ => Vec3::from_array(std::array::from_fn(|k| grad_noise(Vec3::new(p.dot(NOISE_AXES[k]), 0.5, 0.5), c0 + k as u32))),
    }
}

/// Noise module for a particle at `pos` (sim space): (position velocity, rotation speed rad/s, size multiplier).
/// Unity: the field is sampled at position x frequency (+ scrollSpeed x time), summed over octaves (each
/// octaveScale x the frequency, octaveMultiplier x the strength), optionally remapped (curve over the
/// [-1, 1] value), scaled by strength (x frequency without damping), then by positionAmount /
/// rotationAmount (deg/s) / sizeAmount.
fn noise_sample(nd: &NoiseDef, pos: Vec3, time: f32, nt: f32, r: f32) -> (Vec3, f32, f32) {
    let mut v = Vec3::ZERO;
    let mut extra = Vec2::ZERO; // rotation, size channels
    let mut freq = nd.frequency;
    let mut amp = 1.0;
    let scroll = nd.scroll.eval(nt, r) * time;
    let want_rs = !nd.rotation_amount.is_const_zero() || !nd.size_amount.is_const_zero();
    for _ in 0..nd.octaves {
        let q = pos * freq + Vec3::splat(scroll);
        v += amp * noise_field(nd.quality, q, 0);
        if want_rs {
            let w = noise_field(nd.quality.min(1), q + Vec3::splat(17.3), 3);
            extra += amp * Vec2::new(w.x, w.y);
        }
        freq *= nd.octave_scale.max(1.0);
        amp *= nd.octave_mul;
    }
    if let Some(rm) = &nd.remap {
        v = Vec3::new(rm[0].eval((v.x * 0.5 + 0.5).clamp(0.0, 1.0), r), rm[1].eval((v.y * 0.5 + 0.5).clamp(0.0, 1.0), r), rm[2].eval((v.z * 0.5 + 0.5).clamp(0.0, 1.0), r));
        extra = Vec2::new(rm[0].eval((extra.x * 0.5 + 0.5).clamp(0.0, 1.0), r), rm[0].eval((extra.y * 0.5 + 0.5).clamp(0.0, 1.0), r));
    }
    let mut s = Vec3::new(nd.strength[0].eval(nt, r), nd.strength[1].eval(nt, r), nd.strength[2].eval(nt, r));
    // Unity: without damping the strength is not scaled with the frequency, so the curl field's magnitude grows
    // with it; this field is frequency-independent (the damped behaviour of every STRAFTAT prefab).
    if !nd.damping {
        s *= nd.frequency;
    }
    let pos_v = v * s * nd.position_amount.eval(nt, r);
    let rot = extra.x * s.x * nd.rotation_amount.eval(nt, r).to_radians();
    let size = (1.0 + extra.y * s.x * nd.size_amount.eval(nt, r)).max(0.0);
    (pos_v, rot, size)
}

/// One particle's ribbon (Trails module, Particles mode). Points are stored in trail space: world, or the system's
/// local space when the system simulates locally and `worldSpace` is off (the trail then moves with the system).
#[derive(Default)]
pub struct TrailPts {
    pts: Vec<Vec3>,
    born: Vec<f32>,
    alive: bool,
    /// Point lifetime (trail lifetime multiplier * particle lifetime).
    life: f32,
    head: Vec3,
    width: f32,
    color: Vec4,
}

/// Append one camera-facing strip through `pts` (world position, width, colour; index 0 = the head: newest point,
/// curve time 0). widthOverTrail / colorOverTrail are evaluated at the length fraction along the strip, the texture
/// U by textureMode (0 Stretch: length fraction, 1 Tile: once per world unit, 2 DistributePerSegment: vertex
/// fraction, 3 RepeatPerSegment: once per segment).
fn push_strip(b: &mut Batch, pts: &[(Vec3, f32, Vec4)], td: &TrailDef, cam_pos: Vec3, apply_cs: bool) {
    let n = pts.len();
    if n < 2 {
        return;
    }
    let mut cum = vec![0.0f32; n];
    for i in 1..n {
        cum[i] = cum[i - 1] + pts[i].0.distance(pts[i - 1].0);
    }
    let total = cum[n - 1].max(1e-6);
    let base = b.pos.len() as u32;
    let lc = |v: f32| if apply_cs { srgb(v) } else { v.max(0.0) };
    for i in 0..n {
        let (p, w, c) = pts[i];
        let t = cum[i] / total;
        let u = match td.texture_mode {
            1 => cum[i],
            2 => i as f32 / (n - 1) as f32,
            3 => i as f32,
            _ => t,
        };
        let d = if i + 1 < n { pts[i + 1].0 - p } else { p - pts[i - 1].0 };
        let to_cam = cam_pos - p;
        let half = w * td.width.eval(t, 0.5) * 0.5;
        let side = d.cross(to_cam).normalize_or_zero() * half;
        let c = c * td.color_trail.eval(t, 0.5);
        let col = [lc(c.x), lc(c.y), lc(c.z), c.w.clamp(0.0, 1.0)];
        let nrm = to_cam.normalize_or_zero().to_array();
        for (s, v) in [(-1.0, 0.0), (1.0, 1.0)] {
            b.pos.push((p + side * s).to_array());
            b.nrm.push(nrm);
            b.uv.push([u, v]);
            b.col.push(col);
        }
        if i + 1 < n {
            let k = base + 2 * i as u32;
            b.idx.extend_from_slice(&[k, k + 1, k + 3, k, k + 3, k + 2]);
        }
    }
}

/// Trails module. Particles mode: particles drop points every `minVertexDistance`; each point lives
/// `lifetime * particle lifetime` (x particle size with sizeAffectsLifetime). Ribbon mode: the live particles are
/// joined in age order (youngest = head) into `ribbonCount` ribbons (particle n goes to ribbon n % count),
/// optionally starting at the system's transform (attachRibbonsToTransform). Width = widthOverTrail (x particle size
/// with sizeAffectsWidth), colour = colorOverLifetime at the particle's age (x particle colour with
/// inheritParticleColor) x colorOverTrail; drawn with the renderer's trail material.
pub fn sim_trails(
    time: Res<Time>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut q: Query<(&mut Emitter, &GlobalTransform, &Transform)>,
    mut vis: Query<&mut Visibility, Without<Emitter>>,
    mut roots: Query<&mut FxRoot>,
    dbg_info: Query<(&ViewVisibility, &InheritedVisibility, &GlobalTransform, Option<&bevy::camera::visibility::RenderLayers>, Option<&ChildOf>)>,
) {
    let Ok(cam) = cam.single() else { return };
    let cam_pos = cam.translation();
    let now = time.elapsed_secs();
    for (mut e, gt, tf) in &mut q {
        let Some((render, mesh)) = e.trail_render.clone() else { continue };
        let prefab = e.prefab.clone();
        let def = prefab.objects[e.obj].ps.as_ref().unwrap();
        let Some(td) = &def.trail else { continue };
        let (gs, grot, gpos) = gt.to_scale_rotation_translation();
        let eff = match def.scaling {
            0 => gs,
            1 => tf.scale,
            _ => gs, // Shape: positions only
        };
        let sc = if def.scaling == 2 { 1.0 } else { ((eff.x.abs() + eff.y.abs() + eff.z.abs()) / 3.0).max(1e-4) };
        let world = Mat4::from_scale_rotation_translation(eff, grot, gpos);
        let to_world = |v: Vec3| if def.sim_world { v } else { world.transform_point3(v) };
        // Trail points stay in the system's local space only for local simulation with worldSpace off.
        let local_pts = !def.sim_world && !td.world;
        let e = &mut *e;
        // Per-particle current size (size over lifetime, size noise, system scale) and trail colour.
        let look = |p: &Particle| -> (f32, Vec4) {
            let nt = (p.age / p.life).clamp(0.0, 1.0);
            let mut size = p.size.x * p.nsize * sc;
            if let Some(sl) = &def.size_life {
                size *= sl.x.eval(nt, p.r[1]);
            }
            let mut c = td.color_life.eval(nt, p.r[3]);
            if td.inherit_color {
                c *= p.color * def.color_life.as_ref().map(|cl| cl.eval(nt, p.r[3])).unwrap_or(Vec4::ONE);
            }
            (size, c)
        };
        let mut b = Batch::default();
        let mut n_ribbons = 0;
        if td.mode == 1 {
            let mut groups: Vec<Vec<(f32, Vec3, f32, Vec4)>> = (0..td.ribbon_count).map(|_| Vec::new()).collect();
            for p in &e.particles {
                let (size, c) = look(p);
                let w = if td.size_width { size } else { 1.0 };
                groups[p.id as usize % td.ribbon_count].push((p.age, to_world(p.pos), w, c));
            }
            for g in &mut groups {
                g.sort_by(|a, c| a.0.partial_cmp(&c.0).unwrap_or(std::cmp::Ordering::Equal));
                let mut pts: Vec<(Vec3, f32, Vec4)> = g.iter().map(|&(_, p, w, c)| (p, w, c)).collect();
                if td.attach_ribbons && !pts.is_empty() {
                    let (w, c) = (pts[0].1, pts[0].2);
                    pts.insert(0, (gpos, w, c));
                }
                if pts.len() >= 2 {
                    n_ribbons += 1;
                }
                push_strip(&mut b, &pts, td, cam_pos, def.render.apply_cs);
            }
        } else {
            for t in e.trails.values_mut() {
                t.alive = false;
            }
            for p in &e.particles {
                if !p.trail {
                    continue;
                }
                let w = if local_pts { p.pos } else { to_world(p.pos) };
                let nt = (p.age / p.life).clamp(0.0, 1.0);
                let (size, color) = look(p);
                let t = e.trails.entry(p.id).or_default();
                t.alive = true;
                t.life = td.lifetime.eval(nt, p.r[0]) * p.life * if td.size_life { size } else { 1.0 };
                t.head = w;
                if t.pts.last().is_none_or(|l| l.distance(w) >= td.min_dist) {
                    t.pts.push(w);
                    t.born.push(now);
                }
                t.width = if td.size_width { size } else { 1.0 };
                t.color = color;
            }
            if td.die_with {
                e.trails.retain(|_, t| t.alive);
            }
            for t in e.trails.values_mut() {
                let keep = t.born.iter().position(|b| now - b <= t.life).unwrap_or(t.born.len());
                t.pts.drain(..keep);
                t.born.drain(..keep);
            }
            e.trails.retain(|_, t| t.alive || !t.pts.is_empty());
            let tw = |v: Vec3| if local_pts { world.transform_point3(v) } else { v };
            for t in e.trails.values() {
                // head first (curve time 0 = the particle end)
                let mut pts: Vec<(Vec3, f32, Vec4)> = Vec::with_capacity(t.pts.len() + 1);
                if t.alive && t.pts.last().is_none_or(|l| l.distance(t.head) > 1e-4) {
                    pts.push((tw(t.head), t.width, t.color));
                }
                pts.extend(t.pts.iter().rev().map(|&v| (tw(v), t.width, t.color)));
                push_strip(&mut b, &pts, td, cam_pos, def.render.apply_cs);
            }
            n_ribbons = e.trails.len();
        }
        if n_ribbons > 0 {
            if let Ok(mut r) = roots.get_mut(e.root) {
                r.busy = true;
            }
        }
        if debug_vfx() && n_ribbons > 0 {
            println!("TRAILENT {:?}", dbg_info.get(render).map(|(v, iv, g, l, c)| (v.get(), iv.get(), g.translation(), l.cloned(), c.map(|c| c.parent()))));
            println!("TRAIL {} tail={:?} cam={:?} mode={} ribbons={} verts={} local_pts={} col0={:?} v0={:?} v1={:?} pts={:?} life={:?} parts={:?}", prefab.objects[e.obj].path, e.trails.values().map(|t| t.pts.first().copied()).collect::<Vec<_>>(), cam_pos, td.mode, n_ribbons, b.pos.len(), local_pts, b.col.first(), b.pos.first(), b.pos.get(1), e.trails.values().map(|t| t.pts.len()).collect::<Vec<_>>(), e.trails.values().map(|t| t.life).collect::<Vec<_>>(), e.particles.iter().map(|p| (p.trail, p.life, p.pos)).collect::<Vec<_>>());
        }
        if let Ok(mut v) = vis.get_mut(render) {
            *v = if b.pos.is_empty() { Visibility::Hidden } else { Visibility::Visible };
        }
        if !b.pos.is_empty() {
            if let Some(mut m) = meshes.get_mut(&mesh) {
                // The Trails module is drawn by the same ParticleSystemRenderer (same sorting settings).
                b.write_sorted(&mut m, cam_pos, cam.forward().as_vec3(), def.render.fudge, def.render.order);
            }
        }
    }
}
