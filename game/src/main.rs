//! Restrafe: a single-player Bevy re-implementation of STRAFTAT's mechanics.

// Release builds are a windowed program (no console window when started by double-clicking the exe); output still
// reaches a pipe or file when one is attached (autotests, `> log.txt`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod animator;
mod install;
#[allow(dead_code)]
mod unity_fs;
#[allow(dead_code)]
mod owned;
mod fsb5;
#[allow(dead_code)]
mod dotnet_meta;
#[allow(dead_code)]
mod owned_gltf;
#[allow(dead_code)]
mod prefab_json;
mod owned_cosmetics;
mod anim_clip;
mod anim_ctl;
mod owned_docs;
mod audio;
mod autotest;
mod cosmetics;
mod crosshair;
mod screen_ui;
mod death;
mod fparms;
mod fpbob;
mod frame_probe;
mod fx;
mod gpu_timer;
mod health;
mod hud;
mod hud_verlet;
mod humanoid;
mod input;
mod map;
mod physics;
mod pipeline_warmup;
mod props;
mod scene_map;
mod scene_props;
mod weapon_menu;
mod player;
mod postfx;
mod ppv2;
mod ppv2_ssr;
mod unity_light;
mod ragdoll;
mod settings;
mod skinning;
mod suppression;
mod texquality;
mod transparent_sort;
mod settings_menu;
mod outline;
mod tmp_text;
mod tween;
mod ugui;
mod ugui_assets;
mod ugui_scene;
mod unity;
mod unity_pbr;
mod vfx;
mod weapons;

use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use bevy_rapier3d::prelude::*;

/// The folder holding the game's `assets/` (our shaders) and `config/`: the nearest folder above the exe that has
/// them, directly or under `game/` (`game\target\release\restrafe.exe`, or the `Restrafe.exe` copy that
/// `launch.cmd` puts in the repository root), else the folder the game was built in.
fn game_dir() -> std::path::PathBuf {
    let has = |d: &std::path::Path| d.join("assets").join("shaders").is_dir();
    if let Ok(exe) = std::env::current_exe() {
        for a in exe.ancestors().skip(1) {
            if has(a) {
                return a.to_path_buf();
            }
            if has(&a.join("game")) {
                return a.join("game");
            }
        }
    }
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Shows an error to the player: a message box (release builds have no console), and stderr.
fn error_box(text: &str) {
    eprintln!("{text}");
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (t, c) = (wide(text), wide("Restrafe"));
        unsafe {
            MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR);
        }
    }
}

fn main() {
    // Run from the game folder so assets/ and config/ resolve no matter how the exe is launched.
    let _ = std::env::set_current_dir(game_dir());
    let interactive = std::env::var_os("RESTRAFE_AUTOTEST").is_none() && std::env::var_os("RESTRAFE_QUIET").is_none();
    if interactive {
        // Without a console a crash would vanish silently: show it.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            default_hook(info);
            error_box(&format!("Restrafe stopped because of an error:\n\n{info}"));
        }));
        if let Err(e) = install::discover(None) {
            error_box(&format!(
                "Restrafe could not find STRAFTAT.\n\n{e}\n\nInstall STRAFTAT through Steam, or point Restrafe at its folder once with\n  cargo run --release --bin setup -- --game \"<STRAFTAT folder>\"\n(run from the game folder of Restrafe)."
            ));
            return;
        }
    }
    let settings = settings::Settings::load();
    let mut app = App::new();
    // Effects/projectiles are short-lived: a deferred command can target an entity despawned earlier in
    // the same frame. Log those instead of crashing (Bevy's default handler panics).
    app.set_error_handler(bevy::ecs::error::warn);
    // Quiet test mode (any autotest, or RESTRAFE_QUIET=1): window far off-screen, no taskbar entry, never focused, and
    // silent output; screenshots still work and every started sound is logged as `SOUND <t> <clip>` instead.
    let quiet = std::env::var_os("RESTRAFE_AUTOTEST").is_some() || std::env::var_os("RESTRAFE_QUIET").is_some();
    // Synchronous pipeline compilation: with async compilation Bevy skips a draw until its pipeline is ready, and a
    // view-key change (new view flags) leaves the deferred lighting pass out for those frames: all lit geometry black
    // while the sky / unlit text still draw (user-reported black flashes). A compile now costs a hitch instead.
    // RESTRAFE_ASYNC_PIPELINES=1 restores Bevy's default.
    let render = bevy::render::RenderPlugin { synchronous_pipeline_compilation: std::env::var_os("RESTRAFE_ASYNC_PIPELINES").is_none(), ..default() };
    // Images missing from game/assets are read in place from the STRAFTAT installation (owned.rs).
    owned::register_source(&mut app, concat!(env!("CARGO_MANIFEST_DIR"), "/assets"));
    app.add_plugins(DefaultPlugins.set(render).set(AssetPlugin { file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets").into(), ..default() }).set(WindowPlugin {
        primary_window: Some(Window {
            title: "Restrafe".into(),
            position: if quiet { WindowPosition::At(IVec2::new(-30000, -30000)) } else { WindowPosition::Automatic },
            skip_taskbar: quiet,
            focused: !quiet,
            // Debug: RESTRAFE_WINDOW=1920x1080 (physical pixels) e.g. to compare screenshots with the original.
            resolution: std::env::var("RESTRAFE_WINDOW")
                .ok()
                .and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))))
                .map(|(w, h)| bevy::window::WindowResolution::new(w, h).with_scale_factor_override(1.0))
                .unwrap_or_default(),
            ..default()
        }),
        ..default()
    }))
    .add_plugins(RapierPhysicsPlugin::<NoUserData>::default())
    // TimeManager: physics advances by the real frame time up to Unity's Maximum Allowed Timestep (0.333 s); rapier's
    // default caps a step at 1/60 s, so below 60 fps rigidbodies (ragdolls, grenades, dropped weapons) ran slow-motion.
    .insert_resource(bevy_rapier3d::plugin::TimestepMode::Variable { max_dt: 0.333_333_34, time_scale: 1.0, substeps: 1 })
    .insert_resource(settings)
    // Opaque StandardMaterial / UnityPbr surfaces go through the G-buffer on views with DeferredPrepass (PC deferred path).
    .insert_resource(if std::env::var_os("RESTRAFE_FORWARD").is_some() { bevy::pbr::DefaultOpaqueRendererMethod::forward() } else { bevy::pbr::DefaultOpaqueRendererMethod::deferred() })
    .init_resource::<input::PlayerInput>()
    .init_resource::<input::SimPad>()
    .add_plugins(cosmetics::CosmeticsPlugin)
    .add_plugins(ppv2_ssr::Ppv2SsrPlugin)
    .add_plugins(pipeline_warmup::PipelineWarmupPlugin)
    .add_plugins(frame_probe::FrameProbePlugin)
    .add_plugins(gpu_timer::GpuTimerPlugin)
    .add_plugins(humanoid::HumanoidPlugin)
    .add_plugins(skinning::SkinningPlugin)
    .add_plugins(fparms::FpArmsPlugin)
    .add_plugins(fpbob::FpBobPlugin)
    .add_plugins(postfx::PostFxPlugin)
    .add_plugins(ppv2::Ppv2Plugin)
    .add_plugins(unity_light::UnityLightPlugin)
    .add_plugins(suppression::SuppressionPlugin)
    .add_plugins(ragdoll::RagdollPlugin)
    .add_plugins(tmp_text::TmpTextPlugin)
    .add_plugins(outline::OutlinePlugin)
    .add_plugins(unity_pbr::UnityPbrPlugin)
    .add_plugins(hud_verlet::HudVerletPlugin)
    .add_plugins(death::DeathPlugin)
    .add_plugins((texquality::TexQualityPlugin, transparent_sort::TransparentSortPlugin))
    .add_plugins((player::PlayerPlugin, weapons::WeaponsPlugin, audio::AudioPlugin, fx::FxPlugin, hud::HudPlugin, health::HealthPlugin, crosshair::CrosshairPlugin, settings_menu::SettingsMenuPlugin, screen_ui::ScreenUiPlugin))
    .add_systems(Startup, owned::startup_check)
    .add_systems(Startup, (map::setup_map, map::spawn_weapon_rack, map::spawn_training_dummy, map::spawn_props, spawn))
    .add_plugins((props::PropsPlugin, scene_map::SceneMapPlugin, scene_props::ScenePropsPlugin, weapon_menu::WeaponMenuPlugin))
    .add_systems(Update, (weapons::pickup::setup_fparms, map::refill_rack, map::spawn_map_dummy))
    .add_systems(PreUpdate, physics::sync_layers)
    .add_systems(PostUpdate, physics::clamp_angular_speed.before(bevy_rapier3d::prelude::PhysicsSet::SyncBackend))
    .add_systems(PreUpdate, input::configure_gamepads)
    .add_systems(Update, (cursor_lock, input::gather_input).chain());
    if quiet {
        app.insert_resource(bevy::audio::GlobalVolume::new(bevy::audio::Volume::SILENT)).add_systems(PostUpdate, log_sounds);
    }
    if std::env::var_os("RESTRAFE_DEBUG_PHYS").is_some() {
        app.add_plugins(RapierDebugRenderPlugin::default());
    }
    if let Ok(name) = std::env::var("RESTRAFE_AUTOTEST") {
        app.insert_resource(autotest::AutoTest {
            name,
            t: 0.0,
            log_t: 0.0,
            prev: [false; 4],
            ready: false,
            good_frames: 0,
        })
        .add_systems(
            Update,
            (autotest::place, autotest::drive, autotest::fall_test, autotest::debug_nodes)
                .after(input::gather_input)
                .before(player::fpc::fpc_update),
        )
        .add_systems(
            Update,
(autotest::weapon_test, autotest::shot_test, autotest::suicide_test, autotest::barrel_test, autotest::map_view_test, autotest::scene_props_test, autotest::wall_shot_test, autotest::steep_test, autotest::ledge_test, autotest::ramp_test, autotest::disc_test, autotest::katana_wj_test, autotest::walk_test, autotest::hands_test)
                .after(input::gather_input)
                .before(player::fpc::fpc_update),
        )
        .add_systems(Update, autotest::warmup.before(autotest::drive).before(autotest::weapon_test).before(autotest::shot_test).before(autotest::suicide_test).before(autotest::menu_shot))
        .add_systems(Update, autotest::weapon_menu_test.before(weapon_menu::toggle))
        .add_systems(Update, autotest::taunt_test.before(input::gather_input))
        .add_systems(Update, autotest::menu_shot.before(settings_menu::menu_update).before(ugui::ugui_input))
        .add_systems(PostUpdate, (autotest::log, autotest::debug_near, autotest::debug_meshes, autotest::dump_world, autotest::gpu_info, autotest::heights_log.after(TransformSystems::Propagate)));
    }
    // Debug: RESTRAFE_GPU_TIMES=1: GPU timestamp per render pass; frames whose GPU total exceeds 8 ms (or every 1 s)
    // print "GPUT total=.. top passes".
    if std::env::var_os("RESTRAFE_GPU_TIMES").is_some() {
        app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin).add_systems(Last, autotest::gpu_times);
    }
    app.init_asset_loader::<owned::UtexLoader>();
    app.run();
}

fn spawn(mut commands: Commands) {
    player::fpc::spawn_player(&mut commands, death::PLAYER_SPAWN, 0.0);
}

/// Escape releases the mouse (pause stand-in); clicking recaptures it.
fn cursor_lock(
    mouse: Res<ButtonInput<MouseButton>>,
    menu: Res<settings_menu::MenuOpen>,
    weapon_menu: Res<weapon_menu::WeaponMenu>,
    cosmetics_open: Res<cosmetics::CosmeticsMenuOpen>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut input: ResMut<input::PlayerInput>,
) {
    let Ok(mut c) = cursor.single_mut() else {
        return;
    };
    // Escape is handled by the settings menu; clicking the window recaptures the mouse when no
    // menu is open.
    if mouse.just_pressed(MouseButton::Left) && !input.enabled && !menu.0 && !weapon_menu.open && !cosmetics_open.0 {
        c.grab_mode = CursorGrabMode::Locked;
        c.visible = false;
        input.enabled = true;
    }
}

/// Quiet mode: the output is muted, so log every sound as it starts (clip path, volume, 3D or 2D).
fn log_sounds(time: Res<Time>, settings: Res<settings::Settings>, q: Query<(&AudioPlayer, &PlaybackSettings, Option<&Transform>), Added<AudioPlayer>>) {
    let base = settings.master_volume * settings.effects_volume;
    for (p, s, tf) in &q {
        let path = p.0.path().map(|p| p.to_string()).unwrap_or_else(|| "?".into());
        let vol = match s.volume {
            bevy::audio::Volume::Linear(v) => v,
            other => other.to_linear(),
        };
        if base > 0.0 {
            print!("rel={:.3} ", vol / base);
        }
        match (s.spatial, tf) {
            (true, Some(t)) => println!("SOUND t={:.2} {path} vol={vol:.2} speed={:.2} at={:.1?}", time.elapsed_secs(), s.speed, t.translation),
            _ => println!("SOUND t={:.2} {path} vol={vol:.2} speed={:.2}", time.elapsed_secs(), s.speed),
        }
    }
}
