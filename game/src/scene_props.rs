//! Scripted objects of the loaded map (scene_map.rs): `Door` (InteractEnvironment: open / close tween) and `Pigeon`
//! (hops around, flies off when the player gets close, dies when shot). Both sit on the map's glb nodes, whose local
//! space is AssetRipper's X-mirrored one (a Unity Euler(0, a, 0) is a turn of -a about Y there).

use crate::player::fpc::Fpc;
use crate::tween::Ease;
use crate::unity::random_range_f;
use crate::weapons::data::{Fields, UField};
use crate::weapons::pickup::Hands;
use crate::weapons::WeaponSound;
use bevy::prelude::*;

/// `Door` (+ its AudioSource: Start sets maxDistance 34, spatialBlend 1).
#[derive(Component)]
pub struct Door {
    pub is_open: bool,
    pub popup_text: String,
    pub close_text: String,
    open_clip: Option<String>,
    max_rotation: f32,
    opening_time: f32,
    timer: f32,
    /// (from, to, elapsed) local rotation tween (DOLocalRotate, default ease OutQuad).
    tween: Option<(Quat, Quat, f32)>,
    has_audio: bool,
    /// The AudioSource's rolloff with Door.Start's maxDistance 34.
    rolloff: Option<std::sync::Arc<crate::audio::RolloffCurve>>,
}

impl Door {
    pub fn from_fields(f: &Fields, audio: Option<&Fields>) -> Door {
        let num = |k: &str, d: f32| f.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d);
        Door {
            is_open: f.b("isOpen"),
            popup_text: f.s("popupText"),
            close_text: f.get("closeDoor").and_then(|v| v.as_str()).unwrap_or("close door").to_string(),
            open_clip: f.asset("openClip"),
            max_rotation: num("maxRotation", 105.0),
            opening_time: num("doorOpeningTime", 0.33),
            timer: 0.0,
            tween: None,
            has_audio: audio.is_some(),
            rolloff: audio.and_then(|a| crate::audio::RolloffCurve::from_source(a, 34.0)).map(std::sync::Arc::new),
        }
    }
}

/// `Pigeon` state + serialized fields.
#[derive(Component)]
pub struct Pigeon {
    pub die_vfx: Option<String>,
    max_distance: f32,
    fly_time: f32,
    fly_speed: f32,
    vertical_movement: f32,
    random_rot: bool,
    animate: bool,
    fly_clip: Option<String>,
    fly_timer: f32,
    jump_timer: f32,
    value: f32,
    fly: bool,
    fly_dir: Vec3,
    init_pos: Option<Vec3>,
}

impl Pigeon {
    pub fn from_fields(f: &Fields) -> Pigeon {
        let num = |k: &str, d: f32| f.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d);
        Pigeon {
            die_vfx: f.asset("dieVfx"),
            max_distance: num("maxDistance", 5.0),
            fly_time: num("flyTime", 20.0),
            fly_speed: num("flySpeed", 2.0),
            vertical_movement: num("verticalMovement", 1.0),
            random_rot: f.get("randomRot").map(|v| v.as_i64() != Some(0)).unwrap_or(true),
            animate: f.get("animate").map(|v| v.as_i64() != Some(0)).unwrap_or(true),
            fly_clip: f.asset("flyClip"),
            fly_timer: num("flyTimer", 0.0),
            jump_timer: 3.0,
            value: 0.0,
            fly: false,
            fly_dir: Vec3::ZERO,
            init_pos: None,
        }
    }
}

/// Unity Euler(0, a, 0) as a glb-node local rotation.
fn glb_yaw(deg: f32) -> Quat {
    Quat::from_rotation_y(-deg.to_radians())
}

/// Door.OnInteract -> CmdInteract (timerdoor gate) -> TriggerDoor + isOpen toggle; Door.Update (tween + openClip).
fn door_update(
    time: Res<Time>,
    mut hands: Query<&mut Hands>,
    players: Query<&GlobalTransform, With<Fpc>>,
    targets: Query<&crate::props::EnvTarget>,
    mut doors: Query<(Entity, &mut Door, &mut Transform, &GlobalTransform)>,
    mut sounds: MessageWriter<WeaponSound>,
) {
    let dt = time.delta_secs();
    let interact = hands.iter_mut().next().and_then(|h| h.env_interact).map(|e| targets.get(e).map(|t| t.0).unwrap_or(e));
    let player = players.single().ok().map(|g| g.translation());
    for (e, mut d, mut tf, g) in &mut doors {
        // OnInteract (input, before Update): `if (!(timerdoor > 0))` CmdInteract -> TriggerDoor (trigger = true),
        // isOpen = !isOpen, localplayer = player. Door.Update then runs `timerdoor -= dt` before handling the trigger.
        let mut trigger = false;
        if interact == Some(e) && d.timer <= 0.0 {
            trigger = true;
            d.is_open = !d.is_open;
        }
        d.timer -= dt;
        if trigger {
            let side = player.map(|p| g.rotation().mul_vec3(Vec3::Z).dot(p - g.translation())).unwrap_or(1.0);
            let to = if d.is_open { glb_yaw(if side > 0.0 { d.max_rotation } else { -d.max_rotation }) } else { glb_yaw(0.0) };
            d.tween = Some((tf.rotation, to, 0.0));
            if d.timer < 0.0 {
                // Both directions play openClip.
                if let (true, Some(c)) = (d.has_audio, d.open_clip.clone()) {
                    sounds.write(WeaponSound { clip: c, volume: 1.0, at: Some(g.translation()), pitch: 1.0, linear_max: 0.0, source: None, rolloff: d.rolloff.clone() });
                }
                d.timer = 0.1;
            }
        }
        let opening = d.opening_time.max(1e-3);
        if let Some((from, to, t)) = d.tween.as_mut() {
            *t += dt;
            let k = (*t / opening).min(1.0);
            tf.rotation = from.slerp(*to, Ease::OutQuad.eval(k));
            if k >= 1.0 {
                d.tween = None;
            }
        }
    }
}

/// Pigeon.Update (single player: `player` = the local player).
fn pigeon_update(
    time: Res<Time>,
    players: Query<&GlobalTransform, With<crate::player::fpc::Fpc>>,
    parents: Query<&ChildOf>,
    globals: Query<&GlobalTransform>,
    mut pigeons: Query<(Entity, &mut Pigeon, &mut Transform, &GlobalTransform, Option<&mut crate::weapons::anim::NodeAnimator>)>,
    mut sounds: MessageWriter<WeaponSound>,
) {
    let dt = time.delta_secs();
    // player.transform.position: the player root.
    let Ok(root) = players.single() else { return };
    let player = root.translation();
    for (e, mut p, mut tf, g, anim) in &mut pigeons {
        let parent = parents.get(e).ok().and_then(|c| globals.get(c.parent()).ok()).copied().unwrap_or_default();
        let to_local = |w: Vec3| parent.affine().inverse().transform_point3(w);
        let pos = g.translation();
        if p.init_pos.is_none() {
            // Start: random yaw, initPos.
            p.jump_timer = random_range_f(1.0, 2.0);
            if p.random_rot {
                tf.rotation = glb_yaw(random_range_f(0.0, 360.0).floor());
            }
            p.init_pos = Some(pos);
        }
        let dist = pos.distance(player);
        p.fly_timer -= dt;
        // anim.SetBool("fly", fly) (before this frame's take-off check).
        if let Some(mut a) = anim {
            a.bools.insert("fly".into(), p.fly);
        }
        if dist < p.max_distance && !p.fly {
            if let Some(c) = p.fly_clip.clone() {
                sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
            }
            p.fly_dir = -(player - pos).normalize_or_zero();
            // transform.rotation = LookRotation(flyDir): world rotation -> node local (port = G * Y180^-1).
            let world = Transform::IDENTITY.looking_to(p.fly_dir, Vec3::Y).rotation * Quat::from_rotation_y(std::f32::consts::PI);
            tf.rotation = parent.rotation().inverse() * world;
            p.fly = true;
            p.fly_timer = p.fly_time;
        }
        let mut world_pos = pos;
        if p.fly {
            world_pos += Vec3::new(p.fly_dir.x * dt * p.fly_speed, p.vertical_movement * dt, p.fly_dir.z * dt * p.fly_speed);
        } else if p.animate {
            p.jump_timer -= dt;
            if p.jump_timer < 0.1 && p.jump_timer > 0.0 {
                p.value = random_range_f(-1.0, 1.0).floor(); // Random.Range(-1, 1) (int): -1 or 0
                p.jump_timer = 0.0;
            }
            if p.jump_timer < 0.0 {
                // transform.Rotate(0, +-290 * dt, 0) (local)
                let a = if p.value > 0.0 { 290.0 * dt } else { -290.0 * dt };
                tf.rotation *= glb_yaw(a);
                if p.jump_timer > -0.2 {
                    world_pos += Vec3::Y * 2.0 * dt;
                } else if p.jump_timer > -0.35 {
                    world_pos -= Vec3::Y * 2.0 * dt;
                } else {
                    p.jump_timer = random_range_f(2.0, 8.0);
                }
            }
        }
        if let Some(init) = p.init_pos {
            if p.fly_timer < 0.0 && init.distance(player) > 20.0 {
                p.fly = false;
                world_pos = init;
            }
        }
        if world_pos != pos {
            tf.translation = to_local(world_pos);
        }
    }
}

pub struct ScenePropsPlugin;

impl Plugin for ScenePropsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (door_update.after(crate::weapons::pickup::player_pickup_update), pigeon_update));
    }
}
