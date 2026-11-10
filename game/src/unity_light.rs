//! Unity built-in point/spot light attenuation for lights that stand for Unity `Light`s spawned at runtime
//! (particle Lights module + prefab lights of VFX, see `vfx/`).
//!
//! Unity's built-in pipeline does not use inverse-square falloff: a point light of intensity `I` and range `r` lights a
//! surface at distance `d` with `I * atten(d / r)`, where `atten` is the `_LightTextureB0` lookup
//! `1 / (1 + 25 (d/r)^2)`, faded to 0 over the last 20 % of the range (`LightAttenuateNormalized`). It is bounded at
//! `I` when `d -> 0`. Bevy's point lights fall off with `1 / d^2`; with a constant lumen conversion (the port used
//! `I * 80 000 lm`, equal to Unity at ~1.1 m) a muzzle-flash light 0.2-0.5 m from the gun / hands gives 5-30x Unity's
//! light there (the white glow at the bottom of the screen after firing) and far surfaces get far less.
//!
//! Bevy has no per-light falloff option, so this patches the `point_light()` attenuation of `bevy_pbr`'s
//! `pbr_lighting.wgsl` (also used by spot lights) for lights tagged with [`UNITY_LIGHT_RADIUS`] as their `radius`;
//! every other light keeps Bevy's physical falloff. Intensity of a tagged light: [`unity_point_lumens`].

use bevy::prelude::*;
use bevy::shader::Shader;

/// `PointLight::radius` marking a light that uses Unity's attenuation (tiny: no visible area-light effect).
pub const UNITY_LIGHT_RADIUS: f32 = 1.0e-5;
/// The same for a Light with `m_RenderMode: 1` (Important: always a per-pixel light in Unity's forward path, read by
/// the distortion shader's ForwardAdd term in particle.wgsl).
pub const UNITY_LIGHT_RADIUS_IMPORTANT: f32 = 2.0e-5;

/// Tag radius for a Unity light of render mode `mode` (0 Auto, 1 Important, 2 Not Important).
pub fn unity_light_radius(mode: i64) -> f32 {
    if mode == 1 { UNITY_LIGHT_RADIUS_IMPORTANT } else { UNITY_LIGHT_RADIUS }
}

/// Lumens for a Unity light intensity `i` so that, with the Unity falloff above, a Lambert surface receives
/// `albedo * colour * i * atten` like Unity's BRDF (no 1/pi): Bevy divides lumens by 4 pi (candela), multiplies by the
/// view exposure and the Lambert 1/pi.
pub fn unity_point_lumens(i: f32) -> f32 {
    static K: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    let k = *K.get_or_init(|| {
        let ev100 = std::env::var("RESTRAFE_EV100").ok().and_then(|s| s.parse().ok()).unwrap_or(crate::ppv2::EV100);
        4.0 * std::f32::consts::PI * std::f32::consts::PI * 1.2 * 2f32.powf(ev100)
    });
    i * k
}

const TARGET: &str = "let rangeAttenuation = getDistanceAttenuation(distance_square, (*light).color_inverse_square_range.w);";
const REPLACEMENT: &str = "let rangeAttenuation = restrafe_light_attenuation(distance_square, (*light).color_inverse_square_range.w, (*light).position_radius.w);";
const FUNCTIONS: &str = r#"
// Restrafe (unity_light.rs): Unity built-in light attenuation for tagged lights.
fn restrafe_light_attenuation(distanceSquare: f32, inverseRangeSquared: f32, radius: f32) -> f32 {
    if (abs(radius - 1.0e-5) > 1.0e-7 && abs(radius - 2.0e-5) > 1.0e-7) {
        return getDistanceAttenuation(distanceSquare, inverseRangeSquared);
    }
    let x = distanceSquare * inverseRangeSquared;
    var atten = 1.0 / (1.0 + 25.0 * x);
    // LightAttenuateNormalized: fade the last 20 % of the range to zero.
    let cutoff = 0.8 * 0.8;
    if (x >= cutoff) {
        atten *= saturate(1.0 - (x - cutoff) / (1.0 - cutoff));
    }
    // Bevy multiplies by NdotL and the Lambert 1/pi; the lumen conversion (unity_point_lumens) folds pi and 4 pi.
    return atten;
}
"#;

pub struct UnityLightPlugin;

impl Plugin for UnityLightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (patch_pbr_lighting, patch_irradiance_fallback, patch_deferred_lightmap.after(patch_irradiance_fallback)));
    }
}

const IV_TARGET: &str = "        indirect_light += irradiance_volume_light * diffuse_color * diffuse_occlusion;
        found_diffuse_indirect = true;";
const IV_REPLACEMENT: &str = "        indirect_light += irradiance_volume_light * diffuse_color * diffuse_occlusion;
        // Restrafe (unity_light.rs): only a containing volume counts, else the environment map diffuse applies.
        found_diffuse_indirect = any(irradiance_volume_light != vec3(0.0));";

/// `bevy_pbr`'s `pbr_functions.wgsl` stops looking for diffuse indirect light once a view has an irradiance volume,
/// even for fragments outside every volume: those got no diffuse light at all (black dynamic objects and viewmodel
/// wherever the map's light-probe volume does not reach). Patched so such fragments fall back to the camera's
/// environment-map diffuse (the light-probe SH at the player, `scene_map::map_lighting`).
fn patch_irradiance_fallback(mut done: Local<bool>, mut shaders: ResMut<Assets<Shader>>) {
    // Debug: RESTRAFE_NO_IV_PATCH=1 keeps Bevy's behaviour.
    if *done || std::env::var_os("RESTRAFE_NO_IV_PATCH").is_some() {
        return;
    }
    let Some(id) = shaders.iter().find(|(_, s)| s.path.ends_with("pbr_functions.wgsl")).map(|(id, _)| id) else { return };
    let Some(shader) = shaders.get(id) else { return };
    *done = true;
    let bevy::shader::Source::Wgsl(src) = &shader.source else { return };
    // Shader sources can have CRLF line endings (git autocrlf checkouts): match on LF text.
    let src = src.replace("\r\n", "\n");
    if !src.contains(IV_TARGET) {
        warn!("unity_light: pbr_functions.wgsl irradiance volume block not found; no environment-map fallback");
        return;
    }
    let new = Shader::from_wgsl(src.replace(IV_TARGET, IV_REPLACEMENT), shader.path.clone());
    let _ = shaders.insert(id, new);
    info!("unity_light: patched pbr_functions.wgsl (environment-map diffuse outside irradiance volumes)");
}

fn patch_pbr_lighting(mut done: Local<bool>, mut shaders: ResMut<Assets<Shader>>) {
    if *done {
        return;
    }
    let mut found = None;
    for (id, s) in shaders.iter() {
        if s.path.ends_with("pbr_lighting.wgsl") {
            found = Some(id);
            break;
        }
    }
    let Some(id) = found else { return };
    let Some(shader) = shaders.get(id) else { return };
    let bevy::shader::Source::Wgsl(src) = &shader.source else {
        *done = true;
        return;
    };
    // Shader sources can have CRLF line endings (git autocrlf checkouts): match on LF text.
    let src = src.replace("\r\n", "\n");
    if !src.contains(TARGET) {
        warn!("unity_light: pbr_lighting.wgsl point_light attenuation not found; Unity light falloff not applied");
        *done = true;
        return;
    }
    // Insert the helper right after getDistanceAttenuation (it calls it) and swap the call in point_light().
    let anchor = "// Falloff without the distance attenuation";
    let mut patched = src.replace(TARGET, REPLACEMENT);
    patched = match patched.find(anchor) {
        Some(i) => format!("{}{}{}", &patched[..i], FUNCTIONS, &patched[i..]),
        None => format!("{patched}\n{FUNCTIONS}"),
    };
    let path_for_log = shader.path.clone();
    let new = Shader::from_wgsl(patched, shader.path.clone());
    let _ = shaders.insert(id, new);
    info!("unity_light: patched {} (Unity point-light attenuation for tagged lights)", path_for_log);
    *done = true;
}

/// Bevy's camera exposure factor for the port's EV100 (`1 / (1.2 * 2^EV100)`, as in `bevy_camera::Exposure`).
pub fn view_exposure() -> f32 {
    let ev100 = std::env::var("RESTRAFE_EV100").ok().and_then(|s| s.parse().ok()).unwrap_or(crate::ppv2::EV100);
    1.0 / (1.2 * 2f32.powf(ev100))
}

/// Deferred shading loses the per-pipeline `LIGHTMAP` shader def, so lights / ambient / probes with
/// `affects_lightmapped_mesh_diffuse: false` would light lightmapped surfaces in the G-buffer path. A runtime mesh flag
/// (bit 26, unused by Bevy) marks lightmapped fragments: set where the lightmap is sampled (pbr_fragment), carried in
/// the G-buffer's flag byte (bit 3), and tested in pbr_functions instead of `#ifdef LIGHTMAP`.
const LM_BIT: &str = "(1u << 26u)";

fn patch_deferred_lightmap(mut done: Local<u8>, mut shaders: ResMut<Assets<Shader>>) {
    if *done == 0b1111 || std::env::var_os("RESTRAFE_FORWARD").is_some() {
        return;
    }
    let edits: [(&str, u8, Vec<(String, String)>); 4] = [
        ("pbr_fragment.wgsl", 1, vec![(
            "        pbr_input.lightmap_light = lightmap(in.uv_b, lightmap_exposure, in.instance_index);\n".into(),
            format!("        pbr_input.lightmap_light = lightmap(in.uv_b, lightmap_exposure, in.instance_index);\n        pbr_input.flags |= {LM_BIT};\n"),
        )]),
        ("pbr_deferred_types.wgsl", 2, vec![
            (
                "const DEFERRED_MESH_FLAGS_SHADOW_RECEIVER_BIT: u32  = 1u << 2u;".into(),
                "const DEFERRED_MESH_FLAGS_SHADOW_RECEIVER_BIT: u32  = 1u << 2u;\nconst DEFERRED_MESH_FLAGS_PORT_LIGHTMAPPED_BIT: u32 = 1u << 3u;".into(),
            ),
            (
                "    flags |= u32((mesh_flags & MESH_FLAGS_SHADOW_RECEIVER_BIT) != 0u) * DEFERRED_MESH_FLAGS_SHADOW_RECEIVER_BIT;".into(),
                format!("    flags |= u32((mesh_flags & MESH_FLAGS_SHADOW_RECEIVER_BIT) != 0u) * DEFERRED_MESH_FLAGS_SHADOW_RECEIVER_BIT;\n    flags |= u32((mesh_flags & {LM_BIT}) != 0u) * DEFERRED_MESH_FLAGS_PORT_LIGHTMAPPED_BIT;"),
            ),
            (
                "    mesh_flags |= u32((deferred_flags & DEFERRED_MESH_FLAGS_SHADOW_RECEIVER_BIT) != 0u) * MESH_FLAGS_SHADOW_RECEIVER_BIT;".into(),
                format!("    mesh_flags |= u32((deferred_flags & DEFERRED_MESH_FLAGS_SHADOW_RECEIVER_BIT) != 0u) * MESH_FLAGS_SHADOW_RECEIVER_BIT;\n    mesh_flags |= u32((deferred_flags & DEFERRED_MESH_FLAGS_PORT_LIGHTMAPPED_BIT) != 0u) * {LM_BIT};"),
            ),
        ]),
        ("pbr_functions.wgsl", 4, vec![
            (
                "#ifdef LIGHTMAP\n        let enable_diffuse =\n            (view_bindings::clustered_lights.data[light_id].flags &\n                mesh_view_types::POINT_LIGHT_FLAGS_AFFECTS_LIGHTMAPPED_MESH_DIFFUSE_BIT) != 0u;\n#else   // LIGHTMAP\n        let enable_diffuse = true;\n#endif  // LIGHTMAP\n".into(),
                format!("        let enable_diffuse = (in.flags & {LM_BIT}) == 0u ||\n            (view_bindings::clustered_lights.data[light_id].flags &\n                mesh_view_types::POINT_LIGHT_FLAGS_AFFECTS_LIGHTMAPPED_MESH_DIFFUSE_BIT) != 0u;\n"),
            ),
            (
                "#ifdef LIGHTMAP\n        let enable_diffuse =\n            ((*light).flags &\n                mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_AFFECTS_LIGHTMAPPED_MESH_DIFFUSE_BIT) !=\n                0u;\n#else   // LIGHTMAP\n        let enable_diffuse = true;\n#endif  // LIGHTMAP\n".into(),
                format!("        let enable_diffuse = (in.flags & {LM_BIT}) == 0u ||\n            ((*light).flags &\n                mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_AFFECTS_LIGHTMAPPED_MESH_DIFFUSE_BIT) != 0u;\n"),
            ),
            (
                "#ifdef LIGHTMAP\n    indirect_light += in.lightmap_light * diffuse_color;\n    found_diffuse_indirect = true;\n#endif\n".into(),
                format!("#ifdef LIGHTMAP\n    indirect_light += in.lightmap_light * diffuse_color;\n    found_diffuse_indirect = true;\n#endif\n    // Deferred: the lightmap already sits in the G-buffer emissive.\n    if (in.flags & {LM_BIT}) != 0u {{\n        found_diffuse_indirect = true;\n    }}\n"),
            ),
            (
                "#ifdef LIGHTMAP\n    let enable_ambient = view_bindings::lights.ambient_light_affects_lightmapped_meshes != 0u;\n#else   // LIGHTMAP\n    let enable_ambient = true;\n#endif  // LIGHTMAP\n".into(),
                format!("    let enable_ambient = (in.flags & {LM_BIT}) == 0u || view_bindings::lights.ambient_light_affects_lightmapped_meshes != 0u;\n"),
            ),
        ]),
        ("pbr_deferred_functions.wgsl", 8, vec![]),
    ];
    for (name, bit, reps) in edits.iter() {
        if *done & bit != 0 {
            continue;
        }
        if *name == "pbr_deferred_functions.wgsl" {
            *done |= bit; // nothing to change (kept for the bit mask)
            continue;
        }
        let Some(id) = shaders.iter().find(|(_, s)| s.path.ends_with(name)).map(|(id, _)| id) else { continue };
        let Some(shader) = shaders.get(id) else { continue };
        let bevy::shader::Source::Wgsl(src) = &shader.source else { continue };
        let src = src.replace("\r\n", "\n");
        // pbr_functions: wait for patch_irradiance_fallback (it replaces the same asset).
        if *name == "pbr_functions.wgsl" && std::env::var_os("RESTRAFE_NO_IV_PATCH").is_none() && !src.contains("only a containing volume counts") {
            continue;
        }
        let mut s = src.to_string();
        let mut ok = true;
        for (a, b) in reps {
            if s.matches(a.as_str()).count() == 0 {
                warn!("unity_light: deferred lightmap patch: block not found in {name}");
                ok = false;
                continue;
            }
            s = s.replace(a.as_str(), b.as_str());
        }
        let new = Shader::from_wgsl(s, shader.path.clone());
        let _ = shaders.insert(id, new);
        *done |= bit;
        info!("unity_light: patched {name} for deferred lightmapped surfaces (ok={ok})");
    }
}
