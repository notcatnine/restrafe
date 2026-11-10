//! Scripted input playback for verifying the port against expected values from the C# logic.
//! Run with `RESTRAFE_AUTOTEST=<name>`; logs player state to stdout and exits when done.

use crate::input::{Button, PlayerInput};
use crate::player::fpc::Fpc;
use bevy::prelude::*;

#[derive(Resource)]
pub struct AutoTest {
    pub name: String,
    pub t: f32,
    pub log_t: f32,
    pub prev: [bool; 4],
    /// Set once the world is loaded and frame times have settled.
    pub ready: bool,
    pub good_frames: u32,
}

/// (start, end, move, sprint, jump, crouch)
type Step = (f32, f32, Vec2, bool, bool, bool);

fn script(name: &str) -> Vec<Step> {
    let f = Vec2::Y;
    match name {
        // walk -> sprint -> sprint jump -> sprint crouch-slide
        "move" => vec![
            (0.0, 0.5, Vec2::ZERO, false, false, false),
            (0.5, 2.5, f, false, false, false),
            (2.5, 4.5, f, true, false, false),
            (4.5, 4.55, f, true, true, false),
            (4.55, 6.0, f, true, false, false),
            (6.0, 7.5, f, true, false, true),
            (7.5, 8.0, Vec2::ZERO, false, false, false),
        ],
        // steady walk / sprint (backwards) / crouch-walk speeds, 2.5 s each with stops between; compare `log` hspd and
        // x/z deltas with the C# speedFactor lerp
        "speeds" => vec![
            (0.0, 0.5, Vec2::ZERO, false, false, false),
            (0.5, 3.0, f, false, false, false),
            (3.0, 4.0, Vec2::ZERO, false, false, false),
            (4.0, 6.5, -f, true, false, false),
            (6.5, 7.5, Vec2::ZERO, false, false, false),
            (7.5, 7.6, Vec2::ZERO, false, false, true),
            (7.6, 10.1, f, false, false, true),
            (10.1, 10.5, Vec2::ZERO, false, false, true),
        ],
        // stand -> jump (settles the land bob like a real spawn) -> crouch (held) -> stand -> sprint slide; see `heights_log`
        "heights" => vec![
            (0.0, 1.0, Vec2::ZERO, false, false, false),
            (1.0, 1.05, Vec2::ZERO, false, true, false),
            (1.05, 3.0, Vec2::ZERO, false, false, false),
            (3.0, 5.0, Vec2::ZERO, false, false, true),
            (5.0, 6.5, Vec2::ZERO, false, false, false),
            (6.5, 8.5, f, true, false, false),
            (8.5, 9.6, f, true, false, true),
            (9.6, 10.5, Vec2::ZERO, false, false, false),
        ],
        // high fall looking down (highFallVfx + lens distortion), see `fall_test`
        "fall" => vec![(0.0, 3.0, Vec2::ZERO, false, false, false)],
        // standing jump only (apex height check)
        "jump" => vec![
            (0.0, 0.5, Vec2::ZERO, false, false, false),
            (0.5, 0.55, Vec2::ZERO, false, true, false),
            (0.55, 2.0, Vec2::ZERO, false, false, false),
        ],
        // run at wall WJ_A (+X), jump into it, wall-jump off it
        "walljump" => vec![
            (0.0, 0.5, Vec2::ZERO, false, false, false),
            (0.5, 0.8, f, true, false, false),
            (0.8, 0.85, f, true, true, false),
            (0.85, 1.05, f, true, false, false),
            (1.05, 1.1, f, true, true, false),
            (1.1, 2.5, Vec2::ZERO, false, false, false),
        ],
        // run at wall WJ_A (+X), jump into it once and keep pushing into it (must slide down / fall, not stick)
        "wallstick" => vec![
            (0.0, 0.5, Vec2::ZERO, false, false, false),
            (0.5, 0.8, f, true, false, false),
            (0.8, 0.85, f, true, true, false),
            (0.85, 3.0, f, true, false, false),
        ],
        _ => vec![],
    }
}

pub fn drive(
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    mut exit: MessageWriter<AppExit>,
    mut commands: Commands,
    mut fpcs: Query<&mut Fpc>,
    (cams, rapier): (Query<&GlobalTransform, With<crate::player::fpc::PlayerCamera>>, bevy_rapier3d::prelude::ReadRapierContext),
) {
    if at.name == "glock" || at.name == "taunt" || at.name == "katanawj" || at.name == "mapdisc" || at.name == "disc" || at.name == "ramp" || at.name == "ledge" || at.name == "steep" || at.name == "weaponmenu" || at.name == "barrel" || at.name == "mapview" || at.name == "door" || at.name == "pigeon" || at.name == "wallshot" || at.name == "walk" || at.name == "suicide" || at.name == "shots" || at.name == "dummyshot" || at.name == "menushot" || at.name == "cosmeticshot" || at.name == "hudturn" || at.name == "hands" {
        return;
    }
    if !at.ready {
        return;
    }
    let t0 = at.t;
    at.t += time.delta_secs();
    // RESTRAFE_DRIVE_SHOT=<t>[,<t>...]: screenshots at script times -> target/shots/drive_N.png;
    // RESTRAFE_DRIVE_PITCH=<deg>: camera pitch held at that value (rotationX, + = down).
    if let Ok(list) = std::env::var("RESTRAFE_DRIVE_SHOT") {
        for (i, s) in list.split(',').filter_map(|v| v.parse::<f32>().ok()).enumerate() {
            if t0 < s && at.t >= s {
                use bevy::render::view::screenshot::{Screenshot, save_to_disk};
                let _ = std::fs::create_dir_all("target/shots");
                commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/drive_{i}.png")));
                if let (Ok(c), Ok(ctx)) = (cams.single(), rapier.single()) {
                    let p = c.translation();
                    let near = [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z, Vec3::Y, Vec3::NEG_Y]
                        .iter()
                        .filter_map(|d| crate::physics::raycast(&ctx, p, *d, 2.0, crate::physics::DEFAULT_RAYCAST_LAYERS).map(|h| h.distance))
                        .fold(f32::INFINITY, f32::min);
                    let inside = crate::physics::raycast(&ctx, p + c.forward().as_vec3() * 1.0, c.back().as_vec3(), 1.0, crate::physics::DEFAULT_RAYCAST_LAYERS).map(|h| h.distance);
                    println!("DRIVESHOT {i} t={s:.2} cam={p:.3} nearest={near:.3} back_hit={inside:?} fwd={:.2}", c.forward());
                }
            }
        }
    }
    if let Some(p) = std::env::var("RESTRAFE_DRIVE_PITCH").ok().and_then(|v| v.parse::<f32>().ok()) {
        for mut f in &mut fpcs {
            f.rotation_x = p;
        }
    }
    let steps = script(&at.name);
    let Some(end) = steps.last().map(|s| s.1) else {
        exit.write(AppExit::Success);
        return;
    };
    if at.t > end {
        exit.write(AppExit::Success);
        return;
    }
    let (mv, run, jump, crouch) = steps
        .iter()
        .find(|s| at.t >= s.0 && at.t < s.1)
        .map(|s| (s.2, s.3, s.4, s.5))
        .unwrap_or_default();
    let mk = |held: bool, prev: bool| Button::digital(held, held && !prev, !held && prev);
    let p = at.prev;
    *input = PlayerInput {
        enabled: true,
        move_raw: mv,
        run: mk(run, p[0]),
        jump: mk(jump, p[1]),
        crouch: mk(crouch, p[2]),
        ..default()
    };
    at.prev = [run, jump, crouch, false];
}

/// `heights` test: camera / player-root height above the floor (ray 0.7 m beside the player) every 0.25 s.
pub fn heights_log(
    time: Res<Time>,
    at: Res<AutoTest>,
    q: Query<(&Fpc, &GlobalTransform)>,
    cam: Query<&GlobalTransform, With<crate::player::fpc::PlayerCamera>>,
    rapier: bevy_rapier3d::prelude::ReadRapierContext,
    mut acc: Local<f32>,
) {
    if at.name != "heights" || !at.ready {
        return;
    }
    *acc += time.delta_secs();
    if *acc < 0.25 {
        return;
    }
    *acc = 0.0;
    let (Ok((f, g)), Ok(c), Ok(ctx)) = (q.single(), cam.single(), rapier.single()) else { return };
    let o = g.translation() + Vec3::new(0.7, 1.0, 0.0);
    let Some(h) = crate::physics::raycast(&ctx, o, Vec3::NEG_Y, 5.0, crate::physics::DEFAULT_RAYCAST_LAYERS) else { return };
    println!(
        "HEIGHT t={:5.2} eye={:.3} root={:.3} cc_h={:.3} skin={:.3} crouch={} slide={}",
        at.t,
        c.translation().y - h.point.y,
        g.translation().y - h.point.y,
        f.cc_height,
        f.cc_skin,
        f.is_crouching as u8,
        f.is_sliding as u8
    );
}

pub fn log(time: Res<Time>, mut at: ResMut<AutoTest>, q: Query<(&Fpc, &Transform)>) {
    let Ok((f, tf)) = q.single() else { return };
    at.log_t += time.delta_secs();
    if at.log_t < 0.1 {
        return;
    }
    at.log_t = 0.0;
    let hv = Vec2::new(f.cc_velocity.x, f.cc_velocity.z).length();
    println!(
        "t={:5.2} x={:6.2} z={:6.2} cwj={} wjc={} y={:6.3} hspd={:6.3} vy={:7.3} grounded={} sprint={} crouch={} slide={} speedFactor={:5.2} forceFactor={:6.2} h={:4.2} fov={:5.1}",
        at.t,
        tf.translation.x,
        tf.translation.z,
        f.can_wall_jump as u8,
        f.wall_jumps_count,
        tf.translation.y,
        hv,
        f.cc_velocity.y,
        f.cc_grounded as u8,
        f.is_sprinting as u8,
        f.is_crouching as u8,
        f.is_sliding as u8,
        f.speed_factor,
        f.force_factor,
        f.cc_height,
        f.fov
    );
}

/// Places the player at the test's start position on the first frame.
pub fn place(mut done: Local<bool>, at: Res<AutoTest>, mut q: Query<&mut Transform, With<Fpc>>) {
    if *done {
        return;
    }
    let Ok(mut tf) = q.single_mut() else { return };
    *done = true;
    if at.name == "walljump" || at.name == "wallstick" {
        // 2.6 m west of WJ_A's face, facing +X (Unity yaw 90).
        tf.translation = Vec3::new(-17.0, 0.2, -18.0);
        tf.rotation = crate::unity::unity_euler(0.0, 90.0, 0.0);
    }
}

/// Weapon autotest: pick up the Glock from the rack, face the dummy, fire 6 single shots.
#[allow(clippy::too_many_arguments)]
pub fn weapon_test(
    mut commands: Commands,
    mut kill_t: Local<Option<f32>>,
    mut ragshots: Local<u32>,
    mut fx_shots: Local<(Option<f32>, u32)>,
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    rack: Option<Res<crate::map::WeaponRack>>,
    mut players: Query<(&mut Transform, &mut Fpc, Option<&crate::weapons::pickup::Hands>)>,
    items: Query<&crate::weapons::Item>,
    rapier: bevy_rapier3d::prelude::ReadRapierContext,
    loose: Query<(&crate::weapons::Item, &GlobalTransform, Option<&bevy_rapier3d::prelude::RigidBody>, Option<&bevy_rapier3d::prelude::Velocity>, Has<bevy_rapier3d::prelude::ColliderDisabled>, Has<bevy_rapier3d::prelude::RigidBodyDisabled>, Has<ChildOf>, Option<&bevy_rapier3d::prelude::RapierRigidBodyHandle>)>,
    dolls: Query<(&crate::health::PlayerHealth, &Transform), (With<crate::health::Dummy>, Without<Fpc>)>,
    mut exit: MessageWriter<AppExit>,
    (mut step, mut give, rags, parts): (Local<u32>, MessageWriter<crate::weapon_menu::GiveWeapon>, Query<&GlobalTransform, With<crate::ragdoll::RagdollBody>>, Query<(&crate::health::HitPart, &GlobalTransform)>),
) {
    if at.name != "glock" {
        return;
    }
    if !at.ready {
        return;
    }
    at.t += time.delta_secs();
    let Ok((mut tf, mut fpc, hands)) = players.single_mut() else { return };
    let t = at.t;
    // RESTRAFE_RAGDOLL=1: screenshots after the dummy dies (ragdoll check) -> target/shots/ragdoll_N.png
    if std::env::var_os("RESTRAFE_RAGDOLL").is_some() {
        use bevy::render::view::screenshot::{Screenshot, save_to_disk};
        if kill_t.is_none() && dolls.iter().any(|d| d.0.dead) {
            *kill_t = Some(t);
        }
        const AT: [f32; 7] = [0.15, 0.4, 0.8, 1.5, 2.5, 4.0, 5.0];
        if let Some(k) = *kill_t {
            if (*ragshots as usize) < AT.len() && t >= k + AT[*ragshots as usize] {
                let _ = std::fs::create_dir_all("target/shots");
                commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/ragdoll_{}.png", *ragshots)));
                *ragshots += 1;
            }
        }
    }
    // RESTRAFE_IDLE_SHOT=<s>: one screenshot at test time s (before the first shot, e.g. 2.5) -> target/shots/idle.png
    // (comma list: RESTRAFE_IDLE_SHOT=0.3,2,5 -> idle.png, idle_1.png, idle_2.png)
    if let Ok(list) = std::env::var("RESTRAFE_IDLE_SHOT") {
        for (i, s) in list.split(',').filter_map(|v| v.parse::<f32>().ok()).enumerate() {
            if t >= s && t - time.delta_secs() < s {
                let _ = std::fs::create_dir_all("target/shots");
                use bevy::render::view::screenshot::{Screenshot, save_to_disk};
                let name = if i == 0 { "target/shots/idle.png".to_string() } else { format!("target/shots/idle_{i}.png") };
                commands.spawn(Screenshot::primary_window()).observe(save_to_disk(name));
            }
        }
    }
    // RESTRAFE_SHOT_AT=<s>[,<n>]: n screenshots (0.03 s apart) starting <s> after the first shot -> target/shots/fx_N.png
    if let (Some(first), Ok(spec)) = (fx_shots.0, std::env::var("RESTRAFE_SHOT_AT")) {
        use bevy::render::view::screenshot::{Screenshot, save_to_disk};
        let mut it = spec.split(',');
        let at_s: f32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.05);
        let n: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(1);
        if fx_shots.1 < n && t >= first + at_s + fx_shots.1 as f32 * 0.03 {
            let _ = std::fs::create_dir_all("target/shots");
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/fx_{}.png", fx_shots.1)));
            fx_shots.1 += 1;
        }
    }
    *input = PlayerInput { enabled: true, ..default() };
    // RESTRAFE_DROP_AT=<t>: press Interact at test time t (nothing targeted -> drop); logs loose (physics) items.
    if let Some(d) = std::env::var("RESTRAFE_DROP_AT").ok().and_then(|s| s.parse::<f32>().ok()) {
        if t >= d && t - time.delta_secs() < d {
            input.interact = Button::digital(true, true, false);
        }
        if t >= d && (t * 10.0).floor() != ((t - time.delta_secs()) * 10.0).floor() {
            for (it, g, rb, vel, cd, rbd, child, h) in loose.iter() {
                if it.physics {
                    if let (Some(h), Ok(ctx)) = (h, rapier.single()) {
                        if let Some(b) = ctx.rigidbody_set.bodies.get(h.0) {
                            println!("BODY dyn={} sleep={} enabled={} mass={} ncoll={} linvel={:?} gscale={} ccd={} locked={:?} pos={:?}", b.is_dynamic(), b.is_sleeping(), b.is_enabled(), b.mass(), b.colliders().len(), b.linvel(), b.gravity_scale(), b.is_ccd_enabled(), b.locked_axes(), b.translation());
                        }
                    }
                    println!("LOOSE t={t:.2} {} pos=({:.2},{:.2},{:.2}) ammo={} rb={rb:?} vel={vel:?} coll_dis={cd} rb_dis={rbd} child={child} handle={}", it.def.name, g.translation().x, g.translation().y, g.translation().z, it.current_ammo, h.is_some());
                }
            }
        }
    }
    // RESTRAFE_GIVE2=<weapon>@<t>: weapon-menu give at test time t (drops what is held); logs the loose items after.
    if let Some((w, gt)) = std::env::var("RESTRAFE_GIVE2").ok().and_then(|v| v.split_once('@').map(|(a, b)| (a.to_string(), b.parse::<f32>().unwrap_or(3.0)))) {
        if t >= gt && t - time.delta_secs() < gt {
            give.write(crate::weapon_menu::GiveWeapon(w));
        }
        if t >= gt && t < gt + 2.0 && (t * 10.0).floor() != ((t - time.delta_secs()) * 10.0).floor() {
            let r = hands.and_then(|h| h.right).and_then(|e| items.get(e).ok()).map(|i| i.def.name.clone());
            for (it, g, _, vel, ..) in loose.iter() {
                if it.physics {
                    println!("GIVE2 t={t:.2} right={r:?} loose={} pos=({:.2},{:.2},{:.2}) vel={:?}", it.def.name, g.translation().x, g.translation().y, g.translation().z, vel.map(|v| v.linear));
                }
            }
        }
    }
    // RESTRAFE_ZOOM=a,b: hold Zoom (aim) during [a,b); logs FOVLOG lines (camera FOV) every frame in [a, b+0.5).
    if let Some(v) = std::env::var("RESTRAFE_ZOOM").ok().map(|s| s.split(',').filter_map(|x| x.parse::<f32>().ok()).collect::<Vec<f32>>()) {
        if v.len() == 2 {
            if t >= v[0] && t < v[1] {
                input.zoom = Button::digital(true, t - time.delta_secs() < v[0], false);
            }
            if t >= v[0] && t < v[1] + 0.5 {
                println!("FOVLOG t={:.3} aiming={} fov={:.2}", t - v[0], fpc.is_aiming as u8, fpc.fov);
            }
        }
    }
    // RESTRAFE_WALK=a,b[,c,d...]: walk forward during [a,b), [c,d), ... (test time; e.g. onto / off a placed mine).
    if let Ok(spec) = std::env::var("RESTRAFE_WALK") {
        let v: Vec<f32> = spec.split(',').filter_map(|s| s.parse().ok()).collect();
        if v.chunks(2).any(|w| w.len() == 2 && t >= w[0] && t < w[1]) {
            input.move_raw = Vec2::Y;
        }
        if (t * 4.0).floor() != ((t - time.delta_secs()) * 4.0).floor() {
            println!("WALK t={t:.2} pos=({:.2},{:.2},{:.2})", tf.translation.x, tf.translation.y, tf.translation.z);
        }
    }
    if std::env::var_os("RESTRAFE_HOLD").is_some() && (3..15).contains(&*step) && t > 2.2 {
        input.fire = Button::digital(true, false, false);
    }
    // RESTRAFE_BODYGRAB_AT=<t1>,<t2> (with a Taser): aim at the dummy's chest from t1 - 0.3 and press Interact at t1
    // (PlayerGrab on a stunned body), turn 90 deg/s, press again at t2 (release + bump); BODYGRAB lines log it.
    if let Some(g) = std::env::var("RESTRAFE_BODYGRAB_AT").ok().map(|s| s.split(',').filter_map(|v| v.parse::<f32>().ok()).collect::<Vec<_>>()).filter(|v| v.len() == 2) {
        let dt = time.delta_secs();
        if let Some((ph, d)) = dolls.iter().next() {
            if t > g[0] - 0.3 && t < g[0] + 0.05 {
                let away = Vec3::new(tf.translation.x - d.translation.x, 0.0, tf.translation.z - d.translation.z).normalize_or(Vec3::Z);
                tf.translation = Vec3::new(d.translation.x, 0.2, d.translation.z) + away * 1.5;
                // The Body collider sticking out farthest from the 0.4 m CharacterController capsule axis (the capsule
                // stops the interaction ray).
                let far = parts.iter().filter(|(p, _)| dolls.iter().any(|_| true) && p.owner != Entity::PLACEHOLDER).map(|(_, g)| g.translation()).filter(|p| p.y > 0.6 && p.y < 1.9).max_by(|a, b| {
                    let r = |p: &Vec3| Vec2::new(p.x - d.translation.x, p.z - d.translation.z).length();
                    r(a).total_cmp(&r(b))
                });
                let side = far.filter(|_| std::env::var_os("RESTRAFE_BODYGRAB_FAR").is_some()).map(|f| f - d.translation - Vec3::Y * 1.0).unwrap_or(Vec3::ZERO);
                let aim = d.translation + Vec3::Y * 1.0 + side - (tf.translation + Vec3::Y * 1.83);
                tf.rotation = crate::unity::unity_euler(0.0, aim.x.atan2(-aim.z).to_degrees(), 0.0);
                fpc.rotation_x = (-aim.y).atan2(Vec2::new(aim.x, aim.z).length()).to_degrees();
            } else if t > g[0] && t < g[1] {
                tf.rotate_y(-90f32.to_radians() * dt);
            }
            if (t >= g[0] && t - dt < g[0]) || (t >= g[1] && t - dt < g[1]) {
                input.interact = Button::digital(true, true, false);
            }
            if t > g[0] - 0.5 && (t * 5.0).floor() != ((t - dt) * 5.0).floor() {
                println!("BODYGRAB t={t:.2} dummy=({:.2},{:.2},{:.2}) stunned={} player=({:.2},{:.2})", d.translation.x, d.translation.y, d.translation.z, ph.stunned, tf.translation.x, tf.translation.z);
            }
            if t > g[0] - 0.3 && t < g[1] + 2.0 {
                return;
            }
        }
    }
    // RESTRAFE_GRAB_AT=<t1>,<t2>: aim at the ragdoll until t1 and press Interact (PlayerGrab), turn 90 deg/s while it is
    // held, press again at t2 (throw); GRAB lines log the ragdoll centre every 0.2 s.
    if let Some(g) = std::env::var("RESTRAFE_GRAB_AT").ok().map(|s| s.split(',').filter_map(|v| v.parse::<f32>().ok()).collect::<Vec<_>>()).filter(|v| v.len() == 2) {
        let dt = time.delta_secs();
        let n = rags.iter().count();
        if n > 0 {
            let c = rags.iter().map(|g| g.translation()).sum::<Vec3>() / n as f32;
            if t < g[0] {
                if t > g[0] - 0.5 {
                    // Stand 1.2 m from the ragdoll (within interactionDistance).
                    let away = Vec3::new(tf.translation.x - c.x, 0.0, tf.translation.z - c.z).normalize_or(Vec3::Z);
                    tf.translation = Vec3::new(c.x, 0.2, c.z) + away * 0.7;
                }
                let top = rags.iter().map(|g| g.translation()).max_by(|a, b| a.y.total_cmp(&b.y)).unwrap_or(c);
                let d = top - (tf.translation + Vec3::Y * 1.83);
                tf.rotation = crate::unity::unity_euler(0.0, d.x.atan2(-d.z).to_degrees(), 0.0);
                fpc.rotation_x = (-d.y).atan2(Vec2::new(d.x, d.z).length()).to_degrees();
            } else if t < g[1] {
                tf.rotate_y(-90f32.to_radians() * dt);
                fpc.rotation_x = 0.0;
            }
            if (t >= g[0] && t - dt < g[0]) || (t >= g[1] && t - dt < g[1]) {
                input.interact = Button::digital(true, true, false);
            }
            if t > g[0] - 0.5 && (t * 5.0).floor() != ((t - dt) * 5.0).floor() {
                let cam = tf.translation + Vec3::Y * 1.83;
                println!("GRAB t={t:.2} centre=({:.2},{:.2},{:.2}) dist_from_eye={:.2}", c.x, c.y, c.z, c.distance(cam));
            }
        }
        if *step >= 15 {
            return;
        }
    }
    let glock_pos = rack.as_ref().and_then(|r| r.slots.iter().find(|s| s.0 == std::env::var("RESTRAFE_WEAPON").unwrap_or("Glock".into())).map(|s| s.1));
    if *step == 0 && t > 0.5 {
        // Not on the test rack: give it like the weapon menu does.
        let Some(g) = glock_pos else {
            give.write(crate::weapon_menu::GiveWeapon(std::env::var("RESTRAFE_WEAPON").unwrap_or("Glock".into())));
            *step = 2;
            return;
        };
        // 2.5 m south of the slot, facing it (+Z in Bevy = Unity yaw 180).
        tf.translation = Vec3::new(g.x, 0.2, g.z - 1.2);
        tf.rotation = crate::unity::unity_euler(0.0, 180.0, 0.0);
        fpc.rotation_x = 35.0;
        *step = 1;
    } else if *step == 1 && t > 1.0 {
        input.interact = Button::digital(true, true, false);
        *step = 2;
    } else if *step == 2 && t > 1.6 {
        let r = hands.and_then(|h| h.right).and_then(|e| items.get(e).ok()).map(|i| i.def.name.clone());
        println!("RESULT right_hand={r:?}");
        tf.translation = Vec3::new(std::env::var("RESTRAFE_STANDX").ok().and_then(|s| s.parse().ok()).unwrap_or(-8.0), 0.2, std::env::var("RESTRAFE_STANDZ").ok().and_then(|s| s.parse().ok()).unwrap_or(-2.0));
        tf.rotation = crate::unity::unity_euler(0.0, std::env::var("RESTRAFE_YAW").ok().and_then(|s| s.parse().ok()).unwrap_or(0.0), 0.0);
        fpc.rotation_x = std::env::var("RESTRAFE_PITCH").ok().and_then(|s| s.parse().ok()).unwrap_or(12.0);
        *step = 3;
    } else if (3..15).contains(&*step) && t > 2.2 + (*step - 3) as f32 * 0.6 {
        // alternate press / release -> 6 clicks
        let down = (*step - 3) % 2 == 0 || std::env::var_os("RESTRAFE_HOLD").is_some();
        if *step == 3 {
            fx_shots.0 = Some(t);
        }
        // RESTRAFE_AIM_RAGDOLL=1: once a ragdoll exists, aim each shot at its body centre (ragdoll-hit FX check).
        if down && std::env::var_os("RESTRAFE_AIM_RAGDOLL").is_some() && !rags.is_empty() {
            let n = rags.iter().count() as f32;
            let c = rags.iter().map(|g| g.translation()).sum::<Vec3>() / n;
            let d = c - (tf.translation + Vec3::Y * 1.83);
            tf.rotation = crate::unity::unity_euler(0.0, d.x.atan2(-d.z).to_degrees(), 0.0);
            fpc.rotation_x = (-d.y).atan2(Vec2::new(d.x, d.z).length()).to_degrees();
        }
        input.fire = Button::digital(down, down, !down);
        if std::env::var_os("RESTRAFE_AIM").is_some() {
            input.zoom = Button::digital(true, *step == 3, false);
        }
        if !down || std::env::var_os("RESTRAFE_HOLD").is_some() {
            let ammo = hands.and_then(|h| h.right).and_then(|e| items.get(e).ok()).map(|i| i.current_ammo);
            let hp: Vec<f32> = dolls.iter().map(|d| d.0.health).collect();
            let dp: Vec<String> = dolls.iter().map(|d| format!("({:.2},{:.2},{:.2})", d.1.translation.x, d.1.translation.y, d.1.translation.z)).collect();
            let n = rags.iter().count().max(1) as f32;
            let rc = rags.iter().map(|g| g.translation()).sum::<Vec3>() / n;
            println!("RESULT click={} ammo={ammo:?} doll_hp={hp:?} doll_pos={dp:?} ragdoll_bodies={} ragdoll_mean=({:.2},{:.2},{:.2})", (*step - 3) / 2 + 1, rags.iter().count(), rc.x, rc.y, rc.z);
        }
        *step += 1;
    } else if *step == 15 && t > 9.0 && (std::env::var_os("RESTRAFE_RAGDOLL").is_none() || *ragshots >= 7) {
        exit.write(AppExit::Success);
    }
}

/// Screenshot autotest: rack view, then Glock in hand. Saves to target/shots/*.png.
pub fn shot_test(
    mut commands: Commands,
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    rack: Option<Res<crate::map::WeaponRack>>,
    mut players: Query<(&mut Transform, &mut Fpc)>,
    mut exit: MessageWriter<AppExit>,
    mut step: Local<u32>,
) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    if at.name == "dummyshot" {
        if !at.ready {
            return;
        }
        at.t += time.delta_secs();
        if let Ok((mut tf, mut fpc)) = players.single_mut() {
            *input = PlayerInput { enabled: true, ..default() };
            let sp: Vec<f32> = std::env::var("RESTRAFE_SHOT_POS").ok().map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect()).unwrap_or_default();
            tf.translation = if sp.len() == 3 { Vec3::new(sp[0], 0.2, sp[1]) } else { Vec3::new(-8.0, 0.2, -3.0) };
            tf.rotation = crate::unity::unity_euler(0.0, if sp.len() == 3 { sp[2] } else { 0.0 }, 0.0);
            fpc.rotation_x = 12.0;
        }
        if at.t > 3.0 && *step == 0 {
            let _ = std::fs::create_dir_all("target/shots");
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/dummy.png"));
            *step = 1;
        }
        if at.t > 3.6 {
            exit.write(AppExit::Success);
        }
        return;
    }
    if at.name != "shots" {
        return;
    }
    if !at.ready {
        return;
    }
    at.t += time.delta_secs();
    let t = at.t;
    let Ok((mut tf, mut fpc)) = players.single_mut() else { return };
    *input = PlayerInput { enabled: true, ..default() };
    let g = rack.as_ref().and_then(|r| r.slots.iter().find(|s| s.0 == std::env::var("RESTRAFE_WEAPON").unwrap_or("Glock".into())).map(|s| s.1)).unwrap_or(Vec3::ZERO);
    let _ = std::fs::create_dir_all("target/shots");
    match *step {
        0 if t > 0.5 => {
            tf.translation = Vec3::new(g.x, 0.2, g.z - 3.5);
            tf.rotation = crate::unity::unity_euler(0.0, 180.0, 0.0);
            fpc.rotation_x = 15.0;
            *step = 1;
        }
        1 if t > 4.0 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/rack.png"));
            *step = 2;
        }
        2 if t > 4.5 => {
            tf.translation = Vec3::new(g.x, 0.2, g.z - 2.5);
            fpc.rotation_x = 20.0;
            *step = 3;
        }
        3 if t > 4.8 => {
            input.interact = Button::digital(true, true, false);
            *step = 4;
        }
        4 if t > 6.0 => {
            fpc.rotation_x = 0.0;
            if std::env::var("RESTRAFE_WALL").is_ok() {
                // Stand against the west face of WJ_A (x=-14.5), looking at it, to check the viewmodel does not clip.
                tf.translation = Vec3::new(-14.95, 0.2, -18.0);
                tf.rotation = crate::unity::unity_euler(0.0, 90.0, 0.0);
                *step = 5;
                return;
            }
            tf.rotation = crate::unity::unity_euler(0.0, 0.0, 0.0);
            *step = 5;
        }
        5 if t > 7.0 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/held.png"));
            *step = 6;
        }
        6 if t > 7.6 => {
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}

/// Debug: list weapons near the camera (RESTRAFE_DEBUG_NEAR=1).
pub fn debug_near(cams: Query<&GlobalTransform, With<crate::player::fpc::PlayerCamera>>, items: Query<(&crate::weapons::Item, &GlobalTransform, &Transform, Option<&ChildOf>)>, mut t: Local<f32>, time: Res<Time>) {
    *t += time.delta_secs();
    if *t < 1.0 || std::env::var_os("RESTRAFE_DEBUG_NEAR").is_none() {
        return;
    }
    *t = 0.0;
    let Ok(c) = cams.single() else { return };
    for (it, g, l, parent) in &items {
        let d = g.translation().distance(c.translation());
        if d < 3.0 {
            println!("NEAR {} d={d:.2} hand={:?} world={:?} local={:?} parent={:?} scale={:?}", it.def.name, it.hand, g.translation(), l.translation, parent.map(|p| p.0), g.scale());
        }
    }
}

/// Settings / pause UI walkthrough (RESTRAFE_AUTOTEST=menushot): Escape, hover, open Settings, every tab, a dropdown,
/// a slider drag, close. Screenshots: target/shots/menu_*.png (menu.png = pause screen).
#[allow(clippy::too_many_arguments)]
pub fn menu_shot(
    mut commands: Commands,
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut exit: MessageWriter<AppExit>,
    mut step: Local<usize>,
    mut wait: Local<f32>,
    mut release: Local<Option<f32>>,
    (mut tree, mut simpad, mut pad_release): (ResMut<crate::ugui::UiTree>, ResMut<crate::input::SimPad>, Local<Option<f32>>),
    mut sim: ResMut<crate::ugui::SimPointer>,
    settings: Res<crate::settings::Settings>,
    (cos, cos_data, dressed): (Res<crate::cosmetics::Cosmetics>, Res<crate::cosmetics::CosmeticsData>, Query<(Entity, &crate::cosmetics::DressState, Has<crate::health::PreviewBody>)>),
) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    if (at.name != "menushot" && at.name != "cosmeticshot") || !at.ready {
        return;
    }
    let dt = time.delta_secs();
    at.t += dt;
    sim.down = false;
    sim.wheel = 0.0;
    sim.up = false;
    *wait -= dt;
    if *wait > 0.0 {
        return;
    }
    if let Some(after) = pad_release.take() {
        simpad.sample = crate::input::PadSample::default();
        *wait = after;
        return;
    }
    if let Some(after) = release.take() {
        sim.up = true;
        sim.held = false;
        *wait = after;
        return;
    }
    // (action, argument, wait after)
    const SCRIPT: &[(&str, &str, f32)] = &[
        ("wait", "", 1.5),
        ("key", "esc", 0.05),
        ("keyup", "esc", 1.0),
        ("shot", "menu", 0.1),
        ("hover", "--PAUSE MENU--/FirstInterface/ResumeButton", 0.4),
        ("shot", "menu_pause_hover", 0.1),
        ("click", "--PAUSE MENU--/FirstInterface/SettingsButton (1)", 1.2),
        ("shot", "menu_controls", 0.1),
        ("drag", "-- OPTIONS MENU --/PcTab/MouseSensHolder/Mouse Sensitivity", 0.3),
        ("shot", "menu_controls_slider", 0.1),
        ("click", "-- OPTIONS MENU --/Tabsbutton/AUDIOTAB", 0.8),
        ("shot", "menu_audio", 0.1),
        ("click", "-- OPTIONS MENU --/Tabsbutton/GraphTab", 0.8),
        ("shot", "menu_graphics", 0.1),
        ("click", "-- OPTIONS MENU --/GraphTab/Graphics Settings", 0.6),
        ("shot", "menu_graphics_dropdown", 0.1),
        ("clickat", "0,-450", 0.5),
        ("click", "-- OPTIONS MENU --/GraphTab/fullscreentoggle", 0.4),
        ("shot", "menu_graphics_toggle", 0.1),
        ("click", "-- OPTIONS MENU --/Tabsbutton/PCTAB", 0.6),
        ("click", "-- OPTIONS MENU --/PcTab/---REBIND SYSTEM---/KEYBOARD/-Rebind Prefab-/Rebind Button", 0.4),
        ("shot", "menu_rebind_wait", 0.1),
        ("key", "j", 0.05),
        ("keyup", "j", 0.4),
        ("shot", "menu_rebind_done", 0.1),
        ("click", "-- OPTIONS MENU --/PcTab/---REBIND SYSTEM---/KEYBOARD/-Rebind Prefab-/Reset Button", 0.3),
        ("click", "-- OPTIONS MENU --/Exit", 0.8),
        ("shot", "menu_closed", 0.5),
        ("exit", "", 0.0),
    ];
    // RESTRAFE_AUTOTEST=cosmeticshot: K opens the customization screen, hover + pick a hat, Suits tab + suit, Smoke
    // tab + cig, Escape closes. Screenshots target/shots/cosmetics_*.png; COSMETICS lines report selection + bodies.
    const COSMETIC_SCRIPT: &[(&str, &str, f32)] = &[
        ("wait", "", 2.0),
        ("shot", "cosmetics_before", 0.1),
        ("key", "k", 0.05),
        ("keyup", "k", 1.5),
        ("shot", "cosmetics_open_hats", 0.1),
        ("hover", "--HATS MENU--/HatsView/Scroll View/Viewport/Content/Hat (5)", 0.8),
        ("shot", "cosmetics_hover", 0.1),
        ("wheel", "-2", 0.5),
        ("hover", "--HATS MENU--/HatsView/Scroll View/Viewport/Content/Hat (11)", 0.8),
        ("shot", "cosmetics_hover_challenge", 0.1),
        ("click", "--HATS MENU--/HatsView/Scroll View/Viewport/Content/Hat (5)", 1.5),
        ("shot", "cosmetics_hat_selected", 0.1),
        ("click", "--HATS MENU--/2ndTab", 0.8),
        ("shot", "cosmetics_suits", 0.1),
        ("click", "--HATS MENU--/SuitsView/Scroll View/Viewport/Content/Hat (3)", 1.5),
        ("shot", "cosmetics_suit_selected", 0.1),
        ("click", "--HATS MENU--/3rdTab", 0.8),
        ("shot", "cosmetics_smoke", 0.1),
        ("hover", "--HATS MENU--/CigsView/Scroll View/Viewport/Content/Hat (2)", 0.8),
        ("shot", "cosmetics_smoke_hover", 0.1),
        ("click", "--HATS MENU--/CigsView/Scroll View/Viewport/Content/Hat (2)", 1.5),
        ("shot", "cosmetics_cig_selected", 0.1),
        ("key", "esc", 0.05),
        ("keyup", "esc", 1.5),
        ("shot", "cosmetics_closed", 0.1),
        ("exit", "", 0.0),
    ];
    // RESTRAFE_MENU_LEAVE=1: Escape, then the pause menu's "Leave match" (must quit the app).
    const LEAVE_SCRIPT: &[(&str, &str, f32)] = &[("wait", "", 1.5), ("key", "esc", 0.05), ("keyup", "esc", 1.0), ("click", "--PAUSE MENU--/FirstInterface/MenuButton", 20.0), ("exit", "", 0.0)];
    // RESTRAFE_MENU_PAD=1: the same menus driven by the synthetic gamepad only (input::SimPad): Start opens the pause
    // menu, d-pad / stick navigation, A submits, B cancels, a gamepad-column rebind. PADSHOT lines report the
    // EventSystem selection; screenshots target/shots/pad_*.png.
    const PAD_SCRIPT: &[(&str, &str, f32)] = &[
        ("wait", "", 1.5),
        ("pad", "LB", 0.5),
        ("shot", "pad_game", 0.1),
        ("pad", "START", 1.0),
        ("shot", "pad_menu", 0.1),
        ("pad", "DD", 0.5),
        ("shot", "pad_menu_down", 0.1),
        ("pad", "DD", 0.5),
        ("shot", "pad_menu_down2", 0.1),
        ("pad", "DU", 0.5),
        ("pad", "DU", 0.5),
        ("pad", "DU", 0.5),
        ("shot", "pad_menu_up3", 0.1),
        ("hold", "0,-1", 1.0),
        ("shot", "pad_menu_hold", 0.1),
        ("select", "--PAUSE MENU--/FirstInterface/SettingsButton (1)", 0.3),
        ("pad", "S", 1.2),
        ("shot", "pad_options", 0.1),
        ("select", "-- OPTIONS MENU --/PcTab/MouseSensHolder/Mouse Sensitivity", 0.3),
        ("pad", "DR", 0.3),
        ("pad", "DR", 0.3),
        ("shot", "pad_slider", 0.1),
        ("pad", "DD", 0.3),
        ("shot", "pad_slider_down", 0.1),
        ("select", "-- OPTIONS MENU --/Tabsbutton/GraphTab", 0.3),
        ("pad", "S", 0.8),
        ("select", "-- OPTIONS MENU --/GraphTab/Graphics Settings", 0.3),
        ("pad", "S", 0.6),
        ("shot", "pad_dropdown", 0.1),
        ("pad", "DU", 0.3),
        ("shot", "pad_dropdown_up", 0.1),
        ("pad", "E", 0.6),
        ("shot", "pad_dropdown_cancel", 0.1),
        ("pad", "S", 0.6),
        ("pad", "DU", 0.3),
        ("pad", "S", 0.6),
        ("shot", "pad_dropdown_pick", 0.1),
        ("select", "-- OPTIONS MENU --/Tabsbutton/PCTAB", 0.3),
        ("pad", "S", 0.8),
        ("select", "-- OPTIONS MENU --/PcTab/---REBIND SYSTEM---/GAMEPAD/-Rebind Prefab-/Rebind Button", 0.3),
        ("pad", "S", 0.4),
        ("shot", "pad_rebind_wait", 0.1),
        ("pad", "N", 0.4),
        ("shot", "pad_rebind_done", 0.1),
        ("pad", "DR", 0.3),
        ("shot", "pad_rebind_reset_sel", 0.1),
        ("pad", "S", 0.4),
        ("shot", "pad_rebind_reset", 0.1),
        ("pad", "START", 1.0),
        ("shot", "pad_closed", 0.1),
        ("exit", "", 0.0),
    ];
    let pad_mode = std::env::var_os("RESTRAFE_MENU_PAD").is_some();
    let script = if at.name == "cosmeticshot" {
        COSMETIC_SCRIPT
    } else if std::env::var_os("RESTRAFE_MENU_LEAVE").is_some() {
        LEAVE_SCRIPT
    } else if pad_mode {
        PAD_SCRIPT
    } else {
        SCRIPT
    };
    let Some(&(act, arg, after)) = script.get(*step) else { return };
    *step += 1;
    *wait = after;
    let node_pos = |p: &str| tree.find_path(p).map(|n| tree.center(n));
    let key = |s: &str| match s {
        "esc" => KeyCode::Escape,
        "k" => KeyCode::KeyK,
        _ => KeyCode::KeyJ,
    };
    match act {
        "key" => keys.press(key(arg)),
        "keyup" => keys.release(key(arg)),
        "shot" => {
            let _ = std::fs::create_dir_all("target/shots");
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/{arg}.png")));
            println!("MENUSHOT {arg} hit={:?} jump={:?} fullscreen={} mouse_sens={:.2}", tree.raycast_hit.map(|h| tree.nodes[h].name.clone()), settings.bindings.jump, settings.is_fullscreen, settings.mouse_sensitivity);
            if pad_mode {
                let sel = tree.selected.map(|s| tree.ancestors(s).iter().rev().skip(1).map(|&a| tree.nodes[a].name.clone()).collect::<Vec<_>>().join("/"));
                let dd = tree.find_path("-- OPTIONS MENU --/GraphTab/Graphics Settings").and_then(|n| tree.dropdown_value(n));
                println!("PADSHOT {arg} selected={sel:?} gamepad={} quality={} graphics_dd={dd:?} mouse_sens={:.2} pad_jump={:?}", tree.gamepad, settings.quality_setting, settings.mouse_sensitivity, settings.bindings.pad_jump);
            }
            if at.name == "cosmeticshot" {
                let d: Vec<String> = dressed.iter().map(|(e, s, p)| format!("{e}{}:{:?}", if p { "(preview)" } else { "" }, s.applied())).collect();
                println!("COSMETICS suit={} ({}) hat={} ({}) cig={} ({}) dressed={d:?}", cos.suit, cos_data.suits[cos.suit].name, cos.hat, cos_data.hats[cos.hat].name, cos.cig, cos_data.cigs[cos.cig].name);
            }
            if std::env::var_os("RESTRAFE_UI_DUMP").is_some() {
                for (i, n) in tree.nodes.iter().enumerate() {
                    let pm = tree.ancestors(i).iter().any(|&a| tree.nodes[a].name.contains("PAUSE"));
                    if n.on && pm {
                        let path: Vec<String> = tree.ancestors(i).iter().rev().map(|&a| tree.nodes[a].name.clone()).collect();
                        let c = n.world.transform_point3(n.rect.center().extend(0.0));
                        let kind = n.graphic.as_ref().map(|g| match &g.kind { crate::ugui::GKind::Text(t) => format!("text:{:?}", t.text), _ => "img".into() });
                        println!("  UIP {}/{} {:?} alpha={:.2} at=({:.0},{:.0}) size={:?}", path.join("/"), n.name, kind, n.alpha, c.x, c.y, n.rect.size());
                    }
                    if let (true, Some(t)) = (n.on, tree.text(i)) {
                        if !t.is_empty() {
                            let path: Vec<String> = tree.ancestors(i).iter().rev().map(|&a| tree.nodes[a].name.clone()).collect();
                            let c = n.world.transform_point3(n.rect.center().extend(0.0));
                            println!("  UIT {}/{} text={:?} alpha={:.2} at=({:.0},{:.0})", path.join("/"), n.name, t, n.alpha, c.x, c.y);
                        }
                    }
                }
            }
            if std::env::var_os("RESTRAFE_DEBUG_UI").is_some() {
                for (i, n) in tree.nodes.iter().enumerate() {
                    if n.on && tree.ancestors(i).iter().any(|&a| tree.nodes[a].name == "Dropdown List") {
                        let g = n.graphic.as_ref().map(|g| format!("color={:?} tint={:?} order={} en={}", g.color, g.tint, n.order, g.enabled));
                        println!("  UI {} alpha={} rect={:?} {:?}", n.name, n.alpha, n.rect, g);
                    }
                }
            }
        }
        "hover" | "click" | "drag" | "clickat" => {
            let p = if act == "clickat" {
                let v: Vec<f32> = arg.split(',').filter_map(|x| x.parse().ok()).collect();
                Some(Vec2::new(v[0], v[1]))
            } else {
                node_pos(arg)
            };
            match p {
                Some(mut p) => {
                    if act == "drag" {
                        // Click at 75% of the slider width.
                        if let Some(n) = tree.find_path(arg) {
                            let r = tree.nodes[n].rect;
                            p = tree.nodes[n].world.transform_point3(Vec3::new(r.min.x + r.width() * 0.75, r.center().y, 0.0)).truncate();
                        }
                    }
                    sim.active = true;
                    sim.pos = Some(p);
                    if act != "hover" {
                        sim.down = true;
                        sim.held = true;
                        *release = Some(after);
                        *wait = 0.05;
                    }
                }
                None => println!("MENUSHOT missing node {arg}"),
            }
        }
        // Synthetic gamepad: "pad" taps buttons (fake-pad tokens) for 0.05 s, "hold" holds the left stick (x,y).
        "pad" | "hold" => {
            simpad.active = true;
            simpad.sample = if act == "pad" {
                crate::input::PadSample::with_buttons(arg)
            } else {
                let v: Vec<f32> = arg.split(',').filter_map(|x| x.parse().ok()).collect();
                crate::input::PadSample::with_sticks(Vec2::new(v[0], v[1]), Vec2::ZERO)
            };
            *pad_release = Some(after);
            *wait = if act == "pad" { 0.05 } else { after };
            if act == "hold" {
                *pad_release = Some(0.3);
            }
        }
        "select" => match tree.find_path(arg) {
            Some(n) => tree.set_selected(Some(n)),
            None => println!("MENUSHOT missing node {arg}"),
        },
        "wheel" => {
            sim.active = true;
            sim.wheel = arg.parse().unwrap_or(0.0);
        }
        "exit" => {
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}

/// Debug (RESTRAFE_DEBUG_MESHES=1): names + parents of visible mesh entities under the dummy.
/// `RESTRAFE_DUMP_WORLD=<file>`: once, 4 s after start, writes "name	x y z	sx sy sz" for every named entity (scale
/// check of the imported map / props against the scene data).
pub fn dump_world(q: Query<(&Name, &GlobalTransform)>, lm: Query<(&GlobalTransform, &Mesh3d, Option<&ChildOf>, &bevy::pbr::Lightmap)>, names: Query<&Name>, mut t: Local<f32>, mut t2: Local<f32>, time: Res<Time>) {
    *t2 += time.delta_secs();
    if std::env::var_os("RESTRAFE_DUMP_LM").is_some() && *t2 >= 16.0 && *t2 < 100.0 {
        *t2 = 1000.0;
        for (g, m, parent, l) in &lm {
            let n = parent.and_then(|p| names.get(p.parent()).ok()).map(|n| n.as_str().to_string()).unwrap_or_default();
            let det = g.affine().matrix3.determinant();
            println!("LMDUMP {n}	det={det:.3}	mesh={:?}	pos={:.1}	rect={:?}", m.0.id(), g.translation(), l.uv_rect);
        }
    }
    *t += time.delta_secs();
    let Some(path) = std::env::var_os("RESTRAFE_DUMP_WORLD") else { return };
    if *t < 4.0 || *t > 100.0 {
        return;
    }
    *t = 1000.0;
    let mut out = String::new();
    for (n, g) in &q {
        let (s, _, p) = g.to_scale_rotation_translation();
        out.push_str(&format!("{}	{} {} {}	{} {} {}
", n.as_str(), p.x, p.y, p.z, s.x, s.y, s.z));
    }
    let _ = std::fs::write(path, out);
}

pub fn debug_meshes(dummies: Query<Entity, With<crate::health::Dummy>>, children: Query<&Children>, names: Query<&Name>, meshes: Query<(&Mesh3d, &InheritedVisibility, &GlobalTransform)>, mut done: Local<f32>, time: Res<Time>) {
    *done += time.delta_secs();
    if std::env::var_os("RESTRAFE_DEBUG_MESHES").is_none() || *done < 2.5 || *done > 100.0 {
        return;
    }
    *done = 1000.0;
    for d in &dummies {
        let mut stack = vec![(d, String::new())];
        while let Some((e, path)) = stack.pop() {
            let n = names.get(e).map(|n| n.as_str().to_string()).unwrap_or_else(|_| "?".into());
            let p = format!("{path}/{n}");
            if let Ok((_, vis, g)) = meshes.get(e) {
                if vis.get() {
                    println!("MESH {p} y={:.2}", g.translation().y);
                }
            }
            if let Ok(k) = children.get(e) {
                for c in k.iter() {
                    stack.push((c, p.clone()));
                }
            }
        }
    }
}

/// Warm-up: scripts only start after the dummy + weapon rack exist and 30 consecutive frames ran < 50 ms.
pub fn warmup(time: Res<Time>, mut at: ResMut<AutoTest>, rack: Option<Res<crate::map::WeaponRack>>, dummies: Query<(), With<crate::health::Dummy>>) {
    // RESTRAFE_FPS_LOG=1: frame time every 2 s for the whole run.
    if std::env::var_os("RESTRAFE_FPS_LOG").is_some() && (time.elapsed_secs() / 2.0).floor() != ((time.elapsed_secs() - time.delta_secs()) / 2.0).floor() {
        println!("FPS t={:.1} dt={:.1}ms", time.elapsed_secs(), time.delta_secs() * 1000.0);
    }
    // RESTRAFE_SPIKE_LOG=1: every frame slower than 25 ms (after warm-up) -> "SPIKE t= dt=".
    if at.ready && std::env::var_os("RESTRAFE_SPIKE_LOG").is_some() && time.delta_secs() > 0.025 {
        println!("SPIKE t={:.3} test_t={:.3} dt={:.1}ms", time.elapsed_secs(), at.t, time.delta_secs() * 1000.0);
    }
    if at.ready {
        return;
    }
    if std::env::var_os("RESTRAFE_DEBUG_WARMUP").is_some() && (time.elapsed_secs() * 2.0).floor() != ((time.elapsed_secs() - time.delta_secs()) * 2.0).floor() {
        println!("WARMUP t={:.1} dt={:.3} good={}", time.elapsed_secs(), time.delta_secs(), at.good_frames);
    }
    // RESTRAFE_WARMUP_SECS=<s>: ready after s seconds regardless of frame times (heavy scenes).
    if let Some(s) = std::env::var("RESTRAFE_WARMUP_SECS").ok().and_then(|v| v.parse::<f32>().ok()) {
        at.ready = time.elapsed_secs() > s;
        return;
    }
    if rack.is_none() || dummies.is_empty() || time.delta_secs() >= 0.05 {
        at.good_frames = 0;
        return;
    }
    at.good_frames += 1;
    if at.good_frames >= 30 {
        at.ready = true;
    }
}

/// `RESTRAFE_AUTOTEST=fall`: teleport 80 m up looking down 60 deg once the script starts, screenshot the
/// high-fall effects at t = 1.6 s and 2.0 s -> target/shots/fall_N.png.
pub fn fall_test(mut commands: Commands, at: Res<AutoTest>, mut q: Query<(&mut Transform, &mut Fpc)>, mut step: Local<u32>) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    if at.name != "fall" || !at.ready {
        return;
    }
    let Ok((mut tf, mut fpc)) = q.single_mut() else { return };
    if *step == 0 {
        tf.translation = Vec3::new(-8.0, 80.0, -2.0);
        fpc.rotation_x = 60.0;
        *step = 1;
    } else if (*step == 1 && at.t > 1.6) || (*step == 2 && at.t > 2.0) {
        let _ = std::fs::create_dir_all("target/shots");
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/fall_{}.png", *step - 1)));
        println!("FALL t={:.2} y={:.1} alpha={:.2} lens={:.1}", at.t, tf.translation.y, fpc.high_fall_vfx_alpha, fpc.lens_distortion);
        *step += 1;
    }
}

/// Debug (RESTRAFE_DEBUG_NODES=NameA,NameB): once per second, camera-space position (x right, y up, -z forward)
/// and world scale of every entity with one of those names.
pub fn debug_nodes(cams: Query<&GlobalTransform, With<crate::player::fpc::PlayerCamera>>, q: Query<(&Name, &GlobalTransform)>, mut t: Local<f32>, time: Res<Time>) {
    let Ok(list) = std::env::var("RESTRAFE_DEBUG_NODES") else { return };
    *t += time.delta_secs();
    if *t < 1.0 {
        return;
    }
    *t = 0.0;
    let Ok(cam) = cams.single() else { return };
    let inv = cam.affine().inverse();
    for (n, g) in &q {
        if list.split(',').any(|w| w == n.as_str()) {
            let (s, r, p) = g.to_scale_rotation_translation();
            println!("NODE {} cam={:.2?} scale={:.2?} up={:.2?}", n.as_str(), inv.transform_point3(p), s, inv.transform_vector3(r * Vec3::Y).normalize());
        }
    }
}

/// `RESTRAFE_AUTOTEST=hudturn` (registered by hud_verlet.rs): HUD cable/heart flex check. Stands still facing the
/// dummy area, then flicks the camera right and back left (yaw), then a pitch flick, and screenshots at rest, mid-turn
/// and while settling -> target/shots/hudturn_<label>.png. Turn speed: RESTRAFE_TURN_SPEED (deg/s, default 400).
/// With RESTRAFE_DEBUG_VERLET=1 the per-spring root-bone deflection is printed every 3 fixed steps.
pub fn hud_turn_test(mut commands: Commands, time: Res<Time>, mut at: ResMut<AutoTest>, mut input: ResMut<PlayerInput>, mut q: Query<(&mut Transform, &mut Fpc)>, mut exit: MessageWriter<AppExit>, mut st: Local<(u32, f32)>) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    if at.name != "hudturn" || !at.ready {
        return;
    }
    let dt = time.delta_secs();
    at.t += dt;
    let t = at.t;
    let Ok((mut tf, mut fpc)) = q.single_mut() else { return };
    *input = PlayerInput { enabled: true, ..default() };
    let speed: f32 = std::env::var("RESTRAFE_TURN_SPEED").ok().and_then(|s| s.parse().ok()).unwrap_or(400.0);
    if st.0 == 0 {
        tf.translation = Vec3::new(-8.0, 0.2, -2.0);
        fpc.rotation_x = 0.0;
        st.1 = 0.0;
        st.0 = 1;
    }
    // yaw profile: 2.0-2.3 s right, 2.3-2.6 s left (back to 0); pitch 4.0-4.2 s down, 4.2-4.4 s up
    if (2.0..2.3).contains(&t) {
        st.1 += speed * dt;
    } else if (2.3..2.6).contains(&t) {
        st.1 -= speed * dt;
    } else if t >= 2.6 && t < 2.7 {
        st.1 = 0.0;
    }
    if (4.0..4.2).contains(&t) {
        fpc.rotation_x += speed * 0.5 * dt;
    } else if (4.2..4.4).contains(&t) {
        fpc.rotation_x -= speed * 0.5 * dt;
    } else if (4.4..4.5).contains(&t) {
        fpc.rotation_x = 0.0;
    }
    tf.rotation = crate::unity::unity_euler(0.0, st.1, 0.0);
    const SHOTS: [(f32, &str); 7] = [(1.8, "rest"), (2.15, "turn_right"), (2.45, "turn_left"), (2.65, "settle_0"), (3.5, "settled"), (4.15, "pitch_down"), (4.45, "pitch_up")];
    for (s, label) in SHOTS {
        if t >= s && t - dt < s {
            let _ = std::fs::create_dir_all("target/shots");
            println!("HUDTURN shot {label} t={t:.2} yaw={:.1} pitch={:.1}", st.1, fpc.rotation_x);
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/hudturn_{label}.png")));
        }
    }
    if t > 5.2 {
        exit.write(AppExit::Success);
    }
}

/// `RESTRAFE_AUTOTEST=suicide`: pick up a hand grenade from the rack (RESTRAFE_WEAPON overrides, e.g. RocketLauncher),
/// pull the pin and keep it: DualLauncher auto-throws it at 0.06 s left, it explodes at the player's feet (suicide).
/// Logs health / death / respawn and screenshots the kill cam + the respawn -> target/shots/death_*.png.
#[allow(clippy::too_many_arguments)]
pub fn suicide_test(
    mut commands: Commands,
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    rack: Option<Res<crate::map::WeaponRack>>,
    mut players: Query<(&mut Transform, &mut Fpc, &mut crate::health::PlayerHealth, Option<&crate::weapons::pickup::Hands>)>,
    items: Query<&crate::weapons::Item>,
    cam: Query<&GlobalTransform, With<crate::player::fpc::PlayerCamera>>,
    mut exit: MessageWriter<AppExit>,
    mut step: Local<u32>,
    mut log_t: Local<f32>,
    mut death_t: Local<Option<f32>>,
    mut respawn_t: Local<Option<f32>>,
    mut shots: Local<u32>,
    mut rounds: Local<u32>,
    (mut mouse, mut clicked): (ResMut<ButtonInput<MouseButton>>, Local<bool>),
) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    if at.name != "suicide" || !at.ready {
        return;
    }
    at.t += time.delta_secs();
    let t = at.t;
    let Ok((mut tf, mut fpc, mut ph, hands)) = players.single_mut() else { return };
    let weapon = std::env::var("RESTRAFE_WEAPON").unwrap_or("HandGrenade".into());
    let grenade = weapon.contains("Grenade");
    *log_t += time.delta_secs();
    if *log_t >= 0.25 {
        *log_t = 0.0;
        let c = cam.single().map(|g| g.translation()).unwrap_or_default();
        println!("SUICIDE t={t:5.2} hp={:5.1} dead={} pos=({:.2},{:.2},{:.2}) cam=({:.2},{:.2},{:.2})", ph.health, ph.dead as u8, tf.translation.x, tf.translation.y, tf.translation.z, c.x, c.y, c.z);
    }
    if ph.dead && death_t.is_none() {
        *death_t = Some(t);
        println!("SUICIDE death at t={t:.2} suicide={} health={}", ph.suicide, ph.health);
    }
    if death_t.is_some() && !ph.dead && respawn_t.is_none() {
        *respawn_t = Some(t);
        println!("SUICIDE respawn at t={t:.2} hp={} pos=({:.2},{:.2},{:.2})", ph.health, tf.translation.x, tf.translation.y, tf.translation.z);
    }
    // RESTRAFE_KILLCAM_CLICK=<s>: left click <s> seconds after the death (KillCam skip -> spectate the dummy).
    if let (Some(d), Some(c)) = (*death_t, std::env::var("RESTRAFE_KILLCAM_CLICK").ok().and_then(|s| s.parse::<f32>().ok())) {
        if mouse.pressed(MouseButton::Left) {
            mouse.release(MouseButton::Left);
        } else if !*clicked && t >= d + c {
            *clicked = true;
            println!("SUICIDE click at t={t:.2}");
            mouse.press(MouseButton::Left);
        }
    }
    let _ = std::fs::create_dir_all("target/shots");
    // Kill cam shots while dead, one after respawn.
    const KC: [f32; 3] = [0.5, 1.2, 2.4];
    if let (Some(d), None) = (*death_t, *respawn_t) {
        if (*shots as usize) < KC.len() && t >= d + KC[*shots as usize] {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/death_killcam_{}.png", *shots)));
            *shots += 1;
        }
    }
    if let Some(r) = *respawn_t {
        if *shots == KC.len() as u32 && t >= r + 0.8 {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/death_respawn.png"));
            *shots += 1;
        }
        if t >= r + 1.5 {
            // RESTRAFE_SUICIDE_REPEAT=<n>: die n more times (re-death after a respawn, corpse cap).
            let repeat: u32 = std::env::var("RESTRAFE_SUICIDE_REPEAT").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
            if *rounds < repeat {
                *rounds += 1;
                println!("SUICIDE round {}", *rounds + 1);
                at.t = 0.0;
                *step = 0;
                *death_t = None;
                *respawn_t = None;
                *shots = 99;
                return;
            }
            exit.write(AppExit::Success);
        }
    }
    if t > 20.0 {
        println!("SUICIDE timeout");
        exit.write(AppExit::Success);
    }
    *input = PlayerInput { enabled: true, ..default() };
    let slot = rack.as_ref().and_then(|r| r.slots.iter().find(|s| s.0 == weapon).map(|s| s.1));
    match *step {
        // RESTRAFE_WEAPON=void: fall below y -300 instead (FirstPersonController fellVoid death).
        // RESTRAFE_WEAPON=prop: a lethal RemoveHealth without isKilled (PhysicsProp impact) -> SafeDeathFix path.
        0 if t > 0.5 && weapon == "prop" => {
            ph.health -= 10.0;
            *step = 5;
        }
        0 if t > 0.5 && weapon == "void" => {
            tf.translation = Vec3::new(0.0, -290.0, 0.0);
            *step = 5;
        }
        0 if t > 0.5 => {
            let Some(g) = slot else { return };
            tf.translation = Vec3::new(g.x, 0.2, g.z - 1.2);
            tf.rotation = crate::unity::unity_euler(0.0, 180.0, 0.0);
            fpc.rotation_x = 35.0;
            *step = 1;
        }
        1 if t > 1.0 => {
            input.interact = Button::digital(true, true, false);
            *step = 2;
        }
        2 if t > 1.6 => {
            let r = hands.and_then(|h| h.right).and_then(|e| items.get(e).ok()).map(|i| i.def.name.clone());
            println!("SUICIDE right_hand={r:?}");
            if grenade {
                // Open floor between the spawn and the dummy, looking down.
                tf.translation = Vec3::new(3.0, 0.2, -3.0);
                tf.rotation = crate::unity::unity_euler(0.0, 200.0, 0.0);
                fpc.rotation_x = 60.0;
            } else {
                // 3.9 m west of WJ_A's face, facing it (+X): the rocket's 0.1 s safe start ends right at the wall.
                tf.translation = Vec3::new(-18.3, 0.2, -18.0);
                tf.rotation = crate::unity::unity_euler(0.0, 90.0, 0.0);
                fpc.rotation_x = 0.0;
            }
            *step = 3;
        }
        // onePressShoot: released for a few frames (isClicked), then one press pulls the pin / fires.
        3 if t > 2.4 => {
            input.fire = Button::digital(true, true, false);
            println!("SUICIDE fire at t={t:.2}");
            *step = 4;
        }
        4 if t > 2.45 => {
            input.fire = Button::digital(false, false, true);
            *step = 5;
        }
        _ => {}
    }
}

/// `RESTRAFE_AUTOTEST=barrel`: look at the BarrelProp at (2, ., 3), grab it (F), carry it to the dummy, throw it (F).
/// Logs the barrel (grabbed, position) and the dummy health; screenshots barrel_hold.png / barrel_N.png.
#[allow(clippy::too_many_arguments)]
pub fn barrel_test(
    mut commands: Commands,
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    mut players: Query<(&mut Transform, &mut Fpc, Option<&crate::weapons::pickup::Hands>)>,
    props: Query<(Entity, &crate::props::PhysicsProp, &GlobalTransform)>,
    dolls: Query<(&crate::health::PlayerHealth, &GlobalTransform), (With<crate::health::Dummy>, Without<Fpc>)>,
    mut exit: MessageWriter<AppExit>,
    mut step: Local<u32>,
    mut log_t: Local<f32>,
    mut shots: Local<u32>,
    mut picked: Local<Option<Entity>>,
) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    if at.name != "barrel" || !at.ready {
        return;
    }
    at.t += time.delta_secs();
    let t = at.t;
    let Ok((mut tf, mut fpc, hands)) = players.single_mut() else { return };
    *input = PlayerInput { enabled: true, ..default() };
    let _ = std::fs::create_dir_all("target/shots");
    if picked.is_none() {
        *picked = props.iter().min_by(|a, b| a.2.translation().distance(Vec3::new(2.0, 0.5, 3.0)).total_cmp(&b.2.translation().distance(Vec3::new(2.0, 0.5, 3.0)))).map(|b| b.0);
    }
    let barrel = picked.and_then(|e| props.get(e).ok());
    if *step == 0 && t > 0.5 {
        tf.translation = Vec3::new(2.0, 0.2, 0.6);
        tf.rotation = crate::unity::unity_euler(0.0, 180.0, 0.0);
        fpc.rotation_x = 20.0;
        *step = 1;
    } else if *step == 1 && t > 1.2 {
        println!("BARREL focus env={:?} barrel={:?}", hands.and_then(|h| h.current_env), barrel.map(|b| b.0));
        // RESTRAFE_BARREL_MOUSE=1: grab / throw with mouse 0 instead of Interact.
        if std::env::var_os("RESTRAFE_BARREL_MOUSE").is_some() {
            input.fire = Button::digital(true, true, false);
        } else {
            input.interact = Button::digital(true, true, false);
        }
        *step = 2;
    } else if *step == 2 && t > 1.8 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/barrel_hold.png"));
        println!("BARREL grabbed={:?}", barrel.map(|b| b.1.grabbed));
        tf.translation = Vec3::new(-8.0, 0.2, -1.0);
        tf.rotation = crate::unity::unity_euler(0.0, 0.0, 0.0);
        fpc.rotation_x = std::env::var("RESTRAFE_PITCH").ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        *step = 3;
    } else if *step == 3 && t > 2.4 {
        if std::env::var_os("RESTRAFE_BARREL_MOUSE").is_some() {
            input.fire = Button::digital(true, true, false);
        } else {
            input.interact = Button::digital(true, true, false);
        }
        *step = 4;
    } else if *step == 4 {
        if *shots < 4 && t > 2.5 + *shots as f32 * 0.25 {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/barrel_{}.png", *shots)));
            *shots += 1;
        }
        // RESTRAFE_BARREL_REPEAT=n: n more grab + throw cycles at the dummy (3 s each).
        let reps: u32 = std::env::var("RESTRAFE_BARREL_REPEAT").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let c = ((t - 5.0) / 3.0).floor();
        if t > 5.0 && (c as u32) < reps {
            let ph = (t - 5.0) - c * 3.0;
            let prev = (t - time.delta_secs() - 5.0) - c * 3.0;
            if let Some((_, _, g)) = barrel {
                if prev < 0.2 && ph >= 0.2 {
                    let b = g.translation();
                    tf.translation = Vec3::new(b.x, 0.2, b.z + 2.4);
                    tf.rotation = crate::unity::unity_euler(0.0, 0.0, 0.0);
                    fpc.rotation_x = 20.0;
                } else if prev < 0.8 && ph >= 0.8 {
                    input.interact = Button::digital(true, true, false);
                } else if prev < 1.4 && ph >= 1.4 {
                    let d = dolls.iter().next().map(|d| d.1.translation()).unwrap_or(Vec3::new(-8.0, 0.2, -6.0));
                    // RESTRAFE_BARREL_DX=<m>: throw from beside the dummy (clean trajectory check).
                    let dx = std::env::var("RESTRAFE_BARREL_DX").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                    tf.translation = Vec3::new(d.x + dx, 0.2, d.z + 5.0);
                    tf.rotation = crate::unity::unity_euler(0.0, 0.0, 0.0);
                    fpc.rotation_x = 0.0;
                } else if prev < 1.9 && ph >= 1.9 {
                    input.interact = Button::digital(true, true, false);
                    println!("BARREL throw {}", c as u32 + 2);
                }
            }
        } else if t > 7.0 + reps as f32 * 3.0 {
            exit.write(AppExit::Success);
        }
    }
    *log_t -= time.delta_secs();
    if *log_t <= 0.0 && *step >= 2 {
        *log_t = 0.2;
        let hp: Vec<(f32, Vec3)> = dolls.iter().map(|d| (d.0.health, d.1.translation())).collect();
        if let Some((_, p, g)) = barrel {
            println!("BARREL t={t:.2} grabbed={} pos=({:.2},{:.2},{:.2}) doll={hp:?}", p.grabbed, g.translation().x, g.translation().y, g.translation().z);
        }
    }
}

/// `RESTRAFE_AUTOTEST=mapview`: once the map is set up, stand at its spawn point and screenshot four directions
/// (target/shots/map_N.png). RESTRAFE_MAP_POS=x,y,z,yaw overrides the spot.
pub fn map_view_test(
    mut commands: Commands,
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    spawn: Option<Res<crate::scene_map::MapSpawn>>,
    items: Query<(&crate::weapons::Item, &GlobalTransform), Without<Fpc>>,
    bodies: Query<(&Name, &GlobalTransform), (With<bevy_rapier3d::prelude::RigidBody>, Without<crate::weapons::Item>, Without<Fpc>)>,
    names_q: Query<(&Name, &GlobalTransform)>,
    mut players: Query<(&mut Transform, &mut Fpc)>,
    mut exit: MessageWriter<AppExit>,
    mut step: Local<u32>,
    mut t0: Local<Option<f32>>,
    mut give: MessageWriter<crate::weapon_menu::GiveWeapon>,
    mut drop_q: Query<(&crate::weapons::Item, &mut Transform, Option<&mut bevy_rapier3d::prelude::Velocity>), Without<Fpc>>,
    mut hands_q: Query<&mut crate::weapons::pickup::Hands>,
) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    if at.name != "mapview" || !at.ready {
        return;
    }
    at.t += time.delta_secs();
    *input = PlayerInput { enabled: true, ..default() };
    let Some(sp) = spawn.and_then(|s| s.0) else {
        if at.t > 120.0 {
            println!("MAPVIEW no spawn point");
            exit.write(AppExit::Success);
        }
        return;
    };
    let t = *t0.get_or_insert(at.t);
    let dt = at.t - t;
    let Ok((mut tf, mut fpc)) = players.single_mut() else { return };
    let custom: Vec<f32> = std::env::var("RESTRAFE_MAP_POS").ok().map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect()).unwrap_or_default();
    let (pos, yaw0) = if custom.len() == 4 { (Vec3::new(custom[0], custom[1], custom[2]), custom[3]) } else {
        let (y, _, _) = sp.rotation.to_euler(EulerRot::YXZ);
        (sp.translation, -y.to_degrees())
    };
    let k = *step;
    if k == 3 && std::env::var_os("RESTRAFE_KILLZ_DROP").is_some() {
        for mut h in &mut hands_q {
            h.drop_all = true;
        }
    }
    if k < 8 {
        // even steps: place; odd steps (0.6 s later): screenshot
        let delay = std::env::var("RESTRAFE_MAP_DELAY").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(2.0);
        if k % 2 == 0 && dt > delay + k as f32 * 0.6 {
            tf.translation = pos;
            tf.rotation = crate::unity::unity_euler(0.0, yaw0 + (k / 2) as f32 * 90.0, 0.0);
            // RESTRAFE_MAP_WEAPON=<WeaponDb name>: hold that weapon (weapon menu give) for the shots.
            if k == 0 {
                if let Ok(w) = std::env::var("RESTRAFE_MAP_WEAPON") {
                    give.write(crate::weapon_menu::GiveWeapon(w));
                }
            }
            // RESTRAFE_KILLZ_DROP=<h> (with RESTRAFE_MAP_WEAPON): drop the weapon (step 3), then put the loose item h m above
            // the Killz box at rest (step 4) to check the trigger overlap (log KILLZ, MAPITEM count at the end).
            if let Some(h) = std::env::var("RESTRAFE_KILLZ_DROP").ok().and_then(|v| v.parse::<f32>().ok()) {
                if k == 4 {
                    let z = names_q.iter().find(|(n, _)| n.as_str() == "collider:KILLZ").map(|(_, g)| g.translation());
                    for (it, mut itf, v) in &mut drop_q {
                        if let (true, Some(z)) = (it.physics, z) {
                            itf.translation = z + Vec3::Y * h;
                            if let Some(mut v) = v {
                                *v = default();
                            }
                            println!("KILLZ test {} placed at {:?} (Killz at {z:?})", it.def.name, itf.translation);
                        }
                    }
                }
            }
            fpc.rotation_x = std::env::var("RESTRAFE_MAP_PITCH").ok().and_then(|s| s.parse().ok()).unwrap_or(5.0);
            *step += 1;
        } else if k % 2 == 1 && dt > delay + k as f32 * 0.6 {
            let _ = std::fs::create_dir_all("target/shots");
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/map_{}.png", k / 2)));
            println!("MAPVIEW shot {} at {:?} yaw {}", k / 2, tf.translation, yaw0 + (k / 2) as f32 * 90.0);
            for (_, g) in names_q.iter().filter(|(n, _)| n.as_str() == "BarrelProp") {
                println!("MAPBARREL shot {} at {:?}", k / 2, g.translation());
            }
            for (n, g) in &bodies {
                if n.as_str().starts_with("SM_") || n.as_str().to_lowercase().contains("barrel") {
                    println!("MAPBODY shot {} {} at {:?}", k / 2, n, g.translation());
                }
            }
            if k == 1 && std::env::var_os("RESTRAFE_DEBUG_SPAWNERS").is_some() {
                for (it, g) in &items {
                    println!("MAPITEM {} phys={} bob={:?} at {:?}", it.def.name, it.physics, it.bob_origin, g.translation());
                }
            }
            *step += 1;
        }
    } else if dt > 6.0 + std::env::var("RESTRAFE_MAP_DELAY").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(2.0) {
        if std::env::var_os("RESTRAFE_KILLZ_DROP").is_some() {
            println!("KILLZ end loose items {}", items.iter().filter(|(it, _)| it.physics).count());
        }
        exit.write(AppExit::Success);
    }
}

/// `RESTRAFE_AUTOTEST=door`: face the map's `Door (1)`, press Interact (open), screenshot, press again (close).
/// `RESTRAFE_AUTOTEST=pigeon`: walk up to the pigeons and log their positions (they fly off within 5 m).
pub fn scene_props_test(
    mut commands: Commands,
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    mut players: Query<(&mut Transform, &mut Fpc, Option<&crate::weapons::pickup::Hands>)>,
    doors: Query<(&crate::scene_props::Door, &GlobalTransform)>,
    pigeons: Query<&GlobalTransform, With<crate::scene_props::Pigeon>>,
    env_cols: Query<(&crate::props::EnvTarget, &GlobalTransform)>,
    spawn: Option<Res<crate::scene_map::MapSpawn>>,
    mut exit: MessageWriter<AppExit>,
    mut step: Local<u32>,
    mut t0: Local<Option<f32>>,
) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    if (at.name != "door" && at.name != "pigeon") || !at.ready || spawn.and_then(|s| s.0).is_none() {
        return;
    }
    at.t += time.delta_secs();
    let t = at.t - *t0.get_or_insert(at.t);
    *input = PlayerInput { enabled: true, ..default() };
    let Ok((mut tf, mut fpc, hands)) = players.single_mut() else { return };
    let _ = std::fs::create_dir_all("target/shots");
    if at.name == "door" {
        if *step == 0 && t > 1.0 {
            // Stand 2.2 m in front of Door (1)'s box collider, facing it.
            let Some((c, n)) = env_cols.iter().filter_map(|(et, g)| doors.get(et.0).ok().map(|(_, dg)| (g.translation(), dg.rotation() * Vec3::Z))).max_by(|a, b| a.0.z.total_cmp(&b.0.z).reverse()) else { return };
            let n = Vec3::new(n.x, 0.0, n.z).normalize_or_zero();
            let feet = c + n * 2.2 - Vec3::Y * 1.0;
            tf.translation = feet;
            tf.rotation = Transform::IDENTITY.looking_to(-n, Vec3::Y).rotation;
            fpc.rotation_x = 0.0;
            println!("DOOR target collider at {c:?}, player at {feet:?}");
            *step = 1;
        } else if *step == 1 && t > 1.6 {
            let d = doors.iter().map(|(d, g)| (d.is_open, g.translation())).collect::<Vec<_>>();
            println!("DOOR focus env={:?} doors={d:?}", hands.and_then(|h| h.current_env));
            input.interact = Button::digital(true, true, false);
            *step = 2;
        } else if *step == 2 && t > 2.4 {
            println!("DOOR after open: {:?}", doors.iter().map(|(d, _)| d.is_open).collect::<Vec<_>>());
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/door_open.png"));
            *step = 3;
        } else if *step == 3 && t > 3.0 {
            println!("DOOR focus2 env={:?}", hands.and_then(|h| h.current_env));
            input.interact = Button::digital(true, true, false);
            *step = 4;
        } else if *step == 4 && t > 3.8 {
            println!("DOOR after close: {:?}", doors.iter().map(|(d, _)| d.is_open).collect::<Vec<_>>());
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/door_closed.png"));
            *step = 5;
        } else if *step == 5 && t > 4.5 {
            exit.write(AppExit::Success);
        }
    } else {
        if *step == 0 && t > 1.0 {
            tf.translation = Vec3::new(-14.5, 0.7, -237.5);
            tf.rotation = Transform::IDENTITY.looking_to(Vec3::new(4.0, 0.0, 7.0), Vec3::Y).rotation;
            *step = 1;
        } else if *step >= 1 && *step < 40 && t > 1.0 + *step as f32 * 0.25 {
            // walk toward the flock
            input.move_raw = Vec2::Y;
            if *step % 4 == 0 {
                let p: Vec<String> = pigeons.iter().map(|g| format!("({:.1},{:.1},{:.1})", g.translation().x, g.translation().y, g.translation().z)).collect();
                println!("PIGEON t={t:.2} player=({:.1},{:.1},{:.1}) pigeons={}", tf.translation.x, tf.translation.y, tf.translation.z, p.join(" "));
            }
            if *step == 1 {
                commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/pigeon_idle.png"));
            }
            if *step == 7 {
                commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/pigeon_fly.png"));
            }
            *step += 1;
        } else if *step >= 40 {
            exit.write(AppExit::Success);
        }
        if (1..40).contains(&*step) {
            input.move_raw = Vec2::Y;
        }
    }
}

/// `RESTRAFE_AUTOTEST=wallshot`: pick up the spawn room's Webley, face the east wall and fire three shots (SOUND log).
pub fn wall_shot_test(
    mut commands: Commands,
    ambient: Res<GlobalAmbientLight>,
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    mut players: Query<(&mut Transform, &mut Fpc, Option<&crate::weapons::pickup::Hands>)>,
    spawn: Option<Res<crate::scene_map::MapSpawn>>,
    mut exit: MessageWriter<AppExit>,
    mut step: Local<u32>,
    mut t0: Local<Option<f32>>,
) {
    let Some(spawn_tf) = spawn.map(|s| s.0) else { return };
    if at.name != "wallshot" || !at.ready || spawn_tf.is_none() {
        return;
    }
    at.t += time.delta_secs();
    let t = at.t - *t0.get_or_insert(at.t);
    *input = PlayerInput { enabled: true, ..default() };
    let Ok((mut tf, mut fpc, hands)) = players.single_mut() else { return };
    let item = Vec3::new(-19.922, -18.09, -152.548);
    match *step {
        0 if t > 1.0 => {
            tf.translation = item + Vec3::new(-1.5, -0.18, 0.0);
            tf.rotation = Transform::IDENTITY.looking_to(Vec3::X, Vec3::Y).rotation;
            fpc.rotation_x = 45.0;
            *step = 1;
        }
        1 if t > 1.6 => {
            input.interact = Button::digital(true, true, false);
            *step = 2;
        }
        2 if t > 2.2 => {
            println!("WALLSHOT right={:?}", hands.and_then(|h| h.right).is_some());
            fpc.rotation_x = 0.0;
            tf.rotation = Transform::IDENTITY.looking_to(Vec3::new(1.0, 0.0, -1.0), Vec3::Y).rotation;
            *step = 3;
        }
        3 | 4 | 5 if t > 2.8 + (*step - 3) as f32 * 0.8 => {
            println!("WALLSHOT fire {} t={t:.2} ambient={:?} x{}", *step - 2, ambient.color.to_linear(), ambient.brightness);
            input.fire = Button::digital(true, true, false);
            *step += 1;
        }
        6 if t > 5.5 => {
            if let Some(sp) = spawn_tf {
                tf.translation = sp.translation;
                tf.rotation = sp.rotation;
            }
            *step = 8;
        }
        8 if t > 6.0 => {
            let _ = std::fs::create_dir_all("target/shots");
            commands.spawn(bevy::render::view::screenshot::Screenshot::primary_window()).observe(bevy::render::view::screenshot::save_to_disk("target/shots/wallshot.png"));
            *step = 7;
        }
        7 if t > 6.7 => {
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}

/// `RESTRAFE_AUTOTEST=walk` + RESTRAFE_MAP_POS=x,y,z,yaw: walk forward 4 s, turn around, walk back 4 s (WALKLOG every
/// 0.25 s: position, horizontal speed, grounded, sliding). RESTRAFE_WALK_SPRINT=1 sprints.
pub fn walk_test(
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    mut players: Query<(&mut Transform, &mut Fpc)>,
    spawn: Option<Res<crate::scene_map::MapSpawn>>,
    mut exit: MessageWriter<AppExit>,
    mut t0: Local<Option<f32>>,
    mut last: Local<Option<Vec3>>,
    (mut commands, mut shot_n, suns): (Commands, Local<u32>, Query<&GlobalTransform, With<bevy::light::DirectionalLight>>),
) {
    if at.name != "walk" || !at.ready || spawn.as_ref().and_then(|s| s.0).is_none() {
        return;
    }
    at.t += time.delta_secs();
    let t = at.t - *t0.get_or_insert(at.t);
    *input = PlayerInput { enabled: true, ..default() };
    let Ok((mut tf, mut fpc)) = players.single_mut() else { return };
    let mut v: Vec<f32> = std::env::var("RESTRAFE_MAP_POS").ok().map(|s| s.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
    if v.len() < 4 {
        // Default: the map spawn point.
        let Some(sp) = spawn.and_then(|s| s.0) else { return };
        let (y, _, _) = sp.rotation.to_euler(EulerRot::YXZ);
        v = vec![sp.translation.x, sp.translation.y, sp.translation.z, -y.to_degrees()];
    }
    // RESTRAFE_WALK_SHOTS=1: a screenshot every frame after t = 1 (target/shots/walk_N.png; flash hunting).
    if t >= 1.0 && std::env::var_os("RESTRAFE_WALK_SHOTS").is_some() {
        use bevy::render::view::screenshot::{Screenshot, save_to_disk};
        let _ = std::fs::create_dir_all("target/shots");
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/walk_{}.png", *shot_n)));
        println!("WALKSHOT {} t={t:.3}", *shot_n);
        *shot_n += 1;
    }
    if t < 1.0 {
        tf.translation = Vec3::new(v[0], v[1], v[2]);
        tf.rotation = crate::unity::unity_euler(0.0, v[3], 0.0);
        fpc.rotation_x = 0.0;
        *last = Some(tf.translation);
        return;
    }
    // RESTRAFE_WALK_SPIN=1: stand still and sweep the view (yaw 90 deg/s, pitch -60..60 at 0.35 Hz).
    // RESTRAFE_SPIN_SUN=1: look exactly along (t < 5) / against (t >= 5) each directional light's direction.
    if std::env::var_os("RESTRAFE_SPIN_SUN").is_some() {
        let dirs: Vec<Vec3> = suns.iter().map(|g| g.forward().as_vec3()).collect();
        if let Some(d) = dirs.get(((t * 2.0) as usize) % dirs.len().max(1)) {
            let d = if t < 5.0 { *d } else { -*d };
            tf.rotation = crate::unity::unity_euler(0.0, d.x.atan2(-d.z).to_degrees(), 0.0);
            fpc.rotation_x = (-d.y).atan2(Vec2::new(d.x, d.z).length()).to_degrees();
            if (t * 4.0).floor() != ((t - time.delta_secs()) * 4.0).floor() {
                println!("SPINSUN t={t:.2} n={} dir={d:.3} pitch={:.2}", dirs.len(), fpc.rotation_x);
            }
        }
        if t > 9.5 {
            exit.write(AppExit::Success);
        }
        return;
    }
    if std::env::var_os("RESTRAFE_WALK_SPIN").is_some() {
        tf.rotation = crate::unity::unity_euler(0.0, v[3] + (t - 1.0) * 90.0, 0.0);
        fpc.rotation_x = std::env::var("RESTRAFE_SPIN_PITCH").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(89.9) * ((t - 1.0) * 0.35 * std::f32::consts::TAU).sin();
        if t > 9.5 {
            exit.write(AppExit::Success);
        }
        return;
    }
    if (t < 5.0 || (5.2..9.2).contains(&t)) && std::env::var_os("RESTRAFE_WALK_IDLE").is_none() {
        input.move_raw = Vec2::Y;
        input.run = Button::digital(std::env::var_os("RESTRAFE_WALK_SPRINT").is_some(), false, false);
    }
    if (5.0..5.2).contains(&t) {
        tf.rotation = crate::unity::unity_euler(0.0, v[3] + 180.0, 0.0);
    }
    if (t * 4.0).floor() != ((t - time.delta_secs()) * 4.0).floor() {
        let p = tf.translation;
        let sp = last.map(|l| Vec2::new(p.x - l.x, p.z - l.z).length() * 4.0).unwrap_or(0.0);
        println!("WALKLOG t={t:.2} pos=({:.2},{:.2},{:.2}) hspd={sp:.2} grounded={} slide={}", p.x, p.y, p.z, fpc.is_grounded, fpc.is_sliding);
        *last = Some(p);
    }
    if t > 9.5 {
        exit.write(AppExit::Success);
    }
}

/// `RESTRAFE_AUTOTEST=weaponmenu`: in the spawn room, press "`" (menu screenshot target/shots/weaponmenu_open.png),
/// give RESTRAFE_WEAPON (default Prophet) like a button click, screenshot it in hand (weaponmenu_held.png) and fire.
#[allow(clippy::too_many_arguments)]
pub fn weapon_menu_test(
    mut commands: Commands,
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    input: Res<PlayerInput>,
    menu: Res<crate::weapon_menu::WeaponMenu>,
    mut gives: MessageWriter<crate::weapon_menu::GiveWeapon>,
    players: Query<Option<&crate::weapons::pickup::Hands>, With<Fpc>>,
    items: Query<&crate::weapons::Item>,
    dummies: Query<&Transform, With<crate::health::Dummy>>,
    spawn: Option<Res<crate::scene_map::MapSpawn>>,
    mut exit: MessageWriter<AppExit>,
    mut step: Local<u32>,
    mut t0: Local<Option<f32>>,
) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    if at.name != "weaponmenu" || !at.ready || spawn.and_then(|s| s.0).is_none() {
        return;
    }
    at.t += time.delta_secs();
    let t = at.t - *t0.get_or_insert(at.t);
    let held = || players.single().ok().flatten().and_then(|h| h.right).and_then(|e| items.get(e).ok()).map(|i| i.def.name.clone());
    let _ = std::fs::create_dir_all("target/shots");
    match *step {
        0 if t > 2.0 => {
            keys.press(KeyCode::Backquote);
            *step = 1;
        }
        1 => {
            keys.release(KeyCode::Backquote);
            *step = 2;
        }
        2 if t > 2.8 => {
            println!("RESULT menu_open={} input_enabled={} dummies={:?}", menu.open, input.enabled, dummies.iter().map(|d| d.translation).collect::<Vec<_>>());
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/weaponmenu_open.png"));
            *step = 3;
        }
        3 if t > 3.4 => {
            // Like a click on the button: the mouse stays down for a while (must not fire the new weapon).
            gives.write(crate::weapon_menu::GiveWeapon(std::env::var("RESTRAFE_WEAPON").unwrap_or("Prophet".into())));
            mouse.press(MouseButton::Left);
            *step = 4;
        }
        4 if t > 4.0 => {
            mouse.release(MouseButton::Left);
            *step = 40;
        }
        40 if t > 4.4 => {
            println!("RESULT menu_open={} input_enabled={} right_hand={:?}", menu.open, input.enabled, held());
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk("target/shots/weaponmenu_held.png"));
            *step = 5;
        }
        5 if t > 5.0 => {
            mouse.press(MouseButton::Left);
            // RESTRAFE_AT_STRAFE=1: strafe right while the shot flies (side view of the projectile trail).
            if std::env::var_os("RESTRAFE_AT_STRAFE").is_some() {
                keys.press(KeyCode::KeyD);
            }
            *step = 6;
        }
        6 => {
            mouse.release(MouseButton::Left);
            *step = 60;
        }
        60..=63 if t > 5.15 + 0.15 * (*step - 60) as f32 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/weaponmenu_fire{}.png", *step - 60)));
            *step += 1;
        }
        64 => {
            keys.release(KeyCode::KeyD);
            *step = 7;
        }
        7 if t > 6.2 => {
            println!("RESULT after_fire right_hand={:?} ammo={:?}", held(), players.single().ok().flatten().and_then(|h| h.right).and_then(|e| items.get(e).ok()).map(|i| i.current_ammo));
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}

/// `RESTRAFE_AUTOTEST=steep`: finds map surfaces steeper than the slope limit (65 deg, < 88 deg) with stacked downward
/// rays over RESTRAFE_STEEP_AREA=x0,z0,x1,z1 (default the whole map), drops the player onto each (no input) and prints
/// `STEEP stuck` for spots where it is still up there 1.2 s later (Unity's CharacterController slides off them).
#[allow(clippy::too_many_arguments)]
pub fn steep_test(
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    rapier: bevy_rapier3d::prelude::ReadRapierContext,
    mut players: Query<(&mut Transform, &mut Fpc)>,
    spawn: Option<Res<crate::scene_map::MapSpawn>>,
    mut exit: MessageWriter<AppExit>,
    mut spots: Local<Option<Vec<(Vec3, Vec3)>>>,
    mut cur: Local<(usize, f32, Vec3, u32)>,
    mut mid: Local<Vec3>,
) {
    if at.name != "steep" || !at.ready || spawn.and_then(|s| s.0).is_none() {
        return;
    }
    at.t += time.delta_secs();
    *input = PlayerInput { enabled: true, ..default() };
    // RESTRAFE_STEEP_PUSH=1: face the surface and sprint into it the whole time.
    let push = std::env::var_os("RESTRAFE_STEEP_PUSH").is_some();
    if push {
        input.move_raw = Vec2::Y;
        input.run = Button::digital(true, false, false);
    }
    let Ok(ctx) = rapier.single() else { return };
    let Ok((mut tf, mut fpc)) = players.single_mut() else { return };
    if spots.is_none() {
        let a: Vec<f32> = std::env::var("RESTRAFE_STEEP_AREA").ok().map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect()).unwrap_or_else(|| vec![-140.0, -340.0, 100.0, 80.0]);
        let walk = 65f32.to_radians().cos();
        let mut found: Vec<(Vec3, Vec3)> = Vec::new();
        let mut cells = std::collections::HashSet::new();
        let mut z = a[1];
        while z <= a[3] {
            let mut x = a[0];
            while x <= a[2] {
                let mut o = Vec3::new(x, 120.0, z);
                for _ in 0..8 {
                    let Some(h) = crate::physics::raycast(&ctx, o, Vec3::NEG_Y, 300.0, crate::physics::DEFAULT_RAYCAST_LAYERS) else { break };
                    if h.normal.y > 0.035 && h.normal.y < walk - 0.02 {
                        let cell = ((h.point.x / 3.0).floor() as i32, (h.point.y / 3.0).floor() as i32, (h.point.z / 3.0).floor() as i32);
                        if cells.insert(cell) {
                            found.push((h.point, h.normal));
                        }
                    }
                    o = h.point - Vec3::Y * 0.05;
                }
                x += 1.0;
            }
            z += 1.0;
        }
        println!("STEEP {} candidate spots", found.len());
        *spots = Some(found);
        *cur = (0, 0.0, Vec3::ZERO, 0);
    }
    let list = spots.as_ref().unwrap();
    let (i, t, start, stuck) = &mut *cur;
    if *i >= list.len() {
        println!("STEEP done: {} stuck of {}", stuck, list.len());
        exit.write(AppExit::Success);
        return;
    }
    let (p, n) = list[*i];
    if *t == 0.0 {
        // Lower capsule sphere just outside the surface (radius + controller skin offset + 2 cm).
        let r = fpc.p.cc_radius;
        let low_offset = fpc.cc_center - Vec3::Y * (fpc.cc_height * 0.5 - r);
        *start = p + n * (r + fpc.cc_skin + 0.02) - low_offset;
        // A capsule that starts inside geometry (overhangs, crevices) is wedged anyway: skip the spot.
        let low = *start + low_offset;
        let blocked = (0..5).any(|k| !crate::physics::overlap_sphere(&ctx, low + Vec3::Y * (k as f32 * (fpc.cc_height - 2.0 * r) / 4.0), r - 0.02, crate::physics::DEFAULT_RAYCAST_LAYERS & !(1 << crate::physics::layer::PLAYER) & !(1 << crate::physics::layer::SELF) & !(1 << crate::physics::layer::BODY)).is_empty());
        if blocked {
            println!("STEEP skip {:.2?}", p);
            *i += 1;
            return;
        }
        tf.translation = *start;
        if push {
            let into = -Vec3::new(n.x, 0.0, n.z).normalize_or_zero();
            if into != Vec3::ZERO {
                tf.rotation = Transform::IDENTITY.looking_to(into, Vec3::Y).rotation;
            }
        }
        fpc.move_direction = Vec3::ZERO;
        fpc.fall_distance = 0.0;
        // RESTRAFE_STEEP_WJ=1: start with the wall jumps used up (Katana 3/3); the verbose line shows the count.
        if std::env::var_os("RESTRAFE_STEEP_WJ").is_some() {
            fpc.max_wall_jumps = 3;
            fpc.wall_jumps_count = 3;
        }
    }
    let prev_t = *t;
    *t += time.delta_secs();
    if prev_t < 0.5 && *t >= 0.5 {
        *mid = tf.translation;
    }
    if std::env::var_os("RESTRAFE_STEEP_WJ").is_some() && prev_t < 0.2 && *t >= 0.2 {
        println!("STEEP wj at 0.2s {:.2?} n.y={:.2} grounded={} wjc={} y_drop={:.2}", p, n.y, fpc.cc_grounded, fpc.wall_jumps_count, start.y - tf.translation.y);
    }
    if *t > 0.7 {
        let e = tf.translation;
        // Stuck = still on a non-walkable surface (nothing walkable under the capsule) after less than 1 m of drop.
        let r = fpc.p.cc_radius;
        let low = e + fpc.cc_center - Vec3::Y * (fpc.cc_height * 0.5 - r);
        let walk = fpc.p.cc_slope_limit.to_radians().cos();
        let under = crate::physics::sphere_cast(&ctx, low, r, Vec3::NEG_Y, fpc.cc_skin / walk + 0.1, crate::physics::DEFAULT_RAYCAST_LAYERS);
        let steep_only = under.as_ref().is_none_or(|h| h.normal.y < walk - 0.02)
            && crate::physics::raycast(&ctx, low, Vec3::NEG_Y, r + fpc.cc_skin + 0.3, crate::physics::DEFAULT_RAYCAST_LAYERS).is_none_or(|g| g.normal.y < walk);
        // (Unity: not grounded there, gravity builds up; well over 1 m of drop in 0.7 s.)
        let _ = *mid;
        if std::env::var_os("RESTRAFE_STEEP_VERBOSE").is_some() {
            println!("STEEP spot {:.2?} n.y={:.2} drop={:.2} steep_only={} under={:?} grounded={} start={:.2?} end={:.2?} wjc={}", p, n.y, start.y - e.y, steep_only, under.as_ref().map(|h| (h.normal, h.distance)), fpc.cc_grounded, *start, e, fpc.wall_jumps_count);
        }
        if start.y - e.y < 1.0 && steep_only {
            *stuck += 1;
            println!("STEEP stuck at {:.2?} n=({:.2},{:.2},{:.2}) {:.0} deg, end {:.2?} grounded={}", p, n.x, n.y, n.z, n.y.acos().to_degrees(), e, fpc.cc_grounded);
        }
        *i += 1;
        *t = 0.0;
    }
}

/// `RESTRAFE_AUTOTEST=ledge`: stand on the test range's 2.2 m box (Box2, top x 2.5..5.5) with the capsule centre
/// RESTRAFE_LEDGE_OFF metres past its +X edge; prints whether the player is still on top after 1 s
/// (PhysX: the rim contact is non-walkable past ~0.36 m, radius 0.4 x sin 65).
pub fn ledge_test(time: Res<Time>, mut at: ResMut<AutoTest>, mut input: ResMut<PlayerInput>, mut players: Query<(&mut Transform, &mut Fpc)>, mut exit: MessageWriter<AppExit>, mut t0: Local<Option<f32>>) {
    if at.name != "ledge" || !at.ready {
        return;
    }
    at.t += time.delta_secs();
    *input = PlayerInput { enabled: true, ..default() };
    let Ok((mut tf, mut fpc)) = players.single_mut() else { return };
    let off: f32 = std::env::var("RESTRAFE_LEDGE_OFF").ok().and_then(|s| s.parse().ok()).unwrap_or(0.3);
    let t = at.t - *t0.get_or_insert_with(|| {
        tf.translation = Vec3::new(5.5 + off, 2.2 + 0.25, 8.0);
        fpc.move_direction = Vec3::ZERO;
        at.t
    });
    if t > 1.0 {
        println!("RESULT ledge off={off} y={:.2} on_top={} grounded={}", tf.translation.y, tf.translation.y > 2.0, fpc.cc_grounded);
        exit.write(AppExit::Success);
    }
}

/// `RESTRAFE_AUTOTEST=ramp`: walk (RESTRAFE_RAMP_SPRINT=1: sprint) up the test range's 35 deg concrete ramp (Ramp35,
/// x 18, low end at +z) from its foot; prints the height reached (the top is about 4.6 m).
pub fn ramp_test(time: Res<Time>, mut at: ResMut<AutoTest>, mut input: ResMut<PlayerInput>, mut players: Query<(&mut Transform, &mut Fpc)>, mut exit: MessageWriter<AppExit>, mut t0: Local<Option<f32>>, mut max_y: Local<f32>) {
    if at.name != "ramp" || !at.ready {
        return;
    }
    at.t += time.delta_secs();
    *input = PlayerInput { enabled: true, ..default() };
    let Ok((mut tf, mut fpc)) = players.single_mut() else { return };
    let t = at.t - *t0.get_or_insert_with(|| {
        tf.translation = Vec3::new(18.0, 0.2, -6.0);
        tf.rotation = Quat::IDENTITY;
        fpc.move_direction = Vec3::ZERO;
        at.t
    });
    if t > 0.3 && t < 3.3 {
        input.move_raw = Vec2::Y;
        input.run = Button::digital(std::env::var_os("RESTRAFE_RAMP_SPRINT").is_some(), false, false);
    }
    *max_y = max_y.max(tf.translation.y);
    if t > 3.5 {
        println!("RESULT ramp max_y={:.2} end={:.2?}", *max_y, tf.translation);
        exit.write(AppExit::Success);
    }
}

/// `RESTRAFE_AUTOTEST=disc`: stand on the first test-range weapon spawner disc and walk off it (+X); prints LAND lines
/// for landing triggers (land sound index 11) - stepping off a 15 cm disc must not land.
pub fn disc_test(time: Res<Time>, mut at: ResMut<AutoTest>, mut input: ResMut<PlayerInput>, mut players: Query<(&mut Transform, &mut Fpc)>, mut sounds: MessageReader<crate::player::fpc::FpcSound>, mut exit: MessageWriter<AppExit>, mut t0: Local<Option<f32>>, mut lands: Local<u32>) {
    // `mapdisc`: the same on a map ItemSpawner (real SM_Spawner_00 mesh collider), RESTRAFE_DISC_POS=x,y,z.
    if (at.name != "disc" && at.name != "mapdisc") || !at.ready {
        return;
    }
    at.t += time.delta_secs();
    *input = PlayerInput { enabled: true, ..default() };
    let Ok((mut tf, mut fpc)) = players.single_mut() else { return };
    let t = at.t - *t0.get_or_insert_with(|| {
        let p: Vec<f32> = std::env::var("RESTRAFE_DISC_POS").ok().map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect()).unwrap_or_default();
        tf.translation = if p.len() == 3 { Vec3::new(p[0], p[1], p[2]) } else { Vec3::new(-30.25, 0.45, 22.0) };
        tf.rotation = Transform::IDENTITY.looking_to(Vec3::X, Vec3::Y).rotation;
        fpc.move_direction = Vec3::ZERO;
        at.t
    });
    for s in sounds.read() {
        if let crate::player::fpc::FpcSound::Indexed { index: 11, .. } = s {
            *lands += 1;
            println!("LAND t={t:.2} y={:.3}", tf.translation.y);
        }
    }
    if std::env::var_os("RESTRAFE_DISC_VERBOSE").is_some() && t > 0.9 && t < 1.6 {
        println!("DISC t={t:.3} pos={:.3?} grounded={} md.y={:.2} fall={:.2} contact={:?} steep={:?}", tf.translation, fpc.cc_grounded, fpc.move_direction.y, fpc.fall_distance, fpc.cc_ground_contact, fpc.cc_steep_contacts);
    }
    if t > 1.0 && t < 2.0 {
        input.move_raw = Vec2::Y;
        input.run = Button::digital(std::env::var_os("RESTRAFE_DISC_SPRINT").is_some(), false, false);
    }
    if t > 2.5 {
        println!("RESULT disc lands={} end={:.2?}", *lands, tf.translation);
        exit.write(AppExit::Success);
    }
}

/// `RESTRAFE_AUTOTEST=katanawj`: maxWallJumps 3 (Katana) in the WJ_A / WJ_B corridor (walls x -14 / -20): run at WJ_A,
/// jump, wall-jump off it as soon as CanWallJump, turn round, wall-jump off WJ_B, and so on (WJ lines).
pub fn katana_wj_test(time: Res<Time>, mut at: ResMut<AutoTest>, mut input: ResMut<PlayerInput>, mut players: Query<(&mut Transform, &mut Fpc)>, mut exit: MessageWriter<AppExit>, mut st: Local<(Option<f32>, u32, f32)>) {
    if at.name != "katanawj" || !at.ready {
        return;
    }
    at.t += time.delta_secs();
    *input = PlayerInput { enabled: true, ..default() };
    let Ok((mut tf, mut fpc)) = players.single_mut() else { return };
    fpc.max_wall_jumps = 3;
    let t = at.t - *st.0.get_or_insert_with(|| {
        tf.translation = Vec3::new(-17.0, 0.2, -18.0);
        tf.rotation = Transform::IDENTITY.looking_to(Vec3::X, Vec3::Y).rotation;
        at.t
    });
    let (phase, last) = (st.1, st.2);
    if t > 0.3 {
        input.move_raw = Vec2::Y;
        input.run = Button::digital(true, false, false);
    }
    let press = |input: &mut PlayerInput| input.jump = Button::digital(true, true, false);
    if phase == 0 && t > 0.3 && tf.translation.x > -16.0 {
        press(&mut input);
        st.1 = 1;
        st.2 = t;
    } else if phase >= 1 && fpc.can_wall_jump && t - last > 0.15 {
        press(&mut input);
        println!("WJ jump {} t={t:.2} x={:.2} y={:.2} wjc_before={} wall_n={:.2?}", phase, tf.translation.x, tf.translation.y, fpc.wall_jumps_count, fpc.wall_point_normal);
        // turn to the other wall
        let fwd = tf.forward().as_vec3();
        tf.rotation = Transform::IDENTITY.looking_to(Vec3::new(-fwd.x, 0.0, -fwd.z), Vec3::Y).rotation;
        st.1 = phase + 1;
        st.2 = t;
    }
    if (t * 20.0).floor() != ((t - time.delta_secs()) * 20.0).floor() && t > 0.3 {
        println!("WJ t={t:.2} x={:.2} y={:.2} grounded={} cwj={} wjc={} touch={} hits_wall={}", tf.translation.x, tf.translation.y, fpc.cc_grounded, fpc.can_wall_jump, fpc.wall_jumps_count, fpc.is_touching_anything, fpc.cc_steep_contacts.len());
    }
    if t > 4.0 || (st.1 > 4) {
        println!("RESULT katanawj jumps={} wjc={}", st.1.saturating_sub(1), fpc.wall_jumps_count);
        exit.write(AppExit::Success);
    }
}

/// `RESTRAFE_AUTOTEST=taunt`: presses 1..9, 0 one second apart (voice lines; SOUND log).
pub fn taunt_test(mut commands: Commands, time: Res<Time>, mut at: ResMut<AutoTest>, mut keys: ResMut<ButtonInput<KeyCode>>, mut input: ResMut<PlayerInput>, mut exit: MessageWriter<AppExit>, mut step: Local<u32>) {
    if at.name != "taunt" || !at.ready {
        return;
    }
    input.enabled = true;
    at.t += time.delta_secs();
    const KEYS: [KeyCode; 10] = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7, KeyCode::Digit8, KeyCode::Digit9, KeyCode::Digit0];
    let k = (*step / 2) as usize;
    if k >= KEYS.len() {
        if at.t > 12.0 {
            exit.write(AppExit::Success);
        }
        return;
    }
    if *step % 2 == 0 && at.t > 1.0 + k as f32 * 1.0 {
        // RESTRAFE_TAUNT_SHOTS=1 (with RESTRAFE_TAB=1): the Tab preview pose of the previous key -> target/shots/pose_N.png.
        if k > 0 && std::env::var_os("RESTRAFE_TAUNT_SHOTS").is_some() {
            use bevy::render::view::screenshot::{Screenshot, save_to_disk};
            let _ = std::fs::create_dir_all("target/shots");
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("target/shots/pose_{k}.png")));
        }
        keys.press(KEYS[k]);
        println!("TAUNT press {} t={:.2}", (k + 1) % 10, at.t);
        *step += 1;
    } else if *step % 2 == 1 {
        keys.release(KEYS[k]);
        *step += 1;
    }
}

/// `RESTRAFE_GPU_INFO=1`: logs the render device limits / features that gate Bevy's binding arrays (lightmaps in
/// deferred, light probes).
pub fn gpu_info(dev: Option<Res<bevy::render::renderer::RenderDevice>>, mut done: Local<bool>) {
    if *done || std::env::var_os("RESTRAFE_GPU_INFO").is_none() {
        return;
    }
    let Some(dev) = dev else { return };
    *done = true;
    let l = dev.limits();
    println!("GPUINFO max_binding_array_elements_per_shader_stage={} max_sampled_textures_per_shader_stage={} features={:?}", l.max_binding_array_elements_per_shader_stage, l.max_sampled_textures_per_shader_stage, dev.features());
}

/// RESTRAFE_AUTOTEST=hands: PlayerPickup flow. Glock -> right, Revolver (RESTRAFE_WEAPON2) -> left, scroll switch,
/// empty clicks auto-drop the right gun + SwitchWeapons (left moves right), Glock -> left again, then F with nothing
/// targeted drops the empty right gun and does NOT switch (dropTimer was just set). Prints HANDS lines.
pub fn hands_test(
    time: Res<Time>,
    mut at: ResMut<AutoTest>,
    mut input: ResMut<PlayerInput>,
    rack: Option<Res<crate::map::WeaponRack>>,
    mut players: Query<(&mut Transform, &mut Fpc, &crate::weapons::pickup::Hands)>,
    mut items: Query<&mut crate::weapons::Item>,
    mut exit: MessageWriter<AppExit>,
    mut step: Local<u32>,
) {
    if at.name != "hands" || !at.ready {
        return;
    }
    at.t += time.delta_secs();
    let t = at.t;
    *input = PlayerInput { enabled: true, ..default() };
    let Ok((mut tf, mut fpc, hands)) = players.single_mut() else { return };
    let (r, l) = (hands.right, hands.left);
    let name = |items: &Query<&mut crate::weapons::Item>, e: Option<Entity>| e.and_then(|e| items.get(e).ok()).map(|i| format!("{}({})", i.def.name, i.current_ammo));
    let slot = |n: &str| rack.as_ref().and_then(|r| r.slots.iter().find(|s| s.0 == n).map(|s| s.1));
    let b = std::env::var("RESTRAFE_WEAPON2").unwrap_or("Revolver".into());
    let mut face = |n: &str| {
        if let Some(g) = slot(n) {
            tf.translation = Vec3::new(g.x, 0.2, g.z - 1.2);
            tf.rotation = crate::unity::unity_euler(0.0, 180.0, 0.0);
            fpc.rotation_x = 35.0;
        }
    };
    let press = Button::digital(true, true, false);
    let s = *step;
    let next = match s {
        0 => t > 0.5,
        1 => t > 1.0,
        2 => t > 1.5,
        3 => t > 2.0,
        4 => t > 2.5,
        5 => t > 3.0,
        6..=11 => t > 3.2 + (s - 6) as f32 * 0.4,
        12 => t > 6.0,
        13 => t > 6.5,
        14 => t > 7.0,
        15 => t > 7.5,
        16 => t > 8.0,
        17 => t > 8.5,
        _ => t > 9.0,
    };
    if !next {
        return;
    }
    match s {
        0 => face("Glock"),
        1 | 3 | 14 | 16 => input.interact = press,
        2 => face(&b),
        13 => face("Glock"),
        4 | 15 => {
            println!("HANDS step={s} right={:?} left={:?}", name(&items, r), name(&items, l));
            tf.translation = Vec3::new(-8.0, 0.2, 6.0);
            tf.rotation = crate::unity::unity_euler(0.0, 0.0, 0.0);
            fpc.rotation_x = -20.0;
            if s == 4 {
                input.scroll = 1.0;
            } else if let Some(mut it) = r.and_then(|e| items.get_mut(e).ok()) {
                it.current_ammo = 0;
                it.charged_bullets = 0.0;
            }
        }
        5 => {
            println!("HANDS step={s} (after scroll) right={:?} left={:?}", name(&items, r), name(&items, l));
            if let Some(mut it) = r.and_then(|e| items.get_mut(e).ok()) {
                it.current_ammo = 0;
                it.charged_bullets = 0.0;
            }
        }
        6..=11 => {
            let down = (s - 6) % 2 == 0;
            input.fire = Button::digital(down, down, !down);
        }
        12 | 17 => println!("HANDS step={s} right={:?} left={:?}", name(&items, r), name(&items, l)),
        _ => {
            exit.write(AppExit::Success);
        }
    }
    *step += 1;
}

/// RESTRAFE_GPU_TIMES=1 (see main.rs).
pub fn gpu_times(store: Res<bevy::diagnostic::DiagnosticsStore>, time: Res<Time<bevy::time::Real>>, mut last: Local<f32>) {
    let mut v: Vec<(String, f64)> = Vec::new();
    for d in store.iter() {
        let p = d.path().as_str();
        if p.starts_with("render/") && p.ends_with("/elapsed_gpu") && (std::env::var_os("RESTRAFE_GPU_ALL").is_some() || p.matches('/').count() == 2) {
            if let Some(x) = d.measurement().map(|m| m.value) {
                v.push((p[7..p.len() - 12].to_string(), x));
            }
        }
    }
    let total: f64 = v.iter().map(|x| x.1).sum();
    let t = time.elapsed_secs();
    if total > 8.0 || t - *last > 1.0 {
        *last = t;
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        let top: Vec<String> = v.iter().take(8).map(|(n, x)| format!("{n}={x:.2}")).collect();
        println!("GPUT t={t:.3} total={total:.2}ms {}", top.join(" "));
    }
}
