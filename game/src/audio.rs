//! Sound playback: FirstPersonController sounds (footsteps, jump/land, ladder, wall, taunts /
//! voice lines) and weapon one-shots. Clip lists come from the PlayerIK prefab.

use crate::player::fpc::FpcSound;
use crate::settings::Settings;
use crate::weapons::WeaponSound;
use crate::weapons::data::{Prefab, UField};
use bevy::audio::{AudioSinkPlayback, PlaybackMode, Volume};
use bevy::prelude::*;
use std::collections::HashMap;

#[derive(Resource, Default)]
pub struct FpcClips {
    lists: HashMap<String, Vec<String>>,
    single: HashMap<String, String>,
    next_taunt_time: f64,
}

impl FpcClips {
    fn load() -> FpcClips {
        let mut c = FpcClips::default();
        let Ok(p) = Prefab::load("assets/data/player/PlayerIK.json") else { return c };
        let Some((_, f)) = p.first_with("FirstPersonController") else { return c };
        for (k, v) in f {
            if let Some(arr) = v.as_array() {
                let list: Vec<String> = arr.iter().filter_map(|x| x.as_str()?.strip_prefix("asset:").map(str::to_string)).collect();
                if !list.is_empty() {
                    c.lists.insert(k.clone(), list);
                }
            } else if let Some(a) = f.asset(k) {
                if a.starts_with("AudioClip/") {
                    c.single.insert(k.clone(), a);
                }
            }
        }
        c
    }

    /// `PlayOneShot(clips[0]); Shuffle(clips);`
    fn first_then_shuffle(&mut self, key: &str) -> Option<String> {
        let list = self.lists.get_mut(key)?;
        let clip = list.first().cloned();
        if list.len() > 1 {
            let n = crate::unity::random_range_i(1, list.len() as i32) as usize;
            list.swap(0, n);
        }
        clip
    }

    /// `clips[Mathf.RoundToInt(Random.Range(0, clips.Length))]`
    fn random(&self, key: &str) -> Option<String> {
        let list = self.lists.get(key)?;
        list.get(crate::unity::random_range_i(0, list.len() as i32) as usize).cloned()
    }

    /// `clips[Random.Range(0, clips.Length - 1)]`
    fn random_but_last(&self, key: &str) -> Option<String> {
        let list = self.lists.get(key)?;
        list.get(crate::unity::random_range_i(0, list.len() as i32 - 1) as usize).cloned()
    }
}

/// Ground tag -> clip list field name (FirstPersonController.JumpClipObservers).
fn list_for_tag(tag: &str) -> &'static str {
    match tag {
        "Footsteps/Concrete/Dalles" => "dallesClips",
        "Footsteps/Concrete/Solide" => "betonSolideClips",
        "Footsteps/Concrete/PleinAir" => "betonPleinairClips",
        "Footsteps/Wood/Creux" => "woodClips",
        "Footsteps/Wood/Sec" => "woodSecClips",
        "Footsteps/Dirt" => "dirtClips",
        "Footsteps/Sand" => "sandClips",
        "Footsteps/Grass" => "grassClips",
        "Footsteps/Graviers" => "graviersClips",
        "Footsteps/Water" => "waterClips",
        "Footsteps/Metal/Pipe2" => "metalClips",
        "Footsteps/Metal/Grille" => "grilleClips",
        "Footsteps/Metal/Pipe" => "pipeClips",
        "Footsteps/Matelas" => "matelasClips",
        "Footsteps/Moquette" => "moquetteClips",
        _ => "concreteClips",
    }
}

pub fn play(commands: &mut Commands, assets: &AssetServer, settings: &Settings, clip: &str, volume: f32) {
    play_pitched(commands, assets, settings, clip, volume, 1.0);
}

/// 2D one-shot at a given AudioSource pitch (Bevy playback speed).
pub fn play_pitched(commands: &mut Commands, assets: &AssetServer, settings: &Settings, clip: &str, volume: f32, pitch: f32) {
    commands.spawn((
        AudioPlayer::new(assets.load::<AudioSource>(clip.to_string())),
        PlaybackSettings {
            mode: PlaybackMode::Despawn,
            volume: Volume::Linear(volume * settings.effects_volume * settings.master_volume),
            speed: pitch,
            ..default()
        },
    ));
}

pub fn play_fpc_sounds(
    mut commands: Commands,
    assets: Res<AssetServer>,
    settings: Res<Settings>,
    time: Res<Time>,
    mut clips: ResMut<FpcClips>,
    mut reader: MessageReader<FpcSound>,
    players: Query<&crate::player::fpc::Fpc>,
) {
    let vol = settings.player_volume;
    // FirstPersonController.HandleMovement: `audio.pitch = Mathf.Lerp(0.5f, 1f, movementFactor)` every frame; every
    // FPC one-shot (footsteps, jump/land, wall jump, vault, ladder, taunts...) plays on that `audio` source. Heavy
    // weapons (Weapon.movementFactor < 1, e.g. 0.6 -> 0.8) lower it; Lerp clamps factors > 1 to pitch 1.
    let pitch = players.single().map(|f| 0.5 + 0.5 * f.movement_factor.clamp(0.0, 1.0)).unwrap_or(1.0);
    for s in reader.read() {
        let clip = match s {
            FpcSound::Indexed { index, ground_tag } => match index {
                1 => clips.first_then_shuffle("concreteClips"),
                2 => clips.first_then_shuffle("dallesClips"),
                3 => clips.first_then_shuffle("betonSolideClips"),
                4 => clips.first_then_shuffle("woodClips"),
                5 => clips.first_then_shuffle("dirtClips"),
                6 => clips.first_then_shuffle("sandClips"),
                7 => clips.first_then_shuffle("grassClips"),
                8 => clips.first_then_shuffle("graviersClips"),
                9 => clips.first_then_shuffle("waterClips"),
                12 => clips.first_then_shuffle("metalClips"),
                13 => clips.first_then_shuffle("pipeClips"),
                14 => clips.first_then_shuffle("matelasClips"),
                15 => clips.first_then_shuffle("moquetteClips"),
                16 => clips.first_then_shuffle("woodSecClips"),
                19 => clips.first_then_shuffle("grilleClips"),
                20 => clips.first_then_shuffle("betonPleinairClips"),
                17 => clips.random("ladderClips"),
                18 => clips.random("chainClips"),
                // JumpClip(): random clip for the surface below (raycast 3 m), else jumpClip.
                10 | 11 => if ground_tag.is_empty() { clips.single.get("jumpClip").cloned() } else { clips.random(list_for_tag(ground_tag)) },
                21 => clips.single.get("highFallAudioClip").cloned(),
                22 => clips.single.get("wallOutClip").cloned(),
                23 => clips.single.get("teleportClip").cloned(),
                _ => None,
            },
            FpcSound::WallJump { wall_tag } => clips.random(list_for_tag(wall_tag)),
            // VaultClipObservers: `clips[Random.Range(0, clips.Length - 1)]` (int overload: the last clip never plays).
            FpcSound::Vault { surface_tag } => clips.random_but_last(list_for_tag(surface_tag)),
            FpcSound::CeilingHit => clips.single.get("ceilingHitClip").cloned(),
            FpcSound::Taunt(i) => {
                // AboubiPlayObservers: 0.2 s rate limit.
                let now = time.elapsed_secs_f64();
                if now < clips.next_taunt_time {
                    None
                } else {
                    clips.next_taunt_time = now + 0.2;
                    clips.lists.get("tauntClip").and_then(|l| l.get(*i as usize)).cloned()
                }
            }
        };
        if let Some(c) = clip {
            play_pitched(&mut commands, &assets, &settings, &c, vol, pitch);
        }
    }
}

/// A one-shot playing on a weapon's own `audio` AudioSource (see `WeaponSound::source`).
#[derive(Component)]
pub struct WeaponSourceSound(pub Entity);

pub fn play_weapon_sounds(
    mut commands: Commands,
    assets: Res<AssetServer>,
    settings: Res<Settings>,
    mut reader: MessageReader<WeaponSound>,
    mut stops: MessageReader<crate::weapons::WeaponSoundStop>,
    playing: Query<(Entity, &WeaponSourceSound)>,
    time: Res<Time>,
    cam: Query<&GlobalTransform, With<crate::player::fpc::PlayerCamera>>,
) {
    // AudioSource.Stop(): cut every one-shot already playing on that source (before this frame's new clips).
    for st in stops.read() {
        let mut n = 0;
        for (e, src) in &playing {
            if src.0 == st.source {
                commands.entity(e).try_despawn();
                n += 1;
            }
        }
        if std::env::var_os("RESTRAFE_AUTOTEST").is_some() || std::env::var_os("RESTRAFE_QUIET").is_some() {
            println!("SOUND_STOP t={:.2} source={:?} stopped={n}", time.elapsed_secs(), st.source);
        }
    }
    let listener = cam.single().map(|g| g.translation()).ok();
    for s in reader.read() {
        let mut ec = match s.at {
            // 3D AudioSource, min distance 1: Logarithmic rolloff gain = 1/d, Linear = 1 - (d - 1) / (max - 1).
            // rodio's spatial sink pans and applies min(1, 1/d^2), so the volume is scaled by gain * d^2 to land on it.
            Some(at) => {
                let d = listener.map(|l| l.distance(at)).unwrap_or(1.0).max(1.0);
                // linear_max < 0: Logarithmic with min distance -linear_max (custom rolloff curves of that shape).
                let gain = if let Some(c) = &s.rolloff {
                    c.gain(listener.map(|l| l.distance(at)).unwrap_or(0.0))
                } else if s.linear_max > 1.0 {
                    (1.0 - (d - 1.0) / (s.linear_max - 1.0)).clamp(0.0, 1.0)
                } else if s.linear_max < 0.0 {
                    (-s.linear_max / d).min(1.0)
                } else {
                    1.0 / d
                };
                let vol_scale = gain * d * d;
                commands.spawn((
                    AudioPlayer::new(assets.load::<AudioSource>(s.clip.clone())),
                    PlaybackSettings {
                        mode: PlaybackMode::Despawn,
                        volume: Volume::Linear(s.volume * vol_scale * settings.effects_volume * settings.master_volume),
                        spatial: true,
                        speed: s.pitch,
                        ..default()
                    },
                    Transform::from_translation(at),
                ))
            }
            None => commands.spawn((
                AudioPlayer::new(assets.load::<AudioSource>(s.clip.clone())),
                PlaybackSettings {
                    mode: PlaybackMode::Despawn,
                    volume: Volume::Linear(s.volume * settings.effects_volume * settings.master_volume),
                    speed: s.pitch,
                    ..default()
                },
            )),
        };
        if let Some(src) = s.source {
            ec.insert(WeaponSourceSound(src));
        }
    }
}

pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(FpcClips::load()).add_systems(PostUpdate, (play_fpc_sounds, play_weapon_sounds, player_loops, weapon_sources, start_loop_emitters, restart_loop_emitters.after(start_loop_emitters), update_loop_emitters));
    }
}

/// Continuous player sounds driven by FirstPersonController/SlopeSlide state:
/// slideAudio (one-shot ground/wall slide clip), SlopeSlide audio (loop Slide_Gritty_02),
/// fallAudio (loop FallSound, volume lerped by the controller).
#[derive(Default)]
pub struct LoopChannels {
    slide: Option<(Entity, u8)>,
    slope: Option<Entity>,
    fall: Option<Entity>,
}

fn spawn_channel(commands: &mut Commands, assets: &AssetServer, clip: &str, looped: bool, vol: f32) -> Entity {
    commands
        .spawn((
            AudioPlayer::new(assets.load::<AudioSource>(clip.to_string())),
            PlaybackSettings { mode: if looped { PlaybackMode::Loop } else { PlaybackMode::Despawn }, volume: Volume::Linear(vol), ..default() },
        ))
        .id()
}

pub fn player_loops(
    mut commands: Commands,
    assets: Res<AssetServer>,
    settings: Res<Settings>,
    clips: Res<FpcClips>,
    players: Query<&crate::player::fpc::Fpc>,
    mut sinks: Query<&mut AudioSink>,
    exists: Query<()>,
    mut ch: Local<LoopChannels>,
) {
    let Ok(f) = players.single() else { return };
    let base = settings.effects_volume * settings.master_volume * settings.player_volume;
    let mut drive = |slot: &mut Option<Entity>, playing: bool, vol: f32, clip: &str, looped: bool, commands: &mut Commands| {
        if let Some(e) = *slot {
            if exists.get(e).is_err() {
                *slot = None;
            }
        }
        match (*slot, playing) {
            (None, true) => *slot = Some(spawn_channel(commands, &assets, clip, looped, vol * base)),
            (Some(e), true) => {
                if let Ok(mut s) = sinks.get_mut(e) {
                    s.set_volume(Volume::Linear(vol * base));
                }
            }
            (Some(e), false) => {
                commands.entity(e).despawn();
                *slot = None;
            }
            (None, false) => {}
        }
    };
    // slideAudio: clip chosen by ChangeSlideClip(0 = groundSlideClip, 1 = wallSlideClip); not looped.
    let slide_clip = if f.slide_clip == 1 { clips.single.get("wallSlideClip") } else { clips.single.get("groundSlideClip") }.cloned();
    if let Some(c) = slide_clip {
        let mut slot = ch.slide.map(|(e, _)| e);
        if ch.slide.map(|(_, k)| k != f.slide_clip).unwrap_or(false) {
            if let Some(e) = slot.take() {
                commands.entity(e).despawn();
            }
        }
        drive(&mut slot, f.slide_audio_playing, f.slide_audio_volume, &c, false, &mut commands);
        ch.slide = slot.map(|e| (e, f.slide_clip));
    }
    let mut slope = ch.slope;
    drive(&mut slope, f.ss_audio_playing, f.ss_audio_volume, "AudioClip/Slide_Gritty_02.ogg", true, &mut commands);
    ch.slope = slope;
    let mut fall = ch.fall;
    drive(&mut fall, f.fall_audio_playing, f.fall_audio_volume, "AudioClip/FallSound.ogg", true, &mut commands);
    ch.fall = fall;
}

/// `networkAudioSource.Play()/Stop()` on a weapon's own AudioSource (Minigun spin-up, ChargeGun charge, Propeller loop).
/// Weapons request it with `Item.src_audio = Some(true/false)`; the clip / volume / pitch / loop flag come from the
/// prefab's first AudioSource (what `GetComponent<AudioSource>()` returns). Play restarts, Stop cuts the sound; the
/// sound is also cut when the weapon is dropped (Hand::None) or despawned.
pub fn weapon_sources(
    mut commands: Commands,
    assets: Res<AssetServer>,
    settings: Res<Settings>,
    mut items: Query<(Entity, &mut crate::weapons::Item)>,
    mut map: Local<HashMap<Entity, Entity>>,
    exists: Query<()>,
) {
    // Cut sounds of dropped / despawned weapons; forget finished one-shots.
    map.retain(|owner, snd| {
        let held = items.get(*owner).map(|(_, i)| i.hand != crate::weapons::Hand::None).unwrap_or(false);
        let alive = exists.get(*snd).is_ok();
        if !held && alive {
            commands.entity(*snd).despawn();
        }
        held && alive
    });
    for (e, mut item) in &mut items {
        let cmd = if item.src_audio.is_some() { item.src_audio.take() } else { None };
        match cmd {
            Some(false) => {
                if let Some(s) = map.remove(&e) {
                    commands.entity(s).despawn();
                }
            }
            Some(true) => {
                if let Some(s) = map.remove(&e) {
                    commands.entity(s).despawn();
                }
                if item.hand == crate::weapons::Hand::None {
                    continue;
                }
                // NetworkAudioSource.audioSource is the AudioSource that has the clip: usually the first one, but on
                // BeamLoad it is the second (the first, `newSource`, has no clip), so pick the first with a clip.
                let Some(f) = item
                    .def
                    .prefab
                    .objects
                    .iter()
                    .flat_map(|o| o.components.iter())
                    .filter(|(t, f)| t.as_str() == "AudioSource" && f.asset("m_audioClip").is_some())
                    .map(|(_, f)| f)
                    .next()
                else {
                    continue;
                };
                let Some(clip) = f.asset("m_audioClip") else { continue };
                let looped = f.b("Loop");
                let s = commands
                    .spawn((
                        AudioPlayer::new(assets.load::<AudioSource>(clip)),
                        PlaybackSettings {
                            mode: if looped { PlaybackMode::Loop } else { PlaybackMode::Despawn },
                            volume: Volume::Linear(f.f("m_Volume") * settings.effects_volume * settings.master_volume),
                            speed: f.f("m_Pitch").max(0.01),
                            ..default()
                        },
                    ))
                    .id();
                map.insert(e, s);
            }
            None => {}
        }
    }
}

/// A looping 3D AudioSource carried by an entity (Unity `loop` + `playOnAwake`), e.g. ProphetProj's flight hum.
/// An empty `clip` means none. The player child is added by `start_loop_emitters`; its volume follows Unity's
/// rolloff every frame (`linear_max` > 1 = Linear, else Logarithmic), corrected for rodio's 1/d^2 like one-shots.
#[derive(Component, Clone)]
pub struct LoopEmitter {
    pub clip: String,
    /// Same encoding as `WeaponSound::linear_max` (> 1 Linear to that max, < 0 Logarithmic with min distance -v).
    pub linear_max: f32,
    /// AudioSource.volume.
    pub volume: f32,
    /// spatialBlend 0: heard at full volume everywhere.
    pub two_d: bool,
    /// `SetAmbientVolume`: follows SoundManager's ambient source volume instead of the effects volume.
    pub ambient: bool,
    /// Exact custom rolloff curve (overrides `linear_max`).
    pub rolloff: Option<std::sync::Arc<RolloffCurve>>,
}

/// AudioSource custom rolloff (rolloffMode 2): gain = curve(distance / maxDistance), Hermite keys, clamped ends.
#[derive(Debug)]
pub struct RolloffCurve {
    pub max: f32,
    /// Logarithmic rolloff (rolloffMode 0) with this minDistance instead of keys: min / d, constant past maxDistance.
    pub log_min: Option<f32>,
    /// (time, value, inSlope, outSlope)
    pub keys: Vec<[f32; 4]>,
}

impl RolloffCurve {
    pub fn from_keys(keys: &[serde_json::Value], max: f32) -> Option<RolloffCurve> {
        let g = |k: &serde_json::Value, n: &str| k[n].as_f64().unwrap_or(0.0) as f32;
        let keys: Vec<[f32; 4]> = keys.iter().map(|k| [g(k, "time"), g(k, "value"), g(k, "inSlope"), g(k, "outSlope")]).collect();
        (!keys.is_empty() && max > 0.0).then_some(RolloffCurve { max, log_min: None, keys })
    }

    /// Unity Logarithmic rolloff: gain = minDistance / distance, which stops attenuating at maxDistance.
    pub fn logarithmic(min: f32, max: f32) -> RolloffCurve {
        RolloffCurve { max: max.max(min), log_min: Some(min.max(1e-3)), keys: Vec::new() }
    }

    /// From an exported AudioSource (rolloffMode 0 Logarithmic / 2 Custom; Linear stays with `linear_max`), with
    /// maxDistance `max` (scripts may override the serialized one).
    pub fn from_source(a: &crate::weapons::data::Fields, max: f32) -> Option<RolloffCurve> {
        use crate::weapons::data::UField;
        match a.i("rolloffMode") {
            0 => Some(RolloffCurve::logarithmic(a.f("MinDistance"), max)),
            2 => {
                let keys = a.get("rolloffCustomCurve").and_then(|c| c.get("m_Curve")).and_then(|c| c.as_array()).cloned().unwrap_or_default();
                RolloffCurve::from_keys(&keys, max)
            }
            _ => None,
        }
    }

    pub fn gain(&self, distance: f32) -> f32 {
        if let Some(m) = self.log_min {
            return (m / distance.clamp(m, self.max)).min(1.0);
        }
        let t = distance / self.max;
        let k = &self.keys;
        if t <= k[0][0] {
            return k[0][1].max(0.0);
        }
        let last = k[k.len() - 1];
        if t >= last[0] {
            return last[1].max(0.0);
        }
        let n = k.windows(2).position(|w| t >= w[0][0] && t < w[1][0]).unwrap_or(0);
        let (a, b) = (k[n], k[n + 1]);
        let dt = (b[0] - a[0]).max(1e-6);
        let s = (t - a[0]) / dt;
        let (s2, s3) = (s * s, s * s * s);
        let v = (2.0 * s3 - 3.0 * s2 + 1.0) * a[1] + (s3 - 2.0 * s2 + s) * dt * a[3] + (-2.0 * s3 + 3.0 * s2) * b[1] + (s3 - s2) * dt * b[2];
        v.max(0.0)
    }
}

impl LoopEmitter {
    pub fn effect(clip: String, linear_max: f32) -> Self {
        LoopEmitter { clip, linear_max, volume: 1.0, two_d: false, ambient: false, rolloff: None }
    }
}

#[derive(Component)]
pub struct LoopPlayer {
    linear_max: f32,
    volume: f32,
    two_d: bool,
    ambient: bool,
    rolloff: Option<std::sync::Arc<RolloffCurve>>,
    /// Last world position (doppler velocity).
    last_pos: Option<Vec3>,
}

/// FMOD's default speed of sound (Unity AudioManager "Doppler Factor" 1, AudioSource dopplerLevel 1).
const SPEED_OF_SOUND: f32 = 340.0;

/// `audio.Play()` on a playing looping AudioSource: restarts it from the start, still on its entity (ProphetProj hits).
#[derive(Component)]
pub struct RestartLoop;

pub fn restart_loop_emitters(mut commands: Commands, assets: Res<AssetServer>, q: Query<(Entity, &LoopEmitter, Option<&Children>), With<RestartLoop>>, players: Query<(), With<LoopPlayer>>) {
    for (e, l, kids) in &q {
        commands.entity(e).remove::<RestartLoop>();
        for c in kids.into_iter().flatten() {
            if players.contains(*c) {
                commands.entity(*c).despawn();
            }
        }
        spawn_loop_player(&mut commands, &assets, e, l);
    }
}

fn spawn_loop_player(commands: &mut Commands, assets: &AssetServer, e: Entity, l: &LoopEmitter) {
    if l.clip.is_empty() {
        return;
    }
    commands.spawn((
        AudioPlayer::new(assets.load::<AudioSource>(l.clip.clone())),
        PlaybackSettings { mode: PlaybackMode::Loop, volume: Volume::SILENT, spatial: !l.two_d, ..default() },
        Transform::IDENTITY,
        LoopPlayer { linear_max: l.linear_max, volume: l.volume, two_d: l.two_d, ambient: l.ambient, rolloff: l.rolloff.clone(), last_pos: None },
        ChildOf(e),
    ));
}

pub fn start_loop_emitters(mut commands: Commands, assets: Res<AssetServer>, q: Query<(Entity, &LoopEmitter), Added<LoopEmitter>>) {
    for (e, l) in &q {
        if l.clip.is_empty() {
            continue;
        }
        commands.spawn((
            AudioPlayer::new(assets.load::<AudioSource>(l.clip.clone())),
            // spatialBlend 0 plays through a plain sink (no distance or panning at all).
            PlaybackSettings { mode: PlaybackMode::Loop, volume: Volume::SILENT, spatial: !l.two_d, ..default() },
            Transform::IDENTITY,
            LoopPlayer { linear_max: l.linear_max, volume: l.volume, two_d: l.two_d, ambient: l.ambient, rolloff: l.rolloff.clone(), last_pos: None },
            ChildOf(e),
        ));
    }
}

pub fn update_loop_emitters(
    time: Res<Time>,
    settings: Res<Settings>,
    global: Res<GlobalVolume>,
    cam: Query<&GlobalTransform, With<crate::player::fpc::PlayerCamera>>,
    mut q: Query<(&mut LoopPlayer, &GlobalTransform, Option<&mut SpatialAudioSink>, Option<&mut AudioSink>)>,
    mut last_listener: Local<Option<Vec3>>,
) {
    let Ok(cam) = cam.single() else { return };
    let dt = time.delta_secs().max(1e-4);
    let lp = cam.translation();
    // Teleports (respawn, scripted placement) are not motion.
    let tame = |v: Vec3| if v.length() > 100.0 { Vec3::ZERO } else { v };
    let vl = tame(last_listener.map(|p| (lp - p) / dt).unwrap_or(Vec3::ZERO));
    *last_listener = Some(lp);
    for (mut l, gt, spatial, plain) in &mut q {
        let channel = if l.ambient { settings.ambient_volume } else { settings.effects_volume };
        if l.two_d {
            if let Some(mut sink) = plain {
                sink.set_volume(Volume::Linear(l.volume * channel * settings.master_volume * global.volume.to_linear()));
            }
            continue;
        }
        let Some(mut sink) = spatial else { continue };
        let d = cam.translation().distance(gt.translation()).max(1.0);
        // rodio's spatial sink applies min(1, 1/d^2): scaled by d^2 so the gain lands on Unity's rolloff.
        let gain = if let Some(c) = &l.rolloff {
            c.gain(cam.translation().distance(gt.translation()))
        } else if l.linear_max > 1.0 {
            (1.0 - (d - 1.0) / (l.linear_max - 1.0)).clamp(0.0, 1.0)
        } else if l.linear_max < 0.0 {
            (-l.linear_max / d).min(1.0)
        } else {
            1.0 / d
        };
        let v = l.volume * gain * d * d * channel * settings.master_volume * global.volume.to_linear();
        sink.set_volume(Volume::Linear(v));
        // Doppler (FMOD, as Unity's AudioSource with dopplerLevel 1): pitch = (c - vL.dir) / (c - vS.dir), dir from
        // the source to the listener.
        let sp = gt.translation();
        let vs = tame(l.last_pos.map(|p| (sp - p) / dt).unwrap_or(Vec3::ZERO));
        l.last_pos = Some(sp);
        let dir = (lp - sp).normalize_or_zero();
        let num = SPEED_OF_SOUND - vl.dot(dir);
        let den = (SPEED_OF_SOUND - vs.dot(dir)).max(1.0);
        sink.set_speed((num / den).clamp(0.5, 2.0));
    }
}
