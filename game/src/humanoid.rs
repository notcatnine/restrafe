//! Unity Mecanim HUMANOID animation on the glb skeleton (muscle clips -> bone local rotations).
//!
//! Data: `ripped/.../Avatar/SK_Aboubi_00Avatar.asset` (human skeleton nodes with `Axes`: preQ, postQ, sgn,
//! limit min/max in radians; T-pose; root X; human scale) + `AnimationClip/*.anim` (m_FloatCurves, classID 95:
//! "Spine Front-Back", "Left Arm Down-Up", "RootT.x", ... in HumanTrait.MuscleName spelling).
//!
//! Math (Unity Mecanim `human/human.cpp`, `Axes` / `ZYRoll`; same as UniVRM/UniHumanoid pose transfer):
//!   angle[dof] = sgn[dof] * (muscle >= 0 ? muscle * max[dof] : -muscle * min[dof])       (radians; limits in the Axes)
//!   local_rot  = preQ * ZYRoll(angle) * inverse(postQ),   ZYRoll(v) = Rz(v.z) * Ry(v.y) * Rx(v.x)
//!   dof 0 = X = twist, 1 = Y = side/in-out/spread/tilt, 2 = Z = front-back/down-up/stretch/nod
//!   (confirmed by the avatar's limits: arm = (+-90 twist, +-100 front-back, -60..100 down-up)).
//! Hips: body pose from RootQ/RootT: hipsQ = RootQ * inverse(RootX.q); hipsPos = RootT * humanScale - hipsQ * (RootX.t - hipsT_tpose).
//! Space: glb nodes are AssetRipper's X-mirrored Unity space: rotation (x,y,z,w) -> (x,-y,-z,w), position x -> -x.
//! The hips' glb parent ("SK_Aboubi_00" armature node: rot -90 about X, scale 100) is folded into the avatar's hips pose,
//! so hips_glb_local = inverse(parent.rot) * mirror(hips_avatar) (translation additionally / parent scale).

use crate::health::Dummy;
use bevy::prelude::*;
use serde_yaml::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
const ASSET_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../ripped/ExportedProject/Assets");
pub const DEFAULT_CLIP: &str = "Aboubi_Idle_00";

// ---------------------------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------------------------

fn num(v: &Value) -> f32 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0) as f32,
        Value::String(s) => match s.as_str() {
            "Infinity" | ".inf" => f32::INFINITY,
            "-Infinity" | "-.inf" => f32::NEG_INFINITY,
            _ => s.parse().unwrap_or(0.0),
        },
        _ => 0.0,
    }
}

fn v3(v: &Value) -> Vec3 {
    Vec3::new(num(&v["x"]), num(&v["y"]), num(&v["z"]))
}

fn q4(v: &Value) -> Quat {
    Quat::from_xyzw(num(&v["x"]), num(&v["y"]), num(&v["z"]), num(&v["w"]))
}

#[cfg(test)]
fn parse_unity_yaml(text: &str) -> Value {
    let body: String = text.lines().filter(|l| !l.starts_with('%') && !l.starts_with("---")).collect::<Vec<_>>().join("\n");
    serde_yaml::from_str(&body).expect("unity yaml")
}

/// Hex-encoded little-endian i32 array on the `occurrence`-th line starting with `key:` (raw text, since
/// YAML would read these digit-only blobs as numbers).
#[cfg(test)]
fn hex_i32s(text: &str, key: &str, occurrence: usize) -> Vec<i32> {
    let pat = format!("{key}: ");
    let line = text.lines().filter(|l| l.trim_start().starts_with(&pat)).nth(occurrence).unwrap_or("");
    let hex = line.trim_start().trim_start_matches(&pat).trim();
    (0..hex.len() / 8)
        .map(|i| {
            let b: Vec<u8> = (0..4).map(|k| u8::from_str_radix(&hex[i * 8 + k * 2..i * 8 + k * 2 + 2], 16).unwrap_or(0)).collect();
            i32::from_le_bytes([b[0], b[1], b[2], b[3]])
        })
        .collect()
}

pub fn crc32(s: &str) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in s.as_bytes() {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

// ---------------------------------------------------------------------------------------------
// Avatar
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct Axes {
    pub pre: Quat,
    pub post: Quat,
    pub sgn: Vec3,
    pub min: Vec3,
    pub max: Vec3,
}

pub struct HumanNode {
    /// crc32 of the node name (Unity skeleton name id).
    pub name_hash: u32,
    pub parent: i32,
    pub axes: Option<Axes>,
    /// T-pose local transform (Unity space).
    pub t: Vec3,
    pub q: Quat,
    /// T-pose local scale.
    pub s: Vec3,
}

pub struct Avatar {
    pub nodes: Vec<HumanNode>,
    pub root_t: Vec3,
    pub root_q: Quat,
    pub scale: f32,
    /// muscle name -> (human node, dof 0..2)
    pub muscles: HashMap<String, (usize, usize)>,
    /// m_HumanBoneIndex: human bone (HumanBodyBones order) -> skeleton node (-1 = missing).
    pub human_bone: Vec<i32>,
    /// m_HumanBoneMass per human bone.
    pub mass: Vec<f32>,
    /// Calibration of the posed body frame (see `body_frame`): T-pose global hips scale, orientation correction and
    /// centre-of-mass correction (body space) that make the T-pose reproduce m_RootX exactly.
    hips_scale: Vec3,
    body_corr: Quat,
    com_corr: Vec3,
    /// T-pose global rotations of the hips and of the top spine bone (UpperChest / Chest / Spine).
    hips_tq: Quat,
    upper_tq: Quat,
}

impl Avatar {
    /// The Avatar asset of that file name ("SK_Aboubi_00Avatar.asset"), read from the installation.
    pub fn load(file: &str) -> Avatar {
        let name = file.trim_end_matches(".asset");
        let o = crate::owned::Owned::global().expect("STRAFTAT installation");
        let (n, p) = o.same_named(90, name).and_then(|v| v.first().copied()).expect("avatar in the installation");
        let (_, f) = crate::prefab_json::script_object(o, o.assemblies().expect("managed assemblies"), n, p).expect("avatar");
        let ints = |v: &serde_json::Value| -> Vec<i32> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).map(|x| x as i32).collect()).unwrap_or_default() };
        let a = &f["m_Avatar"];
        let h = &a["m_Human"]["data"];
        let arrays = [
            ints(&a["m_SkeletonNameIDArray"]),
            ints(&a["m_HumanSkeletonIndexArray"]),
            ints(&h["m_HumanBoneIndex"]),
            ints(&h["m_LeftHand"]["data"]["m_HandBoneIndex"]),
            ints(&h["m_RightHand"]["data"]["m_HandBoneIndex"]),
        ];
        let doc: Value = serde_yaml::to_value(serde_json::json!({ "Avatar": serde_json::Value::Object(f) })).expect("avatar value");
        Self::from_doc(&doc, arrays)
    }

    /// The same from the old exported YAML (the reference of the comparison test).
    #[cfg(test)]
    fn load_yaml(path: &str) -> Avatar {
        let text = std::fs::read_to_string(path).expect("avatar asset");
        let arrays = [
            hex_i32s(&text, "m_SkeletonNameIDArray", 0),
            hex_i32s(&text, "m_HumanSkeletonIndexArray", 0),
            hex_i32s(&text, "m_HumanBoneIndex", 0),
            hex_i32s(&text, "m_HandBoneIndex", 0),
            hex_i32s(&text, "m_HandBoneIndex", 1),
        ];
        Self::from_doc(&parse_unity_yaml(&text), arrays)
    }

    /// `doc`: the Avatar document ({"Avatar": {...}}); `arrays`: skeleton name ids, human skeleton index, human bone
    /// index, left and right hand bone indices.
    fn from_doc(doc: &Value, arrays: [Vec<i32>; 5]) -> Avatar {
        let h = &doc["Avatar"]["m_Avatar"]["m_Human"]["data"];
        let [name_ids, skel_idx, hbi, left_hand, right_hand] = arrays;

        let axes_arr: Vec<Axes> = h["m_Skeleton"]["data"]["m_AxesArray"]
            .as_sequence()
            .map(|s| {
                s.iter()
                    .map(|a| Axes {
                        pre: q4(&a["m_PreQ"]),
                        post: q4(&a["m_PostQ"]),
                        sgn: v3(&a["m_Sgn"]),
                        min: v3(&a["m_Limit"]["m_Min"]),
                        max: v3(&a["m_Limit"]["m_Max"]),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let node_list = h["m_Skeleton"]["data"]["m_Node"].as_sequence().cloned().unwrap_or_default();
        let pose = h["m_SkeletonPose"]["data"]["m_X"].as_sequence().cloned().unwrap_or_default();
        let nodes: Vec<HumanNode> = node_list
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let axes_id = n["m_AxesId"].as_i64().unwrap_or(-1);
                let avatar_node = skel_idx.get(i).copied().unwrap_or(0) as usize;
                HumanNode {
                    name_hash: name_ids.get(avatar_node).copied().unwrap_or(0) as u32,
                    parent: n["m_ParentId"].as_i64().unwrap_or(-1) as i32,
                    axes: if axes_id >= 0 { axes_arr.get(axes_id as usize).cloned() } else { None },
                    t: v3(&pose[i]["t"]),
                    q: q4(&pose[i]["q"]),
                    s: { let s = v3(&pose[i]["s"]); if s == Vec3::ZERO { Vec3::ONE } else { s } },
                }
            })
            .collect();

        // --- muscle name -> (node, dof), HumanTrait.MuscleName spelling ---
        let mut muscles = HashMap::new();
        let node_of = |internal: usize| -> Option<usize> { hbi.get(internal).copied().filter(|&v| v >= 0).map(|v| v as usize) };
        let mut add = |prefix: String, node: Option<usize>, dofs: &[(&str, usize)]| {
            if let Some(n) = node {
                for (suffix, dof) in dofs {
                    muscles.insert(format!("{prefix} {suffix}"), (n, *dof));
                }
            }
        };
        let spine = [("Front-Back", 2), ("Left-Right", 1), ("Twist Left-Right", 0)];
        let neck = [("Nod Down-Up", 2), ("Tilt Left-Right", 1), ("Turn Left-Right", 0)];
        add("Spine".into(), node_of(7), &spine);
        add("Chest".into(), node_of(8), &spine);
        add("UpperChest".into(), node_of(9), &spine);
        add("Neck".into(), node_of(10), &neck);
        add("Head".into(), node_of(11), &neck);
        add("Jaw".into(), node_of(24), &[("Close", 2), ("Left-Right", 1)]);
        for (side, off) in [("Left", 0usize), ("Right", 1usize)] {
            add(format!("{side} Eye"), node_of(22 + off), &[("Down-Up", 2), ("In-Out", 1)]);
            add(format!("{side} Upper Leg"), node_of(1 + off), &[("Front-Back", 2), ("In-Out", 1), ("Twist In-Out", 0)]);
            add(format!("{side} Lower Leg"), node_of(3 + off), &[("Stretch", 2), ("Twist In-Out", 0)]);
            add(format!("{side} Foot"), node_of(5 + off), &[("Up-Down", 2), ("Twist In-Out", 1)]);
            add(format!("{side} Toes"), node_of(20 + off), &[("Up-Down", 2)]);
            add(format!("{side} Shoulder"), node_of(12 + off), &[("Down-Up", 2), ("Front-Back", 1)]);
            add(format!("{side} Arm"), node_of(14 + off), &[("Down-Up", 2), ("Front-Back", 1), ("Twist In-Out", 0)]);
            add(format!("{side} Forearm"), node_of(16 + off), &[("Stretch", 2), ("Twist In-Out", 0)]);
            add(format!("{side} Hand"), node_of(18 + off), &[("Down-Up", 2), ("In-Out", 1)]);
            let hand = if off == 0 { &left_hand } else { &right_hand };
            for (fi, finger) in ["Thumb", "Index", "Middle", "Ring", "Little"].iter().enumerate() {
                let fnode = |k: usize| hand.get(fi * 3 + k).copied().filter(|&v| v >= 0).map(|v| v as usize);
                let p = format!("{side}Hand.{finger}");
                add(p.clone(), fnode(0), &[("1 Stretched", 2), ("Spread", 1)]);
                add(p.clone(), fnode(1), &[("2 Stretched", 2)]);
                add(p, fnode(2), &[("3 Stretched", 2)]);
            }
        }

        let root = &h["m_RootX"];
        let mass: Vec<f32> = h["m_HumanBoneMass"].as_sequence().map(|s| s.iter().map(num).collect()).unwrap_or_default();
        let mut av = Avatar {
            nodes,
            root_t: v3(&root["t"]),
            root_q: q4(&root["q"]),
            scale: num(&h["m_Scale"]),
            muscles,
            human_bone: hbi.clone(),
            mass,
            hips_scale: Vec3::ONE,
            body_corr: Quat::IDENTITY,
            com_corr: Vec3::ZERO,
            hips_tq: Quat::IDENTITY,
            upper_tq: Quat::IDENTITY,
        };
        av.calibrate();
        if std::env::var_os("RESTRAFE_COM_CHECK").is_some() {
            av.com_check();
        }
        if let Ok(list) = std::env::var("RESTRAFE_GOAL_CHECK") {
            for name in list.split(',') {
                if let Some(c) = Clip::load(name, &av) {
                    goal_check(&av, &c);
                }
            }
        }
        av
    }

    /// Global (root-space) transforms of all skeleton nodes for local rotations `rot` (T-pose local positions).
    pub fn fk(&self, rot: &[Quat]) -> Vec<(Vec3, Quat)> {
        let mut g: Vec<(Vec3, Quat, Vec3)> = Vec::with_capacity(self.nodes.len());
        for (i, n) in self.nodes.iter().enumerate() {
            let q = rot.get(i).copied().unwrap_or(n.q);
            if n.parent < 0 {
                g.push((n.t, q, n.s));
            } else {
                let (pt, pq, ps) = g[n.parent as usize];
                g.push((pt + pq * (ps * n.t), pq * q, ps * n.s));
            }
        }
        g.into_iter().map(|(t, q, _)| (t, q)).collect()
    }

    fn hips_node(&self) -> usize {
        self.human_bone.first().copied().filter(|&v| v >= 0).map(|v| v as usize).unwrap_or(1)
    }

    /// Mecanim body frame of a pose, in the hips' frame (hips at the origin, identity rotation, T-pose global scale):
    /// centre of mass = m_HumanBoneMass-weighted segment centres (+ a T-pose calibration onto m_RootX);
    /// orientation from the upper legs and upper arms (up = arm centre - leg centre, right = normalized leg axis + arm axis).
    fn body_frame(&self, rot: &[Quat]) -> (Vec3, Quat) {
        let hips = self.hips_node();
        let mut g: Vec<Option<(Vec3, Quat, Vec3)>> = vec![None; self.nodes.len()];
        g[hips] = Some((Vec3::ZERO, Quat::IDENTITY, self.hips_scale));
        for (i, n) in self.nodes.iter().enumerate() {
            if i == hips || n.parent < 0 {
                continue;
            }
            if let Some((pt, pq, ps)) = g[n.parent as usize] {
                let q = rot.get(i).copied().unwrap_or(n.q);
                g[i] = Some((pt + pq * (ps * n.t), pq * q, ps * n.s));
            }
        }
        let pos = |hb: usize| self.human_bone.get(hb).copied().filter(|&v| v >= 0).and_then(|v| g[v as usize]).map(|x| x.0);
        let (mut com, mut msum) = (Vec3::ZERO, 0.0);
        // Mass points: segment centres (bone -> next human bone; end bones at the joint). Fitted against the clips'
        // stored IK goals (RESTRAFE_GOAL_CHECK): <= 1.2 cm on every clip. Debug: RESTRAFE_COM_MODE=A uses the joints.
        let seg = !std::env::var("RESTRAFE_COM_MODE").is_ok_and(|v| v == "A");
        let next = |hb: usize| -> Option<usize> {
            Some(match hb {
                1 => 3, 2 => 4, 3 => 5, 4 => 6, 7 => 10, 10 => 11, 12 => 14, 13 => 15, 14 => 16, 15 => 17, 16 => 18, 17 => 19,
                _ => return None,
            })
        };
        for (hb, m) in self.mass.iter().enumerate() {
            if let Some(p) = pos(hb) {
                let p = if seg { next(hb).and_then(pos).map(|q| (p + q) * 0.5).unwrap_or(p) } else { p };
                com += p * *m;
                msum += *m;
            }
        }
        let com = if msum > 0.0 { com / msum } else { Vec3::ZERO };
        // Body orientation: Mecanim builds it from the upper legs' and upper arms' positions (up = arm centre - leg
        // centre, right = leg axis + arm axis, the raw vectors summed so the wider arm axis weighs more). Fitted
        // against the clips' stored IK goals (RESTRAFE_GOAL_CHECK): the torso twist poses go from 3.6 to 0.8 cm and
        // Gun_TwoHanded's feet from 7.6 to 1.4 cm; RESTRAFE_BODY_ORIENT=rotations = the earlier hips / top-spine
        // rotation average.
        let upper = [9usize, 8, 7].iter().find_map(|&hb| self.human_bone.get(hb).copied().filter(|&v| v >= 0)).and_then(|v| g[v as usize]);
        if std::env::var("RESTRAFE_BODY_ORIENT").is_ok_and(|v| v == "rotations") {
            if let Some((_, uq, _)) = upper {
                let lower = self.hips_tq.inverse();
                let upper = uq * self.upper_tq.inverse();
                return (com, lower.slerp(upper, 0.5) * self.root_q);
            }
        }
        let body = match (pos(1), pos(2), pos(14), pos(15)) {
            (Some(ll), Some(lr), Some(al), Some(ar)) => {
                let up = ((al + ar) * 0.5 - (ll + lr) * 0.5).normalize_or(Vec3::Y);
                let right = ((lr - ll) + (ar - al)).normalize_or(Vec3::X);
                let fwd = right.cross(up).normalize_or(Vec3::Z);
                let right = up.cross(fwd);
                Quat::from_mat3(&Mat3::from_cols(right, up, fwd)).normalize()
            }
            _ => Quat::IDENTITY,
        };
        (com, body)
    }

    fn calibrate(&mut self) {
        // T-pose global hips transform (with scale), then the corrections that map the T-pose body frame onto m_RootX.
        let hips = self.hips_node();
        let mut g: Vec<(Vec3, Quat, Vec3)> = Vec::with_capacity(self.nodes.len());
        for n in &self.nodes {
            if n.parent < 0 {
                g.push((n.t, n.q, n.s));
            } else {
                let (pt, pq, ps) = g[n.parent as usize];
                g.push((pt + pq * (ps * n.t), pq * n.q, ps * n.s));
            }
        }
        let (hp, hq, hs) = g[hips];
        self.hips_scale = hs;
        self.hips_tq = hq;
        self.upper_tq = [9usize, 8, 7].iter().find_map(|&hb| self.human_bone.get(hb).copied().filter(|&v| v >= 0)).map(|v| g[v as usize].1).unwrap_or(hq);
        let rot: Vec<Quat> = self.nodes.iter().map(|n| n.q).collect();
        let (com, body) = self.body_frame(&rot);
        self.body_corr = (hq * body).inverse() * self.root_q;
        let com_world = hp + hq * com;
        self.com_corr = self.root_q.inverse() * (self.root_t - com_world);
    }

    /// Global hips (position, rotation) for local rotations `rot` and the clip's body pose (RootT normalized, RootQ).
    pub fn hips_from_body(&self, rot: &[Quat], root_t: Vec3, root_q: Quat) -> (Vec3, Quat) {
        let (com, body) = self.body_frame(rot);
        let body = body * self.body_corr;
        let com = if std::env::var_os("RESTRAFE_NO_COM_CORR").is_some() { com } else { com + body * self.com_corr };
        let hips_q = root_q * body.inverse();
        (root_t * self.scale - hips_q * com, hips_q)
    }

    fn com_check(&self) {
        let rot: Vec<Quat> = self.nodes.iter().map(|n| n.q).collect();
        let g = self.fk(&rot);
        let pos = |hb: usize| self.human_bone.get(hb).copied().filter(|&v| v >= 0).map(|v| g[v as usize].0);
        // A: mass * joint position.
        let mut a = Vec3::ZERO;
        let mut msum = 0.0;
        for (hb, m) in self.mass.iter().enumerate() {
            if let Some(p) = pos(hb) {
                a += p * *m;
                msum += *m;
            }
        }
        a /= msum;
        // B: segment centres (bone -> next bone).
        let next = |hb: usize| -> Option<usize> {
            Some(match hb {
                1 => 3, 2 => 4, 3 => 5, 4 => 6, 7 => 10, 10 => 11, 12 => 14, 13 => 15, 14 => 16, 15 => 17, 16 => 18, 17 => 19,
                _ => return None,
            })
        };
        let mut b = Vec3::ZERO;
        for (hb, m) in self.mass.iter().enumerate() {
            let Some(p) = pos(hb) else { continue };
            let c = next(hb).and_then(pos).map(|q| (p + q) * 0.5).unwrap_or(p);
            b += c * *m;
        }
        b /= msum;
        for hb in 0..self.mass.len() {
            if let Some(p) = pos(hb) {
                println!("COMBONE {hb} mass={:.4} pos={:?}", self.mass[hb], p);
            }
        }
        {
            let rot: Vec<Quat> = self.nodes.iter().map(|n| n.q).collect();
            let (t, q) = self.hips_from_body(&rot, self.root_t / self.scale, self.root_q);
            let gh = self.fk(&rot)[self.hips_node()];
            println!("COMSELF tpose hips from body t={t:?} q={q:?} vs fk t={:?} q={:?}", gh.0, gh.1);
        }
        println!("COMCHECK rootX.t={:?} A(joints)={:?} B(segments)={:?} mass_sum={msum} hips={:?}", self.root_t, a, b, pos(0));
    }

    /// Unity local rotation of a human node for the given per-dof muscle values.
    fn bone_rotation(&self, node: usize, m: [f32; 3]) -> Quat {
        let Some(ax) = &self.nodes[node].axes else {
            return self.nodes[node].q;
        };
        let mut ang = [0.0f32; 3];
        for d in 0..3 {
            let (mn, mx, s) = (ax.min[d], ax.max[d], ax.sgn[d]);
            ang[d] = s * if m[d] >= 0.0 { m[d] * mx } else { -m[d] * mn };
        }
        let zyx = if std::env::var("RESTRAFE_ZYROLL").is_ok_and(|v| v == "euler") {
            Quat::from_rotation_z(ang[2]) * Quat::from_rotation_y(ang[1]) * Quat::from_rotation_x(ang[0])
        } else {
            // Mecanim ZYRoll2Quat on half-angle tangents: swing about (0, ty, tz) then twist about X,
            // q = normalize(tx, ty + tx tz, tz - tx ty, 1).
            let t = |a: f32| (a * 0.5).tan();
            let (tx, ty, tz) = (t(ang[0]), t(ang[1]), t(ang[2]));
            Quat::from_xyzw(tx, ty + tx * tz, tz - tx * ty, 1.0).normalize()
        };
        ax.pre * zyx * ax.post.inverse()
    }
}

// ---------------------------------------------------------------------------------------------
// Clip
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Key {
    t: f32,
    v: f32,
    in_s: f32,
    out_s: f32,
}

#[derive(Clone)]
enum Target {
    Muscle(usize, usize),
    RootT(usize),
    RootQ(usize),
}

struct Curve {
    keys: Vec<Key>,
    target: Target,
}

impl Curve {
    /// Unity AnimationCurve.Evaluate (cubic Hermite; infinite tangent = stepped).
    fn eval(&self, t: f32) -> f32 {
        let k = &self.keys;
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

pub struct Clip {
    pub name: String,
    pub length: f32,
    pub loop_time: bool,
    curves: Vec<Curve>,
    /// IK goal curves stored by the importer (LeftFootT.x ... RightHandQ.w): debug ground truth (goal_check).
    goals: Vec<(String, Curve)>,
}

impl Clip {
    /// The humanoid clip of that name, decoded from the installation (anim_clip.rs): muscle, root and IK goal curves.
    pub fn load(name: &str, avatar: &Avatar) -> Option<Clip> {
        let c = crate::anim_clip::load(name, &|_| None)?;
        let mut curves = Vec::new();
        let mut goals = Vec::new();
        for cv in &c.curves {
            let crate::anim_clip::Kind::Muscle { name: attr, .. } = &cv.kind else { continue };
            let keys: Vec<Key> = cv.comps[0].iter().map(|k| Key { t: k.t, v: k.v, in_s: k.in_s, out_s: k.out_s }).collect();
            if ["LeftFoot", "RightFoot", "LeftHand", "RightHand"].iter().any(|g| attr.starts_with(&format!("{g}T.")) || attr.starts_with(&format!("{g}Q."))) {
                goals.push((attr.clone(), Curve { keys, target: Target::RootT(0) }));
                continue;
            }
            let target = if let Some(&(n, d)) = avatar.muscles.get(attr.as_str()) {
                Target::Muscle(n, d)
            } else if let Some(r) = attr.strip_prefix("RootT.") {
                Target::RootT(["x", "y", "z"].iter().position(|s| *s == r).unwrap_or(0))
            } else if let Some(r) = attr.strip_prefix("RootQ.") {
                Target::RootQ(["x", "y", "z", "w"].iter().position(|s| *s == r).unwrap_or(0))
            } else {
                continue;
            };
            curves.push(Curve { keys, target });
        }
        Some(Clip { name: name.to_string(), length: (c.stop - c.start).max(1e-4), loop_time: c.loop_time, curves, goals })
    }
}

/// Sampled pose: per-human-node Unity local rotation and the hips (pos, rot) in avatar space.
pub struct Pose {
    pub rot: Vec<Quat>,
    pub hips_t: Vec3,
    pub hips_q: Quat,
}

/// Muscle-space pose (what Mecanim blends): per human node 3 muscle values + root (centre of mass) T/Q.
#[derive(Clone)]
pub struct MPose {
    pub musc: Vec<[f32; 3]>,
    pub root_t: [f32; 3],
    pub root_q: [f32; 4],
}

impl MPose {
    pub fn neutral(avatar: &Avatar) -> MPose {
        MPose { musc: vec![[0.0; 3]; avatar.nodes.len()], root_t: [0.0, 1.0, 0.0], root_q: [avatar.root_q.x, avatar.root_q.y, avatar.root_q.z, avatar.root_q.w] }
    }

    /// Weighted mix (weights are normalised). Root rotations are sign-aligned to the first entry.
    pub fn mix(poses: &[(f32, &MPose)]) -> Option<MPose> {
        let total: f32 = poses.iter().map(|p| p.0).sum();
        let first = poses.iter().find(|p| p.0 > 0.0).or(poses.first())?.1;
        if total <= 1e-6 {
            return Some(first.clone());
        }
        let mut out = MPose { musc: vec![[0.0; 3]; first.musc.len()], root_t: [0.0; 3], root_q: [0.0; 4] };
        let ref_q = Quat::from_xyzw(first.root_q[0], first.root_q[1], first.root_q[2], first.root_q[3]);
        for (w, p) in poses {
            let w = w / total;
            if w <= 0.0 {
                continue;
            }
            for (o, m) in out.musc.iter_mut().zip(&p.musc) {
                for d in 0..3 {
                    o[d] += m[d] * w;
                }
            }
            for d in 0..3 {
                out.root_t[d] += p.root_t[d] * w;
            }
            let q = Quat::from_xyzw(p.root_q[0], p.root_q[1], p.root_q[2], p.root_q[3]);
            let s = if q.dot(ref_q) < 0.0 { -w } else { w };
            for d in 0..4 {
                out.root_q[d] += p.root_q[d] * s;
            }
        }
        Some(out)
    }
}

pub fn sample_m(avatar: &Avatar, clip: &Clip, t: f32) -> MPose {
    let mut m = MPose::neutral(avatar);
    for c in &clip.curves {
        let v = c.eval(t);
        match c.target {
            Target::Muscle(n, d) => m.musc[n][d] = v,
            Target::RootT(i) => m.root_t[i] = v,
            Target::RootQ(i) => m.root_q[i] = v,
        }
    }
    m
}

pub fn pose_from(avatar: &Avatar, m: &MPose) -> Pose {
    let root_t = Vec3::from(m.root_t);
    let rq = Quat::from_xyzw(m.root_q[0], m.root_q[1], m.root_q[2], m.root_q[3]).normalize();
    let rot: Vec<Quat> = (0..avatar.nodes.len()).map(|i| avatar.bone_rotation(i, m.musc[i])).collect();
    // Mecanim: RootT / RootQ are the body's centre of mass and orientation, computed from the posed skeleton (so a
    // sliding pose with the legs forward sits higher than a fixed T-pose hips offset would put it).
    let (hips_t, hips_q) = if std::env::var_os("RESTRAFE_OLD_HIPS").is_some() {
        let hips = &avatar.nodes[1];
        let hips_q = rq * avatar.root_q.inverse() * hips.q;
        let offset = avatar.root_t - hips.t;
        (root_t * avatar.scale - hips_q * (hips.q.inverse() * offset), hips_q)
    } else {
        avatar.hips_from_body(&rot, root_t, rq)
    };
    Pose { rot, hips_t, hips_q }
}

/// Debug (RESTRAFE_GOAL_CHECK=clip1,clip2,...): compare the posed feet / hands with the clip's stored IK goals
/// (goal = inverse(RootQ) * (bone - RootT * humanScale) / humanScale, Unity's body-relative goal space).
pub fn goal_check(avatar: &Avatar, clip: &Clip) {
    // T-pose foot frames: Unity's foot goal is the sole (the ankle's T-pose height below it, carried by the foot).
    let trot: Vec<Quat> = avatar.nodes.iter().map(|n| n.q).collect();
    let tg = avatar.fk(&trot);
    let n = (clip.length * 30.0).ceil().max(1.0) as usize;
    let (mut se, mut cnt) = ([0.0f32; 4], 0usize);
    for k in 0..n {
        let t = clip.length * k as f32 / n as f32;
        let m = sample_m(avatar, clip, t);
        let pose = pose_from(avatar, &m);
        let hips = avatar.hips_node();
        let mut g: Vec<Option<(Vec3, Quat, Vec3)>> = vec![None; avatar.nodes.len()];
        g[hips] = Some((pose.hips_t, pose.hips_q, avatar.hips_scale));
        for (i, nd) in avatar.nodes.iter().enumerate() {
            if i == hips || nd.parent < 0 {
                continue;
            }
            if let Some((pt, pq, ps)) = g[nd.parent as usize] {
                g[i] = Some((pt + pq * (ps * nd.t), pq * pose.rot[i], ps * nd.s));
            }
        }
        let rq = Quat::from_xyzw(m.root_q[0], m.root_q[1], m.root_q[2], m.root_q[3]).normalize();
        let rt = Vec3::from(m.root_t) * avatar.scale;
        let goal = |name: &str| -> Vec3 {
            let v = |c: &str| clip.goals.iter().find(|(a, _)| a == &format!("{name}T.{c}")).map(|(_, k)| k.eval(t)).unwrap_or(f32::NAN);
            Vec3::new(v("x"), v("y"), v("z"))
        };
        for (gi, (name, hb)) in [("LeftFoot", 5usize), ("RightFoot", 6), ("LeftHand", 18), ("RightHand", 19)].into_iter().enumerate() {
            let Some(node) = avatar.human_bone.get(hb).copied().filter(|&v| v >= 0).map(|v| v as usize) else { continue };
            let Some(p) = g[node] else { continue };
            let mut pt = p.0;
            if hb == 5 || hb == 6 {
                let down = tg[node].1.inverse() * Vec3::new(0.0, -tg[node].0.y, 0.0);
                pt += p.1 * down;
            }
            let ours = rq.inverse() * (pt - rt) / avatar.scale;
            let e = (ours - goal(name)).length() * avatar.scale;
            se[gi] += e * e;
            if k == 0 && std::env::var_os("RESTRAFE_GOAL_VERBOSE").is_some() {
                println!("GOAL {} {name}: ours={ours:.3?} clip={:.3?} avatar_space_bone={:.3?} sole={:.3?} hips={:.3?}", clip.name, goal(name), p.0, pt, pose.hips_t);
            }
        }
        if k == 0 && std::env::var_os("RESTRAFE_GOAL_VERBOSE").is_some() {
            for hb in [1usize, 2, 3, 4, 5, 6] {
                if let Some(node) = avatar.human_bone.get(hb).copied().filter(|&v| v >= 0).map(|v| v as usize) {
                    if let Some(p) = g[node] {
                        println!("GOALBONE {} hb{hb} pos={:.3?} rel_hips={:.3?}", clip.name, p.0, p.0 - pose.hips_t);
                    }
                }
            }
        }
        cnt += 1;
    }
    let r = se.map(|v| (v / cnt as f32).sqrt() * 100.0);
    println!("GOALRMS {:<22} cm: LFoot {:5.2} RFoot {:5.2} LHand {:5.2} RHand {:5.2}", clip.name, r[0], r[1], r[2], r[3]);
}

pub fn sample(avatar: &Avatar, clip: &Clip, t: f32) -> Pose {
    pose_from(avatar, &sample_m(avatar, clip, t))
}

// ---------------------------------------------------------------------------------------------
// Bevy side
// ---------------------------------------------------------------------------------------------

#[derive(Resource)]
pub struct HumanoidAssets {
    pub avatar: Arc<Avatar>,
    clips: HashMap<String, Option<Arc<Clip>>>,
}

impl HumanoidAssets {
    pub fn clip(&mut self, name: &str) -> Option<Arc<Clip>> {
        if !self.clips.contains_key(name) {
            let c = Clip::load(name, &self.avatar).map(Arc::new);
            if c.is_none() {
                warn!("humanoid: clip {name} not found");
            }
            self.clips.insert(name.to_string(), c);
        }
        self.clips[name].clone()
    }
}

#[derive(Component)]
pub struct HumanoidAnimator {
    pub clip: String,
    pub time: f32,
    pub speed: f32,
    pub looping: bool,
    /// Ragdoll active: stop writing bone poses.
    pub frozen: bool,
}

impl Default for HumanoidAnimator {
    fn default() -> Self {
        HumanoidAnimator { clip: DEFAULT_CLIP.into(), time: 0.0, speed: 1.0, looping: true, frozen: false }
    }
}

/// Bone entities per human skeleton node.
#[derive(Component)]
pub struct HumanoidRig {
    pub(crate) bones: Vec<Option<Entity>>,
    /// rotation / uniform scale of the hips' parent (armature node) in the glb.
    pub(crate) parent_rot: Quat,
    pub(crate) parent_scale: f32,
}

fn find_rig(root: Entity, avatar: &Avatar, children: &Query<&Children>, names: &Query<&Name>, tfs: &Query<&Transform>) -> Option<HumanoidRig> {
    // PF_Aboubi_04 subtree (the FPArms rig has same-named bones).
    let mut stack = vec![root];
    let mut pf = None;
    while let Some(e) = stack.pop() {
        if names.get(e).map(|n| n.as_str() == "PF_Aboubi_04").unwrap_or(false) {
            pf = Some(e);
            break;
        }
        if let Ok(k) = children.get(e) {
            stack.extend(k.iter());
        }
    }
    let pf = pf?;
    let mut by_hash: HashMap<u32, Entity> = HashMap::new();
    let mut armature_of_hips = None;
    let mut stack = vec![(pf, Entity::PLACEHOLDER)];
    while let Some((e, parent)) = stack.pop() {
        if let Ok(n) = names.get(e) {
            by_hash.insert(crc32(n.as_str()), e);
            if n.as_str() == "Hips" {
                armature_of_hips = Some(parent);
            }
        }
        if let Ok(k) = children.get(e) {
            stack.extend(k.iter().map(|c| (c, e)));
        }
    }
    let arm = armature_of_hips.filter(|e| *e != Entity::PLACEHOLDER)?;
    let bones: Vec<Option<Entity>> = avatar.nodes.iter().map(|n| by_hash.get(&n.name_hash).copied()).collect();
    if bones[1].is_none() || bones.iter().filter(|b| b.is_some()).count() < avatar.nodes.len() / 2 {
        return None;
    }
    let ptf = tfs.get(arm).ok()?;
    Some(HumanoidRig { bones, parent_rot: ptf.rotation, parent_scale: ptf.scale.x })
}

fn setup_humanoid(
    mut commands: Commands,
    mut assets: ResMut<HumanoidAssets>,
    dummies: Query<(Entity, Has<crate::health::LocalBody>, Has<crate::health::PreviewBody>, Has<crate::health::CorpseBody>), (Or<(With<Dummy>, With<crate::health::LocalBody>, With<crate::health::PreviewBody>, With<crate::health::CorpseBody>)>, Without<HumanoidAnimator>)>,
    children: Query<&Children>,
    names: Query<&Name>,
    tfs: Query<&Transform>,
    db: Option<Res<crate::weapons::data::WeaponDb>>,
    _asset_server: Res<AssetServer>,
) {
    for (d, local, preview, corpse) in &dummies {
        if let Some(rig) = find_rig(d, &assets.avatar, &children, &names, &tfs) {
            if corpse {
                // aboubiRagdoll: its Animator is disabled (no controller); the ragdoll starts from the rest pose.
                commands.entity(d).insert((rig, HumanoidAnimator { frozen: true, ..default() }));
                continue;
            }
            info!("humanoid: rig ready ({} bones)", rig.bones.iter().flatten().count());
            let _ = assets.clip(DEFAULT_CLIP);
            let mut a = HumanoidAnimator::default();
            if let Ok(c) = std::env::var("RESTRAFE_CLIP") {
                a.clip = c; // debug: preview another clip
            }
            commands.entity(d).insert((rig, a));
            // The local player's body: the same Animator, driven by FirstPersonController.HandleAnimation (fpc.rs
            // local_body_anim).
            if preview || local {
                // Preview Animator (SK_Aboubi_00.controller) with default parameters: unarmed idle.
                commands.entity(d).insert((crate::animator::AnimatorRuntime::new(), crate::animator::DummyAnimParams::default()));
            } else if std::env::var("RESTRAFE_CLIP").is_err() && !local {
                // Third-person Animator (SK_Aboubi_00.controller) driven by DummyAnimParams.
                let mut params = crate::animator::DummyAnimParams::from_env();
                dummy_weapon(&mut params, &mut commands, db.as_deref(), d);
                commands.entity(d).insert((crate::animator::AnimatorRuntime::new(), params));
            }
        }
    }
}

/// Unity (left-handed) -> glb (AssetRipper X-mirrored) rotation / position.
fn mirror_q(q: Quat) -> Quat {
    Quat::from_xyzw(q.x, -q.y, -q.z, q.w)
}
fn mirror_p(p: Vec3) -> Vec3 {
    Vec3::new(-p.x, p.y, p.z)
}

pub(crate) fn apply_pose(avatar: &Avatar, rig: &HumanoidRig, pose: &Pose, tfs: &mut Query<&mut Transform>) {
    for (i, bone) in rig.bones.iter().enumerate() {
        let Some(e) = bone else { continue };
        let Ok(mut tf) = tfs.get_mut(*e) else { continue };
        match i {
            0 => {}
            1 => {
                let inv_p = rig.parent_rot.inverse();
                tf.rotation = inv_p * mirror_q(pose.hips_q);
                tf.translation = inv_p * mirror_p(pose.hips_t) / rig.parent_scale;
            }
            _ => {
                if avatar.nodes[i].axes.is_some() {
                    tf.rotation = mirror_q(pose.rot[i]);
                }
            }
        }
    }
}

pub(crate) fn animate_humanoid(
    time: Res<Time>,
    mut assets: ResMut<HumanoidAssets>,
    mut q: Query<(&mut HumanoidAnimator, &HumanoidRig), Without<crate::animator::AnimatorRuntime>>,
    mut tfs: Query<&mut Transform>,
) {
    for (mut anim, rig) in &mut q {
        if anim.frozen {
            continue;
        }
        let Some(clip) = assets.clip(&anim.clip) else { continue };
        anim.time += time.delta_secs() * anim.speed;
        let t = if anim.looping && clip.loop_time {
            anim.time.rem_euclid(clip.length)
        } else {
            anim.time.clamp(0.0, clip.length)
        };
        let pose = sample(&assets.avatar, &clip, t);
        apply_pose(&assets.avatar, rig, &pose, &mut tfs);
    }
}

pub struct HumanoidPlugin;

impl Plugin for HumanoidPlugin {
    fn build(&self, app: &mut App) {
        let avatar = Arc::new(Avatar::load("SK_Aboubi_00Avatar.asset"));
        app.insert_resource(HumanoidAssets { avatar, clips: HashMap::new() })
            .add_systems(Update, (setup_humanoid, animate_humanoid).chain())
            .add_systems(Update, (attach_dummy_weapons, dummy_camera_pitch));
        crate::animator::build(app);
    }
}

/// `RESTRAFE_DUMMY_WEAPON=<Right>[,<Left>]`: the dummy holds weapons like a remote player. PlayerPickup parents the
/// item to `pickupPositionOnline[right?0:1]` (GunRightOnline / GunLeftOnline under Camera/OnlinePositions, i.e. the
/// camera rig inside the glb) with localScale 1 for non-owners; the animator bools follow PlayerPickup's cases.
fn dummy_weapon(
    params: &mut crate::animator::DummyAnimParams,
    commands: &mut Commands,
    db: Option<&crate::weapons::data::WeaponDb>,
    dummy: Entity,
) {
    let Ok(spec) = std::env::var("RESTRAFE_DUMMY_WEAPON") else { return };
    // ",Glock" = left hand only.
    let mut it = spec.split(',').map(|s| Some(s.trim().to_string()).filter(|s| !s.is_empty()));
    let (right, left) = (it.next().flatten(), it.next().flatten());
    let both = |n: &Option<String>| n.as_ref().and_then(|n| db.and_then(|d| d.defs.iter().find(|w| &w.name == n)));
    let (rd, ld) = (both(&right), both(&left));
    if right.is_some() && rd.is_none() || left.is_some() && ld.is_none() {
        warn!("RESTRAFE_DUMMY_WEAPON: unknown weapon in {spec}");
        return;
    }
    // PlayerPickup (non-owner globalAnimator bools), rightHandAnim/leftHandAnim == "" for the default hold poses.
    match (rd, ld) {
        (Some(r), None) => {
            if r.require_both_hands() {
                params.two_handed = true;
            } else {
                params.single_handed = true;
            }
        }
        (Some(_), Some(_)) => params.double_single = true,
        // Left hand only: LeftHanded regardless of requireBothHands (PlayerPickup.cs Update, objInHand == null case).
        (None, Some(_)) => params.left_handed = true,
        _ => {}
    }
    commands.entity(dummy).insert(PendingDummyWeapons { right, left });
}

#[derive(Component)]
struct PendingDummyWeapons {
    right: Option<String>,
    left: Option<String>,
}

fn find_named(root: Entity, name: &str, children: &Query<&Children>, names: &Query<&Name>) -> Option<Entity> {
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if names.get(e).map(|n| n.as_str() == name).unwrap_or(false) {
            return Some(e);
        }
        if let Ok(k) = children.get(e) {
            stack.extend(k.iter());
        }
    }
    None
}

/// Once the dummy's glb is instanced, parent the held weapons to the online item nodes (scale 1).
fn attach_dummy_weapons(
    mut commands: Commands,
    q: Query<(Entity, &PendingDummyWeapons)>,
    children: Query<&Children>,
    names: Query<&Name>,
    asset_server: Res<AssetServer>,
) {
    for (d, p) in &q {
        let (Some(r), Some(l)) = (
            find_named(d, "GunRightOnline", &children, &names),
            find_named(d, "GunLeftOnline", &children, &names),
        ) else {
            continue;
        };
        for (node, w) in [(r, &p.right), (l, &p.left)] {
            let Some(w) = w else { continue };
            let scene = asset_server.load(GltfAssetLabel::Scene(0).from_asset(format!("models/weapons/{w}.glb")));
            commands.entity(node).insert(Visibility::Visible).with_children(|c| {
                c.spawn((Name::new("DummyWeapon"), WorldAssetRoot(scene), crate::unity_pbr::PrefabRender(format!("assets/data/weapons/{w}.json")), Transform::IDENTITY, Visibility::Visible));
            });
        }
        commands.entity(d).remove::<PendingDummyWeapons>();
    }
}

/// Virtual camera pitch of the remote player: FirstPersonController sets Vertical = -rotationX / 90 and
/// CameraHolder.localRotation = Euler(rotationX, 0, 0); the dummy derives rotationX from its Vertical param.
fn dummy_camera_pitch(
    dummies: Query<(Entity, &crate::animator::DummyAnimParams)>,
    children: Query<&Children>,
    names: Query<&Name>,
    mut tfs: Query<&mut Transform>,
) {
    for (d, p) in &dummies {
        if std::env::var("RESTRAFE_DUMMY_WEAPON").is_err() {
            return;
        }
        let Some(h) = find_named(d, "CameraHolder", &children, &names) else { continue };
        if let Ok(mut t) = tfs.get_mut(h) {
            let rot_x = (-p.vertical * 90.0).clamp(-90.0, 90.0);
            t.rotation = mirror_q(crate::unity::unity_euler(rot_x, 0.0, 0.0));
        }
    }
}

#[cfg(test)]
mod avatar_tests {
    use super::*;

    /// The Avatar read in place equals the old exported YAML (skeleton, axes, T-pose, muscles, masses, calibration).
    #[test]
    fn avatar_matches_export() {
        if crate::owned::Owned::global().is_none() {
            return;
        }
        let path = format!("{ASSET_ROOT}/Avatar/SK_Aboubi_00Avatar.asset");
        if !std::path::Path::new(&path).exists() {
            return;
        }
        let a = Avatar::load_yaml(&path);
        let b = Avatar::load("SK_Aboubi_00Avatar.asset");
        let v = |x: Vec3, y: Vec3, what: &str| assert!((x - y).abs().max_element() < 1e-5, "{what}: {x} vs {y}");
        let q = |x: Quat, y: Quat, what: &str| assert!(x.abs_diff_eq(y, 1e-5), "{what}: {x} vs {y}");
        assert_eq!(a.nodes.len(), b.nodes.len(), "node count");
        for (i, (x, y)) in a.nodes.iter().zip(&b.nodes).enumerate() {
            assert_eq!((x.name_hash, x.parent), (y.name_hash, y.parent), "node {i}");
            v(x.t, y.t, "t");
            q(x.q, y.q, "q");
            v(x.s, y.s, "s");
            assert_eq!(x.axes.is_some(), y.axes.is_some(), "node {i} axes");
            if let (Some(p), Some(r)) = (&x.axes, &y.axes) {
                q(p.pre, r.pre, "pre");
                q(p.post, r.post, "post");
                v(p.sgn, r.sgn, "sgn");
                v(p.min, r.min, "min");
                v(p.max, r.max, "max");
            }
        }
        v(a.root_t, b.root_t, "root t");
        q(a.root_q, b.root_q, "root q");
        assert!((a.scale - b.scale).abs() < 1e-6);
        assert_eq!(a.muscles, b.muscles, "muscles");
        assert_eq!(a.human_bone, b.human_bone, "human bones");
        assert_eq!(a.mass.len(), b.mass.len());
        assert!(a.mass.iter().zip(&b.mass).all(|(x, y)| (x - y).abs() < 1e-6), "masses");
        v(a.hips_scale, b.hips_scale, "hips scale");
        q(a.body_corr, b.body_corr, "body correction");
        v(a.com_corr, b.com_corr, "com correction");
        println!("avatar: {} nodes, {} muscles match", b.nodes.len(), b.muscles.len());
    }
}

#[cfg(test)]
mod clip_probe {
    #[test]
    fn clip_probe() {
        let Ok(name) = std::env::var("RESTRAFE_CLIP_PROBE") else { return };
        let o = crate::owned::Owned::open().unwrap();
        let (n, p) = o.same_named(74, &name).unwrap()[0];
        let (_, f) = crate::prefab_json::script_object(&o, o.assemblies().unwrap(), n, p).unwrap();
        for (k, v) in &f {
            let s = v.to_string();
            println!("{k}: {} [{} chars]", s.chars().take(300).collect::<String>(), s.len());
        }
    }
}
