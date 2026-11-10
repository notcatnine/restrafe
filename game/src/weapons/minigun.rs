//! Port of `Minigun.cs` (spin-up hitscan gun; Minigun prefab). Networking removed: the
//! `RpcLogic___*` bodies run directly.

use super::data::{HandAnchors, UField, WeaponDef};
use super::gun::{pierces, weapon_animation, GunFx, HitCtx};
use super::shotgun::Lookups;
use super::{CamQuery, Hand, Item, WeaponSound, WeaponSoundStop};
use crate::input::PlayerInput;
use crate::physics;
use crate::player::fpc::Fpc;
use crate::tween::Tweens;
use crate::unity::*;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub fn is_minigun(class: &str) -> bool {
    class == "Minigun"
}

#[derive(Component)]
pub struct MinigunState {
    pub fire_timer: f32,
    /// `shootTimer` (spin-up, 0..timeBeforeShooting).
    pub shoot_timer: f32,
    pressed: bool,
    released: bool,
    noammo_bool: bool,
    reload_timer: Option<f32>,
    /// Accumulated `rotativePart.Rotate(0, 0, ..)` in degrees about the barrel's local Z.
    /// TODO: the integrator applies this to the SM_Minigun_barrel child once the model hierarchy exists.
    pub barrel_angle: f32,
    /// `audio.pitch` (changePitchOnShoot re-rolls it 0.97..1.03 every shooting frame; all clips play on `audio`).
    pub pitch: f32,
}

impl Default for MinigunState {
    fn default() -> Self {
        // C# field initialisers: pressed = true, released = true.
        MinigunState { fire_timer: 0.0, shoot_timer: 0.0, pressed: true, released: true, noammo_bool: false, reload_timer: None, barrel_angle: 0.0, pitch: 1.0 }
    }
}

struct Env<'a, 'lw, 'ls, 'hw, 'hs, 'sw, 'fw> {
    fpc: &'a mut Fpc,
    cam_tf: &'a Transform,
    cam_g: &'a GlobalTransform,
    cam_tw: &'a mut Tweens,
    anchors: &'a HandAnchors,
    ctx: &'a RapierContext<'a>,
    lk: &'a mut Lookups<'lw, 'ls>,
    hit: &'a mut HitCtx<'hw, 'hs>,
    sounds: &'a mut MessageWriter<'sw, WeaponSound>,
    fx: &'a mut MessageWriter<'fw, GunFx>,
    /// The Minigun entity (its `audio` source: every clip below plays on it).
    src: Entity,
}

fn play(sounds: &mut MessageWriter<WeaponSound>, def: &WeaponDef, key: &str, pitch: f32, src: Entity) {
    if let Some(c) = def.w.asset(key) {
        sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch, linear_max: 0.0, source: Some(src), rolloff: None });
    }
}

/// AudioStop -> RpcLogic: networkAudioSource.Stop() + PlayReleaseClipObservers. `networkAudioSource` wraps the
/// same AudioSource as `audio`, so Stop() also cuts the one-shots still ringing on it (fireClip tails, hit clips).
fn audio_stop(sounds: &mut MessageWriter<WeaponSound>, stops: &mut MessageWriter<WeaponSoundStop>, def: &WeaponDef, item: &mut Item, pitch: f32, src: Entity) {
    item.src_audio = Some(false);
    stops.write(WeaponSoundStop { source: src });
    play(sounds, def, "releaseClip", pitch, src);
}

#[allow(clippy::too_many_arguments)]
pub fn minigun_update(
    time: Res<Time>,
    input: Res<PlayerInput>,
    anchors: Res<HandAnchors>,
    rapier: ReadRapierContext,
    mut players: Query<&mut Fpc>,
    mut cams: CamQuery,
    mut guns: Query<(Entity, &mut Item, &mut MinigunState, &mut Transform, Option<&mut Tweens>), Without<crate::player::fpc::PlayerCamera>>,
    mut lk: Lookups,
    mut hit: HitCtx,
    mut sounds: MessageWriter<WeaponSound>,
    mut stops: MessageWriter<WeaponSoundStop>,
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
    for (gun, mut item, mut ms, mut tf, mut tweens) in &mut guns {
        let item = &mut *item;
        let def = item.def.clone();
        let w = &def.w;
        // WeaponUpdate() runs in pickup.rs (item_behaviour_update).
        if ms.fire_timer > 0.0 {
            ms.fire_timer -= dt;
        }
        // Reload coroutine tail (reloadWeapon is false on the Minigun prefab; kept for fidelity).
        if let Some(t) = ms.reload_timer.as_mut() {
            *t -= dt;
            if *t <= 0.0 {
                ms.reload_timer = None;
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
        // gameObject.layer == 7 (world item) -> return
        if item.hand == Hand::None || !input.enabled {
            continue;
        }
        let in_r = item.in_right();
        let in_l = item.in_left();
        let f_r = if item.invert_fire { fire2 } else { fire1 };
        if item.current_ammo <= 0 && ms.noammo_bool {
            audio_stop(&mut sounds, &mut stops, &def, item, ms.pitch, gun);
            ms.noammo_bool = false;
        }
        if item.current_ammo > 0 {
            ms.noammo_bool = true;
        }
        if f_r > 0.1 && in_r && item.current_ammo > 0 {
            ms.shoot_timer += dt;
            ms.released = false;
        } else if fire2 > 0.1 && in_l && item.current_ammo > 0 {
            ms.shoot_timer += dt;
            ms.released = false;
        } else {
            ms.shoot_timer -= dt;
            ms.pressed = false;
        }
        if f_r > 0.1 && in_r && !ms.pressed && item.current_ammo > 0 {
            ms.pressed = true;
            item.src_audio = Some(true); // AudioPlay -> networkAudioSource.Play() (MinigunStart)
        }
        if f_r < 0.1 && in_r && !ms.released {
            ms.released = true;
            if item.current_ammo > 0 {
                audio_stop(&mut sounds, &mut stops, &def, item, ms.pitch, gun);
            }
        }
        let tbs = w.f("timeBeforeShooting");
        ms.shoot_timer = ms.shoot_timer.clamp(0.0, tbs);
        let reload_weapon = w.b("reloadWeapon");
        let spun = ms.shoot_timer == tbs;
        let want = (f_r > 0.1 && in_r && (reload_weapon || item.current_ammo > 0) && spun)
            || (fire2 > 0.1 && in_l && (reload_weapon || item.current_ammo > 0) && spun);
        if want {
            let mut env = Env {
                fpc: &mut fpc,
                cam_tf: &cam_tf,
                cam_g: &cam_g,
                cam_tw: &mut cam_tw,
                anchors: &anchors,
                ctx: &ctx,
                lk: &mut lk,
                hit: &mut hit,
                sounds: &mut sounds,
                fx: &mut fx,
                src: gun,
            };
            fire(&def, item, &mut ms, &mut env, &mut tf, tweens.as_deref_mut());
        }
        // rotativePart.Rotate(0, 0, Lerp(0, rotationSpeed * dt, shootTimer / timeBeforeShooting))
        ms.barrel_angle += lerp(0.0, w.f("rotationSpeed") * dt, ms.shoot_timer / tbs);
    }
}

fn fire(def: &WeaponDef, item: &mut Item, ms: &mut MinigunState, env: &mut Env, tf: &mut Transform, tweens: Option<&mut Tweens>) {
    let w = &def.w;
    if !env.fpc.can_move {
        return;
    }
    if w.b("changePitchOnShoot") {
        ms.pitch = crate::unity::random_range_f(0.97, 1.03);
    }
    if ms.fire_timer > 0.0 {
        item.shot = true;
        return;
    }
    item.shot = false;
    ms.fire_timer = w.f("timeBetweenFire");
    let reload_weapon = w.b("reloadWeapon");
    if item.current_ammo <= 0 {
        play(env.sounds, def, "nobulletClip", ms.pitch, env.src);
        item.no_ammo_clicks += 1; // PlayerPickup drops the weapon when > 1
    }
    if (!reload_weapon && item.current_ammo <= 0) || (reload_weapon && item.is_reloading) {
        return;
    }
    if reload_weapon && item.charged_bullets <= 0.0 && item.current_ammo > 0 {
        item.is_reloading = true;
        item.anim_triggers.push("Reload"); // animator.SetTrigger("Reload")
        play(env.sounds, def, "reloadClip", ms.pitch, env.src);
        ms.reload_timer = Some(w.f("reloadTime"));
    }
    if reload_weapon && item.charged_bullets <= 0.0 && item.current_ammo <= 0 {
        play(env.sounds, def, "nobulletClip", ms.pitch, env.src);
    }
    if reload_weapon && item.charged_bullets <= 0.0 {
        return;
    }
    if w.b("burstGun") {
        // TODO BurstFire coroutine (bulletsAmount shots every timeBetweenBullets); burstGun is false on the Minigun prefab.
        return;
    }
    let (min_s, max_s) = (w.f("minSpread"), w.f("maxSpread"));
    let stance = |acc: f32| lerp(max_s, min_s, acc);
    let fpc = &*env.fpc;
    let num = if w.b("ScopeAimWeapon") && !fpc.is_aiming {
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
    let cam_right = env.cam_g.right().as_vec3();
    let cam_up = env.cam_g.up().as_vec3();
    // Random.Range(num, -num)
    let spread = cam_right * random_range_f(num, -num) + cam_up * random_range_f(num, -num);
    let origin = env.cam_g.translation();
    let dir = env.cam_g.forward().as_vec3() + spread;
    shoot_server(def, item, env, w.f("damage"), origin, dir, ms.pitch);
    // revolverShake ? CameraRevolverAnimation : CameraAnimation (both in camera_recoil)
    super::camera_recoil(def, env.fpc, env.cam_tf, env.cam_tw, 1.0);
    weapon_animation(def, item, env.fpc, tf, tweens, env.anchors);
}

/// `ShootServer` (RemoveAmmo, ShootServerEffect, trails/impacts, player hit, doll hit).
fn shoot_server(def: &WeaponDef, item: &mut Item, env: &mut Env, damage: f32, position: Vec3, direction: Vec3, pitch: f32) {
    let w = &def.w;
    // RemoveAmmo (unconditional)
    if w.b("reloadWeapon") {
        item.charged_bullets -= 1.0;
    } else {
        item.current_ammo -= 1;
    }
    // ShootServerEffect -> ShootObserversEffect: fireClip + muzzle flash
    play(env.sounds, def, "fireClip", pitch, env.src);
    env.fx.write(GunFx::Muzzle { at: item.flash_pos.unwrap_or(position + direction.normalize() * 0.6), prefab: w.prefab_ref("muzzleFlash") });
    let cam_right = env.cam_g.right().as_vec3();
    let cam_up = env.cam_g.up().as_vec3();
    for h in physics::raycast_all(env.ctx, position, direction, f32::INFINITY, w.mask("defaultLayer")) {
        env.hit.trigger_environment(h.entity, direction);
        env.hit.trigger_ragdoll(h.entity, w, h.point, h.normal, direction, &mut env.fx);
        let tag = env.lk.tags.get(h.entity).map(|t| t.0.clone()).unwrap_or_default();
        env.fx.write(GunFx::Tracer { from: item.shoot_pos.unwrap_or(position - cam_up * 0.15 + cam_right * 0.2), to: h.point, prefab: w.asset("bulletTrailLocal") });
        let l = env.lk.layers.get(h.entity).map(|l| l.0).unwrap_or(0);
        if tag == "ShatterableGlass" {
            // TODO BreakGlassServer
        } else if l != physics::layer::RAGDOLL {
            env.fx.write(GunFx::Impact { point: h.point, normal: h.normal, hit: Some(h.entity), tag });
        }
        if !pierces(l) {
            break;
        }
    }
    // Player hit (layer 11 = Body hit collider of a PlayerHealth).
    if let Some(h) = physics::raycast(env.ctx, position, direction, f32::INFINITY, w.mask("playerLayer")) {
        let on_body = env.lk.layers.get(h.entity).map(|l| l.0 == physics::layer::BODY).unwrap_or(false);
        if let (true, Ok(part)) = (on_body, env.hit.parts.get(h.entity)) {
            if let Ok(mut ph) = env.hit.healths.get_mut(part.owner) {
                let head = part.name == "Head_Col" || part.name == "Neck_1_Col";
                let mut dmg = damage;
                if head {
                    dmg *= w.f("headMultiplier");
                }
                env.fx.write(GunFx::Impact { point: h.point, normal: h.normal, hit: Some(h.entity), tag: if head { "Head".into() } else { "Body".into() } });
                // ServerFX(hitInfo.point + direction, identity) -> Instantiate(bloodSplatter).
                env.fx.write(GunFx::Spawn { prefab: w.asset("bloodSplatter"), at: h.point + direction, normal: Vec3::NEG_Z });
                env.hit.ui.write(super::WeaponUi::HitMarker { head: part.name == "Head_Col", source: Some(env.src) });
                if head {
                    play(env.sounds, def, "headHitClip", pitch, env.src);
                }
                play(env.sounds, def, "bodyHitClip", pitch, env.src);
                if ph.health - dmg <= 0.0 {
                    play(env.sounds, def, "deathClip", pitch, env.src);
                    env.fpc.kill_shock_wave();
                    ph.health = -8.0; // KillServer
                    ph.last_hit = Some((direction.normalize_or_zero(), w.f("ragdollEjectForce"), h.point));
                } else {
                    ph.health -= dmg; // GiveDamage
                }
            }
        }
    }
    // TODO suppression ray (supLayer, layer 17) -- no AI to suppress.
    if let Some(h) = physics::raycast(env.ctx, position, direction, f32::INFINITY, physics::DEFAULT_RAYCAST_LAYERS) {
        if let Ok(mut d) = env.lk.dolls.get_mut(h.entity) {
            d.health -= damage;
        }
    }
}
