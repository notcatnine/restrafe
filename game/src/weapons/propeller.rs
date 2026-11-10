//! Port of `Propeller.cs` (hold jump to fly; drains `power`, refilled on ground).
//! Networking removed: AudioPlay/AudioStop (ServerRpc) logic runs directly.

use super::data::UField;
use super::{Hand, Item, WeaponSound};
use crate::input::PlayerInput;
use crate::player::fpc::Fpc;
use crate::unity::lerp;
use bevy::prelude::*;

pub fn is_propeller(class: &str) -> bool {
    class == "Propeller"
}

#[derive(Component)]
pub struct PropellerState {
    pub power: f32,
    pub rotate_speed: f32,
    /// Accumulated `rotativePart.Rotate(rotateSpeed, 0, 0)` degrees about local X (TODO: apply to the
    /// "SM_Propeller00_Blades01" child transform of the model).
    pub blade_angle: f32,
    pub isflying: bool,
    /// networkAudioSource playing (looping propeller sound). TODO: integrator loops the prefab
    /// AudioSource while this is true.
    pub audio_playing: bool,
    active3: bool,
    pressed: bool,
    released: bool,
}

impl Default for PropellerState {
    fn default() -> Self {
        // C# `power = 4f` initial, `released = true`.
        PropellerState { power: 4.0, rotate_speed: 0.0, blade_angle: 0.0, isflying: false, audio_playing: false, active3: false, pressed: false, released: true }
    }
}

pub fn propeller_update(
    time: Res<Time>,
    input: Res<PlayerInput>,
    mut players: Query<&mut Fpc>,
    mut props: Query<(Entity, &mut Item, &mut PropellerState)>,
    mut sounds: MessageWriter<WeaponSound>,
    mut stops: MessageWriter<super::WeaponSoundStop>,
) {
    let dt = time.delta_secs();
    let Ok(mut fpc) = players.single_mut() else { return };
    let jump = input.jump.value();
    for (prop, mut item, mut ps) in &mut props {
        let item = &mut *item;
        let def = item.def.clone();
        let w = &def.w;
        // WeaponUpdate() runs in pickup.rs. layer == 7 (world item) or !IsOwner -> skip.
        if item.hand == Hand::None || !input.enabled {
            continue;
        }
        let ps = &mut *ps;
        let play_out = |sounds: &mut MessageWriter<WeaponSound>| {
            if let Some(c) = w.asset("propOutClip") {
                sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: Some(prop), rolloff: None });
            }
        };
        // AudioStop(): networkAudioSource.Stop() (same AudioSource as `audio`: also cuts a ringing propOutClip);
        // PlayReleaseClipObservers() -> audio.PlayOneShot(propOutClip)
        let audio_stop = |ps: &mut PropellerState, item: &mut Item, sounds: &mut MessageWriter<WeaponSound>, stops: &mut MessageWriter<super::WeaponSoundStop>| {
            ps.audio_playing = false;
            item.src_audio = Some(false);
            stops.write(super::WeaponSoundStop { source: prop });
            play_out(sounds);
        };
        if fpc.is_grounded {
            ps.power = w.f("maxPower");
        }
        if ps.power <= 0.0 && ps.active3 {
            audio_stop(ps, item, &mut sounds, &mut stops);
            ps.active3 = false;
        }
        if jump > 0.1 {
            // Fire()
            ps.isflying = ps.power >= 0.0;
            if !(ps.power < 0.0) {
                if ps.power < 0.1 {
                    ps.power = -1.0;
                    play_out(&mut sounds); // audio.PlayOneShot(propOutClip)
                }
                // Fly()
                ps.power -= dt;
                if fpc.move_direction.y < 7.0 {
                    fpc.move_direction.y += w.f("flySpeed") * dt;
                }
            }
        } else {
            ps.isflying = false;
        }
        if jump > 0.1 {
            ps.released = false;
        } else {
            ps.pressed = false;
        }
        if jump > 0.1 && !ps.pressed {
            ps.pressed = true;
            ps.active3 = true;
            ps.audio_playing = true; // AudioPlay()
            item.src_audio = Some(true);
        }
        if jump < 0.1 && !ps.released {
            ps.released = true;
            audio_stop(ps, item, &mut sounds, &mut stops);
        }
        let target = if ps.isflying { w.f("rotationSpeed") } else { 0.0 };
        ps.rotate_speed = lerp(ps.rotate_speed, target, w.f("lerpSpeed") * dt);
        ps.blade_angle += ps.rotate_speed;
    }
}
