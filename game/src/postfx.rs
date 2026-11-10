//! Camera post-processing on the world camera (PlayerCamera only):
//! * `CameraEffect.cs` + `S_PP_Damage00` (`_Damage`, `_Suppression`) — the original shader source is
//!   not recoverable (Amplify, exported as a dummy), the look is a red edge vignette.
//! * PPv2 `LensDistortion.intensity` and `ColorGrading.saturation` / `gamma` driven by `Fpc`
//!   (`lens_distortion`, `saturation`, Settings.brightness).
//! * Local PPv2 `PostProcessVolume`s of effect prefabs ([`PpVolume`]: Prophet ball, Repulsar shockwave, BlankState,
//!   HandCanon charge, Taser / StunMine lightning; all on layer 13, inside the Camera's volumeLayer 3|13): Grain,
//!   Vignette, ChromaticAberration, LensDistortion, ColorGrading gamma, Bloom (`ppv2::Ppv2Bloom` on the world camera)
//!   and DepthOfField (Bevy `DepthOfField`, CoC matched to PPv2's) blended over the Camera Profile like
//!   `PostProcessManager.UpdateSettings`.
//! `RESTRAFE_FX_TEST=damage|kill` forces the effect.

use crate::player::fpc::{Fpc, PlayerCamera};
use crate::settings::Settings;
use bevy::asset::embedded_asset;
use bevy::core_pipeline::fullscreen_material::{FullscreenMaterial, FullscreenMaterialPlugin};
use bevy::prelude::*;
use bevy::render::{extract_component::ExtractComponent, render_resource::ShaderType};
use bevy::shader::ShaderRef;

/// `CameraEffect.TakeHit` (sent when the local player's PlayerHealth drops).
#[derive(Message)]
pub struct TakeHit;

/// `CameraEffect.TrigSup` (Suppression.cs).
#[derive(Message)]
pub struct TrigSuppression;

/// CameraEffect prefab values (speed 5, start 1, suppSpeed 5, suppStart 3).
const SPEED: f32 = 5.0;
const START: f32 = 1.0;
const SUPP_SPEED: f32 = 5.0;
const SUPP_START: f32 = 3.0;

#[derive(Component, ExtractComponent, Clone, Copy, ShaderType, Default)]
pub struct PostFx {
    /// x = _Damage, y = _Suppression, z = lens intensity, w = saturation
    a: Vec4,
    /// xyz = ColorGrading inverse gamma (`ColorUtilities.ColorToInverseGamma`), w = time (grain phase)
    b: Vec4,
    /// Grain: x = intensity * 20 (0 = off), y = lumContrib, z = size, w = frame (random offset seed)
    c: Vec4,
    /// Vignette: x = intensity * 3, y = smoothness * 5, z = roundness exponent; w = ChromaticAberration amount
    d: Vec4,
    /// Vignette colour (rgb)
    e: Vec4,
}

/// A local (non-global) PPv2 `PostProcessVolume` with a SphereCollider trigger, on an effect object.
#[derive(Component, Clone, Debug)]
pub struct PpVolume {
    /// Profile asset name (`assets/data/pp/<name>.asset`).
    pub profile: String,
    /// SphereCollider radius (local; scaled by the object's world scale).
    pub radius: f32,
    pub blend: f32,
    pub weight: f32,
    pub priority: f32,
}

/// Overridden parameters of the active effects of a PPv2 profile (`overrideState: 1`).
#[derive(Default, Debug)]
struct PpProfile {
    grain_enabled: Option<bool>,
    grain_intensity: Option<f32>,
    grain_size: Option<f32>,
    grain_lum: Option<f32>,
    vig_intensity: Option<f32>,
    vig_smoothness: Option<f32>,
    vig_roundness: Option<f32>,
    vig_color: Option<Vec3>,
    ca_enabled: Option<bool>,
    ca_intensity: Option<f32>,
    lens_intensity: Option<f32>,
    cg_gamma: Option<Vec4>,
    bloom_enabled: Option<bool>,
    bloom_intensity: Option<f32>,
    bloom_threshold: Option<f32>,
    bloom_soft_knee: Option<f32>,
    bloom_diffusion: Option<f32>,
    bloom_color: Option<Vec3>,
    dof_enabled: Option<bool>,
    dof_focus: Option<f32>,
    dof_aperture: Option<f32>,
    dof_focal_length: Option<f32>,
    dof_kernel: Option<f32>,
}

impl PpProfile {
    /// Minimal reader of the profile YAML: `m_Name` / `active` per effect, then `param:` / `overrideState: 1` /
    /// `value: v` triples.
    fn load(name: &str) -> PpProfile {
        // the profile read from the installation
        let Some(text) = crate::owned::Owned::global().and_then(|o| o.pp_profile_yaml(name)) else {
            warn!("postfx: profile {name} missing from the installation");
            return PpProfile::default();
        };
        Self::parse(&text)
    }

    fn parse(text: &str) -> PpProfile {
        let mut p = PpProfile::default();
        let lines: Vec<&str> = text.lines().collect();
        let (mut effect, mut active) = (String::new(), false);
        let num = |s: &str| s.trim().parse::<f32>().ok();
        let comps = |s: &str| -> Vec<f32> { s.split(',').filter_map(|kv| kv.split(':').nth(1)).filter_map(|v| v.trim().trim_end_matches('}').trim().parse().ok()).collect() };
        for k in 0..lines.len() {
            let l = lines[k];
            if let Some(n) = l.strip_prefix("  m_Name: ") {
                effect = n.trim().to_string();
                active = false;
            } else if let Some(a) = l.strip_prefix("  active: ") {
                active = a.trim() == "1";
            } else if let (Some(key), true) = (l.strip_prefix("  ").and_then(|r| r.strip_suffix(':')), active) {
                if key.starts_with(' ') || k + 2 >= lines.len() || lines[k + 1].trim() != "overrideState: 1" {
                    continue;
                }
                let Some(v) = lines[k + 2].trim().strip_prefix("value:").map(str::trim) else { continue };
                let b = v == "1";
                match (effect.as_str(), key) {
                    ("Grain", "enabled") => p.grain_enabled = Some(b),
                    ("Grain", "intensity") => p.grain_intensity = num(v),
                    ("Grain", "size") => p.grain_size = num(v),
                    ("Grain", "lumContrib") => p.grain_lum = num(v),
                    ("Vignette", "intensity") => p.vig_intensity = num(v),
                    ("Vignette", "smoothness") => p.vig_smoothness = num(v),
                    ("Vignette", "roundness") => p.vig_roundness = num(v),
                    ("Vignette", "color") => p.vig_color = Some(comps(v)).filter(|c| c.len() >= 3).map(|c| Vec3::new(c[0], c[1], c[2])),
                    ("ChromaticAberration", "enabled") => p.ca_enabled = Some(b),
                    ("ChromaticAberration", "intensity") => p.ca_intensity = num(v),
                    ("LensDistortion", "intensity") => p.lens_intensity = num(v),
                    ("ColorGrading", "gamma") => p.cg_gamma = Some(comps(v)).filter(|c| c.len() >= 4).map(|c| Vec4::new(c[0], c[1], c[2], c[3])),
                    ("Bloom", "enabled") => p.bloom_enabled = Some(b),
                    ("Bloom", "intensity") => p.bloom_intensity = num(v),
                    ("Bloom", "threshold") => p.bloom_threshold = num(v),
                    ("Bloom", "softKnee") => p.bloom_soft_knee = num(v),
                    ("Bloom", "diffusion") => p.bloom_diffusion = num(v),
                    ("Bloom", "color") => p.bloom_color = Some(comps(v)).filter(|c| c.len() >= 3).map(|c| Vec3::new(c[0], c[1], c[2])),
                    ("DepthOfField", "enabled") => p.dof_enabled = Some(b),
                    ("DepthOfField", "focusDistance") => p.dof_focus = num(v),
                    ("DepthOfField", "aperture") => p.dof_aperture = num(v),
                    ("DepthOfField", "focalLength") => p.dof_focal_length = num(v),
                    ("DepthOfField", "kernelSize") => p.dof_kernel = num(v),
                    _ => {}
                }
            }
        }
        p
    }
}

/// Blended PPv2 settings of the main camera (Camera Profile + local volumes).
struct PpState {
    grain_enabled: bool,
    grain_intensity: f32,
    grain_size: f32,
    grain_lum: f32,
    vig_intensity: f32,
    vig_smoothness: f32,
    vig_roundness: f32,
    vig_color: Vec3,
    ca_enabled: bool,
    ca_intensity: f32,
    lens_intensity: f32,
    gamma: Vec4,
    bloom_enabled: bool,
    bloom_intensity: f32,
    bloom_threshold: f32,
    bloom_soft_knee: f32,
    bloom_diffusion: f32,
    bloom_color: Vec3,
    dof_enabled: bool,
    dof_focus: f32,
    dof_aperture: f32,
    dof_focal_length: f32,
    /// KernelSize (Small 0 .. VeryLarge 3)
    dof_kernel: f32,
}

impl PpState {
    /// `ParameterOverride.Interp`: floats / colours / vectors lerp, bools switch as soon as t > 0.
    fn apply(&mut self, p: &PpProfile, t: f32) {
        let lf = |v: &mut f32, to: Option<f32>| {
            if let Some(to) = to {
                *v += (to - *v) * t;
            }
        };
        if let Some(b) = p.grain_enabled.filter(|_| t > 0.0) {
            self.grain_enabled = b;
        }
        lf(&mut self.grain_intensity, p.grain_intensity);
        lf(&mut self.grain_size, p.grain_size);
        lf(&mut self.grain_lum, p.grain_lum);
        lf(&mut self.vig_intensity, p.vig_intensity);
        lf(&mut self.vig_smoothness, p.vig_smoothness);
        lf(&mut self.vig_roundness, p.vig_roundness);
        if let Some(c) = p.vig_color {
            self.vig_color = self.vig_color.lerp(c, t);
        }
        if let Some(b) = p.ca_enabled.filter(|_| t > 0.0) {
            self.ca_enabled = b;
        }
        lf(&mut self.ca_intensity, p.ca_intensity);
        lf(&mut self.lens_intensity, p.lens_intensity);
        if let Some(g) = p.cg_gamma {
            self.gamma = self.gamma.lerp(g, t);
        }
        if let Some(b) = p.bloom_enabled.filter(|_| t > 0.0) {
            self.bloom_enabled = b;
        }
        lf(&mut self.bloom_intensity, p.bloom_intensity);
        lf(&mut self.bloom_threshold, p.bloom_threshold);
        lf(&mut self.bloom_soft_knee, p.bloom_soft_knee);
        lf(&mut self.bloom_diffusion, p.bloom_diffusion);
        if let Some(c) = p.bloom_color {
            self.bloom_color = self.bloom_color.lerp(c, t);
        }
        if let Some(b) = p.dof_enabled.filter(|_| t > 0.0) {
            self.dof_enabled = b;
        }
        lf(&mut self.dof_focus, p.dof_focus);
        lf(&mut self.dof_aperture, p.dof_aperture);
        lf(&mut self.dof_focal_length, p.dof_focal_length);
        // Enum parameter: switches like a bool.
        if let Some(k) = p.dof_kernel.filter(|_| t > 0.0) {
            self.dof_kernel = k;
        }
    }
}

/// `ColorUtilities.ColorToInverseGamma`.
fn inverse_gamma(g: Vec4) -> Vec3 {
    let lum = g.x * 0.2126 + g.y * 0.7152 + g.z * 0.0722;
    let m = g.truncate() - Vec3::splat(lum) + Vec3::splat(g.w + 1.0);
    Vec3::ONE / m.max(Vec3::splat(1e-3))
}

impl FullscreenMaterial for PostFx {
    fn fragment_shader() -> ShaderRef {
        "embedded://restrafe/postfx.wgsl".into()
    }

    /// After tonemapping (None, as Unity's ColorGrading tonemapper; see ppv2.rs): the world camera is HDR (shared
    /// main texture) and the shader clamps its output (Unity resolves the world camera to the 8-bit
    /// back buffer before WeaponCam copies it).
    fn schedule_configs(
        system: bevy::ecs::schedule::ScheduleConfigs<bevy::ecs::system::BoxedSystem>,
    ) -> bevy::ecs::schedule::ScheduleConfigs<bevy::ecs::system::BoxedSystem> {
        system
            .in_set(bevy::core_pipeline::schedule::Core3dSystems::PostProcess)
            .after(bevy::core_pipeline::tonemapping::tonemapping)
    }
}

pub struct PostFxPlugin;

impl Plugin for PostFxPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "postfx.wgsl");
        app.add_plugins(FullscreenMaterialPlugin::<PostFx>::default())
            .add_message::<TakeHit>()
            .add_message::<TrigSuppression>()
            .add_systems(Update, (attach, update).chain());
    }
}

fn attach(mut commands: Commands, cams: Query<Entity, (With<PlayerCamera>, Without<PostFx>)>) {
    for e in &cams {
        commands.entity(e).insert(PostFx::default());
    }
}

fn update(
    time: Res<Time>,
    settings: Res<Settings>,
    mut hits: MessageReader<TakeHit>,
    mut sups: MessageReader<TrigSuppression>,
    fpc: Query<(&Fpc, Option<&crate::health::PlayerHealth>)>,
    mut commands: Commands,
    mut fx: Query<(Entity, &mut PostFx, &GlobalTransform, Option<&Projection>, Has<crate::ppv2::Ppv2Bloom>, Has<bevy::post_process::dof::DepthOfField>), With<PlayerCamera>>,
    volumes: Query<(&PpVolume, &GlobalTransform)>,
    mut profiles: Local<std::collections::HashMap<String, PpProfile>>,
    mut frame: Local<u32>,
    mut state: Local<(f32, f32)>,
    mut intensity_slot: Local<Option<f32>>,
) {
    let Ok((cam_e, mut fx, cam_tf, proj, has_bloom, has_dof)) = fx.single_mut() else { return };
    let dt = time.delta_secs();
    // `public static float Intensity = 1f` until the first TakeHit (TrigSup before any hit uses 1).
    let intensity = intensity_slot.get_or_insert(1.0);
    // OnRenderImage: decay then clamp to [0, start * Intensity].
    let (dmg, sup) = &mut *state;
    *dmg = (*dmg - dt * SPEED).clamp(0.0, START * *intensity);
    *sup = (*sup - dt * SUPP_SPEED).clamp(0.0, SUPP_START * *intensity);
    // TakeHit: Intensity = Settings.damageIntensity; _Damage = start * Intensity.
    if hits.read().count() > 0 {
        *intensity = settings.damage_intensity;
        *dmg = START * *intensity;
    }
    if sups.read().count() > 0 {
        *sup = SUPP_START * *intensity;
    }
    let (mut lens, mut sat) = fpc.single().map(|(f, _)| (f.lens_distortion, f.saturation)).unwrap_or((0.0, 0.0));
    // Dead: the world is seen through the scene Main Camera (KillCam), which has no CameraEffect / player volume.
    let dead = fpc.single().is_ok_and(|(_, h)| h.is_some_and(|h| h.dead));
    let mut d = *dmg;
    match std::env::var("RESTRAFE_FX_TEST").as_deref() {
        Ok("damage") => d = START * settings.damage_intensity,
        Ok("kill") => {
            lens = -55.0;
            sat = -100.0;
        }
        _ => {}
    }
    // Camera Profile (Vignette 0.43 / 0.11, Grain / ChromaticAberration off: class defaults) with the FPC-driven lens
    // intensity and gamma (Vector4(1, 1, 1, brightness - 1)), then the local volumes by ascending priority
    // (PostProcessManager.UpdateSettings: closest point of the trigger collider, t = 1 - d^2 / blend^2).
    let mut st = PpState {
        grain_enabled: false,
        grain_intensity: 0.0,
        grain_size: 1.0,
        grain_lum: 0.8,
        vig_intensity: 0.43,
        vig_smoothness: 0.11,
        vig_roundness: 1.0,
        vig_color: Vec3::ZERO,
        ca_enabled: false,
        ca_intensity: 0.0,
        lens_intensity: lens,
        gamma: Vec4::new(1.0, 1.0, 1.0, settings.brightness - 1.0),
        // Camera Profile Bloom is `active: 0` and it has no DepthOfField: PPv2 class defaults.
        bloom_enabled: false,
        bloom_intensity: 0.0,
        bloom_threshold: 1.0,
        bloom_soft_knee: 0.5,
        bloom_diffusion: 7.0,
        bloom_color: Vec3::ONE,
        dof_enabled: false,
        dof_focus: 10.0,
        dof_aperture: 5.6,
        dof_focal_length: 50.0,
        dof_kernel: 1.0,
    };
    if !dead {
        let cam = cam_tf.translation();
        let mut vols: Vec<(f32, f32, &str)> = volumes
            .iter()
            .filter_map(|(v, gt)| {
                let (s, _, c) = gt.to_scale_rotation_translation();
                let r = v.radius * s.abs().max_element();
                let d = ((cam - c).length() - r).max(0.0);
                let b2 = v.blend * v.blend;
                if d * d > b2 {
                    return None;
                }
                let t = if b2 > 0.0 { 1.0 - d * d / b2 } else { 1.0 };
                Some((v.priority, t * v.weight.clamp(0.0, 1.0), v.profile.as_str()))
            })
            .collect();
        vols.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, t, name) in vols {
            let p = profiles.entry(name.to_string()).or_insert_with(|| PpProfile::load(name));
            st.apply(p, t);
        }
    }
    fx.a = if dead { Vec4::ZERO } else { Vec4::new(d, *sup, st.lens_intensity, sat) };
    *frame = frame.wrapping_add(1);
    fx.b = inverse_gamma(st.gamma).extend(time.elapsed_secs() % 10.0);
    // Grain.cs: _Grain_Params1 = (lumContrib, intensity * 20), texture scale = screen / 192 / size.
    fx.c = Vec4::new(if st.grain_enabled { st.grain_intensity * 20.0 } else { 0.0 }, st.grain_lum, st.grain_size.max(0.3), (*frame % 1024) as f32);
    // Vignette.cs (classic): intensity * 3, smoothness * 5, roundness (1 - r) * 6 + r; ChromaticAberration amount
    // intensity * 0.05.
    fx.d = Vec4::new(
        st.vig_intensity * 3.0,
        st.vig_smoothness * 5.0,
        (1.0 - st.vig_roundness) * 6.0 + st.vig_roundness,
        if st.ca_enabled { st.ca_intensity * 0.05 } else { 0.0 },
    );
    fx.e = st.vig_color.extend(1.0);

    if std::env::var_os("RESTRAFE_DEBUG_PP2").is_some() {
        println!("PP2 a={:?} gamma={:?} vig={:.2} vigcol={:?} e={:?}", fx.a, st.gamma, st.vig_intensity, st.vig_color, fx.e);
    }
    if std::env::var_os("RESTRAFE_DEBUG_PP").is_some() && (st.grain_enabled || st.bloom_enabled || st.dof_enabled) {
        println!("PP grain={} {:.2} bloom={} {:.2} dof={} f={:.1} N={:.1} gamma={:?} vig={:.2}", st.grain_enabled, st.grain_intensity, st.bloom_enabled, st.bloom_intensity, st.dof_enabled, st.dof_focus, st.dof_aperture, st.gamma, st.vig_intensity);
    }
    // Bloom (Bloom.IsEnabledAndSupported: enabled && intensity > 0). The world camera's postfx clamps afterwards.
    let mut ec = commands.entity(cam_e);
    if st.bloom_enabled && st.bloom_intensity > 0.0 && std::env::var_os("RESTRAFE_NO_BLOOM").is_none() {
        ec.insert(crate::ppv2::Ppv2Bloom {
            intensity: st.bloom_intensity,
            threshold: st.bloom_threshold,
            soft_knee: st.bloom_soft_knee,
            clamp: 65472.0,
            diffusion: st.bloom_diffusion,
            color: st.bloom_color,
            clamp_output: false,
        });
    } else if has_bloom {
        ec.remove::<crate::ppv2::Ppv2Bloom>();
    }
    // DepthOfField.Render: CoC radius (fraction of screen height) = (d - s) / d * f^2 / (N (s - f) * 0.024 * 2),
    // capped at min(0.05, (kernelSize * 4 + 6) px / height). Bevy derives f from the FOV and sensor height:
    // coc diameter = f_b^2 / (H N_b) * |d - s| / (d (s - f_b)) * height, so N_b matches PPv2 with H = 0.024.
    let fov = match proj {
        Some(Projection::Perspective(p)) => Some(p.fov),
        _ => None,
    };
    match fov.filter(|_| st.dof_enabled) {
        Some(fov) => {
            const FILM_HEIGHT: f32 = 0.024;
            let f_u = st.dof_focal_length / 1000.0;
            let s = st.dof_focus.max(f_u);
            let f_b = bevy::post_process::dof::calculate_focal_length(FILM_HEIGHT, fov);
            let n_b = st.dof_aperture * (f_b * f_b / (s - f_b).max(1e-4)) / (f_u * f_u / (s - f_u).max(1e-4));
            let h = 1080.0f32;
            let max_radius_px = (st.dof_kernel * 4.0 + 6.0).min(0.05 * h);
            ec.insert(bevy::post_process::dof::DepthOfField {
                mode: bevy::post_process::dof::DepthOfFieldMode::Gaussian,
                focal_distance: s,
                sensor_height: FILM_HEIGHT,
                aperture_f_stops: n_b,
                max_circle_of_confusion_diameter: 2.0 * max_radius_px,
                max_depth: f32::INFINITY,
            });
        }
        None if has_dof => {
            ec.remove::<bevy::post_process::dof::DepthOfField>();
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    /// Profiles read from the installation equal the exported assets.
    #[test]
    fn pp_profiles_match_export() {
        let Some(owned) = crate::owned::Owned::open().ok() else { return };
        for e in std::fs::read_dir(crate::owned::reference_file("assets/data/pp")).into_iter().flatten().flatten() {
            let name = e.path().file_stem().unwrap().to_string_lossy().to_string();
            let a = super::PpProfile::parse(&std::fs::read_to_string(e.path()).unwrap());
            let b = super::PpProfile::parse(&owned.pp_profile_yaml(&name).expect(&name));
            assert_eq!(format!("{a:?}"), format!("{b:?}"), "{name}");
            println!("{name}: {a:?}");
        }
    }
}
