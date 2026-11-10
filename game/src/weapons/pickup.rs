//! Port of `PlayerPickup.cs` (interaction, pickup, drop, hand switching), `ItemBehaviour.cs`
//! (held placement, ground spin/bob, drop physics) and the shared part of `Weapon.WeaponUpdate`.

use super::data::{HandAnchors, UField};
use super::{BobPosition, CamQuery, FpArms, Hand, Item, WeaponSound, WeaponUi};
use crate::input::PlayerInput;
use crate::physics::{self, UnityLayer, layer};
use crate::player::fpc::{Fpc, PlayerCamera};
use crate::unity::*;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

/// PlayerPickup prefab values.
const INTERACTION_LAYER: u32 = 128;
const PICKUP_LAND_LAYER: u32 = 17581823;
const SPHERE_RADIUS: f32 = 0.6;
const MAX_SPHERE_DISTANCE: f32 = 5.0;
const MAX_INTERACTION_DISTANCE: f32 = 5.0;
/// PlayerIK PlayerPickup.environmentInteractionLayer (InteractEnvironment).
const ENV_INTERACTION_LAYER: u32 = 524288;
const PICKUP_CLIP: &str = "AudioClip/grabin.ogg";
/// PlayerPickup.bodyInteractionLayer (PlayerIK prefab): Body(11), 14, 19, 24.
const BODY_INTERACTION_LAYER: u32 = 17319936;
/// PlayerPickup.ragdollInteractionLayer (PlayerIK prefab): Default, 19, Ragdoll(23), 24.
const RAGDOLL_INTERACTION_LAYER: u32 = 25690113;
const DROP_CLIP: &str = "AudioClip/grabout.ogg";
const SWITCH_CLIP: &str = "AudioClip/switchWeapons.ogg";
/// PlayerIK Main Camera `near clip plane`.
const CAM_NEAR_CLIP: f32 = 0.01;

/// PlayerPickup state on the player.
#[derive(Component, Default)]
pub struct Hands {
    pub right: Option<Entity>,
    pub left: Option<Entity>,
    pub drop_timer: f32,
    pub interact_timer: f32,
    pub current_interactable: Option<Entity>,
    pub interaction_sphere: bool,
    pub interaction_distance: f32,
    pub current_hit_distance: f32,
    /// `PlayerHealth.shouldDropWeapon` (death, barrel hit): LeftHandDrop + RightHandDrop on the next update.
    pub drop_all: bool,
    /// `currentEnvironmentInteractable` (HandleInteractEnvironment: environmentInteractionLayer ray, InteractEnvironment).
    pub current_env: Option<Entity>,
    /// `heldEnvironmentInteractable` (the PhysicsProp this player carries).
    pub held_env: Option<Entity>,
    /// `currentEnvironmentInteractable.OnInteract(transform)` requested this frame (F or mouse 0), run by props.rs.
    pub env_interact: Option<Entity>,
    /// Weapon menu (weapon_menu.rs): an item spawned in front of the camera, taken through the normal interact flow
    /// as soon as it exists.
    pub give: Option<Entity>,
    /// PlayerGrab `enemyBody`: a stunned body parented to the player root (dummy entity, root-local offset and rotation).
    pub enemy_body: Option<(Entity, Vec3, Quat)>,
}

/// Adds the FPArms rig under the camera and the Hands component to the player.
pub fn setup_fparms(mut commands: Commands, cams: Query<Entity, Added<PlayerCamera>>, players: Query<Entity, (With<Fpc>, Without<Hands>)>) {
    for cam in &cams {
        commands.entity(cam).with_children(|c| {
            c.spawn((Name::new("BobPosition"), BobPosition, Transform::default(), Visibility::default())).with_children(|b| {
                b.spawn((Name::new("FPArms"), FpArms, crate::tween::Tweens::default(), Transform::default(), Visibility::default(), super::markers::viewmodel_layers()));
            });
        });
    }
    for p in &players {
        commands.entity(p).insert(Hands { interaction_distance: MAX_INTERACTION_DISTANCE, current_hit_distance: MAX_SPHERE_DISTANCE, ..default() });
    }
}

fn anchor_for(anchors: &HandAnchors, item: &Item, right_hand: bool, aiming: bool) -> Transform {
    let ib = &item.def.item;
    let both = item.def.require_both_hands();
    let pick = |v: &Vec<Transform>, i: i32| v.get(i.max(0) as usize).copied().unwrap_or_default();
    if right_hand {
        if aiming && ib.b("aimWeapon") && !item.is_reloading {
            if both { pick(&anchors.aim_both, ib.i("aimIndex")) } else { pick(&anchors.aim_right, ib.i("aimIndex")) }
        } else if both {
            pick(&anchors.both, ib.i("camChildIndex"))
        } else {
            pick(&anchors.right, ib.i("camChildIndex"))
        }
    } else {
        pick(&anchors.left, ib.i("camChildIndexLeftHand"))
    }
}

/// SetObjectInHandObserver (owner): `zoomFOV = aimFOV + (!canScopeAim ? defaultFOV - 68 : 0)` for aim weapons.
fn set_zoom_fov(item: &Item, fpc: &mut Fpc) {
    let ib = &item.def.item;
    let can_scope = ib.b("aimWeapon") && ib.asset("aimCrosshair").is_some();
    if ib.b("aimWeapon") {
        fpc.p.zoom_fov = ib.f("aimFOV") + if !can_scope { fpc.default_fov - 68.0 } else { 0.0 };
    }
}

/// SwitchWeapons moves an item between hands with DropObjectServer + SetObjectInHandServer: DropObjectObserver
/// (isScopeAiming = false, camAnimScript.rotateBack = true, OnDrop: isClicked = false, dispenserStart = false; its
/// Rigidbody is destroyed again by OnGrab before any physics step) then SetObjectInHandObserver (zoomFOV).
fn rehand(item: &mut Item, fpc: &mut Fpc) {
    fpc.is_scope_aiming = false;
    fpc.cam_rotate_back = true;
    item.is_clicked = false;
    item.dispenser_start = false;
    set_zoom_fov(item, fpc);
}

#[allow(clippy::too_many_arguments)]
pub fn player_pickup_update(
    mut commands: Commands,
    time: Res<Time>,
    input: Res<PlayerInput>,
    rapier: ReadRapierContext,
    mut players: Query<(&mut Hands, &mut Fpc, Option<&crate::health::PlayerHealth>, &GlobalTransform)>,
    cams: CamQuery,
    fparms: Query<(Entity, &GlobalTransform), With<FpArms>>,
    mut items: Query<(Entity, &mut Item, &mut Transform, &GlobalTransform)>,
    launchers: Query<&super::launcher::LauncherState>,
    mut sounds: MessageWriter<WeaponSound>,
    mut ui: MessageWriter<WeaponUi>,
    anchors: Res<HandAnchors>,
    (grabbed, mut ragdoll_grab, layers): (Res<crate::ragdoll::GrabbedRagdoll>, MessageWriter<crate::ragdoll::RagdollGrab>, Query<&crate::physics::UnityLayer>),
    (hit_parts, mut bodies): (Query<&crate::health::HitPart>, Query<(&mut crate::health::PlayerHealth, &mut Transform), (With<crate::health::Dummy>, Without<Item>, Without<Fpc>, Without<PlayerCamera>)>),
) {
    let Ok(ctx) = rapier.single() else { return };
    let Ok((mut hands, mut fpc, ph, root_g)) = players.single_mut() else { return };
    // ItemBehaviour.rootObject (the player root: feet, yaw only).
    let root_pos = root_g.translation();
    let root_fwd = root_g.forward().as_vec3();
    let dead = ph.is_some_and(|h| h.dead);
    let Ok((_, cam_g, _)) = cams.single() else { return };
    let Ok((fparms_e, fparms_g)) = fparms.single() else { return };
    let dt = time.delta_secs();
    let cam_pos = cam_g.translation();
    let cam_fwd = cam_g.forward().as_vec3();
    let cam_right = cam_g.right().as_vec3();

    // ---- Update(): RightHandFix / LeftHandFix -------------------------------------------------
    for (slot, right) in [(hands.right, true), (hands.left, false)] {
        if let Some(e) = slot {
            if items.get(e).is_err() {
                if right { hands.right = None } else { hands.left = None }
            }
        }
    }
    hands.drop_timer -= dt;
    hands.interact_timer -= dt;
    if hands.right.is_none() {
        fpc.movement_factor = 1.0;
        fpc.jump_factor = 1.0;
        fpc.max_wall_jumps = 1;
        fpc.wall_jump_factor = 1.0;
    }

    // ---- HandleInteractionCheck ---------------------------------------------------------------
    match physics::raycast(&ctx, cam_pos, cam_fwd, MAX_SPHERE_DISTANCE, PICKUP_LAND_LAYER) {
        Some(h) => hands.current_hit_distance = h.distance + SPHERE_RADIUS,
        None => hands.current_hit_distance = MAX_SPHERE_DISTANCE,
    }
    hands.interaction_distance = physics::raycast(&ctx, cam_pos, cam_fwd, MAX_INTERACTION_DISTANCE, PICKUP_LAND_LAYER).map(|h| h.distance).unwrap_or(MAX_INTERACTION_DISTANCE);
    let ray_hit = physics::raycast(&ctx, cam_pos, cam_fwd, hands.interaction_distance + 1e-3, INTERACTION_LAYER);
    // RightHandPickup plays pickupClip a second time for a one-handed item found by the ray (not the sphere cast).
    let target_by_ray = ray_hit.as_ref().is_some_and(|h| items.get(h.entity).is_ok());
    let target = ray_hit
        .or_else(|| physics::sphere_cast(&ctx, cam_pos, SPHERE_RADIUS, cam_fwd, hands.current_hit_distance, INTERACTION_LAYER))
        .map(|h| h.entity)
        .filter(|e| items.get(*e).is_ok());
    // Dead: PlayerPickup is disabled with the player object (OnDisable also hides the grab popup).
    let target = if dead { None } else { target };
    hands.current_interactable = target;
    hands.interaction_sphere = target.is_some();

    // ---- HandleInteractEnvironment: environmentInteractionLayer ray (InteractEnvironment, layer 19) while no weapon is
    // targeted; a carried PhysicsProp is a trigger and is not hit (queriesHitTriggers off). Mouse 0 with an empty right
    // hand interacts with it.
    hands.current_env = if target.is_none() && !dead {
        physics::raycast(&ctx, cam_pos, cam_fwd, hands.interaction_distance + 1e-3, ENV_INTERACTION_LAYER).map(|h| h.entity)
    } else {
        None
    };
    hands.env_interact = None;
    if let Some(env) = hands.current_env {
        // Input.GetMouseButtonDown(0): legacy mouse only, not the right trigger.
        if hands.right.is_none() && input.fire_kbm.pressed && input.enabled {
            hands.env_interact = Some(env);
        }
    }

    // Helpers ---------------------------------------------------------------------------------
    let ammo_of = |items: &Query<(Entity, &mut Item, &mut Transform, &GlobalTransform)>, e: Option<Entity>| -> i32 {
        e.and_then(|e| items.get(e).ok()).map(|(_, i, _, _)| i.current_ammo).unwrap_or(0)
    };
    let both_of = |items: &Query<(Entity, &mut Item, &mut Transform, &GlobalTransform)>, e: Option<Entity>| -> bool {
        e.and_then(|e| items.get(e).ok()).map(|(_, i, _, _)| i.def.require_both_hands()).unwrap_or(false)
    };

    // SetObjectInHand (owner side): parent under FPArms keeping the world pose, then
    // ItemBehaviour lerps it into the anchor.
    let set_in_hand = |commands: &mut Commands, items: &mut Query<(Entity, &mut Item, &mut Transform, &GlobalTransform)>, e: Entity, right: bool, fpc: &mut Fpc, ui: &mut MessageWriter<WeaponUi>| {
        let Ok((_, mut item, mut tf, g)) = items.get_mut(e) else { return };
        let local = fparms_g.affine().inverse() * g.affine();
        let (_, rot, trans) = local.to_scale_rotation_translation();
        tf.translation = trans;
        tf.rotation = rot;
        tf.scale = Vec3::splat(2.0);
        item.hand = if right { Hand::Right } else { Hand::Left };
        item.held_once = true;
        item.physics = false;
        item.bob_origin = None;
        item.dispenser_start = false;
        commands
            .entity(e)
            .insert((ChildOf(fparms_e), UnityLayer(layer::HELD_WEAPON), ColliderDisabled))
            .remove::<(RigidBody, Velocity, ExternalImpulse, Ccd, Damping, ColliderMassProperties)>()
            // ItemBehaviour.CalculateMovementInput (every held frame): alreadyPlayed = false -> the next drop gets
            // its own impact sound.
            .remove::<HitSurfacePlayed>();
        if let Some(mut tw) = commands.get_entity(e).ok() {
            tw.insert(crate::tween::Tweens::default()); // KillTweens
        }
        // ItemBehaviour.OnGrab -> Crosshair; zoom FOV for aim weapons
        set_zoom_fov(&item, fpc);
        let _ = ui;
    };

    let drop = |commands: &mut Commands, items: &mut Query<(Entity, &mut Item, &mut Transform, &GlobalTransform)>, e: Entity, right: bool, fpc: &mut Fpc, sounds: &mut MessageWriter<WeaponSound>, ui: &mut MessageWriter<WeaponUi>, play_sound: bool| {
        let Ok((_, mut item, mut tf, g)) = items.get_mut(e) else { return };
        if play_sound {
            sounds.write(WeaponSound { clip: DROP_CLIP.into(), volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
        }
        // StickOnGround: tweens get killed by the drop that follows, so only the teleport
        // branch matters (no ground near the hand -> place in front of the camera). The ground test is
        // OverlapSphere(finalPos = the hand anchor's world position, 0.7) || OverlapSphere(root + up + root.forward, 0.7).
        let ground = item.def.item.mask("groundLayer");
        let final_pos = fparms_g.transform_point(anchor_for(&anchors, &item, right, false).translation);
        let near = !physics::overlap_sphere(&ctx, final_pos, 0.7, ground).is_empty()
            || !physics::overlap_sphere(&ctx, root_pos + Vec3::Y + root_fwd, 0.7, ground).is_empty();
        let mut world = g.compute_transform();
        if !near {
            world.translation = cam_pos + cam_fwd;
        }
        ui.write(WeaponUi::Ammo { right, text: "---".into(), charged: String::new() });
        // DropObjectObserver
        fpc.is_scope_aiming = false;
        fpc.cam_rotate_back = true; // camAnimScript.rotateBack = true
        item.hand = Hand::None;
        item.is_clicked = false;
        item.dispenser_start = false;
        item.physics = true;
        *tf = Transform { scale: Vec3::splat(2.0), ..world };
        let ib = &item.def.item;
        let eject = ib.f("ejectForce");
        let torque = ib.f("torqueForce");
        let right_v = tf.right().as_vec3();
        // Torque axes mirror as axial vectors between Unity and Bevy space (sign flip).
        let torque_v = -(cam_fwd * torque + right_v * torque);
        // OnDrop: AddComponent<Rigidbody> (mass 1, drag 0, angularDrag 0.05, ContinuousDynamic) + AddForce/AddTorque
        // impulses. bevy_rapier never attaches an existing collider to a body inserted later, so the collider is
        // re-created ([`ReattachCollider`]) and the impulses wait until it belongs to the body (mass 1).
        commands.entity(e).remove::<ChildOf>().remove::<ColliderDisabled>().remove::<Collider>().insert((
            UnityLayer(layer::INTERACTABLE),
            RigidBody::Dynamic,
            Ccd::enabled(),
            Damping { linear_damping: 0.0, angular_damping: 0.05 },
            ColliderMassProperties::Mass(1.0),
            // PhysicMaterial "Weapon": dynamicFriction 0.51, bounciness 0.1 (average combine) against Unity's default
            // ground material (0.6 / 0): 0.61 here averages with Rapier's 0.5 ground friction to Unity's 0.555.
            Friction { coefficient: 0.61, combine_rule: CoefficientCombineRule::Average },
            Restitution { coefficient: 0.1, combine_rule: CoefficientCombineRule::Average },
            Velocity::default(),
            ActiveEvents::COLLISION_EVENTS,
            ReattachCollider { collider: super::item_collider(&item.def), impulse: cam_fwd * eject, torque: torque_v, stage: 0 },
        ));
    };

    // The pickup/drop flow above needs SwitchWeapons after a right-hand drop; we express the
    // C# control flow explicitly below instead of through nested closures.
    let mut pending_switch = false;

    // PlayerHealth.Update (death): shouldDropWeapon -> LeftHandDrop(); RightHandDrop(). Nothing else runs while dead.
    if dead {
        if std::mem::take(&mut hands.drop_all) {
            if let Some(l) = hands.left.take() {
                drop(&mut commands, &mut items, l, false, &mut fpc, &mut sounds, &mut ui, true);
            }
            if let Some(r) = hands.right.take() {
                drop(&mut commands, &mut items, r, true, &mut fpc, &mut sounds, &mut ui, true);
            }
        }
        fpc.has_left_hand = false;
        return;
    }

    // RightHandDrop / LeftHandDrop do nothing while an environment interactable (door, barrel) is under the crosshair.
    let env_block = hands.current_env.is_some();

    // PlayerHealth.Update shouldDropWeapon (alive, e.g. hit by a thrown barrel): LeftHandDrop(); RightHandDrop().
    if std::mem::take(&mut hands.drop_all) && !env_block {
        if let Some(l) = hands.left.take() {
            drop(&mut commands, &mut items, l, false, &mut fpc, &mut sounds, &mut ui, true);
        }
        if let Some(r) = hands.right.take() {
            drop(&mut commands, &mut items, r, true, &mut fpc, &mut sounds, &mut ui, true);
        }
    }

    // Weapon menu: the given item acts as the interact target for one press.
    let given = hands.give.filter(|g| items.get(*g).is_ok());
    if given.is_some() {
        hands.give = None;
    }
    // Weapon menu (port-only): drop everything held, then the given weapon is in the right hand at once (at its anchor,
    // no fly-in, no world collider).
    if let Some(g) = given {
        if let Some(l) = hands.left.take() {
            drop(&mut commands, &mut items, l, false, &mut fpc, &mut sounds, &mut ui, true);
        }
        if let Some(r) = hands.right.take() {
            drop(&mut commands, &mut items, r, true, &mut fpc, &mut sounds, &mut ui, true);
        }
        sounds.write(WeaponSound { clip: PICKUP_CLIP.into(), volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
        hands.right = Some(g);
        set_in_hand(&mut commands, &mut items, g, true, &mut fpc, &mut ui);
        if let Ok((_, item, mut tf, _)) = items.get_mut(g) {
            let a = anchor_for(&anchors, &item, true, fpc.is_aiming);
            tf.translation = a.translation;
            tf.rotation = a.rotation;
        }
        hands.drop_timer = 0.1;
        hands.interact_timer = 0.1;
    }
    let target = if given.is_some() { None } else { target };

    // The grabbed body is a child of the player root: it keeps its offset while the player moves and turns.
    if let Some((e, off, rot)) = hands.enemy_body {
        match bodies.get_mut(e) {
            Ok((ph, mut t)) if !ph.dead && !dead => {
                let (_, root_rot, root_t) = root_g.to_scale_rotation_translation();
                t.translation = root_t + root_rot * off;
                t.rotation = root_rot * rot;
            }
            _ => hands.enemy_body = None,
        }
    }

    // ---- HandleInteraction (F, performed) -----------------------------------------------------
    if given.is_none() && (input.interact.pressed && input.enabled && hands.interact_timer <= 0.0 && fpc.can_move) {
        hands.interact_timer = 0.1;
        // PlayerGrab: F throws a held ragdoll, or grabs the Ragdoll-layer body under the crosshair (ragdollInteractionLayer
        // ray over interactionDistance) after RightHandDrop + LeftHandDrop (no-ops while an environment item is focused).
        if grabbed.0.is_some() {
            ragdoll_grab.write(crate::ragdoll::RagdollGrab::Release);
        } else if let Some(h) = physics::raycast(&ctx, cam_pos, cam_fwd, hands.interaction_distance, RAGDOLL_INTERACTION_LAYER)
            .filter(|h| layers.get(h.entity).is_ok_and(|l| l.0 == physics::layer::RAGDOLL))
        {
            if hands.current_env.is_none() {
                if let Some(r) = hands.right.take() {
                    drop(&mut commands, &mut items, r, true, &mut fpc, &mut sounds, &mut ui, true);
                }
                if let Some(l) = hands.left.take() {
                    drop(&mut commands, &mut items, l, false, &mut fpc, &mut sounds, &mut ui, true);
                }
            }
            ragdoll_grab.write(crate::ragdoll::RagdollGrab::Grab(h.entity));
        }
        // ... then the stunned-body grab: release = SetEnemyParent(false) (unparent, canMove = true) + BumpPlayer(cam
        // forward, 70); grab = bodyInteractionLayer ray on a Body-layer collider of a player that cannot move.
        if let Some((e, _, _)) = hands.enemy_body.take() {
            if let Ok((mut ph, _)) = bodies.get_mut(e) {
                ph.stunned = false;
                ph.add_force(cam_fwd, 70.0);
            }
        } else if fpc.can_move {
            if std::env::var_os("RESTRAFE_DEBUG_GRAB").is_some() {
                let h = physics::raycast(&ctx, cam_pos, cam_fwd, MAX_INTERACTION_DISTANCE, BODY_INTERACTION_LAYER);
                println!("GRABRAY dist={:.2} hit={:?}", hands.interaction_distance, h.map(|h| (h.entity, layers.get(h.entity).map(|l| l.0).ok(), hit_parts.get(h.entity).is_ok(), h.distance)));
            }
            // RESTRAFE_DEBUG_GRAB: test only, ignore the capsule-limited interactionDistance.
            let reach = if std::env::var_os("RESTRAFE_DEBUG_GRAB").is_some() { MAX_INTERACTION_DISTANCE } else { hands.interaction_distance };
            if let Some(h) = physics::raycast(&ctx, cam_pos, cam_fwd, reach, BODY_INTERACTION_LAYER)
                .filter(|h| layers.get(h.entity).is_ok_and(|l| l.0 == physics::layer::BODY))
            {
                let owner = hit_parts.get(h.entity).map(|p| p.owner).ok();
                if let Some((o, (ph, t))) = owner.and_then(|o| bodies.get(o).ok().map(|b| (o, b))) {
                    if ph.stunned && !ph.dead {
                        let (_, root_rot, root_t) = root_g.to_scale_rotation_translation();
                        hands.enemy_body = Some((o, root_rot.inverse() * (t.translation - root_t), root_rot.inverse() * t.rotation));
                    }
                }
            }
        }
        let mut flag = true;
        if let Some(t) = target {
            if let Ok((_, it, _, _)) = items.get(t) {
                flag = it.current_ammo > 0 && !it.cant_take_safe;
            }
        }
        // currentEnvironmentInteractable.OnInteract(transform) runs before the drop-timer gate.
        // In the game an environment interaction (door, grabbing / throwing a barrel) leaves the held weapons alone
        // (user-verified), although the decompiled HandleInteraction would go on to the drop branch.
        let env_used = given.is_none() && (hands.current_env.is_some() || hands.held_env.is_some());
        // ItemBehaviour.OnInteract (target under the crosshair, before the drop-timer gate): dispenserStart = false,
        // transform.DOKill() (ground bob tween).
        if flag {
            if let Some(t) = target {
                if let Ok((_, mut it, _, _)) = items.get_mut(t) {
                    it.dispenser_start = false;
                    it.bob_origin = None;
                }
                commands.entity(t).try_insert(crate::tween::Tweens::default());
            }
        }
        if flag && given.is_none() {
            if let Some(env) = hands.current_env {
                hands.env_interact = Some(env);
            }
        }
        // A DualLauncher with its grenade open (pin pulled) in either hand: F throws it (DualLauncher.Update), no
        // pickup / drop.
        let grenade_open = [hands.right, hands.left].into_iter().flatten().any(|e| launchers.get(e).is_ok_and(|l| l.grenade_open));
        if flag && !env_used && !grenade_open && (hands.drop_timer < 0.0 || given.is_some()) {
            hands.drop_timer = 0.1;
            let has_r = hands.right.is_some();
            let has_l = hands.left.is_some();
            enum Act { RightPick, LeftPick, RightDrop, LeftDrop }
            let mut acts: Vec<Act> = Vec::new();
            if target.is_some() && !has_r {
                acts.push(Act::RightPick);
                if has_l && both_of(&items, target) {
                    acts.push(Act::LeftDrop);
                }
            } else if target.is_some() && has_r {
                let t_both = both_of(&items, target);
                if both_of(&items, hands.right) {
                    acts.push(Act::RightPick);
                } else if has_l {
                    if t_both {
                        acts.extend([Act::RightDrop, Act::LeftDrop, Act::RightPick]);
                    } else if ammo_of(&items, hands.right) <= 0 {
                        acts.push(Act::RightPick);
                    } else {
                        acts.push(Act::LeftPick);
                    }
                } else if t_both {
                    acts.push(Act::RightPick);
                } else if ammo_of(&items, hands.right) <= 0 {
                    acts.push(Act::RightPick);
                } else {
                    acts.push(Act::LeftPick);
                }
            } else if target.is_none() && has_l {
                if has_r {
                    if ammo_of(&items, hands.left) <= 0 {
                        acts.push(Act::LeftPick);
                    } else if ammo_of(&items, hands.right) <= 0 {
                        acts.push(Act::RightPick);
                    } else {
                        acts.push(Act::LeftPick);
                    }
                } else {
                    acts.push(Act::LeftPick);
                }
            } else if target.is_none() && has_r {
                acts.push(Act::RightPick);
            }
            for a in acts {
                match a {
                    // RightHandPickup: pick target (dropping current), or drop if nothing targeted.
                    Act::RightPick => {
                        if let Some(t) = target.filter(|t| Some(*t) != hands.right && Some(*t) != hands.left) {
                            // PlayerPickup.RightHandPickup: pickupClip, then RightHandDrop (dropClip) when holding;
                            // empty hand + ray hit + one-handed item: pickupClip again.
                            sounds.write(WeaponSound { clip: PICKUP_CLIP.into(), volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
                            if let Some(r) = hands.right.take() {
                                drop(&mut commands, &mut items, r, true, &mut fpc, &mut sounds, &mut ui, true);
                            } else if target_by_ray && !both_of(&items, Some(t)) {
                                sounds.write(WeaponSound { clip: PICKUP_CLIP.into(), volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
                            }
                            hands.right = Some(t);
                            set_in_hand(&mut commands, &mut items, t, true, &mut fpc, &mut ui);
                        } else if let Some(r) = hands.right.take() {
                            drop(&mut commands, &mut items, r, true, &mut fpc, &mut sounds, &mut ui, true);
                            if hands.left.is_some() && target.is_none() {
                                pending_switch = true;
                            }
                        }
                    }
                    Act::LeftPick => {
                        if let Some(t) = target.filter(|t| Some(*t) != hands.right && Some(*t) != hands.left) {
                            sounds.write(WeaponSound { clip: PICKUP_CLIP.into(), volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
                            if let Some(l) = hands.left.take() {
                                drop(&mut commands, &mut items, l, false, &mut fpc, &mut sounds, &mut ui, true);
                            }
                            hands.left = Some(t);
                            set_in_hand(&mut commands, &mut items, t, false, &mut fpc, &mut ui);
                        } else if let Some(l) = hands.left.take() {
                            drop(&mut commands, &mut items, l, false, &mut fpc, &mut sounds, &mut ui, true);
                        }
                    }
                    Act::RightDrop => {
                        if let Some(r) = hands.right.take() {
                            drop(&mut commands, &mut items, r, true, &mut fpc, &mut sounds, &mut ui, true);
                        }
                    }
                    Act::LeftDrop => {
                        if let Some(l) = hands.left.take() {
                            drop(&mut commands, &mut items, l, false, &mut fpc, &mut sounds, &mut ui, true);
                        }
                    }
                }
            }
        }
    }

    // ---- Auto-drop requests from weapons (noAmmoClicks > 1, inHandDespawn) -------------------
    let mut auto_drop: Vec<(Entity, bool)> = Vec::new();
    // Weapon.WeaponUpdate: inHandDespawn && currentAmmo <= 0 (grenades, mines, claymore): Right/LeftHandDrop,
    // DespawnObjectServer, SetActive(false) in the same frame -- the item vanishes as soon as its last charge is used.
    let mut vanish: Vec<(Entity, bool)> = Vec::new();
    for (e, it, _, _) in items.iter() {
        if it.hand != Hand::None && it.def.w.b("inHandDespawn") && it.current_ammo <= 0 && it.current_ammo > -100 {
            vanish.push((e, it.in_right()));
        }
    }
    for (e, right) in vanish {
        // With an environment interactable focused the Right/LeftHandDrop is a no-op, but the item is still despawned
        // (RightHandFix then clears the empty hand).
        if right && hands.right == Some(e) {
            hands.right = None;
            if !env_block {
                drop(&mut commands, &mut items, e, true, &mut fpc, &mut sounds, &mut ui, true);
                if hands.left.is_some() && hands.current_interactable.is_none() {
                    pending_switch = true;
                }
            }
        } else if !right && hands.left == Some(e) {
            hands.left = None;
            if !env_block {
                drop(&mut commands, &mut items, e, false, &mut fpc, &mut sounds, &mut ui, true);
            }
        }
        if let Ok((_, mut it, _, _)) = items.get_mut(e) {
            it.current_ammo = -103;
        }
        commands.entity(e).try_despawn();
    }
    for (e, it, _, _) in items.iter() {
        // Gun / LargeRaycastGun / Minigun / DualLauncher drop at > 1 empty clicks, Shotgun at > 2. FlashLight, Taser and
        // RepulsiveGun count noAmmoClicks but never drop.
        let limit = match it.def.class.as_str() {
            "Gun" | "LargeRaycastGun" | "Minigun" | "DualLauncher" => 1,
            "Shotgun" => 2,
            _ => continue,
        };
        if it.hand != Hand::None && it.no_ammo_clicks > limit {
            auto_drop.push((e, it.in_right()));
        }
    }
    for (e, right) in auto_drop {
        if env_block {
            // The drop was a no-op; the next empty click (count > limit again) retries, as in the C#.
            if let Ok((_, mut it, _, _)) = items.get_mut(e) {
                it.no_ammo_clicks = if it.def.class == "Shotgun" { 2 } else { 1 };
            }
            continue;
        }
        if right && hands.right == Some(e) {
            hands.right = None;
            drop(&mut commands, &mut items, e, true, &mut fpc, &mut sounds, &mut ui, true);
            if hands.left.is_some() && hands.current_interactable.is_none() {
                pending_switch = true;
            }
        } else if !right && hands.left == Some(e) {
            hands.left = None;
            drop(&mut commands, &mut items, e, false, &mut fpc, &mut sounds, &mut ui, true);
        }
        if let Ok((_, mut it, _, _)) = items.get_mut(e) {
            it.no_ammo_clicks = 0;
        }
    }

    // ---- SwitchWeapons (scroll performed, or after a right-hand drop) --------------------------
    // SwitchWeapons returns while dropTimer > 0: a RightHandDrop from HandleInteraction (dropTimer just set to 0.1)
    // never switches; one from a weapon (empty clicks, inHandDespawn) does.
    // ChangeWeapon (scroll) and Drop (gamepad buttonNorth) both call HandleDrop -> SwitchWeapons.
    let scroll = input.enabled && (input.scroll != 0.0 || input.drop.performed);
    if (scroll || pending_switch) && hands.drop_timer <= 0.0 {
        match (hands.right, hands.left) {
            (Some(r), Some(l)) => {
                hands.drop_timer = 0.1;
                if !both_of(&items, Some(r)) {
                    sounds.write(WeaponSound { clip: SWITCH_CLIP.into(), volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
                    // DropObjectServer(r), DropObjectServer(l), SetObjectInHandServer(r, left), (l, right).
                    for (e, right) in [(r, false), (l, true)] {
                        if let Ok((_, mut it, _, _)) = items.get_mut(e) {
                            rehand(&mut it, &mut fpc);
                            it.hand = if right { Hand::Right } else { Hand::Left };
                        }
                    }
                    hands.right = Some(l);
                    hands.left = Some(r);
                }
            }
            (None, Some(l)) => {
                hands.drop_timer = 0.1;
                sounds.write(WeaponSound { clip: SWITCH_CLIP.into(), volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
                ui.write(WeaponUi::Ammo { right: false, text: "---".into(), charged: String::new() });
                if let Ok((_, mut it, _, _)) = items.get_mut(l) {
                    rehand(&mut it, &mut fpc);
                    it.hand = Hand::Right;
                }
                hands.right = Some(l);
                hands.left = None;
            }
            (Some(r), None) => {
                hands.drop_timer = 0.1;
                if !both_of(&items, Some(r)) {
                    sounds.write(WeaponSound { clip: SWITCH_CLIP.into(), volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
                    ui.write(WeaponUi::Ammo { right: true, text: "---".into(), charged: String::new() });
                    if let Ok((_, mut it, _, _)) = items.get_mut(r) {
                        rehand(&mut it, &mut fpc);
                        it.hand = Hand::Left;
                    }
                    hands.right = None;
                    hands.left = Some(r);
                }
            }
            (None, None) => {}
        }
    }

    // FirstPersonController: isAiming only without a left-hand item.
    fpc.has_left_hand = hands.left.is_some();
    let _ = cam_right;
}

/// ItemBehaviour.Update + Weapon.WeaponUpdate (shared parts).
#[allow(clippy::too_many_arguments)]
pub fn item_behaviour_update(
    mut commands: Commands,
    time: Res<Time>,
    anchors: Res<HandAnchors>,
    mut players: Query<&mut Fpc>,
    mut items: Query<(Entity, &mut Item, &mut Transform, &GlobalTransform)>,
    mut ui: MessageWriter<WeaponUi>,
    mut fx: MessageWriter<super::gun::GunFx>,
    rapier: ReadRapierContext,
    cams: Query<&GlobalTransform, (With<crate::player::fpc::PlayerCamera>, Without<Item>)>,
    fparms: Query<&GlobalTransform, (With<FpArms>, Without<Item>)>,
) {
    let dt = time.delta_secs();
    let Ok(mut fpc) = players.single_mut() else { return };
    let ctx = rapier.single().ok();
    let cam = cams.single().ok().map(|c| (c.translation(), c.forward().as_vec3()));
    let fparms_inv = fparms.single().ok().map(|g| g.affine().inverse());
    for (e, mut item, mut tf, gtf) in &mut items {
        item.case_sound_timer -= dt;
        // Weapon.WeaponUpdate despawn rules
        if item.current_ammo <= 0 {
            item.cant_take_safe = true;
        }
        if item.hand == Hand::None && item.current_ammo <= 0 && item.current_ammo > -100 {
            item.despawn_in = Some(0.65);
            item.current_ammo = -103;
        }
        if let Some(t) = item.despawn_in.map(|t| t - dt) {
            item.despawn_in = Some(t);
            if t <= 0.0 && item.hand != Hand::None {
                // Weapon.DespawnObject on a held weapon (layer 8 / 9): currentAmmo = -50, no despawn.
                item.despawn_in = None;
                item.current_ammo = -50;
            } else if t <= 0.0 {
                // Weapon.SpawnEffect(behaviour.depopVFX): Instantiate(fx, transform.position, identity); SetActive(false)
                fx.write(super::gun::GunFx::Spawn { prefab: item.def.item.asset("depopVFX"), at: gtf.translation(), normal: Vec3::NEG_Z });
                commands.entity(e).despawn();
                continue;
            }
        }
        match item.hand {
            Hand::None => {
                if !item.physics && !item.dispenser_start {
                    // transform.Rotate(0, 20*dt, 0) (local, Unity +Y = clockwise from above)
                    tf.rotate_local_y(-(20f32 * dt).to_radians());
                    if let Some(o) = item.bob_origin {
                        // DOLocalMove(+up/2, 1.4s).SetEase(InOutSine).SetLoops(-1, Yoyo)
                        item.bob_t += dt;
                        let cyc = (item.bob_t / 1.4) as i32;
                        let f = (item.bob_t / 1.4).fract();
                        let k = crate::tween::Ease::InOutSine.eval(if cyc % 2 == 0 { f } else { 1.0 - f });
                        tf.translation = o + Vec3::Y * 0.5 * k;
                    }
                }
            }
            hand => {
                let right = hand == Hand::Right;
                let ib = &item.def.item;
                if right {
                    // Weapon.WeaponUpdate: movement modifiers from the right-hand weapon.
                    let w = &item.def.w;
                    fpc.movement_factor = w.f("movementFactor");
                    fpc.jump_factor = w.f("jumpFactor");
                    fpc.max_wall_jumps = w.i("maxWallJumps");
                    fpc.wall_jump_factor = w.f("wallJumpFactor");
                    let can_scope = ib.asset("aimCrosshair").is_some();
                    fpc.is_scope_aiming = can_scope && fpc.is_aiming && !item.is_reloading;
                }
                let target = anchor_for(&anchors, &item, right, fpc.is_aiming);
                // WeaponSway: SphereCast(cam.ViewportPointToRay(centre), radius, distance, clippingLayerMask) -> lerp to
                // initialPosition - cam.forward * offsetCurve(hit.distance / distance) (the gun backs off near walls),
                // else to initialPosition. TiltSway: rotation slerp.
                let mut goal = target.translation;
                if let (Some(ctx), Some((cp, cf)), Some(inv)) = (ctx.as_ref(), cam, fparms_inv) {
                    let dist = ib.f("distance");
                    if dist > 0.0 {
                        // ViewportPointToRay starts on the near clip plane (PlayerIK Main Camera: 0.01).
                        if let Some(h) = physics::sphere_sweep_unity(ctx, cp + cf * CAM_NEAR_CLIP, ib.f("radius"), cf, dist, ib.mask("clippingLayerMask")) {
                            let c = ib.get("offsetCurve").map(crate::vfx::def::Curve::parse).map(|k| k.eval(h.distance / dist)).unwrap_or(0.0);
                            goal -= inv.transform_vector3(cf) * c;
                        }
                    }
                }
                tf.translation = lerp3(tf.translation, goal, dt * ib.f("smoothAmount"));
                tf.rotation = slerp(tf.rotation, target.rotation, dt * ib.f("smoothRotation"));
                tf.scale = Vec3::splat(2.0);
                let w = &item.def.w;
                let text = if w.b("needsAmmo") { item.current_ammo.max(0).to_string() } else { "∞".to_string() };
                let charged = if w.b("reloadWeapon") { format!("{} / ", item.charged_bullets) } else { String::new() };
                ui.write(WeaponUi::Ammo { right, text, charged });
            }
        }
    }
}

/// A dropped item waiting for its collider to be attached to its new rigidbody (see `drop` in
/// [`player_pickup_update`]): stage 0 = old collider being removed, 1 = collider re-inserted, then the drop impulses.
#[derive(Component)]
pub struct ReattachCollider {
    pub collider: Collider,
    pub impulse: Vec3,
    pub torque: Vec3,
    pub stage: u8,
}

pub fn reattach_colliders(
    mut commands: Commands,
    mut q: Query<(Entity, &mut ReattachCollider, Has<Collider>, Has<RapierColliderHandle>, Has<RigidBody>)>,
) {
    for (e, mut r, has_collider, has_handle, has_body) in &mut q {
        if !has_body {
            // Picked up again before the collider came back.
            if !has_collider {
                commands.entity(e).insert(r.collider.clone());
            }
            commands.entity(e).remove::<ReattachCollider>();
            continue;
        }
        match r.stage {
            0 if !has_collider && !has_handle => {
                commands.entity(e).insert(r.collider.clone());
                r.stage = 1;
            }
            1 if has_handle => {
                commands.entity(e).insert(ExternalImpulse { impulse: r.impulse, torque_impulse: r.torque }).remove::<ReattachCollider>();
            }
            _ => {}
        }
    }
}

/// `ItemBehaviour.alreadyPlayed` (never reset): the item has had its one drop-impact sound.
#[derive(Component)]
pub struct HitSurfacePlayed;

/// Min distance of a logarithmic-shaped rolloff for the item root's AudioSource (`audio`): the custom curve's (rolloffMode
/// 2) last full-volume key x MaxDistance, else MinDistance.
/// The item's own AudioSource custom rolloff curve (rolloffMode 2: every weapon prefab, max 36..400 m).
fn item_rolloff(def: &super::data::WeaponDef) -> Option<std::sync::Arc<crate::audio::RolloffCurve>> {
    let a = def.prefab.objects.first().and_then(|o| o.comp("AudioSource"))?;
    if a.i("rolloffMode") != 2 {
        return None;
    }
    let keys = a.get("rolloffCustomCurve").and_then(|c| c.get("m_Curve")).and_then(|c| c.as_array()).cloned().unwrap_or_default();
    crate::audio::RolloffCurve::from_keys(&keys, a.f("MaxDistance")).map(std::sync::Arc::new)
}

fn item_audio_min_distance(def: &super::data::WeaponDef) -> f32 {
    let Some(a) = def.prefab.objects.first().and_then(|o| o.comp("AudioSource")) else { return 1.0 };
    let max = a.f("MaxDistance");
    if a.i("rolloffMode") == 2 {
        let keys = a.get("rolloffCustomCurve").and_then(|c| c.get("m_Curve")).and_then(|c| c.as_array()).cloned().unwrap_or_default();
        let full = keys.iter().filter(|k| k.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) >= 0.999).filter_map(|k| k.get("time").and_then(|v| v.as_f64())).fold(0.0f64, f64::max);
        return ((full as f32) * max).max(1.0);
    }
    a.f("MinDistance").max(1.0)
}

/// `ItemBehaviour.OnCollisionEnter`: first collision of a dropped item (tempRb set) -> if its speed > 0.5,
/// `audio.PlayOneShot(hitSurfaceClip, Clamp(speed, 1.3, 5) / 5)` with spatialBlend 1.
pub fn item_impact_sound(
    mut commands: Commands,
    mut events: MessageReader<CollisionEvent>,
    items: Query<(&Item, &Velocity, &GlobalTransform), (With<RigidBody>, Without<HitSurfacePlayed>)>,
    mut sounds: MessageWriter<WeaponSound>,
) {
    for ev in events.read() {
        let CollisionEvent::Started(a, b, _) = ev else { continue };
        for e in [*a, *b] {
            let Ok((item, vel, g)) = items.get(e) else { continue };
            if !item.physics {
                continue;
            }
            commands.entity(e).try_insert(HitSurfacePlayed);
            let speed = vel.linear.length();
            if speed > 0.5 {
                if let Some(clip) = item.def.item.asset("hitSurfaceClip") {
                    sounds.write(WeaponSound {
                        clip,
                        volume: speed.clamp(1.3, 5.0) / 5.0 * item.audio_gain,
                        at: Some(g.translation()),
                        // Same `audio` source as the weapon's shots: keeps the last changePitchOnShoot pitch.
                        pitch: item.audio_pitch,
                        linear_max: -item_audio_min_distance(&item.def),
                        source: Some(e),
                        rolloff: item_rolloff(&item.def),
                    });
                }
            }
        }
    }
}
