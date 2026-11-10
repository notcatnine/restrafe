//! Port of `ChargeGun.cs` (hold to charge, release at full charge for a piercing beam; HandCanon prefab).
//! Networking removed: the `RpcLogic___*` bodies run directly.

use super::data::{HandAnchors, UField, WeaponDef};
use super::gun::{ChargeOp, pierces, weapon_animation, GunFx, HitCtx};
use super::shotgun::Lookups;
use super::{CamQuery, Hand, Item, WeaponSound};
use crate::input::PlayerInput;
use crate::physics;
use crate::player::fpc::Fpc;
use crate::tween::Tweens;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub fn is_charge_gun(class: &str) -> bool {
    class == "ChargeGun"
}

#[derive(Component, Default)]
pub struct ChargeState {
    pub fire_timer: f32,
    /// `accumulatedPower` (seconds held, 0..maxChargeTime).
    pub accumulated_power: f32,
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
    /// The weapon entity (owner of `tempBeam`, see `ChargeVfx`).
    me: Entity,
}

fn play(sounds: &mut MessageWriter<WeaponSound>, def: &WeaponDef, key: &str) {
    if let Some(c) = def.w.asset(key) {
        sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
    }
}

#[allow(clippy::too_many_arguments)]
pub fn charge_update(
    time: Res<Time>,
    input: Res<PlayerInput>,
    anchors: Res<HandAnchors>,
    rapier: ReadRapierContext,
    mut players: Query<&mut Fpc>,
    mut cams: CamQuery,
    mut guns: Query<(Entity, &mut Item, &mut ChargeState, &mut Transform, Option<&mut Tweens>), Without<crate::player::fpc::PlayerCamera>>,
    mut lk: Lookups,
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
    for (me, mut item, mut cs, mut tf, mut tweens) in &mut guns {
        let item = &mut *item;
        let def = item.def.clone();
        let w = &def.w;
        // WeaponUpdate() runs in pickup.rs (item_behaviour_update).
        if cs.fire_timer > 0.0 {
            cs.fire_timer -= dt;
        }
        if item.hand == Hand::None {
            // gameObject.layer == 7: accumulatedPower = 0; AudioStop()
            item.src_audio = Some(false);
            cs.accumulated_power = 0.0;
            continue;
        }
        if !input.enabled {
            continue; // PauseManager.pause
        }
        let in_r = item.in_right();
        let in_l = item.in_left();
        let (f_r, f_l) = if item.invert_fire { (fire2, fire1) } else { (fire1, fire2) };
        let max_charge = w.f("maxChargeTime");
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
            me,
        };
        if (f_r > 0.1 && in_r && !item.is_clicked && cs.fire_timer <= 0.0) || (f_l > 0.1 && in_l && !item.is_clicked && cs.fire_timer <= 0.0) {
            // ShootServerEffect (charging beam VFX at the muzzle, parented to the weapon) + AudioPlay (networkAudioSource.Play).
            item.src_audio = Some(true);
            let fwd = env.cam_g.forward().as_vec3();
            let (at, rot) = (item.flash_pos.unwrap_or(env.cam_g.translation() + fwd * 0.6), item.shoot_rot.unwrap_or(env.cam_g.rotation()));
            env.fx.write(GunFx::Charge { item: me, op: ChargeOp::Start { prefab: w.asset("muzzleFlash"), at, rot, to_flash_point: false } });
        }
        let shake = |cs: &ChargeState, env: &mut Env| {
            // cam.DOShakeRotation(duration, strength * (acc / maxChargeTime), vibrato, randomness, fadeOut, randomnessMode).SetEase(shakeEase)
            env.cam_tw.shake_rotation(
                w.f("duration"),
                w.v3("strength") * (cs.accumulated_power / max_charge),
                w.i("vibrato"),
                w.f("randomness"),
                w.b("fadeOut"),
                w.i("randomnessMode") == 1,
            ); // .SetEase(shakeEase) is overridden by DOTween's SetShake (Linear)
        };
        if f_r > 0.1 && in_r && cs.fire_timer <= 0.0 {
            cs.accumulated_power += dt;
            shake(&cs, &mut env);
            item.is_clicked = true;
        } else if f_l > 0.1 && in_l && cs.fire_timer <= 0.0 {
            cs.accumulated_power += dt;
            shake(&cs, &mut env);
            item.is_clicked = true;
        }
        env.fpc.cam_rotate_back = true; // camAnimScript.rotateBack = true
        if item.is_clicked && in_r && cs.accumulated_power >= max_charge {
            item.is_clicked = false;
            fire(&def, item, &mut cs, &mut env, &mut tf, tweens.as_deref_mut());
        } else if f_r < 0.1 && item.is_clicked && in_r {
            item.is_clicked = false;
            fire(&def, item, &mut cs, &mut env, &mut tf, tweens.as_deref_mut());
        } else if f_l < 0.1 && item.is_clicked && in_l {
            item.is_clicked = false;
            fire(&def, item, &mut cs, &mut env, &mut tf, tweens.as_deref_mut());
        }
    }
}

fn fire(def: &WeaponDef, item: &mut Item, cs: &mut ChargeState, env: &mut Env, tf: &mut Transform, tweens: Option<&mut Tweens>) {
    let w = &def.w;
    if std::env::var_os("RESTRAFE_FX_LOG").is_some() {
        println!("CHARGEFIRE acc={:.3} max={} timer={:.3} ammo={} -> {}", cs.accumulated_power, w.f("maxChargeTime"), cs.fire_timer, item.current_ammo,
            if cs.fire_timer > 0.0 { "shot-flag" } else if cs.accumulated_power < w.f("maxChargeTime") { "cancel" } else { "FIRE" });
    }
    item.src_audio = Some(false); // AudioStop()
    if cs.fire_timer > 0.0 {
        item.shot = true;
        return;
    }
    if cs.accumulated_power < w.f("maxChargeTime") {
        // BeamEffectServer(1): tempBeam.SetActive(false)
        env.fx.write(GunFx::Charge { item: env.me, op: ChargeOp::Hide });
        cs.accumulated_power = 0.0;
        return;
    }
    cs.accumulated_power = 0.0;
    item.shot = false;
    cs.fire_timer = w.f("timeBetweenFire");
    let origin = env.cam_g.translation();
    let fwd = env.cam_g.forward().as_vec3();
    shoot_server(def, item, env, w.f("damage"), origin, fwd);
    // revolverShake ? CameraRevolverAnimation : CameraAnimation (both in camera_recoil)
    super::camera_recoil(def, env.fpc, env.cam_tf, env.cam_tw, 1.0);
    weapon_animation(def, item, env.fpc, tf, tweens, env.anchors);
    let kb = w.f("playerKnockback"); // prefab value (20), not the C# default
    if kb != 0.0 {
        env.fpc.add_force(-fwd, kb);
    }
}

/// `ShootServer`: BeamEffectServer(0) (detach tempBeam), SoundServerEffect, RemoveAmmo, raycast trails,
/// sphere-cast player hit (doubled head damage), doll hit.
fn shoot_server(def: &WeaponDef, item: &mut Item, env: &mut Env, damage: f32, position: Vec3, direction: Vec3) {
    let w = &def.w;
    env.fx.write(GunFx::Charge { item: env.me, op: ChargeOp::Detach }); // BeamEffectServer(0)
    play(env.sounds, def, "fireClip"); // SoundObserversEffect: newSource.PlayOneShot(fireClip)
    // RemoveAmmo (unconditional)
    if w.b("reloadWeapon") {
        item.charged_bullets -= 1.0;
    } else {
        item.current_ammo -= 1;
    }
    let cam_right = env.cam_g.right().as_vec3();
    let cam_up = env.cam_g.up().as_vec3();
    for h in physics::raycast_all(env.ctx, position, direction, f32::INFINITY, w.mask("defaultLayer")) {
        let tag = env.lk.tags.get(h.entity).map(|t| t.0.clone()).unwrap_or_default();
        env.hit.trigger_environment(h.entity, direction); // TriggerEnvironment
        env.hit.trigger_ragdoll(h.entity, w, h.point, h.normal, direction, &mut env.fx);
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
    // Physics.CapsuleCast(position, position, radius, forward, playerLayer): a sphere sweep that skips start overlaps; radius from the prefab (1.45).
    // damageToGive is doubled in place on a head hit, so the DollHealth ray below takes the doubled value too.
    let mut damage = damage;
    if let Some(h) = physics::sphere_sweep_unity(env.ctx, position, w.f("radius"), env.cam_g.forward().as_vec3(), f32::INFINITY, w.mask("playerLayer")) {
        let on_body = env.lk.layers.get(h.entity).map(|l| l.0 == physics::layer::BODY).unwrap_or(false);
        if let (true, Ok(part)) = (on_body, env.hit.parts.get(h.entity)) {
            if let Ok(mut ph) = env.hit.healths.get_mut(part.owner) {
                let head = part.name == "Head_Col";
                if head {
                    damage *= 2.0; // hard-coded x2 in ChargeGun
                }
                let dmg = damage;
                env.fx.write(GunFx::Impact { point: h.point, normal: h.normal, hit: Some(h.entity), tag: "Body".into() });
                // ServerFX(hitInfo.point + direction, identity) -> Instantiate(bloodSplatter).
                env.fx.write(GunFx::Spawn { prefab: w.asset("bloodSplatter"), at: h.point + env.cam_g.forward().as_vec3(), normal: Vec3::NEG_Z });
                env.hit.ui.write(super::WeaponUi::HitMarker { head, source: Some(env.me) });
                if head {
                    play(env.sounds, def, "headHitClip");
                }
                play(env.sounds, def, "bodyHitClip");
                if ph.health - dmg <= 0.0 {
                    play(env.sounds, def, "deathClip");
                    env.fpc.kill_shock_wave();
                    ph.health = -8.0; // KillServer
                    ph.last_hit = Some((env.cam_g.forward().as_vec3(), w.f("ragdollEjectForce"), h.point));
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
