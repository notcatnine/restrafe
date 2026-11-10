//! First-person arms (PlayerIK `FPArms/PF_FPArm_Container_IK_00`): skinned arm + hand meshes, suit arm material
//! (`PlayerSetup.fpArmsSuits` -> `CosmeticsManager.fparmsMats`) and the RigBuilder two-bone IK
//! (Animation Rigging `TwoBoneIKConstraint`: root = Shoulder, mid = ForeArm, tip = Wrist, hint = Gun/{Right,Left}Hint,
//! target = {Right,Left}IKTarget which `PlayerPickup.UpdateIKPoistion` puts on the held weapon's grip or the idle position).
//!
//! Space conventions: the rig is built from the prefab JSON with the port's Z-mirror (`u_pos`/`u_quat`). The arm
//! meshes are AssetRipper glbs (X-mirrored); bind poses are converted `Sz * B * Sx` so the glb vertices end up
//! Z-mirrored without turning the mesh. IK is solved in FPArms-local space straight from the `Transform`s (no
//! GlobalTransform lag), after all gameplay systems and before transform propagation.

use crate::cosmetics::{CosmeticMaterials, Cosmetics, CosmeticsData};
use crate::skinning::{read_glb_skin, read_mesh_asset};
use crate::weapons::data::{HandAnchors, Prefab, UField};
use crate::weapons::{FpArms, Hand, Item};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::math::Affine3A;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::VertexAttributeValues;
use bevy::prelude::*;
use std::collections::HashMap;

const FPARMS: &str = "PlayerIK/CameraLeanPivot/LandBobPivot/CameraHolder/Camera/BobPosition/FPArms";
const CONTAINER: &str = "PF_FPArm_Container_IK_00";
/// `SkinnedMeshRenderer.m_Bones` of all four arm renderers (28 bones; identical order for each).
const BONES: [&str; 28] = [
    "Shoulder.R", "Arm.R", "ForeArm.R", "Wrist.R", "RingFinger_1.R", "RingFinger_2.R", "Index_1.R", "Index_2.R", "Thumb_1.R", "Thumb_2.R", "Pinkie_1.R",
    "Pinkie_2.R", "MiddleFinger_1.R", "MiddleFinger_2.R", "Shoulder.L", "Arm.L", "ForeArm.L", "Wrist.L", "RingFinger_1.L", "RingFinger_2.L", "Index_1.L",
    "Index_2.L", "Thumb_1.L", "Thumb_2.L", "Pinkie_1.L", "Pinkie_2.L", "MiddleFinger_1.L", "MiddleFinger_2.L",
];

struct Side {
    /// Shoulder (root), Arm, ForeArm (mid), Wrist (tip).
    chain: [Entity; 4],
    hint: Vec3,
    idle: Transform,
}

/// On the FPArms entity.
#[derive(Component)]
pub struct ArmRig {
    /// Container * SK_FPArmAboubi_00 relative to FPArms (the Armature's parent).
    armature_parent: Affine3A,
    armature: Entity,
    /// Every Armature bone with its prefab (animator-less) local transform; keyed by name relative to the Armature.
    bones: Vec<(String, Entity, Transform)>,
    right: Side,
    left: Side,
}

/// Animator states of `Aboubi_IK_00.controller` (static poses; every clip has one key per curve).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ArmState {
    Idle,
    RightHanded,
    TwoHanded,
    DoubleHanded,
    OneHandedMelee,
}

#[derive(Clone, Copy)]
enum Prop {
    Euler,
    Position,
    Scale,
}

#[derive(Resource, Default)]
struct ArmClips(HashMap<usize, Vec<(String, Prop, Vec3)>>);

/// First key of every Euler / Position / Scale curve of an arms clip + its bone path (clips decoded from the
/// installation; paths resolved against the player rig).
fn read_clip(name: &str) -> Vec<(String, Prop, Vec3)> {
    let prefab = crate::weapons::data::Prefab::load("assets/data/player/PlayerIK.json").ok();
    let resolver = crate::anim_clip::hierarchy_resolver(prefab.iter().flat_map(|p| p.objects.iter().map(|o| o.path.as_str())));
    let Some(clip) = crate::anim_clip::load(name, &resolver) else {
        warn!("fparms: missing clip {name}");
        return vec![];
    };
    let mut out = Vec::new();
    for kind in [crate::anim_clip::Kind::Euler, crate::anim_clip::Kind::Position, crate::anim_clip::Kind::Scale] {
        let prop = match kind {
            crate::anim_clip::Kind::Euler => Prop::Euler,
            crate::anim_clip::Kind::Position => Prop::Position,
            _ => Prop::Scale,
        };
        for c in clip.curves.iter().filter(|c| c.kind == kind) {
            let first = |i: usize| c.comps.get(i).and_then(|k| k.first()).map(|k| k.v).unwrap_or(0.0);
            out.push((c.path.clone(), prop, Vec3::new(first(0), first(1), first(2))));
        }
    }
    out
}

#[derive(Component)]
struct ArmMesh {
    hand: bool,
    applied: Option<usize>,
}

#[derive(Component)]
struct PendingArmSkin {
    model: String,
    bones: Vec<Entity>,
}

fn spawn_rig(mut commands: Commands, fparms: Query<Entity, (With<FpArms>, Without<ArmRig>)>, anchors: Res<HandAnchors>, assets: Res<AssetServer>) {
    let Some(prefab) = anchors.player.clone() else { return };
    for fp in &fparms {
        let container = format!("{FPARMS}/{CONTAINER}");
        let mut objs: Vec<_> = prefab
            .objects
            .iter()
            .filter(|o| {
                o.path.starts_with(&container)
                    && !o.path[container.len()..].starts_with("/Gun")
                    && !o.path.contains("/RigLayer")
                    && o.comp("Transform").is_some()
            })
            .collect();
        objs.sort_by_key(|o| o.path.matches('/').count());
        let mut map: HashMap<String, Entity> = HashMap::new();
        let mut by_name: HashMap<String, Entity> = HashMap::new();
        let mut renderers = Vec::new();
        for o in objs {
            let name = o.path.rsplit('/').next().unwrap().to_string();
            let parent = if o.path == container { fp } else { map[o.path.rsplit_once('/').unwrap().0] };
            let e = commands.spawn((Name::new(name.clone()), o.local_transform(), Visibility::default(), ChildOf(parent))).id();
            map.insert(o.path.clone(), e);
            by_name.insert(name.clone(), e);
            if o.comp("SkinnedMeshRenderer").is_some() {
                renderers.push((e, name));
            }
        }
        let bones: Vec<Entity> = BONES.iter().map(|b| by_name[*b]).collect();
        for (e, name) in renderers {
            let model = format!("models/player/{name}.glb");
            let mesh: Handle<Mesh> = assets.load(GltfAssetLabel::Primitive { mesh: 0, primitive: 0 }.from_asset(model.clone()));
            commands.entity(e).insert((Mesh3d(mesh), PendingArmSkin { model, bones: bones.clone() }, ArmMesh { hand: name.contains("Hand"), applied: None }));
        }
        let rel = |p: &str| prefab.relative_transform(&format!("{container}/{p}"), FPARMS);
        let armature_parent = rel("SK_FPArmAboubi_00").compute_affine();
        let arm_path = format!("{container}/SK_FPArmAboubi_00/Armature");
        let mut bones = Vec::new();
        for (path, e) in &map {
            if let Some(r) = path.strip_prefix(&arm_path) {
                if r.is_empty() || r.starts_with('/') {
                    bones.push((r.trim_start_matches('/').to_string(), *e, prefab.find(path).unwrap().local_transform()));
                }
            }
        }
        let side = |s: &str, hint: &str, idle: &str| Side {
            chain: ["Shoulder", "Arm", "ForeArm", "Wrist"].map(|b| by_name[&format!("{b}.{s}")]),
            hint: rel(&format!("Gun/{hint}")).translation,
            idle: rel(&format!("Gun/{idle}")),
        };
        let rig = ArmRig {
            armature_parent,
            armature: map[&arm_path],
            bones,
            right: side("R", "RightHint", "IdlePositionRight"),
            left: side("L", "LeftHint", "IdlePositionLeft"),
        };
        commands.entity(fp).insert(rig);
    }
}

/// Rebuild the arm meshes as skinned meshes once the glb primitive is loaded.
fn skin_arm_meshes(
    mut commands: Commands,
    pending: Query<(Entity, &PendingArmSkin, &Mesh3d)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut ibps: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    for (e, p, m) in &pending {
        let Some(src) = meshes.get(&m.0) else { continue };
        let mut mesh = src.clone();
        let stem = p.model.rsplit('/').next().unwrap().trim_end_matches(".glb");
        let (Some((_, binds)), Some((joints, weights))) = (read_mesh_asset(stem), read_glb_skin(&p.model)) else {
            warn!("fparms: cannot read skin data for {stem}");
            commands.entity(e).remove::<PendingArmSkin>();
            continue;
        };
        if joints.len() != mesh.count_vertices() || binds.len() != p.bones.len() {
            warn!("fparms: {stem}: {} joint verts vs {} mesh verts, {} binds", joints.len(), mesh.count_vertices(), binds.len());
            commands.entity(e).remove::<PendingArmSkin>();
            continue;
        }
        // skinning::read_mesh_asset gives Sx*B*Sx; the rig lives in Z-mirrored space: Sz*B*Sx = diag(-1,1,-1,1) * (Sx*B*Sx).
        let rot = Mat4::from_diagonal(Vec4::new(-1.0, 1.0, -1.0, 1.0));
        let binds: Vec<Mat4> = binds.iter().map(|b| rot * *b).collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(joints));
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, weights);
        let h = meshes.add(mesh);
        commands.entity(e).remove::<PendingArmSkin>().insert((
            Mesh3d(h),
            SkinnedMesh { inverse_bindposes: ibps.add(SkinnedMeshInverseBindposes::from(binds)), joints: p.bones.clone() },
            NoFrustumCulling,
        ));
    }
}

/// Suit arm material on the two arm meshes, face (skin) material on the hands.
fn arm_materials(
    mut commands: Commands,
    cos: Res<Cosmetics>,
    data: Res<CosmeticsData>,
    assets: Res<AssetServer>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut cm: ResMut<CosmeticMaterials>,
    mut arms: Query<(Entity, &mut ArmMesh)>,
) {
    for (e, mut a) in &mut arms {
        if a.applied == Some(cos.suit) {
            continue;
        }
        a.applied = Some(cos.suit);
        let m = if a.hand { cm.face(&data, &assets, &mut mats) } else { cm.arms(cos.suit, &data, &assets, &mut mats) };
        commands.entity(e).insert(MeshMaterial3d(m));
    }
}

/// Grip transform (FPArms-local) of a held weapon: `which` 0 = gripRight (first Grip), 1 = gripLeft (second Grip).
/// Ancestors animated by the weapon's Animator (`WeaponAnimator::live`) replace their static prefab transforms, so the
/// hands follow e.g. a melee swing (Grip nodes are children of the animated `AimStrafePivot/anim`).
/// `live` holds the current local rotations (port convention) of the ElbowPivotPoint / AimStrafePivot nodes, which
/// Weapon.WeaponAnimation (elbow recoil) and ItemBehaviour (aim-strafe lean) rotate at runtime: the Grips are their
/// children, so the hands follow the kick.
fn grip_pose(item: &Item, tf: &Transform, which: usize, anim: Option<&crate::weapons::anim::WeaponAnimator>, live: &[(String, Quat)]) -> Option<Transform> {
    let p: &Prefab = &item.def.prefab;
    let root = &p.objects.first()?.path;
    let grips: Vec<_> = p.objects.iter().filter(|o| o.comp("Grip").is_some()).collect();
    let g = grips.get(which).or(grips.first())?;
    let mut chain = Vec::new();
    let mut cur = g.path.clone();
    while cur != *root && !cur.is_empty() {
        let rel = cur.strip_prefix(&format!("{root}/")).unwrap_or("");
        let live_rot = live.iter().find(|(path, _)| *path == cur).map(|(_, q)| *q);
        let found = anim.and_then(|a| a.live.get(rel)).copied().or_else(|| {
            p.find(&cur).map(|o| {
                let mut t = o.local_transform();
                if let Some(q) = live_rot {
                    t.rotation = q;
                }
                t
            })
        });
        if let Some(t) = found {
            chain.push(t);
        }
        cur = cur.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap_or_default();
    }
    Some(chain.iter().rev().fold(*tf, |acc, t| acc.mul_transform(*t)))
}

fn fk(parent: Affine3A, local: &[Transform; 4]) -> [Affine3A; 4] {
    let mut out = [Affine3A::IDENTITY; 4];
    let mut g = parent;
    for i in 0..4 {
        g *= local[i].compute_affine();
        out[i] = g;
    }
    out
}

fn rot_of(a: &Affine3A) -> Quat {
    a.to_scale_rotation_translation().1
}

fn tri_angle(len: f32, l1: f32, l2: f32) -> f32 {
    (((l1 * l1 + l2 * l2 - len * len) / (2.0 * l1 * l2)).clamp(-1.0, 1.0)).acos()
}

fn from_to(a: Vec3, b: Vec3) -> Quat {
    match (a.try_normalize(), b.try_normalize()) {
        (Some(a), Some(b)) => Quat::from_rotation_arc(a, b),
        _ => Quat::IDENTITY,
    }
}

/// `AnimationRuntimeUtils.SolveTwoBoneIK` (weights 1) on local transforms; `local` = [root, (arm), mid, tip].
fn solve_two_bone(armature: Affine3A, local: &mut [Transform; 4], target: &Transform, hint: Vec3) {
    // chain indices: root 0, mid 2, tip 3; the Arm bone (1) sits in between.
    let g = fk(armature, local);
    let (a, b, c) = (g[0].translation.into(), g[2].translation.into(), g[3].translation.into());
    let (a, b, c): (Vec3, Vec3, Vec3) = (a, b, c);
    let t = target.translation;
    let (ab, bc, ac, at) = (b - a, c - b, c - a, t - a);
    let (ab_len, bc_len, ac_len, at_len) = (ab.length(), bc.length(), ac.length(), at.length());
    if ab_len < 1e-6 || bc_len < 1e-6 {
        return;
    }
    let old_angle = tri_angle(ac_len, ab_len, bc_len);
    let new_angle = tri_angle(at_len, ab_len, bc_len);
    let eps = 1e-8;
    let mut axis = ab.cross(bc);
    if axis.length_squared() < eps {
        axis = (hint - a).cross(bc);
        if axis.length_squared() < eps {
            axis = at.cross(bc);
        }
        if axis.length_squared() < eps {
            axis = Vec3::Y;
        }
    }
    let axis = axis.normalize();
    let half = 0.5 * (old_angle - new_angle);
    let delta = Quat::from_xyzw(axis.x * half.sin(), axis.y * half.sin(), axis.z * half.sin(), half.cos());
    // mid.rotation = delta * mid.rotation
    let new_mid = delta * rot_of(&g[2]);
    local[2].rotation = (rot_of(&g[1]).inverse() * new_mid).normalize();
    // root.rotation = FromTo(ac', at) * root.rotation
    let g = fk(armature, local);
    let ac = Vec3::from(g[3].translation) - a;
    let new_root = from_to(ac, at) * rot_of(&g[0]);
    local[0].rotation = (rot_of(&armature).inverse() * new_root).normalize();
    // hint
    let g = fk(armature, local);
    let ac = Vec3::from(g[3].translation) - a;
    let ac_sq = ac.length_squared();
    if ac_sq > 0.0 {
        let ab = Vec3::from(g[2].translation) - a;
        let ac_n = ac / ac_sq.sqrt();
        let ah = hint - a;
        let ab_proj = ab - ac_n * ab.dot(ac_n);
        let ah_proj = ah - ac_n * ah.dot(ac_n);
        let reach = ab_len + bc_len;
        if ab_proj.length_squared() > reach * reach * 0.001 && ah_proj.length_squared() > 0.0 {
            let hr = from_to(ab_proj, ah_proj);
            let new_root = hr * rot_of(&g[0]);
            local[0].rotation = (rot_of(&armature).inverse() * new_root).normalize();
        }
    }
    // tip.rotation = target rotation
    let g = fk(armature, local);
    local[3].rotation = (rot_of(&g[2]).inverse() * target.rotation).normalize();
}

fn arm_state(right: &Option<(&Item, Transform)>, left: &Option<(&Item, Transform)>) -> ArmState {
    // PlayerPickup animator bools. A non-empty rightHandAnim/leftHandAnim (e.g. "OneHandedMelee") replaces the default.
    let custom = |it: &Item, left: bool| it.def.item.s(if left { "leftHandAnim" } else { "rightHandAnim" }) == "OneHandedMelee";
    match (right, left) {
        (None, None) => ArmState::Idle,
        (Some((r, _)), None) if custom(r, false) => ArmState::OneHandedMelee,
        (None, Some((l, _))) if custom(l, true) => ArmState::OneHandedMelee,
        (Some((r, _)), None) if r.def.require_both_hands() => ArmState::TwoHanded,
        (Some((r, _)), Some((l, _))) if custom(r, false) || custom(l, true) => ArmState::OneHandedMelee,
        (Some(_), Some(_)) => ArmState::DoubleHanded,
        _ => ArmState::RightHanded,
    }
}

fn arm_ik(
    rigs: Query<&ArmRig>,
    items: Query<(&Item, &Transform, Option<&crate::weapons::anim::WeaponAnimator>, Option<&crate::weapons::markers::WeaponMarkers>)>,
    mut tfs: Query<&mut Transform, Without<Item>>,
    clips: Res<ArmClips>,
) {
    let Ok(rig) = rigs.single() else { return };
    let mut right_item = None;
    let mut left_item = None;
    let (mut live_r, mut live_l) = (Vec::new(), Vec::new());
    for (it, tf, an, m) in &items {
        // Live pivot rotations (glb node rotation -> port convention).
        let mut live = Vec::new();
        if let Some(m) = m {
            for (comp, ent) in [("ElbowPivotPoint", m.elbow_pivot), ("AimStrafePivot", m.aim_strafe_pivot)] {
                let path = it.def.prefab.objects.iter().find(|o| o.comp(comp).is_some()).map(|o| o.path.clone());
                if let (Some(path), Some(Ok(t))) = (path, ent.map(|e| tfs.get(e))) {
                    live.push((path, crate::weapons::markers::flip(t.rotation)));
                }
            }
        }
        match it.hand {
            Hand::Right => {
                right_item = Some((it, *tf, an));
                live_r = live;
            }
            Hand::Left => {
                left_item = Some((it, *tf, an));
                live_l = live;
            }
            Hand::None => {}
        }
    }
    // Animator pose: prefab pose + the state's clip (applied before the IK constraint, like the animation job order).
    let state = arm_state(&right_item.map(|(i, t, _)| (i, t)), &left_item.map(|(i, t, _)| (i, t)));
    for (_, e, base) in &rig.bones {
        if let Ok(mut t) = tfs.get_mut(*e) {
            *t = *base;
        }
    }
    if let Some(curves) = clips.0.get(&(state as usize)) {
        for (path, prop, v) in curves {
            let name = path.rsplit('/').next().unwrap_or("");
            let Some((_, e, _)) = rig.bones.iter().find(|(p, _, _)| p == path || (name.is_empty() && p.is_empty())) else { continue };
            let Ok(mut t) = tfs.get_mut(*e) else { continue };
            match prop {
                Prop::Euler => t.rotation = crate::unity::unity_euler(v.x, v.y, v.z),
                Prop::Position => t.translation = Vec3::new(v.x, v.y, -v.z),
                Prop::Scale => t.scale = *v,
            }
        }
    }
    let armature = match tfs.get(rig.armature) {
        Ok(t) => rig.armature_parent * t.compute_affine(),
        Err(_) => return,
    };
    // PlayerPickup target selection: right grip = right weapon's gripRight; left grip = the two-handed weapon's gripLeft
    // or the left-hand weapon's gripLeft, otherwise idle.
    let right_target = right_item.as_ref().and_then(|(it, tf, an)| grip_pose(it, tf, 0, *an, &live_r)).unwrap_or(rig.right.idle);
    let left_target = match (&right_item, &left_item) {
        (Some((it, tf, an)), _) if it.def.require_both_hands() => grip_pose(it, tf, 1, *an, &live_r),
        (_, Some((it, tf, an))) => grip_pose(it, tf, 1, *an, &live_l),
        _ => None,
    }
    .unwrap_or(rig.left.idle);
    for (side, target) in [(&rig.right, right_target), (&rig.left, left_target)] {
        let mut local = [Transform::IDENTITY; 4];
        for i in 0..4 {
            let Ok(t) = tfs.get(side.chain[i]) else { return };
            local[i] = *t;
        }
        solve_two_bone(armature, &mut local, &target, side.hint);
        for i in [0, 2, 3] {
            if let Ok(mut t) = tfs.get_mut(side.chain[i]) {
                t.rotation = local[i].rotation;
            }
        }
    }
}

fn load_clips(mut clips: ResMut<ArmClips>) {
    for (s, n) in [
        (ArmState::RightHanded, "FPArms_IK_RightHand_00"),
        (ArmState::TwoHanded, "FPArms_IK_TwoHanded_00"),
        (ArmState::DoubleHanded, "FPArms_IK_DoubleHanded_00"),
        (ArmState::OneHandedMelee, "FPArms_IK_OneHandedMelee_00"),
    ] {
        clips.0.insert(s as usize, read_clip(n));
    }
}

pub struct FpArmsPlugin;

impl Plugin for FpArmsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ArmClips>()
            .add_systems(Startup, load_clips)
            .add_systems(Update, (spawn_rig, skin_arm_meshes, arm_materials).chain())
            // After DOTween (weapon recoil tweens move the item the grips hang off).
            .add_systems(PostUpdate, arm_ik.after(crate::tween::update_tweens).before(TransformSystems::Propagate));
    }
}

#[cfg(test)]
mod tests {
    /// The arms poses decoded in place equal the old YAML clips (same paths, first-key values).
    #[test]
    fn arm_clips_match_export() {
        if crate::owned::Owned::global().is_none() {
            return;
        }
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../ripped/ExportedProject/Assets/AnimationClip");
        for name in ["FPArms_IK_RightHand_00", "FPArms_IK_TwoHanded_00", "FPArms_IK_DoubleHanded_00", "FPArms_IK_OneHandedMelee_00"] {
            let Ok(text) = std::fs::read_to_string(format!("{dir}/{name}.anim")) else { return };
            let body: String = text.lines().filter(|l| !l.starts_with('%') && !l.starts_with("---")).collect::<Vec<_>>().join("\n");
            let y: serde_yaml::Value = serde_yaml::from_str(&body).unwrap();
            let mut want = Vec::new();
            for sec in ["m_EulerCurves", "m_PositionCurves", "m_ScaleCurves"] {
                for c in y["AnimationClip"][sec].as_sequence().into_iter().flatten() {
                    let k = &c["curve"]["m_Curve"][0]["value"];
                    let f = |a: &str| k[a].as_f64().unwrap_or(0.0) as f32;
                    want.push((sec, c["path"].as_str().unwrap_or("").to_string(), [f("x"), f("y"), f("z")]));
                }
            }
            let got = super::read_clip(name);
            assert_eq!(want.len(), got.len(), "{name} curve count");
            for (sec, path, v) in &want {
                let g = got.iter().find(|(p, prop, _)| p == path && matches!((sec, prop), (&"m_EulerCurves", super::Prop::Euler) | (&"m_PositionCurves", super::Prop::Position) | (&"m_ScaleCurves", super::Prop::Scale))).unwrap_or_else(|| panic!("{name}: {sec} {path} missing"));
                assert!((g.2 - bevy::prelude::Vec3::from_array(*v)).abs().max_element() < 1e-5, "{name} {sec} {path}: {:?} vs {v:?}", g.2);
            }
            println!("{name}: {} curves match", want.len());
        }
    }
}
