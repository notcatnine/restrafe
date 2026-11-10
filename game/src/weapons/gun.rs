//! Port of `Gun.cs` (hitscan pistols/rifles/SMGs) + `Weapon.WeaponAnimation`/`OnShoot`.

use super::data::{HandAnchors, UField, WeaponDef};
use super::{CamQuery, Hand, Item, WeaponSound};
use crate::input::PlayerInput;
use crate::physics;
use crate::player::fpc::Fpc;
use crate::tween::Tweens;
use crate::unity::*;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

#[derive(bevy::ecs::system::SystemParam)]
pub struct HitCtx<'w, 's> {
    pub parts: Query<'w, 's, &'static crate::health::HitPart>,
    pub healths: Query<'w, 's, &'static mut crate::health::PlayerHealth>,
    pub ui: MessageWriter<'w, super::WeaponUi>,
    pub mine_hitboxes: Query<'w, 's, &'static super::mines::MineHitbox>,
    pub mine_shot: ResMut<'w, super::mines::PendingMineShots>,
    pub env_hits: ResMut<'w, crate::props::PendingEnvHits>,
    pub ragdoll_push: MessageWriter<'w, crate::ragdoll::RagdollPush>,
    pub layers: Query<'w, 's, &'static crate::physics::UnityLayer>,
    pub worn_hats: Query<'w, 's, (), With<crate::ragdoll::WornHat>>,
    pub hat_shot: MessageWriter<'w, crate::ragdoll::HatShot>,
}

impl HitCtx<'_, '_> {
    /// `Weapon.TriggerEnvironment(obj, ...)` for an environment ray hit: mines / claymores, PropDamage (barrels), Pot.
    pub fn trigger_environment(&mut self, e: Entity, direction: Vec3) {
        super::mines::trigger_environment_mine(e, &self.mine_hitboxes, &mut self.mine_shot);
        self.env_hits.0.push(e);
        // CompareTag("Hat"): an enemy's worn hat is knocked off.
        if self.worn_hats.contains(e) {
            self.hat_shot.write(crate::ragdoll::HatShot { collider: e, direction });
        }
    }

    /// `Weapon.TriggerEnvironment` Ragdoll branch (Weapon.cs:687): bodyImpact + bloodSplatter at LookRotation(normal) and an
    /// explosion impulse (ragdollEjectForce, origin hitPoint - direction) on the whole ragdoll.
    pub fn trigger_ragdoll(&mut self, e: Entity, w: &super::data::Fields, point: Vec3, normal: Vec3, direction: Vec3, fx: &mut MessageWriter<GunFx>) {
        if self.layers.get(e).map(|l| l.0) != Ok(crate::physics::layer::RAGDOLL) {
            return;
        }
        fx.write(GunFx::Spawn { prefab: w.asset("bodyImpact"), at: point, normal });
        fx.write(GunFx::Spawn { prefab: w.asset("bloodSplatter"), at: point, normal });
        self.ragdoll_push.write(crate::ragdoll::RagdollPush { body: e, origin: point - direction.normalize_or_zero(), force: w.f("ragdollEjectForce") });
    }
}

pub fn is_gun_class(class: &str) -> bool {
    class == "Gun" || class == "RepulsiveGun"
}

#[derive(Component, Default)]
pub struct GunState {
    pub fire_timer: f32,
    pub is_bursting: bool,
    burst_left: i32,
    burst_timer: f32,
    reload_timer: Option<f32>,
}

/// Training dummy health (`DollHealth.cs`, health 4, dies when health < 0).
#[derive(Component)]
pub struct DollHealth {
    pub health: f32,
}

/// Visual effects requested by weapons (rendered by `fx.rs`).
#[derive(Message, Clone, Debug)]
pub enum GunFx {
    Tracer { from: Vec3, to: Vec3, prefab: Option<String> },
    Impact { point: Vec3, normal: Vec3, hit: Option<Entity>, tag: String },
    Muzzle { at: Vec3, prefab: Option<String> },
    /// Instantiate an effect prefab (`GameObject/X.prefab`) at `at` with `Quaternion.LookRotation(normal)`
    /// (projectile `hitVfx` / `bloodVfx` / `explosionVfx`, mines).
    Spawn { prefab: Option<String>, at: Vec3, normal: Vec3 },
    /// `Instantiate(prefab, at, identity)` then `transform.Find("ball").localScale = ball` (PhysicsGrenade explosionVfx).
    SpawnBall { prefab: Option<String>, at: Vec3, ball: Vec3 },
    /// `Instantiate(prefab, at, LookRotation(normal), parent)` (skipped with reduceVFX): projectile surface impacts.
    SpawnOn { prefab: Option<String>, at: Vec3, normal: Vec3, parent: Option<Entity> },
    /// `Instantiate(prefab, item.position + offset, identity, item)` then localScale = scale, forward = item.up
    /// (grenade pin smoke; prefab layer Default), following the held item.
    Attached { prefab: Option<String>, item: Entity, offset: Vec3, scale: f32 },
    /// ChargeGun / BeamGun `tempBeam` lifecycle (the charging `muzzleFlash` ParticleSystem).
    Charge { item: Entity, op: ChargeOp },
}

/// Operations on a weapon's `tempBeam` (handle kept in [`ChargeVfx`] on the weapon entity).
#[derive(Clone, Debug)]
pub enum ChargeOp {
    /// `Instantiate(muzzleFlash, muzzleFlashPoint.position, shootPoint.rotation, parent)`; parent = the weapon
    /// (`ChargeGun`, base.transform) or `muzzleFlashPoint` (`BeamGun.ShootObserversEffect2`). Replaces `tempBeam`.
    Start { prefab: Option<String>, at: Vec3, rot: Quat, to_flash_point: bool },
    /// `BeamEffect(0)`: `tempBeam.transform.SetParent(null)` (the shot beam keeps playing in world space).
    Detach,
    /// `BeamEffect(1)`: `tempBeam.gameObject.SetActive(false)` (early release).
    Hide,
    /// `BeamGun.CancelChargeEffect`: `tempBeam.Stop(); Destroy(tempBeam.gameObject); tempBeam = null`.
    Cancel,
}

/// `tempBeam`: the weapon's current charging-effect root (set by `ChargeOp::Start`).
#[derive(Component, Clone, Copy)]
pub struct ChargeVfx(pub Entity);

/// Layers a bullet passes through (Ladder, Body, ShootThrough, DontCollideWithPlayer, InteractEnvironment, Glass).
pub fn pierces(layer: u32) -> bool {
    matches!(layer, 10 | 11 | 14 | 18 | 19 | 24)
}

#[allow(clippy::too_many_arguments)]
pub fn gun_update(
    time: Res<Time>,
    input: Res<PlayerInput>,
    anchors: Res<HandAnchors>,
    rapier: ReadRapierContext,
    mut players: Query<&mut Fpc>,
    mut cams: CamQuery,
    mut guns: Query<(Entity, &mut Item, &mut GunState, &mut Transform, Option<&mut Tweens>), Without<crate::player::fpc::PlayerCamera>>,
    layers: Query<&physics::UnityLayer>,
    tags: Query<&physics::UnityTag>,
    mut dolls: Query<&mut DollHealth>,
    mut hit: HitCtx,
    mut sounds: MessageWriter<WeaponSound>,
    mut fx: MessageWriter<GunFx>,
) {
    let dt = time.delta_secs();
    let Ok(ctx) = rapier.single() else { return };
    let Ok(mut fpc) = players.single_mut() else { return };
    let Ok((cam_tf, cam_g, mut cam_tw)) = cams.single_mut() else { return };
    let cam_tf = *cam_tf;
    let cam_g = *cam_g;
    let fire1 = input.fire.value();
    let fire2 = input.fire2.value();
    for (me, mut item, mut gs, mut tf, mut tweens) in &mut guns {
        let item = &mut *item;
        let def = item.def.clone();
        let w = &def.w;
        if item.hand == Hand::None {
            gs.is_bursting = false;
        }
        if gs.fire_timer > 0.0 {
            gs.fire_timer -= dt;
        }
        // Reload coroutine
        if let Some(t) = gs.reload_timer.as_mut() {
            *t -= dt;
            if *t <= 0.0 {
                gs.reload_timer = None;
                let charge = w.i("ammoCharge");
                if item.current_ammo - charge >= 0 {
                    item.current_ammo -= charge;
                    item.charged_bullets = charge as f32;
                } else {
                    item.charged_bullets = item.current_ammo as f32;
                    item.current_ammo = 0;
                }
                item.is_reloading = false;
            }
        }
        if item.hand == Hand::None || !input.enabled {
            continue;
        }
        let in_r = item.in_right();
        let in_l = item.in_left();
        let (f_r, f_l) = if item.invert_fire { (fire2, fire1) } else { (fire1, fire2) };
        let reload_weapon = w.b("reloadWeapon");
        let mut want_fire = 0;
        // Burst coroutine continuation
        if gs.is_bursting {
            gs.burst_timer -= dt;
            if gs.burst_timer <= 0.0 {
                if gs.burst_left > 0 && item.current_ammo > 0 {
                    gs.burst_left -= 1;
                    shoot_once(&def, me, item, &mut fpc, &cam_tf, &cam_g, &mut cam_tw, &mut tf, tweens.as_deref_mut(), &anchors, &ctx, &layers, &tags, &mut dolls, &mut hit, &mut sounds, &mut fx, true);
                    gs.burst_timer = w.f("timeBetweenBullets");
                } else {
                    gs.is_bursting = false;
                }
                if gs.burst_left <= 0 && gs.burst_timer <= 0.0 {
                    gs.is_bursting = false;
                }
            }
        }
        if !w.b("onePressShoot") {
            if f_r > 0.1 && in_r && (reload_weapon || item.current_ammo > 0) {
                want_fire += 1;
            }
            if f_l > 0.1 && in_l && (reload_weapon || item.current_ammo > 0) {
                want_fire += 1;
            }
            let ak = w.b("akAnim");
            fpc.cam_rotate_back = !((f_r > 0.1 && in_r && ak && item.current_ammo > 0) || (fire2 > 0.1 && in_l && ak && item.current_ammo > 0));
            if !reload_weapon && item.current_ammo <= 0 {
                if fire1 < 0.1 && in_r {
                    item.is_clicked = true;
                }
                if fire2 < 0.1 && in_l {
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
        for _ in 0..want_fire {
            fire(&def, me, item, &mut gs, &mut fpc, &cam_tf, &cam_g, &mut cam_tw, &mut tf, tweens.as_deref_mut(), &anchors, &ctx, &layers, &tags, &mut dolls, &mut hit, &mut sounds, &mut fx);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fire(
    def: &WeaponDef,
    me: Entity,
    item: &mut Item,
    gs: &mut GunState,
    fpc: &mut Fpc,
    cam_tf: &Transform,
    cam_g: &GlobalTransform,
    cam_tw: &mut Tweens,
    tf: &mut Transform,
    tweens: Option<&mut Tweens>,
    anchors: &HandAnchors,
    ctx: &RapierContext,
    layers: &Query<&physics::UnityLayer>,
    tags: &Query<&physics::UnityTag>,
    dolls: &mut Query<&mut DollHealth>,
    hit: &mut HitCtx,
    sounds: &mut MessageWriter<WeaponSound>,
    fx: &mut MessageWriter<GunFx>,
) {
    let w = &def.w;
    if !fpc.can_move || ((w.b("burstGun") || w.b("aimBurstGun")) && gs.is_bursting) {
        return;
    }
    // Gun.cs:145: pitch is re-rolled at the top of every Fire() (before the fireTimer / no-ammo checks),
    // not per BurstFire bullet.
    if def.class != "RepulsiveGun" && w.b("changePitchOnShoot") {
        item.audio_pitch = crate::unity::random_range_f(0.97, 1.03);
    }
    if gs.fire_timer > 0.0 {
        item.shot = true;
        return;
    }
    item.shot = false;
    gs.fire_timer = w.f("timeBetweenFire");
    if item.current_ammo <= 0 && item.charged_bullets <= 0.0 {
        if let Some(c) = w.asset("nobulletClip") {
            sounds.write(WeaponSound { clip: c, volume: item.audio_gain, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
        }
        item.no_ammo_clicks += 1; // PlayerPickup drops the weapon when > 1
    }
    let reload_weapon = w.b("reloadWeapon");
    if (!reload_weapon && item.current_ammo <= 0) || (reload_weapon && item.is_reloading) {
        return;
    }
    if reload_weapon && item.charged_bullets <= 0.0 && item.current_ammo > 0 {
        item.is_reloading = true;
        item.anim_triggers.push("Reload"); // animator.SetTrigger("Reload")
        if let Some(c) = w.asset("reloadClip") {
            sounds.write(WeaponSound { clip: c, volume: item.audio_gain, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
        }
        gs.reload_timer = Some(w.f("reloadTime"));
    }
    if reload_weapon && item.charged_bullets <= 0.0 && item.current_ammo <= 0 {
        if let Some(c) = w.asset("nobulletClip") {
            sounds.write(WeaponSound { clip: c, volume: item.audio_gain, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
        }
    }
    if reload_weapon && item.charged_bullets <= 0.0 {
        return;
    }
    if w.b("burstGun") || (w.b("aimBurstGun") && fpc.is_aiming) {
        // BurstFire coroutine: first shot now, the rest every timeBetweenBullets.
        // verify: StartCoroutine runs synchronously up to the first yield, so shot #1 fires in this same Fire() call.
        gs.is_bursting = true;
        gs.burst_left = w.i("bulletsAmount");
        gs.burst_timer = 0.0;
        if gs.burst_left > 0 && item.current_ammo > 0 {
            gs.burst_left -= 1;
            shoot_once(def, me, item, fpc, cam_tf, cam_g, cam_tw, tf, tweens, anchors, ctx, layers, tags, dolls, hit, sounds, fx, true);
            gs.burst_timer = w.f("timeBetweenBullets");
        } else {
            gs.is_bursting = false;
        }
        return;
    }
    shoot_once(def, me, item, fpc, cam_tf, cam_g, cam_tw, tf, tweens, anchors, ctx, layers, tags, dolls, hit, sounds, fx, false);
}

/// Spread + ShootServer + camera/weapon animation (shared by Fire and BurstFire).
#[allow(clippy::too_many_arguments)]
fn shoot_once(
    def: &WeaponDef,
    me: Entity,
    item: &mut Item,
    fpc: &mut Fpc,
    cam_tf: &Transform,
    cam_g: &GlobalTransform,
    cam_tw: &mut Tweens,
    tf: &mut Transform,
    tweens: Option<&mut Tweens>,
    anchors: &HandAnchors,
    ctx: &RapierContext,
    layers: &Query<&physics::UnityLayer>,
    tags: &Query<&physics::UnityTag>,
    dolls: &mut Query<&mut DollHealth>,
    hit: &mut HitCtx,
    sounds: &mut MessageWriter<WeaponSound>,
    fx: &mut MessageWriter<GunFx>,
    burst: bool,
) {
    let w = &def.w;
    if def.class == "RepulsiveGun" {
        repulsive_shoot(def, item, fpc, cam_tf, cam_g, cam_tw, tf, tweens, anchors, ctx, hit, sounds, fx);
        return;
    }
    let (min_s, max_s) = (w.f("minSpread"), w.f("maxSpread"));
    let stance = |acc: f32| lerp(max_s, min_s, acc);
    let num = if burst {
        if fpc.is_sprinting || !fpc.is_grounded || !fpc.safe_grounded { stance(w.f("sprintAccuracy")) }
        else if fpc.is_crouching { stance(w.f("standingAccuracy")) }
        else if fpc.is_walking { stance(w.f("walkAccuracy")) }
        else { stance(w.f("standingAccuracy")) }
    } else if w.b("ScopeAimWeapon") && !fpc.is_aiming {
        w.f("notAimingAccuracy")
    } else if fpc.is_sprinting || !fpc.safe_grounded {
        stance(w.f("sprintAccuracy"))
    } else if fpc.is_crouching {
        stance(w.f("standingAccuracy"))
    } else if fpc.is_walking {
        stance(w.f("walkAccuracy"))
    } else {
        stance(w.f("standingAccuracy"))
    };
    let cam_right = cam_g.right().as_vec3();
    let cam_up = cam_g.up().as_vec3();
    let mut spread = cam_right * random_range_f(-num, num) + cam_up * random_range_f(-num, num);
    if burst {
        spread *= w.f("additionalPrecision");
    }
    let origin = cam_g.translation();
    let dir = cam_g.forward().as_vec3() + spread;
    // ---- ShootServer --------------------------------------------------------------------------
    if w.b("needsAmmo") {
        if w.b("reloadWeapon") { item.charged_bullets -= 1.0 } else { item.current_ammo -= 1 }
    }
    if let Some(c) = w.asset("fireClip") {
        sounds.write(WeaponSound { clip: c, volume: item.audio_gain, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
    }
    fx.write(GunFx::Muzzle { at: item.flash_pos.unwrap_or(origin + dir.normalize() * 0.6), prefab: w.prefab_ref("muzzleFlash") });
    let shoot_point = tf.translation; // TODO: ShootPoint child once the model hierarchy is spawned
    let _ = shoot_point;
    for h in physics::raycast_all(ctx, origin, dir, f32::INFINITY, w.mask("defaultLayer")) {
        // Weapon.TriggerEnvironment: shot mines / claymores change state.
        hit.trigger_environment(h.entity, dir);
        hit.trigger_ragdoll(h.entity, w, h.point, h.normal, dir, fx);
        let tag = tags.get(h.entity).map(|t| t.0.clone()).unwrap_or_default();
        fx.write(GunFx::Tracer { from: item.shoot_pos.unwrap_or(origin - cam_up * 0.15 + cam_right * 0.2), to: h.point, prefab: w.asset("bulletTrailLocal") });
        let l = layers.get(h.entity).map(|l| l.0).unwrap_or(0);
        if l != physics::layer::RAGDOLL && tag != "ShatterableGlass" {
            fx.write(GunFx::Impact { point: h.point, normal: h.normal, hit: Some(h.entity), tag });
        }
        if !pierces(l) {
            break;
        }
    }
    // Player hit (Physics.Raycast on playerLayer; layer 11 = Body hit collider of a PlayerHealth).
    if let Some(h) = physics::raycast(ctx, origin, dir, f32::INFINITY, w.mask("playerLayer")) {
        let on_body = layers.get(h.entity).map(|l| l.0 == physics::layer::BODY).unwrap_or(false);
        if let (true, Ok(part)) = (on_body, hit.parts.get(h.entity)) {
            if let Ok(mut ph) = hit.healths.get_mut(part.owner) {
                let head = part.name == "Head_Col" || part.name == "Neck_1_Col";
                if std::env::var_os("RESTRAFE_DEBUG_HITS").is_some() {
                    println!("HIT {} head={head}", part.name);
                }
                let mut dmg = w.f("damage");
                if head {
                    dmg *= w.f("headMultiplier");
                }
                fx.write(GunFx::Impact { point: h.point, normal: h.normal, hit: Some(h.entity), tag: if head { "Head".into() } else { "Body".into() } });
                // ServerFX(hitInfo.point + direction, identity) -> ObserversFX: Instantiate(bloodSplatter).
                fx.write(GunFx::Spawn { prefab: w.asset("bloodSplatter"), at: h.point + dir, normal: Vec3::NEG_Z });
                hit.ui.write(super::WeaponUi::HitMarker { head: part.name == "Head_Col", source: Some(me) });
                if head {
                    if let Some(c) = w.asset("headHitClip") { sounds.write(WeaponSound { clip: c, volume: item.audio_gain, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None }); }
                }
                if let Some(c) = w.asset("bodyHitClip") { sounds.write(WeaponSound { clip: c, volume: item.audio_gain, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None }); }
                if ph.health - dmg <= 0.0 {
                    if let Some(c) = w.asset("deathClip") { sounds.write(WeaponSound { clip: c, volume: item.audio_gain, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None }); }
                    fpc.kill_shock_wave();
                    ph.health = -8.0; // KillServer
                    ph.last_hit = Some((dir.normalize_or_zero(), w.f("ragdollEjectForce"), h.point));
                } else {
                    ph.health -= dmg; // GiveDamage
                }
            }
        }
    }
    if let Some(h) = physics::raycast(ctx, origin, dir, f32::INFINITY, physics::DEFAULT_RAYCAST_LAYERS) {
        if let Ok(mut d) = dolls.get_mut(h.entity) {
            d.health -= w.f("damage");
        }
    }
    // ---- Camera + weapon animation -------------------------------------------------------------
    super::camera_recoil(def, fpc, cam_tf, cam_tw, 1.0);
    weapon_animation(def, item, fpc, tf, tweens, anchors);
}

/// Weapon.WeaponAnimation (viewmodel kick). The weapon is parented to FPArms here, so local
/// targets are expressed in FPArms space (anchor * weapon-local).
pub fn weapon_animation(def: &WeaponDef, item: &mut Item, fpc: &mut Fpc, tf: &mut Transform, tweens: Option<&mut Tweens>, anchors: &HandAnchors) {
    let w = &def.w;
    // OnShoot
    if w.b("fireSlowDown") {
        fpc.set_speed(w.f("fireSlowDownFactor"), w.f("fireSlowDownDuration"));
    }
    item.on_shoot_eject(); // OnShoot: ejectCaseVfx + caseSoundTimer
    if w.b("supplementAnimator") {
        item.anim_triggers.push("Shoot"); // animator.SetTrigger("Shoot")
    }
    let Some(tw) = tweens else { return };
    let anchor = {
        let ib = &def.item;
        let pick = |v: &Vec<Transform>, i: i32| v.get(i.max(0) as usize).copied().unwrap_or_default();
        if item.in_right() {
            if fpc.is_aiming && ib.b("aimWeapon") {
                if def.require_both_hands() { pick(&anchors.aim_both, ib.i("aimIndex")) } else { pick(&anchors.aim_right, ib.i("aimIndex")) }
            } else if def.require_both_hands() {
                pick(&anchors.both, ib.i("camChildIndex"))
            } else {
                pick(&anchors.right, ib.i("camChildIndex"))
            }
        } else {
            pick(&anchors.left, ib.i("camChildIndexLeftHand"))
        }
    };
    if w.b("instantComebackOnFire") {
        tw.kill();
        tf.translation = anchor.translation;
        tf.rotation = anchor.rotation;
    }
    // elbowPivot (ForeArm) tweens: only when not aiming; `markers.rs` applies them to the ElbowPivotPoint node.
    let fore = w.v3("foreArmAnimationPunch");
    let dur0 = w.f("animationDuration");
    let elbow_ok = !fpc.is_aiming;
    let horiz = w.b("horizontalAnimation");
    let both = def.require_both_hands();
    if w.b("instantPush") {
        if elbow_ok && (horiz || !both) {
            item.elbow_req = Some(super::markers::ElbowReq::Rotate { to: -fore, duration: dur0 });
        }
    } else if !w.b("holdback") && elbow_ok && (horiz || !both) {
        item.elbow_req = Some(super::markers::ElbowReq::Punch { punch: -fore, duration: dur0, vibrato: w.i("animationVibrato"), elasticity: w.f("animationElasticity") });
    }
    let p = w.v3("animationPunch");
    let p_pos = Vec3::new(p.x, p.y, -p.z); // Unity local offset -> Bevy
    let dur = w.f("animationDuration");
    let (vib, ela) = (w.i("animationVibrato"), w.f("animationElasticity"));
    if w.b("holdback") {
        tf.translation -= p_pos;
        let fwd = tf.forward().as_vec3();
        tw.local_move(tf.translation - fwd * 0.01, dur, crate::tween::Ease::OutQuad);
    } else if w.b("instantPush") {
        if w.b("horizontalAnimation") {
            tw.local_move(tf.translation - p_pos, dur, crate::tween::Ease::OutQuad);
        } else if def.require_both_hands() {
            // fpArms.DOLocalRotate(fpArms.localEulerAngles - animationPunch, animationDuration)
            fpc.fparms_anim = Some(super::FpArmsAnim::RotateBy { punch: p, duration: dur });
        } else {
            let e = to_unity_euler(anchor.rotation) - p;
            tw.local_rotate(e, dur, crate::tween::Ease::OutQuad);
        }
    } else if w.b("horizontalAnimation") {
        tw.punch_position(-p_pos, dur, vib, ela);
    } else if def.require_both_hands() {
        // fpArms.DOPunchRotation(-animationPunch, animationDuration, animationVibrato, animationElasticity)
        fpc.fparms_anim = Some(super::FpArmsAnim::Punch { punch: -p, duration: dur, vibrato: vib, elasticity: ela });
    } else {
        tw.punch_rotation(-p, dur, vib, ela);
    }
}

/// RepulsiveGun.ShootServer + the tail of RepulsiveGun.Fire. The overlap box only affects other
/// players' PlayerHealth (bounce), so in single-player only the shooter-side effects remain.
#[allow(clippy::too_many_arguments)]
fn repulsive_shoot(
    def: &WeaponDef,
    item: &mut Item,
    fpc: &mut Fpc,
    cam_tf: &Transform,
    cam_g: &GlobalTransform,
    cam_tw: &mut Tweens,
    tf: &mut Transform,
    tweens: Option<&mut Tweens>,
    anchors: &HandAnchors,
    ctx: &RapierContext,
    hit: &mut HitCtx,
    sounds: &mut MessageWriter<WeaponSound>,
    fx: &mut MessageWriter<GunFx>,
) {
    let w = &def.w;
    if let Some(c) = w.asset("fireClip") {
        sounds.write(WeaponSound { clip: c, volume: item.audio_gain, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
    }
    let fwd = cam_g.forward().as_vec3();
    fx.write(GunFx::Muzzle { at: item.flash_pos.unwrap_or(cam_g.translation() + fwd * 0.6), prefab: w.prefab_ref("muzzleFlash") });
    // ShootServer: OverlapBox(pos + dir * box.z/2, box (half extents), LookRotation(dir), playerLayer);
    // first collider under a PlayerHealth -> BumpPlayer(dir + up*2, repulseForce) -> PlayerHealth.AddForce.
    let bx = w.v3("boxdimensions");
    let pos = cam_g.translation();
    let rot = Transform::IDENTITY.looking_to(fwd, Vec3::Y).rotation;
    for e in physics::overlap_box(ctx, pos + fwd * (bx.z / 2.0), bx, rot, w.mask("playerLayer")) {
        if let Ok(part) = hit.parts.get(e) {
            if let Ok(mut ph) = hit.healths.get_mut(part.owner) {
                ph.add_force(fwd + Vec3::Y * 2.0, w.f("repulseForce"));
            }
            break;
        }
    }
    super::camera_recoil(def, fpc, cam_tf, cam_tw, 1.0);
    weapon_animation(def, item, fpc, tf, tweens, anchors);
    let kb = w.f("playerKnockback");
    if kb != 0.0 {
        fpc.add_force(-fwd, kb);
    }
}
