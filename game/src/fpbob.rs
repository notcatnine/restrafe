//! Port of FPArms.cs `Update` (viewmodel bob/jump/fall/idle offsets on BobPosition + FPArms, TiltSway, landing DOLocalMove).
//! PlayerIK FPArms prefab values. `heavy` / `vertical` come from the held weapon (Weapon.Update: fpArms.heavy =
//! behaviour.heavy; they keep their last value when nothing is held). restPos = FPArms start local pos (0,0,0).
use crate::player::fpc::Fpc;
use crate::tween::{Ease, Tweens};
use crate::unity::{lerp3, slerp, unity_euler};
use crate::weapons::{BobPosition, FpArms};
use bevy::prelude::*;
use crate::weapons::data::UField;
use bevy_rapier3d::prelude::ReadRapierContext;

#[derive(Component, Default)]
pub struct FpBob {
    idle_timer: f32,
    bob_timer: f32,
    land_timer: f32,
    jumped: bool,
    heavy: bool,
    vertical: bool,
}

#[allow(clippy::too_many_arguments)]
fn fparms_bob(
    time: Res<Time>,
    input: Res<crate::input::PlayerInput>,
    rapier: ReadRapierContext,
    mut players: Query<(&mut Fpc, &Transform), (Without<FpArms>, Without<BobPosition>)>,
    mut arms: Query<(&mut Transform, &mut Tweens, Option<&mut FpBob>, Entity, &ChildOf), With<FpArms>>,
    mut bobs: Query<&mut Transform, (With<BobPosition>, Without<FpArms>, Without<Fpc>)>,
    items: Query<&crate::weapons::Item>,
    mut commands: Commands,
) {
    // Weapon.Update of every held weapon writes fpArms.heavy / vertical (right hand last here).
    let held: Vec<(bool, bool)> = {
        let mut v: Vec<_> = items.iter().filter(|i| i.hand != crate::weapons::Hand::None).map(|i| (i.hand == crate::weapons::Hand::Right, i.def.item.b("heavy"), i.def.item.b("vertical"))).collect();
        v.sort_by_key(|x| x.0);
        v.into_iter().map(|(_, h, ve)| (h, ve)).collect()
    };
    let (Ok((mut fpc_mut, ptf)), Ok(ctx)) = (players.single_mut(), rapier.single()) else { return };
    // Weapon.WeaponAnimation's fpArms tween (requested by the weapon this frame; DOTween runs after FPArms.Update).
    let arms_anim = fpc_mut.fparms_anim.take();
    let fpc = &*fpc_mut;
    let dt = time.delta_secs();
    let t = time.elapsed_secs();
    for (mut tf, mut tweens, st, e, parent) in &mut arms {
        let Some(mut st) = st else {
            commands.entity(e).insert(FpBob::default());
            continue;
        };
        let Ok(mut bob) = bobs.get_mut(parent.parent()) else { continue };
        if let Some(&(h, v)) = held.last() {
            st.heavy = h;
            st.vertical = v;
        }
        let (jump_off, fall_off) = (Vec3::new(0.0, 0.3, 0.0), Vec3::new(0.0, 0.3, 0.0));
        let (jump_bob_speed, fall_bob_speed) = (5.0, 3.5);
        let (bob_scale, bob_speed, crouch_scale, crouch_speed, sprint_scale, sprint_speed) = (0.6, 6.0, 0.15, 4.0, 1.2, 11.0);
        let (aim_scale, aim_speed, reset) = (0.4, 0.5, 3.0);
        let (idle_sin, idle_scale, idle_lerp, max_idle) = (3.0, 0.01, 31.43, 0.18);
        let rest = Vec3::ZERO;
        let paused = !input.enabled;
        let pos = ptf.translation;
        let mask = crate::physics::DEFAULT_RAYCAST_LAYERS;
        let no_ground_near = !crate::physics::ray(&ctx, pos, Vec3::NEG_Y, 0.5, mask);
        // Jump callback (fires before Update).
        if input.jump.pressed && crate::physics::ray(&ctx, pos, Vec3::NEG_Y, 1.3, mask) {
            st.jumped = true;
        }
        st.land_timer -= dt;
        let crouchy = fpc.is_crouching || fpc.is_leaning;
        let sc = if crouchy { crouch_scale } else if fpc.is_sprinting { sprint_scale } else { bob_scale };
        let sp = if crouchy { crouch_speed } else if fpc.is_sprinting { sprint_speed } else { bob_speed };
        let air_jump = !fpc.is_grounded && st.jumped && !fpc.is_aiming;
        let air_fall = !air_jump && !fpc.is_grounded && no_ground_near && !fpc.is_aiming;
        if air_jump {
            tf.translation = lerp3(tf.translation, rest + jump_off, jump_bob_speed * dt);
        } else if air_fall {
            tf.translation = lerp3(tf.translation, rest + fall_off, fall_bob_speed * dt);
        } else if !fpc.is_walking && !fpc.is_sprinting && !fpc.is_sliding && fpc.is_grounded && fpc.player_speed < 0.3 {
            st.idle_timer += dt;
            let v = ((idle_sin * st.idle_timer).sin() * idle_scale).clamp(-max_idle, max_idle);
            tf.translation = lerp3(tf.translation, Vec3::new(rest.x, rest.y + v, rest.z), idle_lerp * dt);
            tf.rotation = slerp(tf.rotation, Quat::IDENTITY, reset * dt);
        } else {
            tf.translation = lerp3(tf.translation, rest, reset * dt);
            tf.rotation = slerp(tf.rotation, Quat::IDENTITY, reset * dt);
        }
        let moving = input.move_raw != Vec2::ZERO && !fpc.is_sliding && fpc.player_speed > 1.0 && !paused;
        if air_jump || air_fall {
            bob.translation = lerp3(bob.translation, rest, reset * dt);
        } else if moving && fpc.is_aiming {
            st.bob_timer += dt;
            st.idle_timer = 0.0;
            let num = 2.0 / (3.0 - (2.0 * t).cos());
            let b = bob.translation;
            let mut n = lerp3(b, Vec3::new(rest.x, b.y, rest.z), reset * dt);
            n.y += sc * aim_scale * (num * (2.0 * t * (sp * aim_speed)).sin() / 2.0 * dt);
            bob.translation = n;
        } else if moving && st.heavy {
            st.bob_timer += dt;
            st.idle_timer = 0.0;
            let tb = st.bob_timer;
            let num = 2.0 / (3.0 - (2.0 * tb).cos());
            let sp_y = if crouchy { crouch_speed } else if fpc.is_sprinting { sprint_speed / 3.0 } else { bob_speed / 3.0 };
            bob.translation.x += sc / 2.0 * (num * (tb * sp).sin() * dt);
            bob.translation.y += (sc + 2.0) * (num * (2.0 * tb * sp_y).sin() / 2.0 * dt);
        } else if moving && st.vertical {
            st.bob_timer += dt * sp / 6.0;
            st.idle_timer = 0.0;
            let tb = st.bob_timer;
            let num = 2.0 / (3.0 - (2.0 * tb).cos());
            let sc_v = if crouchy { crouch_scale } else if fpc.is_sprinting { sprint_scale } else { bob_scale / 10.0 };
            bob.translation.y += (sc_v + 1.7) * (num * (4.0 * tb).sin() / 2.0 * dt);
        } else if moving {
            st.bob_timer += dt;
            st.idle_timer = 0.0;
            let num = 2.0 / (3.0 - (2.0 * t).cos());
            bob.translation.x += sc * (num * (t * sp).cos() * dt);
            bob.translation.y += sc * (num * (2.0 * t * sp).sin() / 2.0 * dt);
        } else {
            bob.translation = lerp3(bob.translation, rest, reset * dt);
        }
        if (st.land_timer < 0.0 || st.jumped) && fpc.is_grounded {
            st.idle_timer = 0.0;
            tweens.local_move(rest, if st.jumped { 0.2 } else { 0.3 }, Ease::OutBack);
        }
        if fpc.is_grounded && st.jumped {
            st.jumped = false;
        }
        if fpc.is_grounded {
            st.land_timer = 0.15;
        }
        match arms_anim {
            Some(crate::weapons::FpArmsAnim::RotateBy { punch, duration }) => {
                tweens.local_rotate(crate::unity::to_unity_euler(tf.rotation) - punch, duration, Ease::OutQuad);
            }
            Some(crate::weapons::FpArmsAnim::Punch { punch, duration, vibrato, elasticity }) => {
                tweens.punch_rotation(punch, duration, vibrato, elasticity);
            }
            None => {}
        }
        if !paused {
            // TiltSway: Input.GetAxis("Mouse X/Y") = delta * 0.1 (InputManager sensitivity); rotationAmount/max 19, smooth 6.
            let a = (-input.mouse_delta.x * 0.1 * 19.0).clamp(-19.0, 19.0);
            let b = (input.mouse_delta.y * 0.1 * 19.0).clamp(-19.0, 19.0);
            let target = unity_euler(b, a, a);
            let init = if fpc.wall_slide_lean.abs() > 1.0 { unity_euler(0.0, 0.0, fpc.wall_slide_lean) } else { Quat::IDENTITY };
            tf.rotation = slerp(tf.rotation, target * init, dt * 6.0);
        }
    }
}

pub struct FpBobPlugin;
impl Plugin for FpBobPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, fparms_bob.after(crate::player::fpc::fpc_update));
    }
}
