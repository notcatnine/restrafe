//! Port of `Settings.cs` (gameplay/video/audio options only; stats, Steam and lobby parts dropped).
//! Persisted as RON in `config/settings.ron` instead of Unity PlayerPrefs.

use crate::input::Bindings;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

const PATH: &str = "config/settings.ron";

#[derive(Resource, Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Controls
    pub toggle_two_axis: bool,
    pub invert_mouse_x: bool,
    pub invert_mouse_y: bool,
    pub sprint_toggle: bool,
    pub aim_toggle: bool,
    pub lean_toggle: bool,
    pub crouch_toggle: bool,
    pub reverse_sprint_bind: bool,
    pub inverse_fire_binding: bool,
    pub mouse_sensitivity: f32,
    pub mouse_aim_scope_sensitivity: f32,
    pub mouse_aim_sensitivity: f32,
    pub horizontal_sensitivity: f32,
    pub vertical_sensitivity: f32,
    pub bindings: Bindings,
    // Video / HUD
    pub normal_fov_value: f32,
    pub brightness: f32,
    pub damage_intensity: f32,
    pub disable_crosshair: bool,
    pub enable_fixed_crosshair: bool,
    pub show_speedometer: bool,
    pub use_vsync: i32,
    pub is_fullscreen: bool,
    pub exclusive_fullscreen: bool,
    pub minimalist_ui: bool,
    pub motion_blur: bool,
    pub target_fps: i32,
    pub target_fps_toggle: bool,
    pub reduce_vfx: bool,
    pub resolution: i32,
    pub quality_setting: i32,
    // Audio (SoundManager sources + AudioListener)
    pub master_volume: f32,
    pub effects_volume: f32,
    pub ambient_volume: f32,
    pub player_volume: f32,
    // Options shown in the settings UI whose systems are out of scope (music, voice chat): kept for the UI only.
    pub music_volume: f32,
    pub menu_music_volume: f32,
    pub voice_chat_volume: f32,
    pub enable_voice_chat: bool,
    /// VoiceChatMode: 0 OpenMic, 1 PushToTalk, 2 ToggleMute.
    pub voice_chat_mode: i32,
    pub in_game_music: bool,
    /// Runtime only: `Application.targetFrameRate` (-1 = none) as Settings.FirstTimeSetup / ChangeFrameRate /
    /// ChangeTargetFpsToggle left it (startup needs targetFps > 30; the menu handlers also apply 30).
    #[serde(skip)]
    pub target_frame_rate: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            toggle_two_axis: false,
            invert_mouse_x: false,
            invert_mouse_y: false,
            sprint_toggle: false,
            aim_toggle: false,
            lean_toggle: false,
            crouch_toggle: false,
            reverse_sprint_bind: false,
            inverse_fire_binding: false,
            mouse_sensitivity: 2.0,
            mouse_aim_scope_sensitivity: 0.8,
            mouse_aim_sensitivity: 2.0,
            horizontal_sensitivity: 2.0,
            vertical_sensitivity: 2.0,
            bindings: Bindings::default(),
            normal_fov_value: 75.0,
            brightness: 1.0,
            damage_intensity: 1.0,
            disable_crosshair: false,
            enable_fixed_crosshair: false,
            show_speedometer: false,
            use_vsync: 0,
            is_fullscreen: true,
            exclusive_fullscreen: false,
            minimalist_ui: false,
            motion_blur: false,
            target_fps: 120,
            target_fps_toggle: false,
            reduce_vfx: false,
            resolution: 0,
            quality_setting: 3,
            master_volume: 1.0,
            effects_volume: 1.0,
            ambient_volume: 1.0,
            player_volume: 1.0,
            music_volume: 1.0,
            menu_music_volume: 0.6,
            voice_chat_volume: 1.0,
            enable_voice_chat: true,
            voice_chat_mode: 1,
            in_game_music: false,
            target_frame_rate: -1,
        }
    }
}

/// Quiet test mode: any autotest or RESTRAFE_QUIET=1 (off-screen, muted).
pub fn quiet() -> bool {
    std::env::var_os("RESTRAFE_AUTOTEST").is_some() || std::env::var_os("RESTRAFE_QUIET").is_some()
}

impl Settings {
    pub fn load() -> Self {
        let mut s: Settings = std::fs::read_to_string(PATH)
            .ok()
            .and_then(|s| {
                ron::from_str(&s)
                    .map_err(|e| warn!("settings.ron invalid, using defaults: {e}"))
                    .ok()
            })
            .unwrap_or_default();
        // Autotests script held keys: the user's toggle settings (sprint / crouch / aim / lean) would change what they do.
        if std::env::var_os("RESTRAFE_AUTOTEST").is_some() {
            s.sprint_toggle = false;
            s.crouch_toggle = false;
            s.aim_toggle = false;
            s.lean_toggle = false;
        }
        // Debug: RESTRAFE_QUALITY=0..3 overrides the quality level (tests; never saved by autotests).
        if let Some(q) = std::env::var("RESTRAFE_QUALITY").ok().and_then(|v| v.parse().ok()) {
            s.quality_setting = q;
        }
        // Debug: RESTRAFE_TARGET_FPS=<n> turns the frame cap on at n (tests).
        if let Some(n) = std::env::var("RESTRAFE_TARGET_FPS").ok().and_then(|v| v.parse().ok()) {
            s.target_fps = n;
            s.target_fps_toggle = true;
        }
        // Settings.Start -> FirstTimeSetup: the cap applies only if targetFps > 30 (else Unity's default -1).
        s.target_frame_rate = if s.target_fps_toggle && s.target_fps > 30 { s.target_fps } else { -1 };
        // Quiet test mode (any autotest / RESTRAFE_QUIET): master volume 0, so every sound path is silent (some sinks
        // bypass GlobalVolume). Never saved (see save).
        if quiet() {
            // RESTRAFE_SOUND_VOL=1: an inaudible but non-zero master, so the SOUND log can report each sound's gain
            // relative to master * effects (`rel=`).
            s.master_volume = if std::env::var_os("RESTRAFE_SOUND_VOL").is_some() { 1e-6 } else { 0.0 };
        }
        s
    }

    pub fn save(&self) {
        // Autotests / quiet runs must not overwrite the user's settings (quiet mode zeroes the master volume).
        if quiet() {
            return;
        }
        let _ = std::fs::create_dir_all("config");
        match ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()) {
            Ok(s) => {
                let _ = std::fs::write(PATH, s);
            }
            Err(e) => error!("failed to save settings: {e}"),
        }
    }

    /// `Settings.Update` → localPlayer look speeds.
    /// Returns (lookSpeedX, lookSpeedY, lookSpeedAim, lookSpeedAimNoScope); `None` keeps the current value.
    pub fn look_speeds(&self) -> (f32, f32, Option<f32>, Option<f32>) {
        if self.toggle_two_axis {
            (
                self.horizontal_sensitivity,
                self.vertical_sensitivity,
                None,
                None,
            )
        } else {
            (
                self.mouse_sensitivity,
                self.mouse_sensitivity,
                Some(self.mouse_aim_scope_sensitivity),
                Some(self.mouse_aim_sensitivity),
            )
        }
    }
}
