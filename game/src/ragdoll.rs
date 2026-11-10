//! Death ragdoll for the training dummy (`PlayerHealth.RpcLogic___ExplodeForAll`, non-explode branch).
//!
//! The original instantiates the `SK_Aboubi_01_Ragdoll 5` prefab at the player's pose. Here the dummy's own skinned
//! body is kept and its bones are driven by Rapier bodies built from that prefab's data
//! (`assets/data/player/SK_Aboubi_01_Ragdoll 5.json`): 11 Rigidbodies (mass, drag) with their Box/Sphere/Capsule colliders on
//! Unity layer Ragdoll(23) and CharacterJoints (twist/swing limits). Initial pose = the prefab's saved pose (the
//! PlayerIK rest T-pose) at the root, as `Instantiate(aboubiRagdoll, position, rotation)`. Every body gets `AddExplosionForce(force*1.5, point - dir*1.5, 100, 0, Impulse)`; the hat pops off
//! (`PlayerHealth.Hat`: unparent, +dir*1.5 impulse, torque forward*5+right*5, Rigidbody mass 1).
//!
//! Space notes: glb bone nodes are AssetRipper's X-mirrored Unity space, so Unity bone-local vectors map with
//! `(x,y,z) -> (-x,y,z)` (same as the Body hit colliders) and joint frames are re-derived as a proper rotation
//! (`x' = -M x_u`, `y' = M y_u`, `z' = M z_u`; twist angle then keeps its sign).
//! Unity's default max angular speed is 7 rad/s (DynamicsManager) -> clamped here.
//! The local player's death reuses this path through a `Corpse` dummy (death.rs: death sound, kill cam, respawn).
//! (The death sound is owner-only in PlayerHealth: not for the dummy.)

use crate::cosmetics::DressState;
use crate::health::{Dummy, PlayerHealth};
use crate::humanoid::HumanoidAnimator;
use crate::physics::layer;
use crate::weapons::data::{Prefab, PrefabObject, UField};
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use bevy::transform::TransformSystems;
use std::collections::HashMap;

const PREFAB: &str = "assets/data/player/SK_Aboubi_01_Ragdoll 5.json";
const ROOT: &str = "SK_Aboubi_01_Ragdoll 5/SK_Aboubi_01/";
/// DynamicsManager.m_DefaultMaxAngularSpeed
const MAX_ANGULAR_SPEED: f32 = 7.0;
fn ragdoll_filter() -> Group {
    Group::from_bits_retain(crate::physics::RAGDOLL_COLLIDES)
}

#[derive(Resource)]
pub struct RagdollPrefab(Prefab);

/// Marker on the dummy while its ragdoll exists.
#[derive(Component)]
pub struct HasRagdoll {
    bodies: Vec<RagBone>,
    /// Entity whose GlobalTransform is the parent frame of the Hips bone.
    static_parent: Entity,
    hat: Option<HatState>,
    /// RagdollSound on Hips: (hips body, trigger half extents, groundHitClip); None once played.
    sound: Option<(Entity, Vec3, String, Option<std::sync::Arc<crate::audio::RolloffCurve>>)>,
    /// The Hips body (KillCam looks at `FindRecursive("Hips", ragdoll)`).
    pub hips: Option<Entity>,
}

/// Marker: remove this corpse together with its ragdoll bodies and popped hat (death.rs).
#[derive(Component)]
pub struct DespawnCorpse;

#[derive(Clone, Copy)]
struct HatState {
    /// The hat scene entity (child of `body` while flying; child of the pivot normally).
    hat: Entity,
    body: Entity,
    pivot: Entity,
}

#[derive(Clone)]
struct RagBone {
    /// Rapier body entity.
    body: Entity,
    /// The dummy's bone entity driven by the body.
    bone: Entity,
    /// Global scale of the bone at spawn (kept so skinning sees the same bone scale).
    scale: Vec3,
}

#[derive(Component)]
pub struct RagdollBody;

fn glb(v: Vec3) -> Vec3 {
    Vec3::new(-v.x, v.y, v.z)
}

/// Collider shapes of one ragdoll bone (bone-local, scaled by `s`), skipping triggers.
fn bone_shapes(obj: &PrefabObject, s: f32) -> Vec<(Vec3, Quat, Collider)> {
    let mut out = Vec::new();
    for (ty, c) in &obj.components {
        if c.b("m_IsTrigger") {
            continue;
        }
        match ty.as_str() {
            "CapsuleCollider" => {
                let r = c.f("m_Radius");
                let half = (c.f("m_Height") * 0.5 - r).max(0.0);
                let a = match c.i("m_Direction") {
                    0 => Vec3::X,
                    2 => Vec3::Z,
                    _ => Vec3::Y,
                } * half;
                out.push((glb(c.v3("m_Center")) * s, Quat::IDENTITY, Collider::capsule(-a * s, a * s, r * s)));
            }
            "BoxCollider" => {
                let h = c.v3("m_Size").abs() * 0.5 * s;
                out.push((glb(c.v3("m_Center")) * s, Quat::IDENTITY, Collider::cuboid(h.x, h.y, h.z)));
            }
            "SphereCollider" => out.push((glb(c.v3("m_Center")) * s, Quat::IDENTITY, Collider::ball(c.f("m_Radius") * s))),
            _ => {}
        }
    }
    out
}

/// CharacterJoint -> Rapier joint between `parent` (frame1) and the child (frame2). Unity auto-configures the
/// connected frame so the pose at Awake is the joint zero.
fn joint_for(c: &crate::weapons::data::Fields, s: f32, child: (Vec3, Quat), parent: (Vec3, Quat)) -> GenericJoint {
    let axis = c.v3("m_Axis").normalize_or_zero();
    let swing = c.v3("m_SwingAxis").normalize_or_zero();
    // Unity CharacterJoint frame: x = twist axis, z = axis x swingAxis, y = z x x.
    let zu = axis.cross(swing).normalize_or_zero();
    let yu = zu.cross(axis);
    let xp = -glb(axis);
    let yp = glb(yu);
    let zp = glb(zu);
    let basis = Quat::from_mat3(&Mat3::from_cols(xp, yp, zp)).normalize();
    let anchor_child = glb(c.v3("m_Anchor")) * s;
    let world_anchor = child.0 + child.1 * anchor_child;
    let anchor_parent = parent.1.inverse() * (world_anchor - parent.0);
    let basis_parent = (parent.1.inverse() * child.1 * basis).normalize();

    let lim = |k: &str| c.get(k).and_then(|v| v.get("limit")).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let (lo, hi) = (lim("m_LowTwistLimit").to_radians(), lim("m_HighTwistLimit").to_radians());
    let (s1, s2) = (lim("m_Swing1Limit").to_radians(), lim("m_Swing2Limit").to_radians());
    let mut locked = JointAxesMask::LOCKED_SPHERICAL_AXES;
    if hi - lo < 1e-4 {
        locked |= JointAxesMask::ANG_X;
    }
    if s1 < 1e-4 {
        locked |= JointAxesMask::ANG_Y;
    }
    if s2 < 1e-4 {
        locked |= JointAxesMask::ANG_Z;
    }
    let mut b = GenericJointBuilder::new(locked)
        // bevy_rapier: the joint component's `parent` is body1, the entity carrying it is body2.
        .local_anchor1(anchor_parent)
        .local_basis1(basis_parent)
        .local_anchor2(anchor_child)
        .local_basis2(basis);
    if hi - lo >= 1e-4 {
        b = b.limits(JointAxis::AngX, [lo, hi]);
    }
    if s1 >= 1e-4 {
        b = b.limits(JointAxis::AngY, [-s1, s1]);
    }
    if s2 >= 1e-4 {
        b = b.limits(JointAxis::AngZ, [-s2, s2]);
    }
    let mut j = b.build();
    j.set_contacts_enabled(false);
    j
}

#[allow(clippy::too_many_arguments)]
fn spawn_ragdolls(
    mut commands: Commands,
    prefab: Res<RagdollPrefab>,
    mut dummies: Query<(Entity, &mut PlayerHealth, &GlobalTransform, Option<&mut HumanoidAnimator>, Option<&DressState>, Option<&crate::health::DummyGraphics>, Option<&crate::health::Corpse>), (With<Dummy>, Without<HasRagdoll>)>,
    children: Query<&Children>,
    parents: Query<&ChildOf>,
    names: Query<&Name>,
    gts: Query<&GlobalTransform>,
    mut locals: Query<&mut Transform, Without<Dummy>>,
    mut vfx: crate::vfx::Vfx,
    cos_data: Option<Res<crate::cosmetics::CosmeticsData>>,
    mut hull_cache: Local<HashMap<String, Option<Vec<Vec3>>>>,
    (shot, worn): (Query<&ShotHat>, Query<(Entity, &WornHat)>),
    frame: Res<bevy::diagnostic::FrameCount>,
) {
    for (dummy, mut ph, root_gt, anim, dress, graphics, corpse) in &mut dummies {
        if !ph.dead {
            continue;
        }
        // Bone entities by name under PF_Aboubi_04 (the FPArms rig has same-named bones).
        let mut stack = vec![dummy];
        let mut pf = None;
        while let Some(n) = stack.pop() {
            if names.get(n).is_ok_and(|x| x.as_str() == "PF_Aboubi_04") {
                pf = Some(n);
                break;
            }
            if let Ok(k) = children.get(n) {
                stack.extend(k.iter());
            }
        }
        let Some(pf) = pf else { continue };
        let mut by_name: HashMap<String, Entity> = HashMap::new();
        let mut stack = vec![pf];
        while let Some(n) = stack.pop() {
            if let Ok(x) = names.get(n) {
                by_name.insert(x.as_str().to_string(), n);
            }
            if let Ok(k) = children.get(n) {
                stack.extend(k.iter());
            }
        }
        let Some(&hips) = by_name.get("Hips") else { continue };
        let Ok(static_parent) = parents.get(hips).map(|p| p.parent()) else { continue };

        let (dir, force, point) = ph.last_hit.take().unwrap_or((root_gt.forward().as_vec3(), 0.0, root_gt.translation() + Vec3::Y));
        let expl_pos = point - dir * 1.5;

        // Instantiate(aboubiRagdoll, transform.position, transform.rotation): the ragdoll starts in the prefab's saved
        // pose (its Animator is disabled and has no controller), which is the PlayerIK rig's rest T-pose (all 38 bone
        // locals equal to 5e-8), at the player ROOT: without PlayerSetup's Graphics (0, -0.2, 0) offset.
        // Armature (Hips parent) world in that pose = root * (Graphics^-1 * root^-1 * armature now).
        let Ok(static_gt) = gts.get(static_parent) else { continue };
        let g_local = graphics.and_then(|g| locals.get(g.0).ok()).map(|t| t.to_matrix()).unwrap_or(Mat4::IDENTITY);
        let rel = root_gt.to_matrix().inverse() * static_gt.to_matrix();
        let mut world: HashMap<String, Mat4> = HashMap::new();
        let armature = root_gt.to_matrix() * g_local.inverse() * rel;
        let mut rest: Vec<(&str, Mat4)> = prefab
            .0
            .objects
            .iter()
            .filter_map(|o| {
                let path = o.path.strip_prefix(ROOT)?;
                let t = o.comp("Transform")?;
                let q = t.get("m_LocalRotation")?;
                let qf = |k: &str| q.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let rq = Quat::from_xyzw(qf("x"), -qf("y"), -qf("z"), qf("w")).normalize();
                Some((path, Mat4::from_scale_rotation_translation(t.v3("m_LocalScale"), rq, glb(t.v3("m_LocalPosition")))))
            })
            .collect();
        rest.sort_by_key(|(p, _)| p.matches('/').count());
        for (path, m) in &rest {
            let parent_m = path.rsplit_once('/').and_then(|(pp, _)| world.get(pp).copied()).unwrap_or(armature);
            world.insert(path.to_string(), parent_m * *m);
            let name = path.rsplit('/').next().unwrap();
            if let Some(&b) = by_name.get(name) {
                if let Ok(mut t) = locals.get_mut(b) {
                    *t = Transform::from_matrix(*m);
                }
            }
        }

        // Rigid bodies: (name, object) for every ragdoll bone.
        struct Pending<'a> {
            name: String,
            obj: &'a PrefabObject,
            bone: Entity,
            pos: Vec3,
            rot: Quat,
            scale: Vec3,
        }
        let mut pend: Vec<Pending> = Vec::new();
        for o in &prefab.0.objects {
            let Some(path) = o.path.strip_prefix(ROOT) else { continue };
            if o.comp("Rigidbody").is_none() {
                continue;
            }
            let name = path.rsplit('/').next().unwrap().to_string();
            let Some(&bone) = by_name.get(&name) else { continue };
            let Some(m) = world.get(path) else { continue };
            let (scale, rot, pos) = m.to_scale_rotation_translation();
            pend.push(Pending { name, obj: o, bone, pos, rot, scale });
        }
        if pend.len() < 11 {
            warn!("ragdoll: only {} bones found", pend.len());
            continue;
        }

        let mut ents: HashMap<String, Entity> = HashMap::new();
        for p in &pend {
            ents.insert(p.name.clone(), commands.spawn_empty().id());
        }
        let mut sound = None;
        let mut hips_body = None;
        let mut bodies = Vec::new();
        for p in &pend {
            let body = ents[&p.name];
            if p.name == "Hips" {
                hips_body = Some((body, p.pos, p.rot));
            }
            if let Some(rs) = p.obj.comp("RagdollSound") {
                let trig = p.obj.components.iter().find(|(t, f)| t == "BoxCollider" && f.b("m_IsTrigger")).map(|(_, f)| f.v3("m_Size"));
                if let (Some(size), Some(clip)) = (trig, rs.asset("groundHitClip")) {
                    // Hips AudioSource: custom rolloff (0,1)->(1,0) with flat tangents over MaxDistance 15.
                    let curve = p.obj.comp("AudioSource").filter(|a| a.i("rolloffMode") == 2).and_then(|a| {
                        let keys = a.get("rolloffCustomCurve").and_then(|c| c.get("m_Curve")).and_then(|c| c.as_array()).cloned().unwrap_or_default();
                        crate::audio::RolloffCurve::from_keys(&keys, a.f("MaxDistance")).map(std::sync::Arc::new)
                    });
                    sound = Some((body, size * p.scale.x.abs() * 0.5, clip, curve));
                }
            }
            let rb = p.obj.comp("Rigidbody").unwrap();
            let s = p.scale.x.abs();
            let shapes = bone_shapes(p.obj, s);
            let limb = p.obj.comp("CapsuleCollider").is_some();
            let mass = rb.f("m_Mass").max(0.01);
            // Unity impulse AddExplosionForce on the body's position (upwardsModifier 0, radius 100, linear falloff).
            let off = p.pos - expl_pos;
            let dist = off.length();
            let fall = (1.0 - dist / 100.0).max(0.0);
            let impulse = off.normalize_or_zero() * force * 1.5 * fall;
            commands.entity(body).insert((
                Name::new(format!("Ragdoll_{}", p.name)),
                RagdollBody,
                Transform::from_translation(p.pos).with_rotation(p.rot),
                Visibility::default(),
                RigidBody::Dynamic,
                Collider::compound(shapes),
                ColliderMassProperties::Mass(mass),
                Friction { coefficient: if limb { 0.4 } else { 0.6 }, combine_rule: CoefficientCombineRule::Average },
                Restitution { coefficient: 0.0, combine_rule: CoefficientCombineRule::Average },
                Damping { linear_damping: rb.f("m_Drag"), angular_damping: rb.f("m_AngularDrag") },
                CollisionGroups::new(Group::from_bits_retain(1 << layer::RAGDOLL), ragdoll_filter()),
                Ccd::enabled(),
                Velocity::zero(),
                ExternalImpulse { impulse, torque_impulse: Vec3::ZERO },
            ));
            // Ragdoll layer: weapons' TriggerEnvironment / MeleeChildCollision ragdoll branches look it up.
            commands.entity(body).insert(crate::physics::UnityLayer(layer::RAGDOLL));
            // CharacterJoint -> parent body (m_ConnectedBody).
            if let Some(j) = p.obj.components.iter().find(|(t, f)| t == "Unknown" && f.contains_key("m_SwingAxis")).map(|(_, f)| f) {
                let parent_name = j
                    .get("m_ConnectedBody")
                    .and_then(|v| v.as_str())
                    .and_then(|r| prefab.0.deref(r))
                    .map(|o| o.path.rsplit('/').next().unwrap().to_string());
                if let Some(pp) = parent_name.and_then(|n| pend.iter().find(|q| q.name == n)) {
                    let jt = joint_for(j, s, (p.pos, p.rot), (pp.pos, pp.rot));
                    commands.entity(body).insert(ImpulseJoint::new(ents[&pp.name], TypedJoint::GenericJoint(jt)));
                }
            }
            bodies.push(RagBone { body, bone: p.bone, scale: p.scale });
        }
        // PlayerHealth: Instantiate(bloodFX (PlayerIK: VFX_BloodFountain_01), position, LookRotation(ejectForceDir), ragdoll Hips).
        if let Some((hb, hpos, hrot)) = hips_body {
            let opts = crate::vfx::SpawnOpts { parent: Some((hb, Mat4::from_rotation_translation(hrot, hpos))), ..default() };
            vfx.spawn("GameObject/VFX_BloodFountain_01.prefab", point, crate::fx::look_rotation(dir), opts);
        }

        // Hat pops off (PlayerHealth.Hat), unless it was already shot off (it keeps flying / lying where it is).
        for (c, w) in &worn {
            if w.owner == dummy {
                commands.entity(c).try_despawn();
            }
        }
        let mut hat_state = None;
        if let Ok(sh) = shot.get(dummy) {
            hat_state = Some(sh.0);
            commands.entity(dummy).remove::<ShotHat>();
        } else if let Some(d) = dress {
            if let Some(hat) = d.hat {
                // A corpse pops the living player's hat (Corpse::hat_from); the dummy its own.
                if let Some(gt) = corpse.and_then(|c| c.hat_from).or_else(|| gts.get(hat).ok().copied()) {
                    let (scale, rot, pos) = gt.affine().to_scale_rotation_translation();
                    let flat = Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero();
                    let (_, rroot, _) = root_gt.affine().to_scale_rotation_translation();
                    // torque = forward*5 + right*5 (Unity pseudovector -> mirrored Z flips it); mass 1 -> angular speed hits the 7 rad/s cap.
                    let t = -((rroot * Vec3::NEG_Z) + (rroot * Vec3::X));
                    // The prefab root's convex MeshCollider (85 of 91 hats; m_Mesh is sometimes another hat's mesh,
                    // e.g. 8 hats use SM_Lizzard_00). Root and mesh frames are identity in every hat prefab, so the
                    // hull is the mesh's whole vertex buffer (PhysX cooks all vertices) in the hat root's space.
                    let hat_idx = d.applied().map(|c| c.hat);
                    let col_path = cos_data.as_ref().zip(hat_idx).and_then(|(cd, i)| cd.hats.get(i)).and_then(|h| h.collider.clone());
                    let collider = col_path.as_ref().and_then(|p| hat_hull(p, scale, &mut hull_cache));
                    info!("hat pop: hat {:?} collider {:?} ({})", hat_idx, col_path, if collider.is_some() { "convex hull" } else { "none" });
                    if corpse.is_some() {
                        let own = gts.get(hat).map(|g| g.translation()).unwrap_or_default();
                        println!("HATPOP start=({:.3},{:.3},{:.3}) corpse_head=({:.3},{:.3},{:.3})", pos.x, pos.y, pos.z, own.x, own.y, own.z);
                    }
                    let body = commands
                        .spawn((
                            Name::new("HatBody"),
                            HatBody(0.0),
                            RagdollBody,
                            Transform::from_translation(pos).with_rotation(rot),
                            Visibility::default(),
                            RigidBody::Dynamic,
                            // Hat(): Rigidbody added at runtime (defaults: mass 1, angularDrag 0.05; drag 0, Interpolate,
                            // ContinuousDynamic). No PhysicMaterial: Unity default (0.6 friction, 0 bounce, average).
                            Friction { coefficient: 0.6, combine_rule: CoefficientCombineRule::Average },
                            Restitution { coefficient: 0.0, combine_rule: CoefficientCombineRule::Average },
                            Damping { linear_damping: 0.0, angular_damping: 0.05 },
                            crate::physics::groups_for_layer(layer::DONT_COLLIDE_WITH_PLAYER),
                            crate::physics::UnityLayer(layer::DONT_COLLIDE_WITH_PLAYER),
                            Ccd::enabled(),
                            Velocity { linear: flat * 1.5, angular: t.normalize_or_zero() * MAX_ANGULAR_SPEED },
                        ))
                        .id();
                    match collider {
                        // Mass 1 with the hull's own centre of mass / inertia (Unity derives both from the colliders).
                        Some(c) => commands.entity(body).insert((c, ColliderMassProperties::Mass(1.0))),
                        // No collider: the Rigidbody falls through everything (as in the original); unit inertia.
                        None => commands.entity(body).insert(AdditionalMassProperties::MassProperties(MassProperties {
                            local_center_of_mass: Vec3::ZERO,
                            mass: 1.0,
                            principal_inertia_local_frame: Quat::IDENTITY,
                            principal_inertia: Vec3::ONE,
                        })),
                    };
                    commands.entity(hat).remove::<ChildOf>().insert(Transform::from_scale(scale));
                    if corpse.is_some() {
                        // Hidden with the spare corpse until now (cosmetics.rs sync_dress_visibility follows later).
                        commands.entity(hat).insert(Visibility::Visible);
                    }
                    commands.entity(body).add_child(hat);
                    hat_state = Some(HatState { hat, body, pivot: d.hat_pivot });
                }
            }
        }

        if let Some(mut a) = anim {
            a.frozen = true;
        }
        if corpse.is_some() {
            println!("RAGDOLL corpse frame={} bodies={} hat_popped={}", frame.0, bodies.len(), hat_state.is_some() as u8);
        }
        commands.entity(dummy).insert(HasRagdoll { bodies, static_parent, hat: hat_state, sound, hips: hips_body.map(|h| h.0) });
    }
}

/// An enemy's worn hat collider (PlayerSetup.ChangeDress: the hat keeps its MeshCollider, tag "Hat", layer 18; only
/// the local player's hatToWearPosition is deactivated). Child of the hat entity.
#[derive(Component)]
pub struct WornHat {
    pub owner: Entity,
}

/// On a dummy: the hat entity that has a [`WornHat`] collider.
#[derive(Component)]
struct WornHatDone(Entity);

/// On a dummy: its hat was shot off (Weapon.TriggerEnvironment "Hat"); restored with the next life.
#[derive(Component)]
struct ShotHat(HatState);

/// `Weapon.TriggerEnvironment` on a "Hat"-tagged collider (the shot direction).
#[derive(Message, Clone, Copy, Debug)]
pub struct HatShot {
    pub collider: Entity,
    pub direction: Vec3,
}

fn worn_hat_colliders(
    mut commands: Commands,
    dummies: Query<(Entity, &DressState, Option<&WornHatDone>), (With<Dummy>, Without<crate::health::Corpse>, Without<HasRagdoll>, Without<ShotHat>)>,
    cos_data: Option<Res<crate::cosmetics::CosmeticsData>>,
    mut hull_cache: Local<HashMap<String, Option<Vec<Vec3>>>>,
) {
    for (dummy, d, done) in &dummies {
        let Some(hat) = d.hat else { continue };
        if done.is_some_and(|w| w.0 == hat) {
            continue;
        }
        commands.entity(dummy).insert(WornHatDone(hat));
        let path = cos_data.as_ref().zip(d.applied().map(|c| c.hat)).and_then(|(cd, i)| cd.hats.get(i)).and_then(|h| h.collider.clone());
        let Some(c) = path.as_ref().and_then(|p| hat_hull(p, Vec3::ONE, &mut hull_cache)) else { continue };
        let col = commands
            .spawn((
                Name::new("WornHatCollider"),
                WornHat { owner: dummy },
                c,
                crate::physics::groups_for_layer(layer::DONT_COLLIDE_WITH_PLAYER),
                crate::physics::UnityLayer(layer::DONT_COLLIDE_WITH_PLAYER),
                Transform::default(),
                Visibility::default(),
            ))
            .id();
        commands.entity(hat).add_child(col);
    }
}

/// TriggerEnvironment "Hat": SetParent(null), Rigidbody (ContinuousDynamic, drag 0, Interpolate), AddForce(direction * 10,
/// Impulse), AddTorque(cam.forward * 40 + shooter.right * 40, Impulse) (mass 1: the spin hits the 7 rad/s cap).
fn shoot_hats(
    mut commands: Commands,
    mut reader: MessageReader<HatShot>,
    worn: Query<(&WornHat, &ChildOf)>,
    dummies: Query<&DressState>,
    gts: Query<&GlobalTransform>,
    cams: Query<&GlobalTransform, With<crate::player::fpc::PlayerCamera>>,
    players: Query<&GlobalTransform, With<crate::player::fpc::Fpc>>,
    cos_data: Option<Res<crate::cosmetics::CosmeticsData>>,
    mut hull_cache: Local<HashMap<String, Option<Vec<Vec3>>>>,
) {
    for s in reader.read() {
        let Ok((w, parent)) = worn.get(s.collider) else { continue };
        let hat = parent.parent();
        let (Ok(d), Ok(gt)) = (dummies.get(w.owner), gts.get(hat)) else { continue };
        let (scale, rot, pos) = gt.affine().to_scale_rotation_translation();
        let fwd = cams.single().map(|c| c.forward().as_vec3()).unwrap_or(Vec3::NEG_Z);
        let right = players.single().map(|p| p.right().as_vec3()).unwrap_or(Vec3::X);
        // Unity pseudovector -> the mirrored port frame flips it (as the death pop's torque).
        let spin = -(fwd + right);
        let path = cos_data.as_ref().zip(d.applied().map(|c| c.hat)).and_then(|(cd, i)| cd.hats.get(i)).and_then(|h| h.collider.clone());
        let Some(c) = path.as_ref().and_then(|p| hat_hull(p, scale, &mut hull_cache)) else { continue };
        let body = commands
            .spawn((
                Name::new("HatBody"),
                HatBody(0.0),
                RagdollBody,
                Transform::from_translation(pos).with_rotation(rot),
                Visibility::default(),
                RigidBody::Dynamic,
                c,
                ColliderMassProperties::Mass(1.0),
                Friction { coefficient: 0.6, combine_rule: CoefficientCombineRule::Average },
                Restitution { coefficient: 0.0, combine_rule: CoefficientCombineRule::Average },
                Damping { linear_damping: 0.0, angular_damping: 0.05 },
                crate::physics::groups_for_layer(layer::DONT_COLLIDE_WITH_PLAYER),
                crate::physics::UnityLayer(layer::DONT_COLLIDE_WITH_PLAYER),
                Ccd::enabled(),
                Velocity { linear: s.direction.normalize_or_zero() * 10.0, angular: spin.normalize_or_zero() * MAX_ANGULAR_SPEED },
            ))
            .id();
        commands.entity(s.collider).try_despawn();
        commands.entity(hat).remove::<ChildOf>().insert(Transform::from_scale(scale));
        commands.entity(body).add_child(hat);
        commands.entity(w.owner).insert(ShotHat(HatState { hat, body, pivot: d.hat_pivot }));
        info!("hat shot off {:?} dir {:?}", w.owner, s.direction);
    }
}

/// The popped hat's Rapier body (seconds since the pop, for the test log).
#[derive(Component)]
struct HatBody(f32);

/// RESTRAFE_RAGDOLL=1: `RESULT hat` lines (position, speed, collider AABB) every 0.5 s for 6 s after the pop.
fn log_hat(time: Res<Time>, mut q: Query<(&mut HatBody, &Transform, &Velocity, Option<&Collider>)>) {
    if std::env::var_os("RESTRAFE_RAGDOLL").is_none() {
        return;
    }
    for (mut h, t, v, c) in &mut q {
        let prev = h.0;
        h.0 += time.delta_secs();
        if (prev / 0.5).floor() != (h.0 / 0.5).floor() && h.0 <= 6.0 {
            let aabb = c.map(|c| c.raw.compute_local_aabb()).map(|a| (a.mins, a.maxs));
            println!(
                "RESULT hat t={:.1} pos=({:.3},{:.3},{:.3}) speed={:.3} ang={:.2} collider_aabb={:?}",
                h.0, t.translation.x, t.translation.y, t.translation.z, v.linear.length(), v.angular.length(), aabb.map(|(a, b)| ([a.x, a.y, a.z], [b.x, b.y, b.z]))
            );
        }
    }
}

/// Convex hull of a hat's MeshCollider mesh (`assets/<path>`, Unity Mesh asset), in the hat root's glb space
/// (Unity (x,y,z) -> (-x,y,z); `decode_mesh` gives (x,y,-z)), scaled by the hat's world scale.
fn hat_hull(path: &str, scale: Vec3, cache: &mut HashMap<String, Option<Vec<Vec3>>>) -> Option<Collider> {
    let pts = cache.entry(path.to_string()).or_insert_with(|| {
        let meshes = crate::ugui_assets::decode_mesh(&format!("assets/{path}"));
        let pos = meshes.as_ref().and_then(|m| m.first()).and_then(|m| match m.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) => Some(p.iter().map(|p| Vec3::new(-p[0], p[1], -p[2])).collect::<Vec<_>>()),
            _ => None,
        });
        if pos.is_none() {
            warn!("hat collider {path}: mesh not decoded");
        }
        pos
    });
    let scaled: Vec<Vec3> = pts.as_ref()?.iter().map(|p| *p * scale).collect();
    Collider::convex_hull(&scaled)
}

/// Mirror the bodies onto the dummy's bone entities (Unity: the ragdoll's own transforms are the bones).
/// Runs after the physics write-back and before transform propagation; bones are processed parents-first.
fn drive_bones(
    ragdolls: Query<&HasRagdoll>,
    bodies: Query<&Transform, (With<RagdollBody>, Without<HasRagdoll>)>,
    parents: Query<&ChildOf>,
    gts: Query<&GlobalTransform>,
    mut locals: Query<&mut Transform, Without<RagdollBody>>,
) {
    for rag in &ragdolls {
        // Driven bones sorted parents-first by hierarchy depth.
        let mut order: Vec<&RagBone> = rag.bodies.iter().collect();
        let depth = |mut e: Entity| {
            let mut d = 0;
            while let Ok(p) = parents.get(e) {
                e = p.parent();
                d += 1;
            }
            d
        };
        order.sort_by_key(|b| depth(b.bone));
        let mut computed: HashMap<Entity, Mat4> = HashMap::new();
        for rb in order {
            let Ok(bt) = bodies.get(rb.body) else { continue };
            // Parent frame: first driven ancestor's computed matrix, composed with the plain bones in between.
            let mut chain = Vec::new();
            let mut cur = parents.get(rb.bone).map(|p| p.parent()).unwrap_or(rag.static_parent);
            let base = loop {
                if let Some(m) = computed.get(&cur) {
                    break *m;
                }
                if cur == rag.static_parent {
                    break gts.get(cur).map(|g| g.to_matrix()).unwrap_or(Mat4::IDENTITY);
                }
                chain.push(cur);
                match parents.get(cur) {
                    Ok(p) => cur = p.parent(),
                    Err(_) => break Mat4::IDENTITY,
                }
            };
            let mut parent_m = base;
            for &e in chain.iter().rev() {
                if let Ok(t) = locals.get(e) {
                    parent_m *= t.to_matrix();
                }
            }
            let desired = Mat4::from_scale_rotation_translation(rb.scale, bt.rotation, bt.translation);
            if let Ok(mut t) = locals.get_mut(rb.bone) {
                *t = Transform::from_matrix(parent_m.inverse() * desired);
            }
            computed.insert(rb.bone, desired);
        }
    }
}

/// Unity max angular speed clamp for ragdoll bodies and the hat.
fn clamp_angular_speed(mut q: Query<&mut Velocity, With<RagdollBody>>) {
    for mut v in &mut q {
        let l = v.angular.length();
        if l > MAX_ANGULAR_SPEED {
            v.angular *= MAX_ANGULAR_SPEED / l;
        }
    }
}

/// Respawn (existing test-map logic): drop the ragdoll, restore the hat on its pivot, resume the animator.
fn despawn_ragdolls(
    mut commands: Commands,
    mut dummies: Query<(Entity, &PlayerHealth, &HasRagdoll, Option<&mut HumanoidAnimator>)>,
) {
    for (dummy, ph, rag, anim) in &mut dummies {
        if ph.dead {
            continue;
        }
        for b in &rag.bodies {
            commands.entity(b.body).despawn();
        }
        if let Some(h) = &rag.hat {
            commands.entity(h.hat).remove::<ChildOf>().insert(Transform::default());
            commands.entity(h.pivot).add_child(h.hat);
            commands.entity(h.body).despawn();
        }
        if let Some(mut a) = anim {
            a.frozen = false;
        }
        // The restored hat gets its worn collider again.
        commands.entity(dummy).remove::<HasRagdoll>().remove::<WornHatDone>();
    }
}

/// Removes corpses marked [`DespawnCorpse`]: ragdoll bodies (separate top-level entities), the hat body (the hat is
/// its child) and the corpse itself.
fn despawn_corpses(mut commands: Commands, q: Query<(Entity, Option<&HasRagdoll>), With<DespawnCorpse>>) {
    for (e, rag) in &q {
        info!("despawn corpse {e:?} (ragdoll: {})", rag.is_some());
        if let Some(r) = rag {
            for b in &r.bodies {
                commands.entity(b.body).try_despawn();
            }
            if let Some(h) = &r.hat {
                commands.entity(h.body).try_despawn();
            }
        }
        commands.entity(e).try_despawn();
    }
}

/// `Weapon.TriggerEnvironment` / `MeleeChildCollision` on a Ragdoll-layer collider: every Rigidbody of that ragdoll gets
/// `AddExplosionForce(force, origin, 100, 1, Impulse)`.
#[derive(Message, Clone, Copy, Debug)]
pub struct RagdollPush {
    /// The hit ragdoll body (any bone).
    pub body: Entity,
    /// Explosion origin (`hitPoint - direction`).
    pub origin: Vec3,
    pub force: f32,
}

fn push_ragdolls(mut commands: Commands, mut reader: MessageReader<RagdollPush>, rags: Query<&HasRagdoll>, gts: Query<&GlobalTransform>, masses: Query<&ColliderMassProperties>) {
    for p in reader.read() {
        let Some(r) = rags.iter().find(|r| r.bodies.iter().any(|b| b.body == p.body)) else { continue };
        for b in &r.bodies {
            let Ok(g) = gts.get(b.body) else { continue };
            // Unity AddExplosionForce: upwardsModifier 1 moves the apparent origin 1 m down; linear falloff over radius 100.
            let origin = p.origin - Vec3::Y;
            let off = g.translation() - origin;
            let fall = (1.0 - (g.translation() - p.origin).length() / 100.0).max(0.0);
            let impulse = off.normalize_or_zero() * p.force * fall;
            let _ = masses.get(b.body);
            commands.entity(b.body).try_insert(ExternalImpulse { impulse, torque_impulse: Vec3::ZERO });
        }
    }
}

/// PlayerPickup.PlayerGrab (F) on a ragdoll, sent by pickup.rs (which also drops both hands on a grab).
#[derive(Message, Clone, Copy, Debug)]
pub enum RagdollGrab {
    /// The Ragdoll-layer body hit by the interaction ray.
    Grab(Entity),
    /// Unparent; every body non-kinematic + AddExplosionForce(150, cam - fwd, 0, 1, Impulse).
    Release,
}

/// The grabbed ragdoll (PlayerPickup.ragdoll / hips): its kinematic Hips body and the Hips rotation relative to the
/// camera at the grab (the root is parented to the camera, so the Hips turn with it).
#[derive(Resource, Default)]
pub struct GrabbedRagdoll(pub Option<(Entity, Quat)>);

fn ragdoll_grab(
    mut commands: Commands,
    mut reader: MessageReader<RagdollGrab>,
    mut grabbed: ResMut<GrabbedRagdoll>,
    rags: Query<&HasRagdoll>,
    cams: Query<&GlobalTransform, With<crate::player::fpc::PlayerCamera>>,
    mut tfs: Query<&mut Transform, With<RagdollBody>>,
) {
    let Ok(cam) = cams.single() else { return };
    let (cam_pos, cam_fwd, cam_rot) = (cam.translation(), cam.forward().as_vec3(), cam.rotation());
    // The ragdoll can vanish while held (dummy respawn / corpse cleanup).
    if grabbed.0.is_some_and(|(h, _)| tfs.get(h).is_err()) {
        grabbed.0 = None;
    }
    for g in reader.read() {
        match *g {
            RagdollGrab::Grab(body) => {
                let Some(r) = rags.iter().find(|r| r.bodies.iter().any(|b| b.body == body)) else { continue };
                let Some(h) = r.hips else { continue };
                let Ok(t) = tfs.get(h) else { continue };
                grabbed.0 = Some((h, cam_rot.inverse() * t.rotation));
                commands.entity(h).try_insert((RigidBody::KinematicPositionBased, Velocity::zero()));
            }
            RagdollGrab::Release => {
                let Some((h, _)) = grabbed.0.take() else { continue };
                let Some(r) = rags.iter().find(|r| r.hips == Some(h)) else { continue };
                commands.entity(h).try_insert(RigidBody::Dynamic);
                // Radius 0: full force at any distance; upwardsModifier 1 lowers the apparent origin by 1 m.
                let origin = cam_pos - cam_fwd - Vec3::Y;
                for b in &r.bodies {
                    let Ok(t) = tfs.get(b.body) else { continue };
                    let impulse = (t.translation - origin).normalize_or_zero() * 150.0;
                    commands.entity(b.body).try_insert(ExternalImpulse { impulse, torque_impulse: Vec3::ZERO });
                }
            }
        }
    }
    // HandleAboubiGrab: hips.position = cam + forward * 1.5 every frame.
    if let Some((h, rel)) = grabbed.0 {
        if let Ok(mut t) = tfs.get_mut(h) {
            t.translation = cam_pos + cam_fwd * 1.5;
            t.rotation = cam_rot * rel;
        }
    }
}

/// `spawn_ragdolls`: dead dummies / corpses become ragdolls (death.rs orders the local death before it).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct RagdollSpawn;

pub struct RagdollPlugin;

impl Plugin for RagdollPlugin {
    fn build(&self, app: &mut App) {
        let prefab = Prefab::load(PREFAB).expect("ragdoll prefab json");
        app.add_message::<RagdollPush>().add_systems(Update, push_ragdolls);
        app.add_message::<HatShot>().add_systems(Update, (worn_hat_colliders, shoot_hats.after(worn_hat_colliders)));
        app.add_message::<RagdollGrab>().init_resource::<GrabbedRagdoll>().add_systems(Update, ragdoll_grab.after(crate::weapons::pickup::player_pickup_update));
        app.insert_resource(RagdollPrefab(prefab))
            .add_systems(Update, (spawn_ragdolls.in_set(RagdollSpawn), despawn_ragdolls, clamp_angular_speed, ragdoll_sound, despawn_corpses, log_hat).after(crate::health::dummy_update))
            .add_systems(PostUpdate, drive_bones.after(PhysicsSet::Writeback).before(TransformSystems::Propagate));
    }
}

/// `RagdollSound.OnTriggerEnter`: the Hips trigger box touching a layer-0 (Default) collider plays
/// groundHitClip once, 3D at the hips, pitch 0.9-1.1.
fn ragdoll_sound(rapier: ReadRapierContext, mut q: Query<&mut HasRagdoll>, gts: Query<&GlobalTransform>, mut sounds: MessageWriter<crate::weapons::WeaponSound>) {
    let Ok(ctx) = rapier.single() else { return };
    for mut r in &mut q {
        let Some((body, half, clip, curve)) = r.sound.clone() else { continue };
        let Ok(gt) = gts.get(body) else { continue };
        let (_, rot, pos) = gt.to_scale_rotation_translation();
        if !crate::physics::overlap_box(&ctx, pos, half, rot, 1).is_empty() {
            info!("ragdoll ground hit sound at {pos:?}");
            sounds.write(crate::weapons::WeaponSound { clip, volume: 1.0, at: Some(pos), pitch: crate::unity::random_range_f(0.9, 1.1), linear_max: 15.0, source: None, rolloff: curve });
            r.sound = None;
        }
    }
}
