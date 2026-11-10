//! Port of `Shotgun.cs` (multi-pellet hitscan: Shotgun, AAA12, Havoc, SawedOff, Tromblonj).
//! Networking removed: the `RpcLogic___*` bodies run directly.

use super::data::{Fields, HandAnchors, UField, WeaponDef};
use super::gun::{pierces, weapon_animation, DollHealth, GunFx, HitCtx};
use super::{CamQuery, Hand, Item, WeaponSound};
use crate::input::PlayerInput;
use crate::physics;
use crate::player::fpc::Fpc;
use crate::tween::Tweens;
use crate::unity::*;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub fn is_shotgun(class: &str) -> bool {
    class == "Shotgun"
}

#[derive(Component, Default)]
pub struct ShotgunState {
    pub fire_timer: f32,
    reload_timer: Option<f32>,
    /// `Start()` -> InitChargedBullets has run (done lazily on the first update tick).
    inited: bool,
}

#[derive(bevy::ecs::system::SystemParam)]
pub struct Lookups<'w, 's> {
    pub layers: Query<'w, 's, &'static physics::UnityLayer>,
    pub tags: Query<'w, 's, &'static physics::UnityTag>,
    pub dolls: Query<'w, 's, &'static mut DollHealth>,
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
    me: Entity,
}

#[allow(clippy::too_many_arguments)]
pub fn shotgun_update(
    time: Res<Time>,
    input: Res<PlayerInput>,
    anchors: Res<HandAnchors>,
    rapier: ReadRapierContext,
    mut players: Query<&mut Fpc>,
    mut cams: CamQuery,
    mut guns: Query<(Entity, &mut Item, &mut ShotgunState, &mut Transform, &GlobalTransform, Option<&mut Tweens>), Without<crate::player::fpc::PlayerCamera>>,
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
    for (me, mut item, mut gs, mut tf, gtf, mut tweens) in &mut guns {
        let item = &mut *item;
        let def = item.def.clone();
        let w = &def.w;
        // Start(): InitChargedBullets -> RpcLogic: currentAmmo -= ammoCharge; chargedBullets = ammoCharge.
        if !gs.inited {
            gs.inited = true;
            let charge = w.i("ammoCharge");
            item.current_ammo -= charge;
            item.charged_bullets = charge as f32;
        }
        // Reload coroutine tail.
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
        if gs.fire_timer > 0.0 {
            gs.fire_timer -= dt;
        }
        // gameObject.layer == 7 (world item) -> return
        if item.hand == Hand::None || !input.enabled {
            continue;
        }
        let reload_weapon = w.b("reloadWeapon");
        if input.reload.value() > 0.1
            && reload_weapon
            && item.charged_bullets != w.i("ammoCharge") as f32
            && !item.is_reloading
            && gs.fire_timer <= 0.0
            && item.current_ammo > 0
        {
            start_reload(item, &mut gs, w, &mut sounds);
        }
        let in_r = item.in_right();
        let in_l = item.in_left();
        let (f_r, f_l) = if item.invert_fire { (fire2, fire1) } else { (fire1, fire2) };
        let one_press = w.b("onePressShoot");
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
        if !one_press {
            if f_r > 0.1 && in_r {
                fire(&def, item, &mut gs, &mut env, &mut tf, gtf, tweens.as_deref_mut());
            }
            if f_l > 0.1 && in_l {
                fire(&def, item, &mut gs, &mut env, &mut tf, gtf, tweens.as_deref_mut());
            }
        } else {
            if f_r < 0.1 && in_r {
                item.is_clicked = true;
            }
            if f_l < 0.1 && in_l {
                item.is_clicked = true;
            }
        }
        if item.is_clicked && one_press {
            if f_r > 0.1 && in_r {
                fire(&def, item, &mut gs, &mut env, &mut tf, gtf, tweens.as_deref_mut());
                item.is_clicked = false;
            }
            if f_l > 0.1 && in_l {
                fire(&def, item, &mut gs, &mut env, &mut tf, gtf, tweens.as_deref_mut());
                item.is_clicked = false;
            }
        }
        if item.shot && w.f("reloadTime") < 0.2 {
            fire(&def, item, &mut gs, &mut env, &mut tf, gtf, tweens.as_deref_mut());
        }
    }
}

/// `StartCoroutine(Reload())`: isReloading, reloadClip, wait reloadTime (finished in the update tick).
fn start_reload(item: &mut Item, gs: &mut ShotgunState, w: &Fields, sounds: &mut MessageWriter<WeaponSound>) {
    item.is_reloading = true;
    item.anim_triggers.push("Reload"); // animator.SetTrigger("Reload")
    if let Some(c) = w.asset("reloadClip") {
        sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
    }
    item.on_reload_eject(); // OnReload()
    gs.reload_timer = Some(w.f("reloadTime"));
}

fn fire(def: &WeaponDef, item: &mut Item, gs: &mut ShotgunState, env: &mut Env, tf: &mut Transform, gtf: &GlobalTransform, tweens: Option<&mut Tweens>) {
    let w = &def.w;
    if !env.fpc.can_move {
        return;
    }
    if w.b("changePitchOnShoot") {
        item.audio_pitch = crate::unity::random_range_f(0.97, 1.03);
    }
    if gs.fire_timer > 0.0 {
        item.shot = true;
        return;
    }
    item.shot = false;
    let reload_weapon = w.b("reloadWeapon");
    if item.current_ammo <= 0 && item.charged_bullets <= 0.0 {
        if let Some(c) = w.asset("nobulletClip") {
            env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
        }
        // C#: noAmmoClicks > 2 drops the weapon (PlayerPickup auto-drops at > 1 in this port).
        item.no_ammo_clicks += 1;
    }
    if (!reload_weapon && item.current_ammo <= 0) || (reload_weapon && item.is_reloading) {
        return;
    }
    if reload_weapon && item.charged_bullets <= 0.0 && item.current_ammo > 0 {
        start_reload(item, gs, w, env.sounds);
    }
    if !reload_weapon || item.charged_bullets > 0.0 {
        let bullets = w.i("bulletAmount").max(0) as usize;
        let spread = w.f("spread");
        // hitPosition[i] = transform.right * Range(spread,-spread) + transform.up * Range(spread,-spread)
        let (right, up) = (gtf.right().as_vec3(), gtf.up().as_vec3());
        let offsets: Vec<Vec3> = (0..bullets).map(|_| right * random_range_f(spread, -spread) + up * random_range_f(spread, -spread)).collect();
        let origin = env.cam_g.translation();
        let fwd = env.cam_g.forward().as_vec3();
        let mut destroy_glass_once = false;
        for off in offsets {
            shoot_server(def, item, env, w.f("damage"), origin, fwd + off, &mut destroy_glass_once);
        }
        // ShootServerEffect -> ShootObserversEffect
        if let Some(c) = w.asset("fireClip") {
            env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
        }
        env.fx.write(GunFx::Muzzle { at: item.flash_pos.unwrap_or(origin + fwd * 0.6), prefab: w.prefab_ref("muzzleFlash") });
        super::camera_recoil(def, env.fpc, env.cam_tf, env.cam_tw, 1.0); // CameraRevolverAnimation
        weapon_animation(def, item, env.fpc, tf, tweens, env.anchors);
        gs.fire_timer = w.f("timeBetweenFire");
    }
}

/// One pellet (`ShootServer`). Note RemoveAmmo runs per pellet, as in the C#.
fn shoot_server(def: &WeaponDef, item: &mut Item, env: &mut Env, damage: f32, position: Vec3, direction: Vec3, destroy_glass_once: &mut bool) {
    let w = &def.w;
    // RemoveAmmo (unconditional in Shotgun.cs)
    if w.b("reloadWeapon") {
        item.charged_bullets -= 1.0;
    } else {
        item.current_ammo -= 1;
    }
    let cam_right = env.cam_g.right().as_vec3();
    let cam_up = env.cam_g.up().as_vec3();
    for h in physics::raycast_all(env.ctx, position, direction, f32::INFINITY, w.mask("defaultLayer")) {
        env.hit.trigger_environment(h.entity, direction);
        env.hit.trigger_ragdoll(h.entity, w, h.point, h.normal, direction, &mut env.fx);
        let tag = env.lk.tags.get(h.entity).map(|t| t.0.clone()).unwrap_or_default();
        env.fx.write(GunFx::Tracer { from: item.shoot_pos.unwrap_or(position - cam_up * 0.15 + cam_right * 0.2), to: h.point, prefab: w.asset("bulletTrailLocal") });
        let l = env.lk.layers.get(h.entity).map(|l| l.0).unwrap_or(0);
        if tag == "ShatterableGlass" {
            if !*destroy_glass_once {
                // TODO BreakGlassServer(point, direction, glass)
            }
            *destroy_glass_once = true;
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
                env.hit.ui.write(super::WeaponUi::HitMarker { head: part.name == "Head_Col", source: Some(env.me) });
                if head {
                    if let Some(c) = w.asset("headHitClip") {
                        env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
                    }
                }
                if let Some(c) = w.asset("bodyHitClip") {
                    env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
                }
                if ph.health - dmg <= 0.0 {
                    if let Some(c) = w.asset("deathClip") {
                        env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
                    }
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
