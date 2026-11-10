//! Port of `Taser.cs` (box-overlap stun gun). Networking removed: `RpcLogic___*` bodies run directly.

use super::data::{UField, WeaponDef};
use super::gun::{GunFx, HitCtx};
use super::{CamQuery, Hand, Item, WeaponSound};
use crate::input::PlayerInput;
use crate::player::fpc::Fpc;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub fn is_taser(class: &str) -> bool {
    class == "Taser"
}

#[derive(Component, Default)]
pub struct TaserState {
    /// `fireTimer` (starts 0; set to timeBetweenFire on fire, -2 once the "ready" state was applied).
    pub fire_timer: f32,
    /// readyVfx active / light material = readyMat (false while charging = chargingMat).
    /// Visual hook for the integrator (TODO: swap `light` MeshRenderer material, toggle readyVfx, loop chargingAudio).
    pub ready: bool,
    inited: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn taser_update(
    mut commands: Commands,
    time: Res<Time>,
    input: Res<PlayerInput>,
    rapier: ReadRapierContext,
    mut players: Query<(Entity, &mut Fpc)>,
    mut cams: CamQuery,
    mut tasers: Query<(Entity, &mut Item, &mut TaserState), Without<crate::player::fpc::PlayerCamera>>,
    mut hit: HitCtx,
    mut sounds: MessageWriter<WeaponSound>,
    mut fx: MessageWriter<GunFx>,
) {
    let dt = time.delta_secs();
    let Ok(ctx) = rapier.single() else { return };
    let Ok((player, fpc)) = players.single_mut() else { return };
    let Ok((_cam_tf, cam_g, _cam_tw)) = cams.single_mut() else { return };
    let cam_g = *cam_g;
    let fire1 = input.fire.value();
    let fire2 = input.fire2.value();
    for (item_e, mut item, mut ts) in &mut tasers {
        let item = &mut *item;
        let def = item.def.clone();
        if !ts.inited {
            ts.inited = true;
            // fireTimer starts at 0: readyVfx keeps its prefab state (inactive) and `light` its prefab material until
            // the first charge completes.
            ts.ready = true;
        }
        // WeaponUpdate() runs in pickup.rs (item_behaviour_update).
        if ts.fire_timer > 0.0 {
            ts.fire_timer -= dt;
        }
        // gameObject.layer == 7 (world item) -> skip. verify: pause only gates Fire(), not the ready transition below.
        if item.hand == Hand::None {
            continue;
        }
        let in_r = item.in_right();
        let in_l = item.in_left();
        let (f_r, f_l) = if item.invert_fire { (fire2, fire1) } else { (fire1, fire2) };
        let was_charging = ts.fire_timer > 0.0;
        if input.enabled && f_r > 0.1 && in_r && item.current_ammo > 0 {
            do_fire(&def, item, &mut ts, &fpc, &cam_g, &ctx, &mut hit, player, &mut sounds, &mut fx);
        }
        if input.enabled && f_l > 0.1 && in_l && item.current_ammo > 0 {
            do_fire(&def, item, &mut ts, &fpc, &cam_g, &ctx, &mut hit, player, &mut sounds, &mut fx);
        }
        // `fireTimer < 0 && fireTimer > -1` -> charge finished: chargingAudio.Stop(), readyMat, SetVfxActive(true).
        if ts.fire_timer < 0.0 && ts.fire_timer > -1.0 {
            ts.fire_timer = -2.0;
            ts.ready = true;
            // chargingAudio.Stop() (the clip has ended by now); light.materials = readyMat; SetVfxActive(true).
            visuals(&mut commands, item_e, &def, true);
        }
        if !was_charging && ts.fire_timer > 0.0 {
            // Fire(): light.materials = chargingMat; SetVfxActive(false).
            visuals(&mut commands, item_e, &def, false);
        }
    }
}

/// Taser `light` (MeshRenderer) material and `readyVfx` (GameObject) state, see `attached_fx.rs`.
fn visuals(commands: &mut Commands, item: Entity, def: &WeaponDef, ready: bool) {
    let w = &def.w;
    let path_of = |r: String| r.strip_prefix("ref:").and_then(|r| r.split("::").next()).map(str::to_string);
    if let Some(p) = path_of(w.s("readyVfx")) {
        commands.entity(item).try_insert(super::attached_fx::FxToggle { path: p, on: ready });
    }
    let mat = w.asset(if ready { "readyMat" } else { "chargingMat" }).map(|m| m.rsplit('/').next().unwrap_or(&m).trim_end_matches(".mat").to_string());
    // The serialized field named `light` is the MeshRenderer (the Light shares the name; the MeshRenderer ref wins).
    let node = def.prefab.objects.iter().flat_map(|o| o.components.iter()).filter_map(|(_, f)| f.get("light").and_then(|v| v.as_str())).find(|r| r.ends_with("::MeshRenderer")).and_then(|r| path_of(r.to_string()));
    if let (Some(node), Some(mat)) = (node, mat) {
        commands.entity(item).try_insert(super::attached_fx::NodeMaterial { node: node.rsplit('/').next().unwrap_or(&node).to_string(), mat });
    }
}

/// `Fire()`.
#[allow(clippy::too_many_arguments)]
fn do_fire(
    def: &WeaponDef,
    item: &mut Item,
    ts: &mut TaserState,
    fpc: &Fpc,
    cam_g: &GlobalTransform,
    ctx: &RapierContext,
    hit: &mut HitCtx,
    player: Entity,
    sounds: &mut MessageWriter<WeaponSound>,
    fx: &mut MessageWriter<GunFx>,
) {
    let w = &def.w;
    // PauseManager.pause handled by input.enabled; playerController.IsOwner always true.
    if !fpc.can_move || ts.fire_timer > 0.0 {
        return;
    }
    if !w.b("reloadWeapon") && item.current_ammo <= 0 {
        if let Some(c) = w.asset("nobulletClip") {
            sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
        }
        item.no_ammo_clicks += 1;
    }
    // chargingAudio.Play(); light.materials = chargingMat; SetVfxActive(false)
    // Taser.cs:100: chargingAudio = ElbowPivotPoint AudioSource (taserReload, Linear 1..200, right next to the camera
    // -> full volume). Its Stop() at the end of the 4 s fireTimer comes long after the short clip has finished.
    let charging = def
        .prefab
        .objects
        .iter()
        .flat_map(|o| o.components.iter())
        .filter(|(t, _)| t.as_str() == "AudioSource")
        .find_map(|(_, f)| f.asset("m_audioClip"));
    if let Some(c) = charging {
        sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
    }
    ts.ready = false; // TODO visuals
    let position = cam_g.translation();
    let direction = cam_g.forward().as_vec3();
    shoot_server(def, ctx, hit, player, position, direction, item.flash_pos, sounds, fx);
    ts.fire_timer = w.f("timeBetweenFire");
}

/// `ShootServer`: ShootServerEffect + OverlapBox(playerLayer) -> TaserEnemy.
#[allow(clippy::too_many_arguments)]
fn shoot_server(
    def: &WeaponDef,
    ctx: &RapierContext,
    hit: &mut HitCtx,
    player: Entity,
    position: Vec3,
    direction: Vec3,
    flash_pos: Option<Vec3>,
    sounds: &mut MessageWriter<WeaponSound>,
    fx: &mut MessageWriter<GunFx>,
) {
    let w = &def.w;
    // ShootServerEffect -> ShootObserversEffect: fireClip + muzzleFlash (at muzzleFlashPoint; approximated near the camera).
    if let Some(c) = w.asset("fireClip") {
        sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
    }
    fx.write(GunFx::Muzzle { at: flash_pos.unwrap_or(position + direction * 0.6), prefab: w.prefab_ref("muzzleFlash") });
    // Physics.OverlapBox(position + direction * (boxdimensions.z / 2), halfExtents = boxdimensions, LookRotation(direction), playerLayer)
    let dims = w.v3("boxdimensions");
    let center = position + direction * (dims.z / 2.0);
    let rot = Transform::IDENTITY.looking_to(direction.normalize_or_zero(), Vec3::Y).rotation;
    let mask = w.mask("playerLayer");
    let filter = QueryFilter::new().exclude_sensors().groups(CollisionGroups::new(Group::ALL, Group::from_bits_retain(mask)));
    let shape = Collider::cuboid(dims.x.abs(), dims.y.abs(), dims.z.abs());
    let mut found: Vec<Entity> = Vec::new();
    ctx.intersect_shape(center, rot, &*shape.raw, filter, |e| {
        found.push(e);
        true
    });
    // First collider with a PlayerHealth in its parents (HitPart.owner).
    let target = found.iter().find_map(|e| {
        let p = hit.parts.get(*e).ok()?;
        hit.healths.get(p.owner).ok()?;
        Some(p.owner)
    });
    if let Some(owner) = target {
        // Settings.Instance.taserShots += 1 (TODO stats)
        // enemyHealth.gameObject == transform.root.gameObject -> no stun on self.
        if owner != player {
            // TaserEnemy: canMove=false + TaserEnemyTarget -> UnfreezePlayer(stunTime)
            if let Ok(mut ph) = hit.healths.get_mut(owner) {
                ph.stun(w.f("stunTime"));
            }
        }
    }
}
