// Shader-specific terms of the STRAFTAT Amplify surface shaders that Bevy's StandardMaterial cannot express
// (see src/unity_pbr.rs and UnityAssets::surface_material). Project-authored; recovered from the compiled shaders.
//
// glTF (AssetRipper) uv has v pointing down: Unity uv = (u, 1 - v).
// Also the deferred fragment shader (PREPASS_PIPELINE, like Bevy's pbr.wgsl): the opaque lit kinds (0-4, 11) write
// the G-buffer from the same PbrInput; the special kinds are transparent / unlit and always forward.
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
    mesh_view_bindings::view,
    mesh_functions::{get_local_from_world, get_world_from_local},
    view_transformations::depth_ndc_to_view_z,
}
#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#import bevy_render::globals::Globals
// Prepass view layout: globals at binding 1 (forward: 11).
@group(0) @binding(1) var<uniform> globals: Globals;
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    mesh_view_bindings::globals,
}
#ifdef DEPTH_PREPASS
#import bevy_pbr::prepass_utils::prepass_depth
#endif
#endif

struct UnityExtParams {
    kind: u32,
    ao_power: f32,
    boost: f32,
    pad: f32,
    color: vec4<f32>,
    p0: vec4<f32>,
    p1: vec4<f32>,
    st: vec4<f32>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> ext: UnityExtParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var ext_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var ext_samp: sampler;

// Unity uv <-> glTF uv (the flip is its own inverse).
fn flip_v(uv: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(uv.x, 1.0 - uv.y);
}

// Unity pow (exp2(y * log2(x))): 0 for x = 0.
fn upow(x: f32, y: f32) -> f32 {
    return exp2(y * log2(x));
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool, @builtin(sample_index) sample_index: u32) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
    var uv = vec2<f32>(0.0);
#ifdef VERTEX_UVS_A
    uv = in.uv;
#endif
    var uv2 = uv;
#ifdef VERTEX_UVS_B
    uv2 = in.uv_b;
#endif
    let k = ext.kind;
    let t = globals.time; // Unity _Time.y
    let unity_uv = flip_v(uv);
    // One sample of the extension texture (uniform control flow), uv chosen per kind.
    var suv = uv;
    if k == 1u {
        suv = uv2; // _AO uses texcoord1, no _ST
    } else if k == 2u {
        suv = flip_v(unity_uv * ext.st.xy + ext.st.zw); // TRANSFORM_TEX(uv, _Emissive)
    } else if k == 4u {
        suv = flip_v(unity_uv * ext.p0.x); // _DirtTex at uv * _Float0
    } else if k == 11u {
        suv = flip_v(unity_uv * ext.st.xy + ext.st.zw); // _TextureSample0_ST
    }
    let s = textureSample(ext_tex, ext_samp, suv);

    var emission = vec3<f32>(0.0);
    if k == 1u {
        // S_WeaponOutline_00 _USEAO_ON: Albedo = BC (* Tint) * pow(AO.r, _AO_Power).
        let ao = upow(s.r, ext.ao_power);
        pbr_input.material.base_color = vec4<f32>(pbr_input.material.base_color.rgb * ao, pbr_input.material.base_color.a);
    } else if k == 2u {
        // S_WeaponOutline_01_Emissive: Emission = (0, _Emissive.g * _EmissiveBoost * sin(_timeglow * _Time.y)
        // * 0.5 * pow(uv.x, 6.88), 0) (can go negative and darken, as in Unity).
        let g = s.g * ext.boost * sin(ext.p0.x * t) * 0.5 * upow(unity_uv.x, 6.88);
        emission = vec3<f32>(0.0, g, 0.0);
    } else if k == 3u {
        // S_WeaponOutline_00_Special: L = frac(objY * _lines1_number * _wavelength + _Time.y * _time),
        // Emission = pow(saturate(1 - L (1 - L) _Float1), _specialPower) * _special * _emissiveBoost
        //            * pow(1 - dot(vertexNormal, viewDir), 5).
        let oy = (get_local_from_world(in.instance_index) * vec4<f32>(in.world_position.xyz, 1.0)).y;
        let l = fract(oy * ext.p0.x * ext.p0.y + t * ext.p0.z);
        let band = upow(clamp(1.0 - l * (1.0 - l) * ext.p0.w, 0.0, 1.0), ext.p1.x);
        let v = normalize(view.world_position.xyz - in.world_position.xyz);
        let f = 1.0 - dot(in.world_normal, v);
        let fres = f * f * f * f * f;
        emission = band * ext.color.rgb * ext.p1.y * fres;
    } else if k == 4u {
        // S_EmissiveColor: Emission = saturate(pow(Dirt, _Power) + _NoiseValue) * _Color0 * _EmissiveBoost * Albedo.
        let d = vec3<f32>(upow(s.r, ext.p0.y), upow(s.g, ext.p0.y), upow(s.b, ext.p0.y));
        emission = clamp(d + vec3<f32>(ext.p0.z), vec3<f32>(0.0), vec3<f32>(1.0)) * ext.color.rgb * ext.p0.w * pbr_input.material.base_color.rgb;
        if ext.p1.x > 0.5 {
            // S_FlickeringLight_00 _FLICKER_ON (DXBC blob 1650): * frac(sin(dot(sin(_Float1) * _Time.y, (12.9898, 78.233)))
            // * 43758.55) * 0.9 + 0.1.
            let h = sin(ext.p1.y) * t;
            emission = emission * (fract(sin(h * 12.9898 + h * 78.233) * 43758.55) * 0.9 + 0.1);
        }
    } else if k == 11u {
        // S_Emissive_Mask_00: Emission = lerp(_MaskedColor, _LightColor, pow(tex.r, _Power)) * _EmissiveBoost.
        emission = mix(ext.color.rgb, ext.p1.rgb, upow(s.r, ext.p0.x)) * ext.p0.y;
    }
#ifndef PREPASS_PIPELINE
    if k == 6u || k == 7u {
        var out: FragmentOutput;
        if k == 6u {
            // S_ProximityMine_00 (Blend One One, Cull Off): albedo 0, Emission = saturate(((1 - y) ceil(y)
            // - saturate((y + 12.54) _gradient)) * min(sin(_time _Time.y) + 1.27, 1) * _Color0), y = saturate(objectY).
            // (Its faint specular term is dropped.)
            let y = clamp((get_local_from_world(in.instance_index) * vec4<f32>(in.world_position.xyz, 1.0)).y, 0.0, 1.0);
            let c = min(sin(ext.p0.x * t) + 1.27, 1.0) * ext.color.rgb;
            let r = (1.0 - y) * ceil(y) - clamp((y + 12.54) * ext.p0.y, 0.0, 1.0);
            out.color = vec4<f32>(clamp(r * c, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
        } else {
            // S_ProximityMine_01 (alpha blend, Cull Off, ZWrite Off): rgb = _Color0, alpha = pow(max(sin(_time _Time.y), 0), 70)
            // * max(1 - |sceneEyeDepth - eyeDepth| / _DepthFade, 0) * _opacitymult.
            var fade = 0.0;
#ifdef DEPTH_PREPASS
            let scene_d = prepass_depth(in.position, sample_index);
            if scene_d > 0.0 {
                let scene_z = -depth_ndc_to_view_z(scene_d);
                let frag_z = -depth_ndc_to_view_z(in.position.z);
                fade = max(1.0 - abs((scene_z - frag_z) / ext.p0.y), 0.0);
            }
#endif
            let pulse = upow(max(sin(ext.p0.x * t), 0.0), 70.0);
            out.color = vec4<f32>(ext.color.rgb, pulse * fade * ext.p0.z);
        }
        out.color = main_pass_post_lighting_processing(pbr_input, out.color);
        return out;
    }
    if k == 8u {
        // S_HandGrenadeRadius_00 (alpha blend, Cull Off, ZWrite Off; black albedo, specular term dropped):
        // rgb = _Color0, alpha = saturate(_BaseOpacity * min(pow(max(sin(_time t), 0), _power), 1)
        //                      + max(1 - |sceneEyeDepth - eyeDepth| / _DepthFade, 0) * _opacitymult).
        var fade = 0.0;
#ifdef DEPTH_PREPASS
        let scene_d = prepass_depth(in.position, sample_index);
        if scene_d > 0.0 {
            let scene_z = -depth_ndc_to_view_z(scene_d);
            let frag_z = -depth_ndc_to_view_z(in.position.z);
            fade = max(1.0 - abs((scene_z - frag_z) / ext.p0.y), 0.0);
        }
#endif
        let pulse = min(upow(max(sin(ext.p0.x * t), 0.0), ext.p0.w), 1.0);
        var out: FragmentOutput;
        out.color = vec4<f32>(ext.color.rgb, clamp(ext.p1.x * pulse + fade * ext.p0.z, 0.0, 1.0));
        out.color = main_pass_post_lighting_processing(pbr_input, out.color);
        return out;
    }
    if k == 12u {
        // S_FogCard02 (recovered from DXBC; alpha blend, Cull Off, ZWrite Off): rgb = _Color0,
        // alpha = min(|sceneEyeDepth - eyeDepth| / _DepthFade, 1)
        //         [* saturate(pow(saturate(16 u(1-u) v(1-v)), _GradientPowr) * _GrtadientMult) with _USEGRADIENT_ON].
        var a = 1.0;
#ifdef DEPTH_PREPASS
        let scene_d = prepass_depth(in.position, sample_index);
        if scene_d > 0.0 {
            let scene_z = -depth_ndc_to_view_z(scene_d);
            let frag_z = -depth_ndc_to_view_z(in.position.z);
            a = min(abs((scene_z - frag_z) / ext.p0.x), 1.0);
        }
#endif
        if ext.p0.w > 0.5 {
            let g = clamp((1.0 - unity_uv.x) * unity_uv.x * 4.0 * (1.0 - unity_uv.y) * unity_uv.y * 4.0, 0.0, 1.0);
            a = a * clamp(upow(g, ext.p0.y) * ext.p0.z, 0.0, 1.0);
        }
        var out: FragmentOutput;
        out.color = vec4<f32>(ext.color.rgb, a);
        out.color = main_pass_post_lighting_processing(pbr_input, out.color);
        return out;
    }
    if k == 10u {
        // Shader Graphs/DF_Cyst_Liquid_Shader (Built-in Unlit, Cull Off, alpha-tested): keep fragments with
        //   worldY - objectY + _WobbleZ * worldX <= _Fill * 0.03 + 0.02   (Wobble.MaxWobble = 1e-6 -> wobble 0)
        // colour = front face ? _SideColour : _TopColour (the back faces seen through the cut read as the surface).
        let obj = get_world_from_local(in.instance_index)[3].xyz;
        let v = in.world_position.y - obj.y;
        if v > ext.p0.x * 0.03 + 0.02 {
            discard;
        }
        var out: FragmentOutput;
        out.color = vec4<f32>(select(ext.p1.rgb, ext.color.rgb, is_front), 1.0);
        out.color = main_pass_post_lighting_processing(pbr_input, out.color);
        return out;
    }
    if k == 9u {
        // S_BlackHole_00 (alpha blend, Cull Off, ZWrite Off): Standard with albedo 0 / smoothness 0 (in the
        // StandardMaterial), Emission = f^5 * _SmissiveBoost, Alpha = (1 - f^5) * _Float0, f = 1 - dot(N, V).
        let v = normalize(view.world_position.xyz - in.world_position.xyz);
        let f = clamp(1.0 - dot(normalize(in.world_normal), v), 0.0, 1.0);
        let f5 = f * f * f * f * f;
        pbr_input.material.base_color = vec4<f32>(pbr_input.material.base_color.rgb, 1.0);
        var out: FragmentOutput;
        let lit = apply_pbr_lighting(pbr_input);
        out.color = vec4<f32>(lit.rgb + vec3<f32>(f5 * ext.boost), (1.0 - f5) * ext.p0.x);
        out.color = main_pass_post_lighting_processing(pbr_input, out.color);
        return out;
    }
#endif
    pbr_input.material.emissive = vec4<f32>(pbr_input.material.emissive.rgb + emission, pbr_input.material.emissive.a);
#ifdef PREPASS_PIPELINE
    return deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
#endif
}
