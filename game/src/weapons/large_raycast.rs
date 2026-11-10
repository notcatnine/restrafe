//! Port of `LargeRaycastGun.cs` (fat/box hitscan: snipers, Bayshore, Bender, QCW05...).
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

pub fn is_large_raycast(class: &str) -> bool {
    class == "LargeRaycastGun"
}

#[derive(Component)]
pub struct LargeRaycastState {
    pub fire_timer: f32,
    reload_timer: Option<f32>,
    /// `hitOK` field (initially true; set by a player hit, consumed by the doll check).
    hit_ok: bool,
    /// `Start()` -> InitChargedBullets (only if reloadWeapon) has run.
    inited: bool,
}

impl Default for LargeRaycastState {
    fn default() -> Self {
        LargeRaycastState { fire_timer: 0.0, reload_timer: None, hit_ok: true, inited: false }
    }
}

#[derive(bevy::ecs::system::SystemParam)]
pub struct Lookups<'w, 's> {
    pub layers: Query<'w, 's, &'static physics::UnityLayer>,
    pub tags: Query<'w, 's, &'static physics::UnityTag>,
    pub dolls: Query<'w, 's, &'static mut DollHealth>,
    pub gtfs: Query<'w, 's, &'static GlobalTransform, Without<crate::player::fpc::PlayerCamera>>,
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
pub fn large_raycast_update(
    time: Res<Time>,
    input: Res<PlayerInput>,
    anchors: Res<HandAnchors>,
    rapier: ReadRapierContext,
    mut players: Query<&mut Fpc>,
    mut cams: CamQuery,
    mut guns: Query<(Entity, &mut Item, &mut LargeRaycastState, &mut Transform, &GlobalTransform, Option<&mut Tweens>), Without<crate::player::fpc::PlayerCamera>>,
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
        let reload_weapon = w.b("reloadWeapon");
        // Start(): if reloadWeapon -> InitChargedBullets (currentAmmo -= ammoCharge; chargedBullets = ammoCharge).
        if !gs.inited {
            gs.inited = true;
            if reload_weapon {
                let charge = w.i("ammoCharge");
                item.current_ammo -= charge;
                item.charged_bullets = charge as f32;
            }
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
        if item.hand == Hand::None || !input.enabled {
            continue;
        }
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
        let can_fire = |it: &Item| if reload_weapon { it.current_ammo > 0 || it.charged_bullets > 0.0 } else { it.current_ammo > 0 };
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
            if f_r > 0.1 && in_r && can_fire(item) {
                fire(&def, item, &mut gs, &mut env, &mut tf, gtf, tweens.as_deref_mut());
            }
            if f_l > 0.1 && in_l && can_fire(item) {
                fire(&def, item, &mut gs, &mut env, &mut tf, gtf, tweens.as_deref_mut());
            }
            let ak = w.b("akAnim");
            env.fpc.cam_rotate_back = !((f_r > 0.1 && in_r && ak && item.current_ammo > 0) || (f_l > 0.1 && in_l && ak && item.current_ammo > 0));
            let no_ammo = if !reload_weapon {
                Some(item.current_ammo <= 0)
            } else if item.current_ammo <= 0 {
                Some(item.charged_bullets <= 0.0)
            } else {
                None
            };
            if no_ammo == Some(true) {
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
            env.fpc.cam_rotate_back = true;
        }
        if item.is_clicked {
            if f_r > 0.1 && in_r {
                fire(&def, item, &mut gs, &mut env, &mut tf, gtf, tweens.as_deref_mut());
                item.is_clicked = false;
            }
            if f_l > 0.1 && in_l {
                fire(&def, item, &mut gs, &mut env, &mut tf, gtf, tweens.as_deref_mut());
                item.is_clicked = false;
            }
        }
        if item.shot && w.f("reloadTime") < 0.2 && one_press {
            fire(&def, item, &mut gs, &mut env, &mut tf, gtf, tweens.as_deref_mut());
        }
    }
}

fn start_reload(item: &mut Item, gs: &mut LargeRaycastState, w: &Fields, sounds: &mut MessageWriter<WeaponSound>) {
    item.is_reloading = true;
    item.anim_triggers.push("Reload"); // animator.SetTrigger("Reload")
    if let Some(c) = w.asset("reloadClip") {
        sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
    }
    item.on_reload_eject(); // OnReload()
    gs.reload_timer = Some(w.f("reloadTime"));
}

fn fire(def: &WeaponDef, item: &mut Item, gs: &mut LargeRaycastState, env: &mut Env, tf: &mut Transform, gtf: &GlobalTransform, tweens: Option<&mut Tweens>) {
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
    gs.fire_timer = w.f("timeBetweenFire");
    let reload_weapon = w.b("reloadWeapon");
    if item.current_ammo <= 0 && item.charged_bullets <= 0.0 {
        if let Some(c) = w.asset("nobulletClip") {
            env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
        }
        item.no_ammo_clicks += 1; // PlayerPickup drops the weapon when > 1
    }
    if (!reload_weapon && item.current_ammo <= 0) || (reload_weapon && item.is_reloading) {
        return;
    }
    if reload_weapon && item.charged_bullets <= 0.0 && item.current_ammo > 0 {
        start_reload(item, gs, w, env.sounds);
    }
    if reload_weapon && item.charged_bullets <= 0.0 && item.current_ammo <= 0 {
        if let Some(c) = w.asset("nobulletClip") {
            env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
        }
    }
    if reload_weapon && item.charged_bullets <= 0.0 {
        return;
    }
    // revolverShake ? CameraRevolverAnimation : CameraAnimation (camera_recoil switches on the flag)
    super::camera_recoil(def, env.fpc, env.cam_tf, env.cam_tw, 1.0);
    weapon_animation(def, item, env.fpc, tf, tweens, env.anchors);
    let weapon_pos = gtf.translation();
    let cam_pos = env.cam_g.translation();
    let cam_fwd = env.cam_g.forward().as_vec3();
    if w.b("boxcast") {
        shoot_server_box(def, item, gs, env, w.f("damage"), cam_pos, cam_fwd, weapon_pos);
        return;
    }
    let (cam_right, cam_up) = (env.cam_g.right().as_vec3(), env.cam_g.up().as_vec3());
    let mut direction = cam_fwd;
    let mut position = cam_pos;
    let mut found = false;
    let mut spread = Vec3::ZERO;
    let radius = w.f("bulletRadius");
    let (min_s, max_s) = (w.f("minSpread"), w.f("maxSpread"));
    let scope = w.b("ScopeAimWeapon");
    for i in 0..8 {
        let fpc = &*env.fpc;
        let stance = |acc: f32| lerp(max_s, min_s, acc);
        let num = if !fpc.is_grounded && scope && fpc.is_aiming {
            0.0
        } else if scope && !fpc.is_aiming {
            w.f("notAimingAccuracy")
        } else if fpc.is_sprinting || !fpc.is_grounded || !fpc.safe_grounded {
            stance(w.f("sprintAccuracy"))
        } else if fpc.is_crouching {
            stance(w.f("standingAccuracy"))
        } else if fpc.is_walking {
            stance(w.f("walkAccuracy"))
        } else {
            stance(w.f("standingAccuracy"))
        };
        spread = cam_right * random_range_f(num, -num) + cam_up * random_range_f(num, -num);
        let origin = match i {
            7 => cam_pos + (-cam_right + -cam_up).normalize() * radius,
            6 => cam_pos + (-cam_right + cam_up).normalize() * radius,
            5 => cam_pos + (cam_right + -cam_up).normalize() * radius,
            4 => cam_pos + (cam_right + cam_up).normalize() * radius,
            3 => cam_pos + -cam_up * radius,
            2 => cam_pos + cam_up * radius,
            1 => cam_pos + -cam_right * radius,
            0 => cam_pos + cam_right * radius,
            _ => cam_pos,
        };
        let dir2 = cam_fwd + spread;
        if let Some(h) = physics::raycast(env.ctx, origin, dir2, f32::INFINITY, w.mask("playerLayer")) {
            // hit.transform.root.TryGetComponent<PlayerHealth>
            let has_health = env.hit.parts.get(h.entity).map(|p| env.hit.healths.get(p.owner).is_ok()).unwrap_or(false);
            if has_health {
                direction = dir2;
                position = origin;
                found = true;
                break;
            }
        }
    }
    if !found {
        direction = cam_fwd + spread;
        position = cam_pos;
    }
    shoot_server(def, item, gs, env, w.f("damage"), position, direction);
    let kb = w.f("playerKnockback");
    if kb != 0.0 {
        env.fpc.add_force(-cam_fwd, kb);
    }
}

/// RemoveAmmo + ShootServerEffect (shared prologue of ShootServer / ShootServerBox).
fn remove_ammo_and_effect(def: &WeaponDef, item: &mut Item, env: &mut Env, position: Vec3, direction: Vec3) {
    let w = &def.w;
    if w.b("reloadWeapon") {
        item.charged_bullets -= 1.0;
    } else {
        item.current_ammo -= 1;
    }
    if let Some(c) = w.asset("fireClip") {
        env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: item.audio_pitch, linear_max: 0.0, source: None, rolloff: None });
    }
    env.fx.write(GunFx::Muzzle { at: item.flash_pos.unwrap_or(position + direction.normalize_or_zero() * 0.6), prefab: w.prefab_ref("muzzleFlash") });
}

/// The RaycastAll tracer/impact loop shared by both shoot functions.
fn environment_trace(def: &WeaponDef, shoot_pos: Option<Vec3>, env: &mut Env, position: Vec3, direction: Vec3) {
    let w = &def.w;
    let cam_right = env.cam_g.right().as_vec3();
    let cam_up = env.cam_g.up().as_vec3();
    for h in physics::raycast_all(env.ctx, position, direction, f32::INFINITY, w.mask("defaultLayer")) {
        env.hit.trigger_environment(h.entity, direction);
        env.hit.trigger_ragdoll(h.entity, w, h.point, h.normal, direction, &mut env.fx);
        let tag = env.lk.tags.get(h.entity).map(|t| t.0.clone()).unwrap_or_default();
        env.fx.write(GunFx::Tracer { from: shoot_pos.unwrap_or(position - cam_up * 0.15 + cam_right * 0.2), to: h.point, prefab: w.asset("bulletTrailLocal") });
        let l = env.lk.layers.get(h.entity).map(|l| l.0).unwrap_or(0);
        if tag == "ShatterableGlass" {
            // TODO BreakGlassServer(point, direction, glass)
        } else if l != physics::layer::RAGDOLL {
            env.fx.write(GunFx::Impact { point: h.point, normal: h.normal, hit: Some(h.entity), tag });
        }
        if !pierces(l) {
            break;
        }
    }
}

/// Damage / kill / feedback on a PlayerHealth (shared "Player hit" block).
#[allow(clippy::too_many_arguments)]
fn damage_player(def: &WeaponDef, env: &mut Env, hit: Entity, owner: Entity, part_name: &str, head_includes_neck: bool, point: Vec3, normal: Vec3, direction: Vec3, damage: f32, pitch: f32) -> bool {
    let w = &def.w;
    let Ok(mut ph) = env.hit.healths.get_mut(owner) else { return false };
    let head = part_name == "Head_Col" || (head_includes_neck && part_name == "Neck_1_Col");
    let mut dmg = damage;
    if head {
        dmg *= w.f("headMultiplier");
    }
    // TODO M2000 noscope stat / kill-feed word (GetKillWord)
    // head/generic impacts parented to the hit collider while the victim survives (resolved after the damage below).
    env.fx.write(GunFx::Impact { point, normal, hit: Some(hit), tag: if head { "Head".into() } else { "Body".into() } });
    // ServerFX(point + direction, identity) -> Instantiate(bloodSplatter).
    env.fx.write(GunFx::Spawn { prefab: w.asset("bloodSplatter"), at: point + direction, normal: Vec3::NEG_Z });
    env.hit.ui.write(super::WeaponUi::HitMarker { head: part_name == "Head_Col", source: Some(env.me) });
    if head {
        if let Some(c) = w.asset("headHitClip") {
            env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch, linear_max: 0.0, source: None, rolloff: None });
        }
    }
    if let Some(c) = w.asset("bodyHitClip") {
        env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch, linear_max: 0.0, source: None, rolloff: None });
    }
    if ph.health - dmg <= 0.0 {
        if let Some(c) = w.asset("deathClip") {
            env.sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch, linear_max: 0.0, source: None, rolloff: None });
        }
        env.fpc.kill_shock_wave();
        ph.health = -8.0; // KillServer
        ph.last_hit = Some((env.cam_g.forward().as_vec3(), w.f("ragdollEjectForce"), point));
    } else {
        ph.health -= dmg; // GiveDamage
    }
    true
}

/// `ShootServer` (single ray, thin/large bullets).
fn shoot_server(def: &WeaponDef, item: &mut Item, gs: &mut LargeRaycastState, env: &mut Env, damage: f32, position: Vec3, direction: Vec3) {
    let w = &def.w;
    remove_ammo_and_effect(def, item, env, position, direction);
    environment_trace(def, item.shoot_pos, env, position, direction);
    let mask = w.mask("playerLayer");
    if let Some(h) = physics::raycast(env.ctx, position, direction, f32::INFINITY, mask) {
        let on_body = env.lk.layers.get(h.entity).map(|l| l.0 == physics::layer::BODY).unwrap_or(false);
        if on_body {
            if let Ok(part) = env.hit.parts.get(h.entity) {
                let (owner, name) = (part.owner, part.name.clone());
                if damage_player(def, env, h.entity, owner, &name, true, h.point, h.normal, direction, damage, item.audio_pitch) {
                    gs.hit_ok = true;
                }
            }
        }
    }
    // TODO suppression ray (supLayer, layer 17) -- no AI to suppress.
    finish_doll(env, gs, direction, position, damage);
}

/// Final DollHealth ray: `... && hitOK`, then `hitOK = false`.
fn finish_doll(env: &mut Env, gs: &mut LargeRaycastState, direction: Vec3, position: Vec3, damage: f32) {
    if let Some(h) = physics::raycast(env.ctx, position, direction, f32::INFINITY, physics::DEFAULT_RAYCAST_LAYERS) {
        if let Ok(mut d) = env.lk.dolls.get_mut(h.entity) {
            if gs.hit_ok {
                d.health -= damage;
            }
        }
    }
    gs.hit_ok = false;
}

/// `ShootServerBox`: OverlapBox along the ray (Bayshore, BlankState, Phoenix).
#[allow(clippy::too_many_arguments)]
fn shoot_server_box(def: &WeaponDef, item: &mut Item, gs: &mut LargeRaycastState, env: &mut Env, damage: f32, position: Vec3, direction: Vec3, weapon_pos: Vec3) {
    let w = &def.w;
    remove_ammo_and_effect(def, item, env, position, direction);
    let mask = w.mask("playerLayer");
    let mut dims = w.v3("boxdimensions");
    let end_point = match physics::raycast(env.ctx, position, direction, f32::INFINITY, mask) {
        Some(h) => h.point,
        None => direction * 10000.0,
    };
    if w.b("determineLengthWithRay") {
        // boxdimensions.z = Distance(transform.position, endPoint)
        dims.z = weapon_pos.distance(end_point);
        environment_trace(def, item.shoot_pos, env, position, direction);
    }
    // Physics.OverlapBox(center, halfExtents = boxdimensions, LookRotation(direction), playerLayer)
    let center = position + direction * (dims.z / 2.0);
    let rot = Transform::IDENTITY.looking_to(direction.normalize_or_zero(), Vec3::Y).rotation;
    let filter = QueryFilter::new().exclude_sensors().groups(CollisionGroups::new(Group::ALL, Group::from_bits_retain(mask)));
    let shape = Collider::cuboid(dims.x.abs(), dims.y.abs(), dims.z.abs());
    let mut found: Vec<Entity> = Vec::new();
    env.ctx.intersect_shape(center, rot, &*shape.raw, filter, |e| {
        found.push(e);
        true
    });
    // first collider that has a PlayerHealth in its parents (HitPart.owner)
    let target = found.iter().find_map(|e| {
        let p = env.hit.parts.get(*e).ok()?;
        env.hit.healths.get(p.owner).ok()?;
        Some((*e, p.owner, p.name.clone()))
    });
    if let Some((col, owner, name)) = target {
        // Impacts at the collider's transform.position, bodyImpact at LookRotation(-transform.forward) (Unity forward = -Z
        // here), ServerFX at transform.position + direction (LargeRaycastGun.cs:419-431).
        let (at, back) = env.lk.gtfs.get(col).map(|g| (g.translation(), g.back().as_vec3())).unwrap_or((end_point, -direction));
        if damage_player(def, env, col, owner, &name, false, at, back, direction, damage, item.audio_pitch) {
            gs.hit_ok = true;
        }
    }
    // TODO suppression ray (supLayer, layer 17).
    finish_doll(env, gs, direction, position, damage);
}
