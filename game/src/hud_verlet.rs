//! HUD cable/heart wobble: port of Heathen PhysKit `VerletSpring` / `VerletHierarchy` / `VerletParticle`
//! (decompiled/Heathen.PhysKit/HeathenEngineering.PhysKit/*.cs) for the 10 VerletSpring components under
//! PlayerIK `Camera/UICam` (SK_CableHud_*, SK_BulletCounterCable_*, SK_Heart_00). All of them reference
//! `MonoBehaviour/Verlet_HUDRopes.asset` (settings Mode 2 = Referenced); a constant (Mode 0/1) value would be
//! read from `m_constantValue`.
//!
//! Also renders their `SkinnedMeshRenderer`s (SM_CableHud_*, SM_BulletCounterCable_*, SM_Heart_01): the prefab glb
//! carries no mesh on those nodes, so each mesh is built from the installation's Mesh (`Owned::mesh_glb`:
//! POSITION/NORMAL/TANGENT/UVs/JOINTS_0/WEIGHTS_0) and skinned to the renderer's `m_Bones` with the Unity
//! `m_BindPose` (as in `skinning.rs`). Materials are the PlayerIK.glb materials of the same name.
//!
//! Simulation runs in Bevy world space on a fixed-step accumulator at Unity's Fixed Timestep (TimeManager.asset
//! 0.033333335, Maximum Allowed Timestep 0.33333334), in PostUpdate after the camera has moved and before transform
//! propagation. World transforms are composed from local `Transform`s on demand (like Unity's always-current
//! `transform.position`), and results are written back as local transforms, so between fixed steps the bones ride
//! rigidly with the HUD exactly like Unity's locally-stored transforms. The glb nodes live in AssetRipper's X-mirrored
//! space under the HUD pivot; every operation used (vector math, parent matrix * initLocalPosition, FromToRotation,
//! LookRotation angle) is equivariant under the mirror, and gravity (0,-0.03,0) is unchanged by Unity->Bevy (z flip).
//!
//! Not ported: collision (SphereCast) — all HUD hierarchies have collisionRadius 0 and collisionLayers 0, so
//! `GetNodeCollisionResult` is never called / would never hit. AddForce / AddForceAtPosition are never called by the game.

use crate::health::PlayerPrefab;
use crate::hud::HudNodes;
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use std::sync::Arc;

/// ProjectSettings/TimeManager.asset: Fixed Timestep.
const FIXED_DT: f32 = 0.033333335;
/// TimeManager.asset: Maximum Allowed Timestep (Time.maximumDeltaTime).
const MAX_DT: f32 = 0.33333334;
const SPRING_SCRIPT: &str = "MonoBehaviour:7a6afae867390d037956a4f4b3d036d0";
#[cfg(test)]
const RIPPED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../ripped/ExportedProject/Assets");
/// hud.rs HUD_LAYER.
const HUD_LAYER: usize = 1;

// ---------------------------------------------------------------------------------------------
// Settings (VerletHierarchySettings)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Key {
    t: f32,
    v: f32,
    in_s: f32,
    out_s: f32,
}

/// Unity AnimationCurve (non-weighted Hermite keys, clamped outside the key range).
#[derive(Clone, Default)]
struct Curve(Vec<Key>);

impl Curve {
    fn eval(&self, t: f32) -> f32 {
        let k = &self.0;
        if k.is_empty() {
            return 0.0;
        }
        if t <= k[0].t {
            return k[0].v;
        }
        let last = k[k.len() - 1];
        if t >= last.t {
            return last.v;
        }
        let i = k.partition_point(|x| x.t <= t) - 1;
        let (a, b) = (k[i], k[i + 1]);
        let dt = b.t - a.t;
        if a.out_s.is_infinite() || b.in_s.is_infinite() || dt <= 0.0 {
            return a.v;
        }
        let u = (t - a.t) / dt;
        let (m0, m1) = (a.out_s * dt, b.in_s * dt);
        let (u2, u3) = (u * u, u * u * u);
        (2.0 * u3 - 3.0 * u2 + 1.0) * a.v + (u3 - 2.0 * u2 + u) * m0 + (-2.0 * u3 + 3.0 * u2) * b.v + (u3 - u2) * m1
    }
}

struct Settings {
    update_rate: f32,
    /// Bevy world space.
    constant_velocity: Vec3,
    clear_reseting_velocity: bool,
    damping: f32,
    damping_curve: Curve,
    elasticity: f32,
    elasticity_curve: Curve,
    stiffness: f32,
    stiffness_curve: Curve,
    inert: f32,
    inert_curve: Curve,
    collision_layers: u64,
    collision_radius: f32,
    radius_curve: Curve,
    falloff_angle: f32,
    falloff_curve: Curve,
}

impl Settings {
    fn from_value(v: &serde_json::Value) -> Settings {
        let f = |k: &str| v[k].as_f64().unwrap_or(0.0) as f32;
        let curve = |k: &str| {
            Curve(
                v[k]["m_Curve"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|e| {
                                let g = |n: &str| e[n].as_f64().map(|x| x as f32).unwrap_or(0.0);
                                Key { t: g("time"), v: g("value"), in_s: g("inSlope"), out_s: g("outSlope") }
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        };
        let cv = &v["constantVelocity"];
        let g = |n: &str| cv[n].as_f64().unwrap_or(0.0) as f32;
        Settings {
            update_rate: f("updateRate"),
            // Unity -> Bevy world: z flipped.
            constant_velocity: Vec3::new(g("x"), g("y"), -g("z")),
            clear_reseting_velocity: v["clearResetingVelocity"].as_i64().unwrap_or(0) != 0 || v["clearResetingVelocity"].as_bool().unwrap_or(false),
            damping: f("damping"),
            damping_curve: curve("dampingCurve"),
            elasticity: f("elasticity"),
            elasticity_curve: curve("elasticityCurve"),
            stiffness: f("stiffness"),
            stiffness_curve: curve("stiffnessCurve"),
            inert: f("inert"),
            inert_curve: curve("inertCurve"),
            collision_layers: v["collisionLayers"]["m_Bits"].as_u64().unwrap_or(0),
            collision_radius: f("collisionRadius"),
            radius_curve: curve("radiusCurve"),
            falloff_angle: f("falloffAngle"),
            falloff_curve: curve("falloffCurve"),
        }
    }

    /// `Variable` ScriptableObject (`m_value`), e.g. `MonoBehaviour/Verlet_HUDRopes.asset`, read from the installation.
    fn load_asset(rel: &str) -> Option<Settings> {
        Some(Settings::from_value(&variable_value(rel)?))
    }
}

/// `m_value` of a Variable ScriptableObject by its asset path (the file stem is the object name).
fn variable_value(rel: &str) -> Option<serde_json::Value> {
    let stem = rel.rsplit('/').next()?.trim_end_matches(".asset");
    let o = crate::owned::Owned::global()?;
    let (node, pid) = *o.same_named(114, stem)?.first()?;
    let (_, f) = crate::prefab_json::script_object(o, o.assemblies()?, node, pid).map_err(|e| warn!("{rel}: {e}")).ok()?;
    f.get("m_value").cloned()
}

#[cfg(test)]
mod variable_tests {
    /// The Verlet settings read in place equal the old asset YAML (numbers and curve keys).
    #[test]
    fn verlet_variables_match_export() {
        let dir = format!("{}/MonoBehaviour", super::RIPPED);
        let mut n = 0;
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with("Verlet") || !name.ends_with(".asset") {
                continue;
            }
            let txt = std::fs::read_to_string(e.path()).unwrap();
            let body: String = txt.lines().skip_while(|l| !l.starts_with("---")).skip(1).collect::<Vec<_>>().join("\n");
            let y: serde_yaml::Value = serde_yaml::from_str(&body).unwrap();
            let want: serde_json::Value = serde_json::to_value(y).unwrap()["MonoBehaviour"]["m_value"].clone();
            let got = super::variable_value(&format!("MonoBehaviour/{name}")).expect(&name);
            fn close(a: &serde_json::Value, b: &serde_json::Value, path: &str) {
                match (a, b) {
                    (serde_json::Value::Object(a), _) => a.iter().filter(|(k, _)| *k != "serializedVersion").for_each(|(k, v)| close(v, &b[k], &format!("{path}.{k}"))),
                    (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
                        assert_eq!(a.len(), b.len(), "{path}");
                        a.iter().zip(b).enumerate().for_each(|(i, (x, y))| close(x, y, &format!("{path}[{i}]")));
                    }
                    _ => {
                        let (x, y) = (a.as_f64().unwrap_or(f64::NAN), b.as_f64().or_else(|| b.as_bool().map(|v| v as u8 as f64)).unwrap_or(f64::NAN));
                        assert!((x - y).abs() <= 1e-5 * x.abs().max(1.0), "{path}: {a} vs {b}");
                    }
                }
            }
            close(&want, &got, &name);
            n += 1;
        }
        println!("{n} Verlet variables match");
    }
}

impl Settings {
}

// ---------------------------------------------------------------------------------------------
// VerletParticle / VerletHierarchy / VerletSpring
// ---------------------------------------------------------------------------------------------

struct Particle {
    target: Entity,
    parent: Option<usize>,
    damping: f32,
    elasticity: f32,
    stiffness: f32,
    inert: f32,
    falloff_angle: f32,
    collision_radius: f32,
    distance: f32,
    added_force: Vec3,
    weight: f32,
    position: Vec3,
    prev_position: Vec3,
    init_local_position: Vec3,
    init_local_rotation: Quat,
}

struct Hierarchy {
    root: Entity,
    settings: Arc<Settings>,
    nodes: Vec<Particle>,
    ignore: Vec<Entity>,
    scalar: f32,
    resting_velocity: Vec3,
    distance: f32,
}

struct Spring {
    entity: Entity,
    previous_position: Vec3,
    hierarchies: Vec<Hierarchy>,
    /// Prefab instance the spring belongs to (hats): the spring is dropped when it is despawned.
    owner: Option<Entity>,
}

#[derive(Resource, Default)]
pub struct HudVerlet {
    springs: Vec<Spring>,
    registered: bool,
    accumulator: f32,
}

type TfQuery<'w, 's> = Query<'w, 's, (&'static mut Transform, Option<&'static ChildOf>)>;

/// Unity `transform.localToWorldMatrix`, composed from local transforms (always current, like Unity).
fn world(q: &TfQuery, e: Entity) -> GlobalTransform {
    let Ok((t, p)) = q.get(e) else { return GlobalTransform::IDENTITY };
    match p {
        Some(p) => world(q, p.parent()).mul_transform(*t),
        None => GlobalTransform::from(*t),
    }
}

fn parent_world(q: &TfQuery, e: Entity) -> GlobalTransform {
    match q.get(e) {
        Ok((_, Some(p))) => world(q, p.parent()),
        _ => GlobalTransform::IDENTITY,
    }
}

fn world_rot(q: &TfQuery, e: Entity) -> Quat {
    world(q, e).to_scale_rotation_translation().1
}

/// `transform.rotation = r`.
fn set_world_rot(q: &mut TfQuery, e: Entity, r: Quat) {
    let pr = parent_world(q, e).to_scale_rotation_translation().1;
    if let Ok((mut t, _)) = q.get_mut(e) {
        t.rotation = (pr.inverse() * r).normalize();
    }
}

/// `transform.position = p`.
fn set_world_pos(q: &mut TfQuery, e: Entity, p: Vec3) {
    let pw = parent_world(q, e);
    if let Ok((mut t, _)) = q.get_mut(e) {
        t.translation = pw.affine().inverse().transform_point3(p);
    }
}

fn local(q: &TfQuery, e: Entity) -> Transform {
    q.get(e).map(|(t, _)| *t).unwrap_or_default()
}

/// Quaternion.FromToRotation (identity for degenerate input).
fn from_to(from: Vec3, to: Vec3) -> Quat {
    let (a, b) = (from.normalize_or_zero(), to.normalize_or_zero());
    if a == Vec3::ZERO || b == Vec3::ZERO {
        return Quat::IDENTITY;
    }
    Quat::from_rotation_arc(a, b)
}

/// Quaternion.Angle(LookRotation(a), LookRotation(b)) in degrees (up = world Y).
fn look_angle(a: Vec3, b: Vec3) -> f32 {
    let look = |d: Vec3| Transform::IDENTITY.looking_to(d, Vec3::Y).rotation;
    let d = look(a).dot(look(b)).abs().min(1.0);
    (2.0 * d.acos()).to_degrees()
}

impl Hierarchy {
    fn register_nodes(&mut self, q: &mut TfQuery, children: &Query<&Children>) {
        if !self.nodes.is_empty() {
            self.reset_nodes(q);
        }
        self.nodes.clear();
        self.add_nodes(q, children, self.root, None, 0.0);
        self.set_node_data();
        self.scalar = world(q, self.root).to_scale_rotation_translation().0.x;
        self.resting_velocity = world_rot(q, self.root).inverse() * self.settings.constant_velocity;
    }

    fn add_nodes(&mut self, q: &TfQuery, children: &Query<&Children>, node: Entity, parent: Option<usize>, mut bone_distance: f32) {
        let l = local(q, node);
        let pos = world(q, node).translation();
        let mut p = Particle {
            target: node,
            parent,
            damping: 0.0,
            elasticity: 0.0,
            stiffness: 0.0,
            inert: 0.0,
            falloff_angle: 0.0,
            collision_radius: 0.0,
            distance: 0.0,
            added_force: Vec3::ZERO,
            weight: 0.0,
            position: pos,
            prev_position: pos,
            init_local_position: l.translation,
            init_local_rotation: l.rotation,
        };
        if let Some(pi) = parent {
            bone_distance += (world(q, self.nodes[pi].target).translation() - p.position).length();
            p.distance = bone_distance;
            self.distance = self.distance.max(bone_distance);
        }
        let idx = self.nodes.len();
        self.nodes.push(p);
        if let Ok(k) = children.get(node) {
            for c in k.iter().collect::<Vec<_>>() {
                if !self.ignore.contains(&c) && q.get(c).is_ok() {
                    self.add_nodes(q, children, c, Some(idx), bone_distance);
                }
            }
        }
    }

    fn coefficients(s: &Settings, n: &mut Particle) {
        let w = n.weight;
        n.damping = (s.damping * s.damping_curve.eval(w)).clamp(0.0, 1.0);
        n.elasticity = (s.elasticity * s.elasticity_curve.eval(w)).clamp(0.0, 1.0);
        n.stiffness = (s.stiffness * s.stiffness_curve.eval(w)).clamp(0.0, 1.0);
        n.inert = (s.inert * s.inert_curve.eval(w)).clamp(0.0, 1.0);
        n.falloff_angle = (s.falloff_angle * s.falloff_curve.eval(w)).clamp(0.0, 360.0);
        n.collision_radius = (s.collision_radius * s.radius_curve.eval(w)).max(0.0);
    }

    fn set_node_data(&mut self) {
        let d = self.distance;
        for n in &mut self.nodes {
            n.weight = if d == 0.0 { 0.0 } else { n.distance / d };
            Self::coefficients(&self.settings, n);
        }
    }

    fn reset_nodes(&mut self, q: &mut TfQuery) {
        for n in &mut self.nodes {
            if let Ok((mut t, _)) = q.get_mut(n.target) {
                t.translation = n.init_local_position;
                t.rotation = n.init_local_rotation;
            }
        }
        for i in 0..self.nodes.len() {
            self.nodes[i].prev_position = world(q, self.nodes[i].target).translation();
        }
    }

    /// VerletHierarchy.Update -> UnifiedUpdate.
    fn update(&mut self, velocity: Vec3, dt: f32, q: &mut TfQuery) {
        let s = self.settings.clone();
        let time = dt * s.update_rate;
        let mut constant_velocity = s.constant_velocity;
        for n in &mut self.nodes {
            if let Ok((mut t, _)) = q.get_mut(n.target) {
                t.translation = n.init_local_position;
                t.rotation = n.init_local_rotation;
            }
            Self::coefficients(&s, n);
        }
        if s.clear_reseting_velocity {
            let normalized = constant_velocity.normalize_or_zero();
            let lhs = world_rot(q, self.root) * self.resting_velocity;
            constant_velocity -= normalized * lhs.dot(normalized).max(0.0);
        }
        if s.collision_layers != 0 && self.nodes.iter().any(|n| n.collision_radius > 0.0) {
            warn_once!("hud_verlet: node collision not ported (collisionLayers != 0)");
        }
        for i in 0..self.nodes.len() {
            let target = self.nodes[i].target;
            if target == self.root {
                let pos = world(q, target).translation();
                let n = &mut self.nodes[i];
                n.prev_position = n.position;
                n.position = pos;
                continue;
            }
            let Some(pi) = self.nodes[i].parent else { continue };
            let parent_target = self.nodes[pi].target;
            let parent_position = self.nodes[pi].position;
            let magnitude = (world(q, parent_target).translation() - world(q, target).translation()).length();
            let parent_lin = world(q, parent_target).affine().matrix3;
            let n = &mut self.nodes[i];
            let vector = (constant_velocity + n.added_force) * self.scalar * time;
            n.added_force = Vec3::ZERO;
            let vector2 = n.position - n.prev_position;
            let vector3 = velocity * n.inert;
            n.prev_position = n.position + vector3;
            n.position += vector2 * (1.0 - n.damping) + (vector + vector3);
            // (collision: radius 0 / layer mask 0 -> position unchanged)
            let mut vector4 = Vec3::ZERO;
            // localToWorldMatrix of the parent with its translation column replaced by parent.position.
            let rest = Vec3::from(parent_lin * bevy::math::Vec3A::from(n.init_local_position)) + parent_position;
            if n.stiffness > 0.0 || n.elasticity > 0.0 {
                vector4 = rest - n.position;
                n.position += vector4 * (n.elasticity * time);
                if n.stiffness > 0.0 {
                    vector4 = rest - n.position;
                    let magnitude2 = vector4.length();
                    let num = magnitude * (1.0 - n.stiffness) * 2.0;
                    if magnitude2 > num {
                        n.position += vector4 * ((magnitude2 - num) / magnitude2);
                    }
                }
            }
            let mut vector6 = parent_position - n.position;
            if n.falloff_angle > 0.0 {
                let normalized2 = (parent_position - rest).normalize_or_zero();
                let num2 = (look_angle(vector6.normalize_or_zero(), normalized2) / n.falloff_angle).clamp(0.0, 1.0);
                n.position = n.position.lerp(rest, num2);
                vector6 = parent_position - n.position;
            }
            let magnitude3 = vector6.length();
            if magnitude3 > 0.0 {
                n.position += vector6 * ((magnitude3 - magnitude) / magnitude3);
            }
            // Parent stiffness correction (only for nodes whose parent is not the root node).
            if let Some(ppi) = self.nodes[pi].parent {
                if self.nodes[pi].stiffness != 0.0 {
                    let v8 = -vector4 * self.nodes[pi].stiffness * time;
                    let pp_pos = self.nodes[ppi].position;
                    let pp_target = self.nodes[ppi].target;
                    let pn = &mut self.nodes[pi];
                    pn.position += v8;
                    pn.position = pp_pos + (pn.position - pp_pos).normalize_or_zero() * pn.init_local_position.length();
                    let new_parent_pos = pn.position;
                    let n = &mut self.nodes[i];
                    n.position = new_parent_pos + (n.position - new_parent_pos).normalize_or_zero() * magnitude;
                    let to_direction = new_parent_pos - pp_pos;
                    let from = world_rot(q, pp_target) * local(q, parent_target).translation;
                    let r = from_to(from, to_direction) * world_rot(q, pp_target);
                    set_world_rot(q, pp_target, r);
                }
            }
            let parent_position = self.nodes[pi].position;
            let position = self.nodes[i].position;
            let to_direction2 = position - parent_position;
            let from2 = world_rot(q, parent_target) * local(q, target).translation;
            let r = from_to(from2, to_direction2) * world_rot(q, parent_target);
            set_world_rot(q, parent_target, r);
            set_world_pos(q, target, position);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Registration (VerletSpring.Start) + skinned renderers
// ---------------------------------------------------------------------------------------------

/// Entity at `rel` ("Transform/TransformUp/SK_x/..." relative to UICam).
fn find_rel(uicam: Entity, rel: &str, children: &Query<&Children>, names: &Query<&Name>) -> Option<Entity> {
    let mut cur = uicam;
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        cur = children.get(cur).ok()?.iter().find(|c| names.get(*c).is_ok_and(|n| n.as_str() == part))?;
    }
    Some(cur)
}

/// Prefab ref ("ref:<path>::Transform") -> path relative to UICam.
fn rel_path(path: &str) -> Option<&str> {
    let p = path.strip_prefix("ref:").unwrap_or(path).split("::").next()?;
    p.split_once("/Camera/UICam/").map(|(_, r)| r)
}

struct GlbPrim {
    positions: Vec<[f32; 3]>,
    normals: Option<Vec<[f32; 3]>>,
    tangents: Option<Vec<[f32; 4]>>,
    uv0: Option<Vec<[f32; 2]>>,
    uv1: Option<Vec<[f32; 2]>>,
    joints: Vec<[u16; 4]>,
    weights: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

/// Every primitive of every mesh (= Unity submesh order) of an AssetRipper mesh glb.
fn read_mesh_glb(path: &str) -> Option<Vec<GlbPrim>> {
    // the exported file, else the same glb built from the installation's Mesh
    let data = std::fs::read(path).ok().or_else(|| {
        let stem = std::path::Path::new(path).file_stem()?.to_str()?;
        crate::owned::Owned::global()?.mesh_glb(stem).map_err(|e| warn!("{e}")).ok()
    })?;
    let u32at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
    let jlen = u32at(12);
    let json: serde_json::Value = serde_json::from_slice(&data[20..20 + jlen]).ok()?;
    let bo = 20 + jlen;
    let bin = &data[bo + 8..bo + 8 + u32at(bo)];
    let read = |acc: u64| -> Option<(Vec<f32>, usize)> {
        let a = &json["accessors"][acc as usize];
        let bv = &json["bufferViews"][a["bufferView"].as_u64()? as usize];
        let off = bv["byteOffset"].as_u64().unwrap_or(0) as usize + a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let count = a["count"].as_u64()? as usize;
        let ct = a["componentType"].as_u64()?;
        let n = match a["type"].as_str()? {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => return None,
        };
        let size = match ct {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            _ => 4,
        };
        let stride = bv["byteStride"].as_u64().map(|s| s as usize).unwrap_or(size * n);
        let norm = a["normalized"].as_bool().unwrap_or(false);
        let mut v = Vec::with_capacity(count * n);
        for i in 0..count {
            for k in 0..n {
                let p = off + i * stride + k * size;
                let x = match ct {
                    5121 => bin[p] as f32 / if norm { 255.0 } else { 1.0 },
                    5123 => u16::from_le_bytes([bin[p], bin[p + 1]]) as f32 / if norm { 65535.0 } else { 1.0 },
                    5125 => u32::from_le_bytes(bin[p..p + 4].try_into().unwrap()) as f32,
                    _ => f32::from_le_bytes(bin[p..p + 4].try_into().unwrap()),
                };
                v.push(x);
            }
        }
        Some((v, n))
    };
    // Indices as u32 straight from the buffer (f32 would lose precision past 2^24, not an issue here, but keep exact).
    let read_idx = |acc: u64| -> Option<Vec<u32>> {
        let a = &json["accessors"][acc as usize];
        let bv = &json["bufferViews"][a["bufferView"].as_u64()? as usize];
        let off = bv["byteOffset"].as_u64().unwrap_or(0) as usize + a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let count = a["count"].as_u64()? as usize;
        Some(match a["componentType"].as_u64()? {
            5121 => (0..count).map(|i| bin[off + i] as u32).collect(),
            5123 => (0..count).map(|i| u16::from_le_bytes([bin[off + i * 2], bin[off + i * 2 + 1]]) as u32).collect(),
            _ => (0..count).map(|i| u32::from_le_bytes(bin[off + i * 4..off + i * 4 + 4].try_into().unwrap())).collect(),
        })
    };
    let mut out = Vec::new();
    for m in json["meshes"].as_array()? {
        for p in m["primitives"].as_array()? {
            let at = &p["attributes"];
            let get = |k: &str| at[k].as_u64().and_then(read);
            let v3 = |v: (Vec<f32>, usize)| v.0.chunks(3).map(|c| [c[0], c[1], c[2]]).collect::<Vec<_>>();
            let v2 = |v: (Vec<f32>, usize)| v.0.chunks(2).map(|c| [c[0], c[1]]).collect::<Vec<_>>();
            let v4 = |v: (Vec<f32>, usize)| v.0.chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect::<Vec<_>>();
            out.push(GlbPrim {
                positions: v3(get("POSITION")?),
                normals: get("NORMAL").map(v3),
                tangents: get("TANGENT").map(v4),
                uv0: get("TEXCOORD_0").map(v2),
                uv1: get("TEXCOORD_1").map(v2),
                joints: get("JOINTS_0")?.0.chunks(4).map(|c| [c[0] as u16, c[1] as u16, c[2] as u16, c[3] as u16]).collect(),
                weights: v4(get("WEIGHTS_0")?),
                indices: read_idx(p["indices"].as_u64()?)?,
            });
        }
    }
    Some(out)
}

#[allow(clippy::too_many_arguments)]
fn register(
    mut commands: Commands,
    mut state: ResMut<HudVerlet>,
    nodes: Res<HudNodes>,
    prefab: Res<PlayerPrefab>,
    children: Query<&Children>,
    names: Query<&Name>,
    mut q: TfQuery,
    gltf_mats: Query<(&bevy::gltf::GltfMaterialName, &MeshMaterial3d<StandardMaterial>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut ibps: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    if state.registered || !nodes.attached {
        return;
    }
    let Some(uicam) = nodes.uicam else { return };
    // Wait until UICam sits under the player camera (attach_hud's commands applied).
    if !matches!(q.get(uicam), Ok((_, Some(_)))) {
        return;
    }
    state.registered = true;
    let mut cache: Vec<(String, Arc<Settings>)> = Vec::new();
    // VerletSpring.Start: previousPosition + RegisterTrees.
    for o in &prefab.0.objects {
        let Some(c) = o.comp(SPRING_SCRIPT).or_else(|| o.comp("VerletSpring")) else { continue };
        let Some(rel) = rel_path(&o.path) else { continue };
        let Some(entity) = find_rel(uicam, rel, &children, &names) else {
            warn!("hud_verlet: no entity for {rel}");
            continue;
        };
        let mut spring = Spring { entity, previous_position: world(&q, entity).translation(), hierarchies: Vec::new(), owner: None };
        for h in c.get("transformHierarchies").and_then(|v| v.as_array()).into_iter().flatten() {
            let Some(root) = h["root"].as_str().and_then(rel_path).and_then(|r| find_rel(uicam, r, &children, &names)) else { continue };
            let set = &h["settings"];
            let settings = if set["Mode"].as_i64() == Some(2) {
                // Referenced: the Variable asset (null -> RegisterNodes does nothing).
                let Some(var) = set["Variable"].as_str().and_then(|s| s.strip_prefix("asset:")) else { continue };
                if let Some((_, s)) = cache.iter().find(|(k, _)| k == var) {
                    s.clone()
                } else {
                    let Some(s) = Settings::load_asset(var) else {
                        warn!("hud_verlet: cannot read {var}");
                        continue;
                    };
                    let s = Arc::new(s);
                    cache.push((var.to_string(), s.clone()));
                    s
                }
            } else {
                Arc::new(Settings::from_value(&set["m_constantValue"]))
            };
            let ignore = h["ignoreList"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r.as_str().and_then(rel_path).and_then(|r| find_rel(uicam, r, &children, &names)))
                .collect();
            let mut hier = Hierarchy { root, settings, nodes: Vec::new(), ignore, scalar: 1.0, resting_velocity: Vec3::ZERO, distance: 0.0 };
            hier.register_nodes(&mut q, &children);
            spring.hierarchies.push(hier);
        }
        state.springs.push(spring);
    }
    info!("hud_verlet: {} springs registered", state.springs.len());

    // Skinned renderers under UICam.
    // Materials: the StandardMaterials Bevy built for PlayerIK.glb's HUD meshes, by glTF material name.
    let mut mat_by_name: Vec<(String, Handle<StandardMaterial>)> = Vec::new();
    let mut stack = vec![uicam];
    while let Some(e) = stack.pop() {
        if let Ok((n, m)) = gltf_mats.get(e) {
            if !mat_by_name.iter().any(|(k, _)| k.as_str() == &**n) {
                mat_by_name.push((n.0.clone(), m.0.clone()));
            }
        }
        if let Ok(k) = children.get(e) {
            stack.extend(k.iter());
        }
    }
    for o in &prefab.0.objects {
        let Some(smr) = o.comp("SkinnedMeshRenderer") else { continue };
        let Some(rel) = rel_path(&o.path) else { continue };
        if smr.get("m_Enabled").and_then(|v| v.as_i64()) == Some(0) {
            continue;
        }
        let Some(renderer) = find_rel(uicam, rel, &children, &names) else { continue };
        let Some(mesh_rel) = smr.get("m_Mesh").and_then(|v| v.as_str()).and_then(|s| s.strip_prefix("asset:")) else { continue };
        let stem = mesh_rel.trim_start_matches("Mesh/").trim_end_matches(".asset");
        let bones: Vec<Entity> = smr
            .get("m_Bones")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str().and_then(rel_path).and_then(|r| find_rel(uicam, r, &children, &names)))
            .collect();
        let (Some((_, binds)), Some(prims)) = (crate::skinning::read_mesh_asset(stem), read_mesh_glb(&format!("Mesh/{stem}.glb"))) else {
            warn!("hud_verlet: cannot read mesh {stem}");
            continue;
        };
        if binds.len() != bones.len() {
            warn!("hud_verlet: {stem}: {} bind poses vs {} bones", binds.len(), bones.len());
            continue;
        }
        let ibp = ibps.add(SkinnedMeshInverseBindposes::from(binds));
        let mats: Vec<String> = smr.get("m_Materials").and_then(|v| v.as_array()).into_iter().flatten().map(|m| m.as_str().unwrap_or("").rsplit('/').next().unwrap_or("").trim_end_matches(".mat").to_string()).collect();
        for (si, p) in prims.into_iter().enumerate() {
            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, p.positions);
            if let Some(n) = p.normals {
                mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, n);
            }
            if let Some(t) = p.tangents {
                mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, t);
            }
            if let Some(u) = p.uv0 {
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, u);
            }
            if let Some(u) = p.uv1 {
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, u);
            }
            mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(p.joints));
            mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, p.weights);
            mesh.insert_indices(Indices::U32(p.indices));
            let mat_name = mats.get(si).or(mats.last()).cloned().unwrap_or_default();
            // Placeholder until hud::hud_materials swaps in the Unity material of that name (GltfMaterialName).
            let material = mat_by_name.iter().find(|(k, _)| *k == mat_name).map(|(_, h)| h.clone()).unwrap_or_default();
            commands.spawn((
                Name::new(format!("{stem}_skinned_{si}")),
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material),
                bevy::gltf::GltfMaterialName(mat_name.clone()),
                SkinnedMesh { inverse_bindposes: ibp.clone(), joints: bones.clone() },
                NoFrustumCulling,
                RenderLayers::layer(HUD_LAYER),
                Transform::default(),
                Visibility::default(),
                ChildOf(renderer),
            ));
        }
    }
}

/// VerletSpring.FixedUpdate for every spring, on Unity's fixed-step clock.
fn fixed_steps(time: Res<Time>, mut state: ResMut<HudVerlet>, mut q: TfQuery, mut dbg: Local<u32>) {
    // Springs of despawned prefab instances (a hat swapped in the cosmetics menu).
    state.springs.retain(|s| s.owner.is_none_or(|o| q.get(o).is_ok()));
    if state.springs.is_empty() {
        return;
    }
    state.accumulator += time.delta_secs().min(MAX_DT);
    let debug = std::env::var_os("RESTRAFE_DEBUG_VERLET").is_some();
    while state.accumulator >= FIXED_DT {
        state.accumulator -= FIXED_DT;
        for spring in &mut state.springs {
            let p = world(&q, spring.entity).translation();
            let velocity = p - spring.previous_position;
            spring.previous_position = p;
            for h in &mut spring.hierarchies {
                h.update(velocity, FIXED_DT, &mut q);
            }
        }
        if debug {
            *dbg += 1;
            if *dbg % 3 == 0 {
                // Root-bone deflection from its rest rotation (degrees), per spring.
                let a: Vec<String> = state
                    .springs
                    .iter()
                    .flat_map(|s| s.hierarchies.iter())
                    .map(|h| {
                        let r = local(&q, h.root).rotation;
                        format!("{:.1}", h.nodes[0].init_local_rotation.angle_between(r).to_degrees())
                    })
                    .collect();
                println!("VERLET t={:.2} root_deflection_deg=[{}]", time.elapsed_secs(), a.join(","));
            }
        }
    }
}

pub struct HudVerletPlugin;

impl Plugin for HudVerletPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HudVerlet>().add_systems(
            PostUpdate,
            (register, register_prefab_skins, fixed_steps)
                .chain()
                .after(crate::player::fpc::camera_post_update)
                .after(bevy_rapier3d::prelude::PhysicsSet::Writeback)
                .before(TransformSystems::Propagate),
        );
        // RESTRAFE_AUTOTEST=hudturn (see autotest::hud_turn_test).
        if std::env::var("RESTRAFE_AUTOTEST").is_ok_and(|n| n == "hudturn") {
            app.add_systems(
                Update,
                crate::autotest::hud_turn_test
                    .after(crate::autotest::warmup)
                    .after(crate::input::gather_input)
                    .before(crate::player::fpc::fpc_update),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Prefab instances (hats): SkinnedMeshRenderers + VerletSprings of an extracted prefab
// ---------------------------------------------------------------------------------------------

/// On a spawned prefab instance (glb scene root, e.g. a cosmetic hat): renders its `SkinnedMeshRenderer`s (the glb has
/// only their bones) and registers its `VerletSpring`s, once the scene's nodes exist. `.0`: the extracted prefab JSON.
/// The 5 skinned hats (Boppers, Fishy, Jester, Peasant Hat, Saint) each jiggle with a VerletSpring on their bones.
#[derive(Component)]
pub struct PrefabSkin(pub String);

/// Set once [`PrefabSkin`] was applied.
#[derive(Component)]
pub struct PrefabSkinDone;

/// Entity of the prefab object at `path` ("Root/Child/..."; "ref:...::Transform" accepted) below `inst`.
fn find_prefab_path(inst: Entity, path: &str, children: &Query<&Children>, names: &Query<&Name>) -> Option<Entity> {
    let p = path.strip_prefix("ref:").unwrap_or(path).split("::").next()?;
    let mut parts = p.split('/');
    let first = parts.next()?;
    let mut cur = if names.get(inst).is_ok_and(|n| n.as_str() == first) { inst } else { crate::unity_pbr::find_named(inst, first, children, names)? };
    for part in parts {
        cur = children.get(cur).ok()?.iter().find(|c| names.get(*c).is_ok_and(|n| n.as_str() == part))?;
    }
    Some(cur)
}

#[allow(clippy::too_many_arguments)]
fn register_prefab_skins(
    mut commands: Commands,
    mut state: ResMut<HudVerlet>,
    insts: Query<(Entity, &PrefabSkin), Without<PrefabSkinDone>>,
    children: Query<&Children>,
    names: Query<&Name>,
    layers: Query<&RenderLayers>,
    mut q: TfQuery,
    mut meshes: ResMut<Assets<Mesh>>,
    mut ibps: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut umats: crate::unity_pbr::UnityMatParams,
) {
    for (inst, ps) in &insts {
        let Ok(prefab) = crate::weapons::data::Prefab::load(&ps.0) else {
            warn!("prefab skin: cannot read {}", ps.0);
            commands.entity(inst).insert(PrefabSkinDone);
            continue;
        };
        let Some(root_path) = prefab.objects.first().map(|o| o.path.clone()) else { continue };
        // The glb scene spawns asynchronously: wait for its root node.
        if find_prefab_path(inst, &root_path, &children, &names).is_none() {
            continue;
        }
        commands.entity(inst).insert(PrefabSkinDone);
        let resolve = |r: &str| find_prefab_path(inst, r, &children, &names);
        // VerletSpring.Start.
        let mut n_springs = 0;
        for o in &prefab.objects {
            let Some(c) = o.comp(SPRING_SCRIPT).or_else(|| o.comp("VerletSpring")) else { continue };
            if c.get("m_Enabled").and_then(|v| v.as_i64()) == Some(0) {
                continue;
            }
            let Some(entity) = resolve(&o.path) else { continue };
            let mut spring = Spring { entity, previous_position: world(&q, entity).translation(), hierarchies: Vec::new(), owner: Some(inst) };
            for h in c.get("transformHierarchies").and_then(|v| v.as_array()).into_iter().flatten() {
                let Some(root) = h["root"].as_str().and_then(resolve) else { continue };
                let set = &h["settings"];
                let settings = if set["Mode"].as_i64() == Some(2) {
                    let Some(s) = set["Variable"].as_str().and_then(|s| s.strip_prefix("asset:")).and_then(Settings::load_asset) else { continue };
                    Arc::new(s)
                } else {
                    Arc::new(Settings::from_value(&set["m_constantValue"]))
                };
                let ignore = h["ignoreList"].as_array().into_iter().flatten().filter_map(|r| r.as_str().and_then(resolve)).collect();
                let mut hier = Hierarchy { root, settings, nodes: Vec::new(), ignore, scalar: 1.0, resting_velocity: Vec3::ZERO, distance: 0.0 };
                hier.register_nodes(&mut q, &children);
                spring.hierarchies.push(hier);
            }
            state.springs.push(spring);
            n_springs += 1;
        }
        // SkinnedMeshRenderers: mesh glb built from the installation, Unity bind poses, m_Bones joints, Unity
        // materials.
        let mut n_meshes = 0;
        for o in &prefab.objects {
            let Some(smr) = o.comp("SkinnedMeshRenderer") else { continue };
            if smr.get("m_Enabled").and_then(|v| v.as_i64()) == Some(0) {
                continue;
            }
            let Some(renderer) = resolve(&o.path) else { continue };
            let Some(mesh_rel) = smr.get("m_Mesh").and_then(|v| v.as_str()).and_then(|s| s.strip_prefix("asset:")) else { continue };
            let stem = mesh_rel.trim_start_matches("Mesh/").trim_end_matches(".asset");
            let bones: Vec<Entity> = smr.get("m_Bones").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|r| r.as_str().and_then(resolve)).collect();
            let glb = format!("{}/assets/cosmetics/hats/mesh/{stem}.glb", env!("CARGO_MANIFEST_DIR"));
            let (Some((_, binds)), Some(prims)) = (crate::skinning::read_mesh_asset(stem), read_mesh_glb(&glb)) else {
                warn!("prefab skin: cannot read mesh {stem}");
                continue;
            };
            if binds.len() != bones.len() {
                warn!("prefab skin: {stem}: {} bind poses vs {} bones", binds.len(), bones.len());
                continue;
            }
            let ibp = ibps.add(SkinnedMeshInverseBindposes::from(binds));
            let mats: Vec<String> = smr
                .get("m_Materials")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .map(|m| m.as_str().unwrap_or("").rsplit('/').next().unwrap_or("").trim_end_matches(".mat").to_string())
                .collect();
            let layer = layers.get(renderer).ok().cloned();
            for (si, p) in prims.into_iter().enumerate() {
                let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, p.positions);
                if let Some(n) = p.normals {
                    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, n);
                }
                if let Some(t) = p.tangents {
                    mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, t);
                }
                if let Some(u) = p.uv0 {
                    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, u);
                }
                if let Some(u) = p.uv1 {
                    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, u);
                }
                mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(p.joints));
                mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, p.weights);
                mesh.insert_indices(Indices::U32(p.indices));
                let mat_name = mats.get(si).or(mats.last()).cloned().unwrap_or_default();
                let Some(material) = umats.material(&mat_name) else {
                    warn!("prefab skin: {stem}: no material {mat_name}");
                    continue;
                };
                let mut e = commands.spawn((
                    Name::new(format!("{stem}_skinned_{si}")),
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(material),
                    SkinnedMesh { inverse_bindposes: ibp.clone(), joints: bones.clone() },
                    NoFrustumCulling,
                    Transform::default(),
                    Visibility::default(),
                    ChildOf(renderer),
                ));
                if let Some(l) = &layer {
                    e.insert(l.clone());
                }
                n_meshes += 1;
            }
        }
        info!("prefab skin {}: {n_meshes} skinned meshes, {n_springs} springs", ps.0);
    }
}
