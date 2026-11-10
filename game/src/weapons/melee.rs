//! Port of `MeleeWeapon.cs` + `MeleeChildCollision.cs` (swords, bat, knives...).
//!
//! Single-player: `[ServerRpc]`/`[ObserversRpc]` bodies run directly. The Unity melee hit is an
//! animated child collider (layer Melee, `OnCollisionEnter`); the port has no attack animation
//! clips, so while `canHit`/`canHitEnvi` are set a forward sphere sweep (sized from the prefab's
//! `Collision` BoxCollider) stands in for the swinging collider. Collision-enter semantics are kept
//! by remembering which entities were already hit during the current swing.

use super::data::UField;
use super::gun::{DollHealth, GunFx, HitCtx};
use super::{CamQuery, Item, WeaponSound};
use crate::input::PlayerInput;
use crate::physics;
use crate::player::fpc::Fpc;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub fn is_melee(class: &str) -> bool {
    class == "MeleeWeapon"
}

#[derive(Component, Default)]
pub struct MeleeState {
    /// Pending `attackAnimator.SetTrigger(..)` (consumed by `anim::update`).
    pub anim_trigger: Option<String>,
    /// `fireTimer`.
    pub fire_timer: f32,
    /// `collisionScript.canHit` / `canHitEnvi`.
    pub can_hit: bool,
    pub can_hit_envi: bool,
    pub second_attack_playing: bool,
    zoom_trigger: bool,
    /// `MeleeChildCollision.hitsCounter` (never reset in the C#).
    hits_counter: i32,
    /// Pending `FireCoroutine`s: remaining `WaitForSeconds`.
    pending: Vec<f32>,
    /// Colliders touching the Collision shape at the last physics step (OnCollisionEnter = new pair).
    touching: Vec<Entity>,
    /// Resolved `Collision` child collider.
    collider: Option<MeleeCollider>,
}

/// MeleeChildCollision serialized fields used on hits.
struct Swing {
    hit_sfx: Option<String>,
    /// MeleeChildCollision.hitVFX (environment / glass hits: Instantiate(hitVFX, contact, LookRotation(normal))).
    hit_vfx: Option<String>,
}

fn swing_of(def: &super::data::WeaponDef) -> Swing {
    match def.prefab.first_with("MeleeChildCollision") {
        Some((_, mcc)) => Swing { hit_sfx: mcc.asset("hitSFX"), hit_vfx: mcc.asset("hitVFX") },
        None => Swing { hit_sfx: None, hit_vfx: None },
    }
}

/// The weapon's `Collision` child (kinematic Rigidbody, layer Melee, child of the animated `AimStrafePivot/anim`):
/// its enabled collider, resolved on the weapon's glb instance.
#[derive(Clone)]
pub struct MeleeCollider {
    node: Entity,
    /// BoxCollider: centre + half size in the node's glb-local (X-mirrored) space. None = convex MeshCollider (`hull`).
    bx: Option<(Vec3, Vec3)>,
    hull: Option<Collider>,
}

#[derive(bevy::ecs::system::SystemParam)]
pub struct MeleeShapes<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    names: Query<'w, 's, &'static Name>,
    gts: Query<'w, 's, &'static GlobalTransform>,
    mesh3d: Query<'w, 's, &'static Mesh3d>,
    meshes: Res<'w, Assets<Mesh>>,
}

fn resolve_collider(me: Entity, def: &super::data::WeaponDef, sh: &MeleeShapes) -> Option<MeleeCollider> {
    let (obj, _) = def.prefab.first_with("MeleeChildCollision")?;
    let node = super::anim::find_path(me, &obj.path, &sh.children, &sh.names)?;
    for (ty, f) in &obj.components {
        if f.i("m_Enabled") == 0 {
            continue;
        }
        if ty == "BoxCollider" {
            let c = f.v3("m_Center");
            return Some(MeleeCollider { node, bx: Some((Vec3::new(-c.x, c.y, c.z), f.v3("m_Size").abs() * 0.5)), hull: None });
        }
        if ty == "MeshCollider" {
            // Convex MeshCollider (BaseballBat: SM_BaseballBat00): hull of the same mesh as rendered by the prefab's
            // MeshFilter, in mesh space, placed at the Collision node.
            let mesh = f.asset("m_Mesh")?;
            let vis = def.prefab.objects.iter().find(|o| o.comp("MeshFilter").and_then(|m| m.asset("m_Mesh")).as_deref() == Some(mesh.as_str()))?;
            let vnode = super::anim::find_path(me, &vis.path, &sh.children, &sh.names)?;
            let prim = std::iter::once(vnode).chain(sh.children.iter_descendants(vnode)).find(|e| sh.mesh3d.contains(*e))?;
            let m = sh.meshes.get(&sh.mesh3d.get(prim).ok()?.0)?;
            let hull = Collider::from_bevy_mesh(m, &ComputedColliderShape::ConvexHull)?;
            return Some(MeleeCollider { node, bx: None, hull: Some(hull) });
        }
    }
    None
}

impl MeleeCollider {
    /// World shape + pose of the collider (the node's GlobalTransform: last frame's animated pose, as PhysX sees the
    /// kinematic body in the next FixedUpdate).
    fn pose(&self, sh: &MeleeShapes) -> Option<(Collider, Vec3, Quat)> {
        let gt = sh.gts.get(self.node).ok()?;
        let (scale, rot, pos) = gt.to_scale_rotation_translation();
        let scale = scale.abs();
        if let Some((c, h)) = self.bx {
            let h = h * scale;
            return Some((Collider::cuboid(h.x, h.y, h.z), gt.transform_point(c), rot));
        }
        let mut hull = self.hull.clone()?;
        hull.set_scale(scale, 4);
        Some((hull, pos, rot))
    }
}

fn play(sounds: &mut MessageWriter<WeaponSound>, clip: Option<String>) {
    if let Some(c) = clip {
        sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
    }
}

/// MeleeChildCollision.Update: `trailObject.SetActive(canHit)` (BigFattyBro, Flamberge, JahvalMahmaerd, Nizeh): the
/// blade trail only exists while a swing can hit (the prefab's active trail is switched off on the first frame).
pub fn melee_trails(mut commands: Commands, q: Query<(Entity, &Item, &MeleeState)>, mut shown: Local<std::collections::HashMap<Entity, bool>>) {
    shown.retain(|e, _| q.contains(*e));
    for (e, item, ms) in &q {
        let Some(path) = item.def.prefab.first_with("MeleeChildCollision").and_then(|(_, m)| m.get("trailObject").and_then(|v| v.as_str()).map(str::to_string)) else { continue };
        let Some(path) = path.strip_prefix("ref:").and_then(|r| r.split("::").next()).map(str::to_string) else { continue };
        if shown.get(&e) != Some(&ms.can_hit) {
            shown.insert(e, ms.can_hit);
            commands.entity(e).try_insert(super::attached_fx::FxToggle { path, on: ms.can_hit });
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn melee_update(
    time: Res<Time>,
    input: Res<PlayerInput>,
    rapier: ReadRapierContext,
    mut players: Query<(Entity, &mut Fpc)>,
    mut cams: CamQuery,
    mut weapons: Query<(Entity, &mut Item, &mut MeleeState, &mut Transform), Without<crate::player::fpc::PlayerCamera>>,
    layers: Query<&physics::UnityLayer>,
    tags: Query<&physics::UnityTag>,
    mut dolls: Query<&mut DollHealth>,
    mut hit: HitCtx,
    mut sounds: MessageWriter<WeaponSound>,
    mut fx: MessageWriter<GunFx>,
    shapes: MeleeShapes,
    mut fixed_acc: Local<f32>,
) {
    let dt = time.delta_secs();
    // Unity fixed step (TimeManager Fixed Timestep 1/30 s): physics contacts are only evaluated on fixed steps.
    const FIXED_DT: f32 = 1.0 / 30.0;
    *fixed_acc += dt;
    let mut steps = 0;
    while *fixed_acc >= FIXED_DT {
        *fixed_acc -= FIXED_DT;
        steps += 1;
    }
    let Ok(ctx) = rapier.single() else { return };
    let Ok((player, mut fpc)) = players.single_mut() else { return };
    let Ok((cam_tf, cam_g, mut cam_tw)) = cams.single_mut() else { return };
    let cam_tf = *cam_tf;
    let cam_g = *cam_g;
    let fire1 = input.fire.value();
    let fire2 = input.fire2.value();
    for (me, mut item, mut ms, mut tf) in &mut weapons {
        let item = &mut *item;
        let def = item.def.clone();
        let w = &def.w;
        let aim_weapon = def.item.b("aimWeapon");
        let fwd = cam_g.forward().as_vec3();
        let in_r = item.in_right();
        let in_l = item.in_left();

        // ---- MeleeChildCollision.OnCollisionEnter ---------------------------------------------------
        // The hit volume is the prefab's `Collision` child (kinematic Rigidbody, layer Melee) under the animated
        // `AimStrafePivot/anim`: PhysX tests it at its current (teleported) pose every fixed step (1/30 s, before
        // Update) and OnCollisionEnter fires for each collider whose contact pair starts. A dropped weapon (layer 7)
        // returns early; the own player's colliders (SelfBody / Self) are not in the Melee collision row.
        if item.hand == super::Hand::None {
            ms.touching.clear();
        } else if steps > 0 {
            if ms.collider.is_none() {
                ms.collider = resolve_collider(me, &def, &shapes);
            }
            let contacts = ms
                .collider
                .as_ref()
                .and_then(|c| c.pose(&shapes))
                .map(|(shape, pos, rot)| physics::shape_contacts(&ctx, &shape, pos, rot, physics::MELEE_COLLIDES))
                .unwrap_or_default();
            let mut contacts = contacts;
            for h in contacts.iter_mut() {
                // Contact normal points out of the hit collider (toward the blade).
                if let Ok(g) = shapes.gts.get(h.entity) {
                    if h.normal.dot(h.point - g.translation()) < 0.0 {
                        h.normal = -h.normal;
                    }
                }
            }
            let now: Vec<Entity> = contacts.iter().map(|h| h.entity).collect();
            let entered: Vec<_> = contacts.into_iter().filter(|h| !ms.touching.contains(&h.entity)).collect();
            ms.touching = now;
            let swing = swing_of(&def);
            let bounce = w.b("bounceHolder") && w.f("playerKnockback") != 0.0;
            let root_fwd = shapes.gts.get(player).map(|g| g.forward().as_vec3()).unwrap_or(fwd);
            for h in entered {
                let l = layers.get(h.entity).map(|l| l.0).unwrap_or(0);
                let tag = tags.get(h.entity).map(|t| t.0.clone()).unwrap_or_default();
                if std::env::var_os("RESTRAFE_DEBUG_HITS").is_some() {
                    let name = hit.parts.get(h.entity).map(|p| p.name.clone()).or_else(|_| shapes.names.get(h.entity).map(|n| n.as_str().to_string())).unwrap_or_default();
                    println!(
                        "MELEE enter {name} layer={l} tag={tag} can_hit={} envi={} at=({:.2},{:.2},{:.2}) n=({:.2},{:.2},{:.2}) timer={:.3}",
                        ms.can_hit, ms.can_hit_envi, h.point.x, h.point.y, h.point.z, h.normal.x, h.normal.y, h.normal.z, ms.fire_timer
                    );
                }
                if tag == "ShatterableGlass" && ms.can_hit_envi {
                    if bounce {
                        fpc.add_force(-fwd, w.f("playerKnockback")); // BounceHolder
                    }
                    // TODO BreakGlassServer(point, -normal, glass object)
                    fx.write(GunFx::Spawn { prefab: swing.hit_vfx.clone(), at: h.point, normal: h.normal });
                    play(&mut sounds, swing.hit_sfx.clone());
                }
                ms.hits_counter += 1; // hitsCounter++ on every collision enter
                if ms.can_hit {
                    // GetComponentInParent<PlayerHealth>() (hit colliders on layer Body).
                    if let Some(part) = hit.parts.get(h.entity).ok().filter(|p| p.owner != player) {
                        let head = part.name == "Head_Col"; // melee: Head_Col only (no Neck)
                        let owner = part.owner;
                        // HitServer
                        if let Ok(mut ph) = hit.healths.get_mut(owner) {
                            // BumpPlayerServer(cam.forward + up * 2, repulseForce, enemy) -> BumpPlayer -> PlayerHealth bounce.
                            let rf = w.f("repulseForce");
                            if rf != 0.0 {
                                ph.add_force(fwd + Vec3::Y * 2.0, rf);
                            }
                            let mut dmg = if ms.second_attack_playing { w.f("secondAttackDamage") } else { w.f("baseAttackDamage") };
                            if head {
                                dmg *= w.f("headMultiplier");
                            }
                            // MeleeWeapon.HitServer (MeleeWeapon.cs:297-303): bodyImpact at LookRotation(normal), bloodSplatter at
                            // identity, and when aimWeapon && aiming also headImpact + genericBodyImpact at LookRotation(normal).
                            fx.write(GunFx::Spawn { prefab: w.asset("bodyImpact"), at: h.point, normal: h.normal });
                            fx.write(GunFx::Spawn { prefab: w.asset("bloodSplatter"), at: h.point, normal: Vec3::NEG_Z });
                            if aim_weapon && fpc.is_aiming {
                                fx.write(GunFx::Spawn { prefab: w.asset("headImpact"), at: h.point, normal: h.normal });
                                fx.write(GunFx::Spawn { prefab: w.asset("genericBodyImpact"), at: h.point, normal: h.normal });
                            }
                            hit.ui.write(super::WeaponUi::HitMarker { head, source: Some(me) });
                            if head {
                                play(&mut sounds, w.asset("headHitClip")); // LocalSound(0)
                            }
                            play(&mut sounds, w.asset("bodyHitClip")); // LocalSound(1)
                            if ph.health - dmg <= 0.0 {
                                play(&mut sounds, w.asset("deathClip")); // LocalSound(2)
                                fpc.kill_shock_wave();
                                ph.health = -8.0; // KillServer
                                ph.last_hit = Some((fwd, w.f("ragdollEjectForce"), h.point));
                            } else {
                                ph.health -= dmg; // GiveDamage
                            }
                        }
                        if ms.hits_counter >= w.i("hitsAmount") {
                            ms.can_hit = false;
                        }
                    } else if let Ok(mut d) = dolls.get_mut(h.entity) {
                        // Port test doll (no PlayerHealth): treated like a body hit.
                        d.health -= if ms.second_attack_playing { w.f("secondAttackDamage") } else { w.f("baseAttackDamage") };
                        hit.ui.write(super::WeaponUi::HitMarker { head: false, source: Some(me) });
                        play(&mut sounds, w.asset("bodyHitClip"));
                        if ms.hits_counter >= w.i("hitsAmount") {
                            ms.can_hit = false;
                        }
                    }
                    // Ragdoll branch (MeleeChildCollision.cs:64-74): bodyImpact + bloodSplatter at LookRotation(normal),
                    // AddExplosionForce(ragdollEjectForce, contact - rootObject.forward, 100, 1, Impulse) on every body.
                    hit.trigger_ragdoll(h.entity, w, h.point, h.normal, root_fwd, &mut fx);
                }
                let env_layer = matches!(l, physics::layer::DEFAULT | physics::layer::SHOOT_THROUGH | physics::layer::INTERACT_ENVIRONMENT);
                if env_layer && ms.can_hit_envi {
                    if bounce {
                        fpc.add_force(-fwd, w.f("playerKnockback")); // BounceHolder (self-knockback)
                    }
                    ms.can_hit_envi = false;
                    // Only the swing's own hitVFX + hitSFX: MeleeWeapon never calls its SpawnVFX (no surface impact / bullet hole).
                    fx.write(GunFx::Spawn { prefab: swing.hit_vfx.clone(), at: h.point, normal: h.normal });
                    play(&mut sounds, swing.hit_sfx.clone());
                }
            }
        }

        // ---- Update ----------------------------------------------------------------------------
        // WeaponUpdate() is item_behaviour_update in pickup.rs.
        if ms.fire_timer > 0.0 {
            ms.fire_timer -= dt;
        }
        // outofhandsAnim SetBool: weapons/anim.rs (OutOfHand while not held).
        if item.hand == super::Hand::None {
            // gameObject.layer == 7 (not held) returns early; coroutines keep ticking.
            let started = ms.pending.len();
            tick_coroutines(&def, &mut ms, &mut fpc, &cam_tf, &cam_g, &mut cam_tw, dt, aim_weapon, &mut sounds, started);
            continue;
        }
        if w.b("minusScale") && in_l {
            tf.scale = Vec3::new(-tf.scale.x.abs(), tf.scale.y, tf.scale.z);
        }
        let t_base = w.f("timeBetweenBaseAttack");
        let t_second = w.f("timeBetweenSecondAttack");
        let aiming = fpc.is_aiming;
        if ms.fire_timer < t_base - w.f("baseAttackDuration") && !aiming {
            ms.can_hit = false;
        } else if ms.fire_timer < t_second - w.f("secondAttackDuration") && aiming {
            ms.can_hit = false;
            ms.second_attack_playing = false;
        }
        if ms.fire_timer < t_base - w.f("baseAttackDuration") && !aiming {
            ms.can_hit_envi = false;
        } else if ms.fire_timer < t_second - w.f("secondAttackDuration") && aiming {
            ms.can_hit_envi = false;
        }
        let (f_r, f_l) = if item.invert_fire { (fire2, fire1) } else { (fire1, fire2) };
        let reload_weapon = w.b("reloadWeapon");
        let mut want_fire = 0;
        if !w.b("onePressShoot") {
            if f_r > 0.1 && in_r && (reload_weapon || item.current_ammo > 0) {
                want_fire += 1;
            }
            if f_l > 0.1 && in_l && (reload_weapon || item.current_ammo > 0) {
                want_fire += 1;
            }
            let ak = w.b("akAnim");
            fpc.cam_rotate_back = !((f_r > 0.1 && in_r && ak && item.current_ammo > 0) || (f_l > 0.1 && in_l && ak && item.current_ammo > 0));
            if !reload_weapon && item.current_ammo <= 0 {
                if f_r < 0.1 && in_r {
                    item.is_clicked = true;
                }
                if f_l < 0.1 && in_l {
                    item.is_clicked = true;
                }
            }
        } else {
            if f_r < 0.1 && in_r {
                item.is_clicked = true;
            }
            if f_l < 0.1 && in_l {
                item.is_clicked = true;
            }
            fpc.cam_rotate_back = true;
        }
        if item.is_clicked {
            if f_r > 0.1 && in_r {
                want_fire += 1;
                item.is_clicked = false;
            }
            if f_l > 0.1 && in_l {
                want_fire += 1;
                item.is_clicked = false;
            }
        }
        if item.shot && w.f("timeBetweenFire") < 0.2 && w.b("onePressShoot") {
            want_fire += 1;
        }
        // Coroutines started this frame first resume next frame (WaitForSeconds, even with a 0 s delay).
        let started = ms.pending.len();
        for _ in 0..want_fire {
            fire(&def, &mut ms, &fpc, in_l, aim_weapon, !input.enabled, &mut sounds);
        }
        let second_delay = w.f("secondAttackDelay");
        if ms.fire_timer > 0.1 && second_delay > 0.0 && ms.second_attack_playing {
            fpc.is_zooming = true;
            ms.zoom_trigger = true;
        } else if ms.zoom_trigger && !ms.can_hit && second_delay > 0.0 {
            fpc.is_zooming = false;
            ms.zoom_trigger = false;
        }
        tick_coroutines(&def, &mut ms, &mut fpc, &cam_tf, &cam_g, &mut cam_tw, dt, aim_weapon, &mut sounds, started);
    }
}

/// `Fire()` (`PauseManager.pause` = `!input.enabled`; the rest of Update still runs while paused).
#[allow(clippy::too_many_arguments)]
fn fire(def: &super::data::WeaponDef, ms: &mut MeleeState, fpc: &Fpc, in_left: bool, aim_weapon: bool, paused: bool, sounds: &mut MessageWriter<WeaponSound>) {
    let w = &def.w;
    if paused || !fpc.can_move || ms.fire_timer > 0.0 {
        return;
    }
    let second = aim_weapon && fpc.is_aiming;
    // StartCoroutine(FireCoroutine()): wait first/secondAttackDelay, then TriggerAttack.
    ms.pending.push(if second { w.f("secondAttackDelay") } else { w.f("firstAttackDelay") });
    if w.asset("firstAttackStartClip").is_some() && (!aim_weapon || !fpc.is_aiming) {
        play(sounds, w.asset("firstAttackStartClip")); // ShootServerEffect(2)
    } else if w.asset("secondAttackStartClip").is_some() && aim_weapon && fpc.is_aiming {
        play(sounds, w.asset("secondAttackStartClip")); // ShootServerEffect(3)
    }
    ms.fire_timer = if second { w.f("timeBetweenSecondAttack") } else { w.f("timeBetweenBaseAttack") };
    ms.second_attack_playing = second;
    // attackAnimator.SetTrigger(second ? secondAttackAnim : (inLeftHand ? baseAttackLeftAnim : baseAttackAnim))
    ms.anim_trigger = Some(w.s(if second { "secondAttackAnim" } else if in_left { "baseAttackLeftAnim" } else { "baseAttackAnim" }));
}

/// Advances `FireCoroutine` waits; on expiry runs `TriggerAttack` + camera animation.
#[allow(clippy::too_many_arguments)]
fn tick_coroutines(
    def: &super::data::WeaponDef,
    ms: &mut MeleeState,
    fpc: &mut Fpc,
    cam_tf: &Transform,
    cam_g: &GlobalTransform,
    cam_tw: &mut crate::tween::Tweens,
    dt: f32,
    aim_weapon: bool,
    sounds: &mut MessageWriter<WeaponSound>,
    started: usize,
) {
    if ms.pending.is_empty() {
        return;
    }
    let w = &def.w;
    // Entries from index `started` on were pushed this frame and are not checked until the next one.
    for t in ms.pending.iter_mut().take(started) {
        *t -= dt;
    }
    let before = ms.pending.len();
    let mut i = 0;
    ms.pending.retain(|t| {
        i += 1;
        i > started || *t > 0.0
    });
    let fired = before - ms.pending.len();
    for _ in 0..fired {
        // TriggerAttack
        let second = aim_weapon && fpc.is_aiming;
        // ShootServerEffect(second ? 1 : 0)
        play(sounds, if second { w.asset("secondAttackClip") } else { w.asset("fireClip") });
        ms.can_hit = true;
        ms.can_hit_envi = true;
        let amount = if second { w.f("secondPropulsionAmount") } else { w.f("basePropulsionAmount") };
        let fwd = cam_g.forward().as_vec3();
        if w.b("bforce") && w.b("propulsion") {
            fpc.b_force(fwd, amount, true, false, w.f("bforceDecel"), true);
        } else if w.b("propulsion") {
            fpc.custom_add_force(fwd, amount);
        }
        // revolverShake ? CameraRevolverAnimation : CameraAnimation (both handled by camera_recoil)
        super::camera_recoil(def, fpc, cam_tf, cam_tw, 1.0);
    }
}
