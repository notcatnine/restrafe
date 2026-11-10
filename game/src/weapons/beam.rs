//! Port of `BeamGun.cs` (BeamLoad: tap = small projectile blast, hold = charged piercing beam).
//! Networking removed: the `RpcLogic___*` bodies run directly.

use super::data::{HandAnchors, UField, WeaponDef};
use super::gun::{ChargeOp, weapon_animation, GunFx, HitCtx};
use super::shotgun::Lookups;
use super::{CamQuery, Hand, Item, WeaponSound};
use crate::input::PlayerInput;
use crate::physics;
use crate::player::fpc::Fpc;
use crate::tween::Tweens;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub fn is_beam_gun(class: &str) -> bool {
    class == "BeamGun"
}

#[derive(Component)]
pub struct BeamState {
    pub fire_timer: f32,
    /// `accumulatedPower` (seconds held, 0..maxChargeTime).
    pub accumulated_power: f32,
    is_clicked2: bool,
    is_clicked3: bool,
    is_clicked4: bool,
    last_in_right: bool,
    last_in_left: bool,
}

impl Default for BeamState {
    fn default() -> Self {
        // C# field initialisers: isClicked3 = true.
        BeamState { fire_timer: 0.0, accumulated_power: 0.0, is_clicked2: false, is_clicked3: true, is_clicked4: false, last_in_right: false, last_in_left: false }
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
    /// The weapon entity (owner of `tempBeam`, see `ChargeVfx`).
    me: Entity,
}

fn play(sounds: &mut MessageWriter<WeaponSound>, def: &WeaponDef, key: &str, pitch: f32) {
    if let Some(c) = def.w.asset(key) {
        sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch, linear_max: 0.0, source: None, rolloff: None });
    }
}

/// BeamGun's pierce list: like `gun::pierces` but without layer 10 (Ladder).
fn beam_pierces(layer: u32) -> bool {
    matches!(layer, 11 | 14 | 18 | 19 | 24)
}

#[allow(clippy::too_many_arguments)]
pub fn beam_update(
    time: Res<Time>,
    input: Res<PlayerInput>,
    anchors: Res<HandAnchors>,
    rapier: ReadRapierContext,
    mut commands: Commands,
    lassets: Res<super::launcher::LauncherAssets>,
    mut players: Query<(&mut Fpc, &Transform), Without<Item>>,
    mut cams: CamQuery,
    mut guns: Query<(Entity, &mut Item, &mut BeamState, &mut Transform, Option<&mut Tweens>, Has<super::gun::ChargeVfx>), Without<crate::player::fpc::PlayerCamera>>,
    mut lk: Lookups,
    mut hit: HitCtx,
    mut sounds: MessageWriter<WeaponSound>,
    mut fx: MessageWriter<GunFx>,
) {
    let dt = time.delta_secs();
    let Ok(ctx) = rapier.single() else { return };
    let Ok((mut fpc, root_tf)) = players.single_mut() else { return };
    let root_tf = *root_tf;
    let Ok((cam_tf, cam_g, mut cam_tw)) = cams.single_mut() else { return };
    let cam_tf = *cam_tf;
    let cam_g = *cam_g;
    let fire1 = input.fire.value();
    let fire2 = input.fire2.value();
    for (me, mut item, mut bs, mut tf, mut tweens, has_beam) in &mut guns {
        let item = &mut *item;
        let def = item.def.clone();
        let w = &def.w;
        // WeaponUpdate() runs in pickup.rs (item_behaviour_update).
        if bs.fire_timer > 0.0 {
            bs.fire_timer -= dt;
        }
        if item.hand == Hand::None {
            // gameObject.layer == 7: accumulatedPower = 0; AudioStop(); CancelChargeEffect()
            item.src_audio = Some(false);
            if has_beam {
                fx.write(GunFx::Charge { item: me, op: ChargeOp::Cancel });
            }
            bs.accumulated_power = 0.0;
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
        if (bs.last_in_right && !in_r) || (bs.last_in_left && !in_l) {
            bs.accumulated_power = 0.0;
            item.is_clicked = false;
            bs.is_clicked2 = false;
            bs.is_clicked3 = true;
            bs.is_clicked4 = false;
            // AudioStop(); CancelChargeEffect()
            item.src_audio = Some(false);
            env.fx.write(GunFx::Charge { item: me, op: ChargeOp::Cancel });
        }
        bs.last_in_right = in_r;
        bs.last_in_left = in_l;
        if f_r > 0.1 && in_r {
            bs.is_clicked2 = true;
        }
        if f_l > 0.1 && in_l {
            bs.is_clicked2 = true;
        }
        if bs.is_clicked2 && bs.accumulated_power < 0.3 && bs.is_clicked4 {
            if f_r < 0.1 && in_r && bs.is_clicked3 {
                fire_blast(&def, item, &mut bs, &mut env, &mut tf, tweens.as_deref_mut(), &root_tf, &lassets, &mut commands);
                bs.is_clicked2 = false;
            }
            if f_l < 0.1 && in_l && bs.is_clicked3 {
                fire_blast(&def, item, &mut bs, &mut env, &mut tf, tweens.as_deref_mut(), &root_tf, &lassets, &mut commands);
                bs.is_clicked2 = false;
            }
        }
        if (f_r > 0.1 && in_r && !item.is_clicked && bs.is_clicked3 && bs.fire_timer <= 0.0)
            || (f_l > 0.1 && in_l && !item.is_clicked && bs.is_clicked3 && bs.fire_timer <= 0.0)
        {
            // ShootServerEffect (charging beam VFX parented to the muzzle) + AudioPlay (silent on BeamLoad: first AudioSource clip is null)
            item.src_audio = Some(true);
            let fwd = env.cam_g.forward().as_vec3();
            let (at, rot) = (item.flash_pos.unwrap_or(env.cam_g.translation() + fwd * 0.6), item.shoot_rot.unwrap_or(env.cam_g.rotation()));
            env.fx.write(GunFx::Charge { item: me, op: ChargeOp::Start { prefab: w.asset("muzzleFlash"), at, rot, to_flash_point: true } });
            bs.is_clicked4 = true;
        }
        let shake = |bs: &BeamState, env: &mut Env| {
            // cam.DOShakeRotation(duration, strength * (acc / maxChargeTime), vibrato, randomness, fadeOut, randomnessMode).SetEase(shakeEase)
            env.cam_tw.shake_rotation(
                w.f("duration"),
                w.v3("strength") * (bs.accumulated_power / max_charge),
                w.i("vibrato"),
                w.f("randomness"),
                w.b("fadeOut"),
                w.i("randomnessMode") == 1,
            ); // .SetEase(shakeEase) is overridden by DOTween's SetShake (Linear)
        };
        if f_r > 0.1 && in_r && bs.fire_timer <= 0.0 && bs.is_clicked3 {
            bs.accumulated_power += dt;
            shake(&bs, &mut env);
            item.is_clicked = true;
        } else if f_l > 0.1 && in_l && bs.fire_timer <= 0.0 && bs.is_clicked3 {
            bs.accumulated_power += dt;
            shake(&bs, &mut env);
            item.is_clicked = true;
        }
        if (item.is_clicked && in_r && bs.accumulated_power >= max_charge) || (item.is_clicked && in_l && bs.accumulated_power >= max_charge) {
            item.is_clicked = false;
            bs.is_clicked2 = false;
            bs.is_clicked3 = false;
            bs.is_clicked4 = false;
            fire(&def, item, &mut bs, &mut env, &mut tf, tweens.as_deref_mut());
            bs.accumulated_power = 0.0;
        } else if (f_r < 0.1 && item.is_clicked && in_r) || (f_l < 0.1 && item.is_clicked && in_l) {
            item.is_clicked = false;
            bs.is_clicked2 = false;
            bs.is_clicked3 = true;
            fire(&def, item, &mut bs, &mut env, &mut tf, tweens.as_deref_mut());
            bs.accumulated_power = 0.0;
        } else if (f_r < 0.1 && in_r) || (f_l < 0.1 && in_l) {
            bs.is_clicked3 = true;
        }
    }
}

/// Quick tap: small projectile (`FireBlast` + `ServerFire`).
#[allow(clippy::too_many_arguments)]
fn fire_blast(def: &WeaponDef, item: &mut Item, bs: &mut BeamState, env: &mut Env, tf: &mut Transform, tweens: Option<&mut Tweens>, root_tf: &Transform, lassets: &super::launcher::LauncherAssets, commands: &mut Commands) {
    let w = &def.w;
    if bs.fire_timer > 0.0 {
        return;
    }
    bs.fire_timer = w.f("timeBetweenFire");
    if item.current_ammo <= 0 {
        play(env.sounds, def, "nobulletClip", item.audio_pitch);
    }
    if item.current_ammo > 0 {
        let position = env.cam_g.translation();
        let forward = env.cam_g.forward().as_vec3();
        // revolverShake ? CameraRevolverAnimation : CameraAnimation (both in camera_recoil)
        super::camera_recoil(def, env.fpc, env.cam_tf, env.cam_tw, 1.0);
        weapon_animation(def, item, env.fpc, tf, tweens, env.anchors);
        // SpawnProjectile(position, forward, 0): PredictedProjectile "_projectile" (BeamLoadBullet.prefab, MOVE_RATE 50,
        // damage 1, radius 0.2) via the launcher's projectile spawner (data: assets/data/projectiles/BeamLoadBullet.json).
        super::launcher::spawn_predicted_shot(w.asset("_projectile"), w.f("launchForce"), position, forward, root_tf, lassets, commands);
        // ServerFire -> RpcLogic: ammo (needsAmmo) + ShootObserversEffect (smallFireClip + muzzleFlash2)
        if w.b("needsAmmo") {
            item.current_ammo -= 1;
        }
        play(env.sounds, def, "smallFireClip", item.audio_pitch);
        env.fx.write(GunFx::Muzzle { at: item.flash_pos.unwrap_or(position + forward * 0.6), prefab: w.prefab_ref("muzzleFlash2") });
        let kb = w.f("playerKnockback"); // prefab value (0)
        if kb != 0.0 {
            env.fpc.add_force(-forward, kb);
        }
    }
}

/// Full charge: piercing beam (`Fire`).
fn fire(def: &WeaponDef, item: &mut Item, bs: &mut BeamState, env: &mut Env, tf: &mut Transform, tweens: Option<&mut Tweens>) {
    let w = &def.w;
    if !env.fpc.can_move {
        return;
    }
    if w.b("changePitchOnShoot") {
        item.audio_pitch = crate::unity::random_range_f(0.97, 1.03);
    }
    item.src_audio = Some(false); // AudioStop()
    if bs.accumulated_power < w.f("maxChargeTime") {
        // BeamEffectServer(1): tempBeam.SetActive(false)
        env.fx.write(GunFx::Charge { item: env.me, op: ChargeOp::Hide });
        bs.accumulated_power = 0.0;
        return;
    }
    bs.accumulated_power = 0.0;
    item.shot = false;
    bs.fire_timer = w.f("timeBetweenFire");
    let origin = env.cam_g.translation();
    let fwd = env.cam_g.forward().as_vec3();
    shoot_server(def, item.shoot_pos, env, w.f("damage"), origin, fwd, item.audio_pitch);
    // revolverShake ? AltCameraRevolverAnimation : CameraAnimation
    super::camera_recoil(def, env.fpc, env.cam_tf, env.cam_tw, 3.0);
    weapon_animation(def, item, env.fpc, tf, tweens, env.anchors);
}

/// `ShootServer`: BeamEffectServer(0) (detach tempBeam), SoundServerEffect (fireClip), raycast trails
/// (no RemoveAmmo here), sphere-cast player hit (doubled head damage), doll hit.
fn shoot_server(def: &WeaponDef, shoot_pos: Option<Vec3>, env: &mut Env, damage: f32, position: Vec3, direction: Vec3, pitch: f32) {
    let w = &def.w;
    env.fx.write(GunFx::Charge { item: env.me, op: ChargeOp::Detach }); // BeamEffectServer(0)
    play(env.sounds, def, "fireClip", pitch); // newSource.PlayOneShot(fireClip): on BeamLoad newSource is the root `audio` source (same fileID), so it carries audio.pitch
    let cam_right = env.cam_g.right().as_vec3();
    let cam_up = env.cam_g.up().as_vec3();
    for h in physics::raycast_all(env.ctx, position, direction, f32::INFINITY, w.mask("defaultLayer")) {
        let tag = env.lk.tags.get(h.entity).map(|t| t.0.clone()).unwrap_or_default();
        env.hit.trigger_environment(h.entity, direction); // TriggerEnvironment
        env.hit.trigger_ragdoll(h.entity, w, h.point, h.normal, direction, &mut env.fx);
        env.fx.write(GunFx::Tracer { from: shoot_pos.unwrap_or(position - cam_up * 0.15 + cam_right * 0.2), to: h.point, prefab: w.asset("bulletTrailLocal") });
        let l = env.lk.layers.get(h.entity).map(|l| l.0).unwrap_or(0);
        if tag == "ShatterableGlass" {
            // TODO BreakGlassServer
        } else if l != physics::layer::RAGDOLL {
            env.fx.write(GunFx::Impact { point: h.point, normal: h.normal, hit: Some(h.entity), tag });
        }
        if !beam_pierces(l) {
            break;
        }
    }
    // Physics.CapsuleCast(position, position, radius, forward, playerLayer): a sphere sweep that skips start overlaps; radius from the prefab (1).
    // damageToGive is doubled in place on a head hit, so the DollHealth ray below takes the doubled value too.
    let mut damage = damage;
    if let Some(h) = physics::sphere_sweep_unity(env.ctx, position, w.f("radius"), env.cam_g.forward().as_vec3(), f32::INFINITY, w.mask("playerLayer")) {
        let on_body = env.lk.layers.get(h.entity).map(|l| l.0 == physics::layer::BODY).unwrap_or(false);
        if let (true, Ok(part)) = (on_body, env.hit.parts.get(h.entity)) {
            if let Ok(mut ph) = env.hit.healths.get_mut(part.owner) {
                let head = part.name == "Head_Col";
                if head {
                    damage *= 2.0; // hard-coded x2 in BeamGun
                }
                let dmg = damage;
                env.fx.write(GunFx::Impact { point: h.point, normal: h.normal, hit: Some(h.entity), tag: "Body".into() });
                // ServerFX(hitInfo.point + direction, identity) -> Instantiate(bloodSplatter).
                env.fx.write(GunFx::Spawn { prefab: w.asset("bloodSplatter"), at: h.point + env.cam_g.forward().as_vec3(), normal: Vec3::NEG_Z });
                env.hit.ui.write(super::WeaponUi::HitMarker { head, source: Some(env.me) });
                // BeamGun.cs:532 LocalSound only handles index 0 (headHitClip); LocalSound(1)/(2) are silent,
                // so no bodyHitClip / deathClip for the BeamGun.
                if head {
                    play(env.sounds, def, "headHitClip", pitch);
                }
                if ph.health - dmg <= 0.0 {
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
