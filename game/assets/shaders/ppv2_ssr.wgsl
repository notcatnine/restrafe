// PPv2 ScreenSpaceReflections (Unity PostProcessing v2, ScreenSpaceReflections.hlsl) on Bevy's deferred G-buffer.
// Passes: test (McGuire/Mara screen-space march, square POT target), resolve, reproject (history), composite.
// View space is right-handed with -z forward in both engines. Depth: Bevy reverse-Z with infinite far plane.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput
#import bevy_pbr::pbr_deferred_types::{unpack_unorm4x8_, unpack_24bit_normal}
#import bevy_pbr::utils::octahedral_decode

struct Ssr {
    view_from_world: mat4x4<f32>,
    world_from_view: mat4x4<f32>,
    view_from_clip: mat4x4<f32>,
    clip_from_view: mat4x4<f32>,
    // x vignette intensity, y distance fade, z maximum march distance, w blur pyramid lod count
    params: vec4<f32>,
    // x aspect ratio, y noise tiling, z thickness (bandwidth), w maximum iteration count
    params2: vec4<f32>,
    // xy test target size, zw main target size
    sizes: vec4<f32>,
    // x near plane, y frame index
    misc: vec4<f32>,
}
@group(0) @binding(0) var<uniform> ssr: Ssr;
@group(0) @binding(1) var depth_tex: texture_depth_2d;
@group(0) @binding(2) var gbuffer: texture_2d<u32>;
@group(0) @binding(3) var src_a: texture_2d<f32>;
@group(0) @binding(4) var src_b: texture_2d<f32>;
@group(0) @binding(5) var lin: sampler;
@group(0) @binding(6) var motion: texture_2d<f32>;
@group(0) @binding(7) var env_cube: texture_cube<f32>;
@group(0) @binding(8) var noise_tex: texture_2d<f32>;

const MIN_ATTEN: f32 = 0.275;
const ATTEN_SCALE: f32 = 0.725; // 1 - SSR_MINIMUM_ATTENUATION
const ATTENUATION: f32 = 0.25;
const VIGNETTE_SMOOTHNESS: f32 = 5.0;

fn load_depth(uv: vec2<f32>) -> f32 {
    let p = vec2<i32>(clamp(uv, vec2(0.0), vec2(0.99999)) * ssr.sizes.zw);
    return textureLoad(depth_tex, p, 0);
}

// Unity GetViewSpacePosition: inverse projection of (uv, depth). uv has y down here (Bevy), NDC y up.
fn view_position(uv: vec2<f32>) -> vec3<f32> {
    let d = load_depth(uv);
    let ndc = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, d, 1.0);
    let v = ssr.view_from_clip * ndc;
    return v.xyz / v.w;
}

// Positive eye distance of a depth sample (Unity -LinearEyeDepth is the view z).
fn view_z(d: f32) -> f32 {
    let v = ssr.view_from_clip * vec4<f32>(0.0, 0.0, d, 1.0);
    return v.z / v.w;
}

fn gbuffer_normal(uv: vec2<f32>) -> vec3<f32> {
    let g = textureLoad(gbuffer, vec2<i32>(clamp(uv, vec2(0.0), vec2(0.99999)) * ssr.sizes.zw), 0);
    return octahedral_decode(unpack_24bit_normal(g.a));
}

// Screen-space projection of a view-space point into test-target pixels (Unity _ScreenSpaceProjectionMatrix rows:
// size/2 * (ndc + 1), y flipped to Bevy's top-down texture rows).
fn project(p: vec3<f32>) -> vec4<f32> {
    let c = ssr.clip_from_view * vec4<f32>(p, 1.0);
    let half = ssr.sizes.xy * 0.5;
    return vec4<f32>(half.x * (c.x + c.w), half.y * (c.w - c.y), c.z, c.w);
}

// NaN / Inf -> 0 on every pass output: one non-finite pixel (scene colour, a degenerate march) would otherwise spread
// through the Gaussian pyramid into every reflecting pixel and turn the whole lit scene black for that frame.
fn finite4(v: vec4<f32>) -> vec4<f32> {
    let bad = (v != v) | (abs(v) > vec4<f32>(65000.0));
    return select(v, vec4<f32>(0.0), bad);
}

fn hash_noise(p: vec2<f32>) -> f32 {
    // PPv2 samples blueNoise256[0].a; interleaved gradient noise stands in (frame-stable like the static texture).
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}

@fragment
fn test(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let uv0 = in.uv;
    let d = load_depth(uv0);
    if d <= 0.0 {
        return vec4<f32>(0.0);
    }
    let n_world = gbuffer_normal(uv0);
    let normal = (ssr.view_from_world * vec4<f32>(n_world, 0.0)).xyz;
    let origin = view_position(uv0);
    let max_dist = ssr.params.z;
    if origin.z < -max_dist {
        return vec4<f32>(0.0);
    }
    let dir = normalize(reflect(normalize(origin), normal));
    if dir.z > 0.0 {
        return vec4<f32>(0.0);
    }
    // March (Unity Result March(Ray)).
    var magnitude = max_dist;
    let end_z = origin.z + dir.z * max_dist;
    let near = ssr.misc.x;
    if end_z > -near {
        magnitude = (-near - origin.z) / dir.z;
    }
    var seg_start = origin;
    var seg_end = origin + dir * magnitude;
    let r = project(seg_start);
    let q = project(seg_end);
    let hom = vec2<f32>(1.0 / r.w, 1.0 / q.w);
    seg_start *= hom.x;
    seg_end *= hom.y;
    var ep = vec4<f32>(r.xy * hom.x, q.xy * hom.y);
    let dd = ep.xy - ep.zw;
    // endPoints.zw += step(dist^2, 0.0001) * max(_Test_TexelSize.x, _Test_TexelSize.y)
    if dot(dd, dd) <= 0.0001 {
        ep = vec4<f32>(ep.xy, ep.zw + vec2<f32>(1.0 / ssr.sizes.x));
    }
    var disp = ep.zw - ep.xy;
    var permuted = false;
    if abs(disp.x) < abs(disp.y) {
        permuted = true;
        disp = disp.yx;
        ep = ep.yxwz;
    }
    let sgn = sign(disp.x);
    let normalizer = sgn / disp.x;
    var seg_dir = (seg_end - seg_start) * normalizer;
    var deriv = vec4<f32>(sgn, disp.y * normalizer, (hom.y - hom.x) * normalizer, seg_dir.z);
    var stride = 1.0 - min(1.0, -origin.z * 0.01);
    var nuv = uv0 * ssr.params2.y;
    nuv.y *= ssr.params2.x;
    // _Noise.SampleLevel(uv + _WorldSpaceCameraPos.xz).a: blueNoise256[0], point / repeat (Unity z = -port z).
    let cam = ssr.world_from_view[3].xyz;
    let nd = vec2<f32>(textureDimensions(noise_tex));
    let jitter = textureLoad(noise_tex, vec2<i32>(fract(nuv + vec2<f32>(cam.x, -cam.z)) * nd), 0).a;
    stride *= ssr.params2.z;
    deriv *= stride;
    seg_dir *= stride;
    var z = vec2<f32>(0.0);
    var tracker = vec4<f32>(ep.xy, hom.x, seg_start.z) + deriv * jitter;
    // PPv2 checks result.uv, which is only set on a hit, so the march never stops at the screen edge: off-screen
    // steps read the clamped depth texture and a hit there is faded out by Attenuate / Vignette in the resolve.
    let iters = i32(ssr.params2.w);
    for (var i = 0; i < iters; i++) {
        tracker += deriv;
        z.x = z.y;
        z.y = (tracker.w + deriv.w * 0.5) / (tracker.z + deriv.z * 0.5);
        if z.y > z.x {
            let k = z.x;
            z.x = z.y;
            z.y = k;
        }
        var uv = tracker.xy;
        if permuted {
            uv = uv.yx;
        }
        uv = uv / ssr.sizes.xy;
        let sd = load_depth(uv);
        let depth = view_z(sd);
        if z.y < depth {
            let confidence = f32(i + 1) / ssr.params2.w;
            return finite4(vec4<f32>(uv, confidence, 1.0));
        }
    }
    return vec4<f32>(0.0);
}

fn attenuate(uv: vec2<f32>) -> f32 {
    let offset = min(1.0 - max(uv.x, uv.y), min(uv.x, uv.y));
    let r = clamp(offset / (ATTEN_SCALE * ATTENUATION + MIN_ATTEN), 0.0, 1.0);
    return sqrt(r);
}

fn vignette(uv: vec2<f32>) -> f32 {
    var k = abs(uv - vec2(0.5)) * ssr.params.x;
    k.x *= ssr.sizes.z / ssr.sizes.w; // _MainTex_TexelSize.y * .z = width / height
    return pow(clamp(1.0 - dot(k, k), 0.0, 1.0), VIGNETTE_SMOOTHNESS);
}

// src_a = test target, src_b = scene colour (opaque, lit).
@fragment
fn resolve(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let t = textureLoad(src_a, vec2<i32>(in.position.xy), 0);
    if t.w == 0.0 {
        // Unity's opaque colour buffer has alpha 1, which the composite's fade relies on to drop misses; Bevy's does not.
        return finite4(vec4<f32>(textureSampleLevel(src_b, lin, in.uv, 0.0).rgb, 1.0));
    }
    var c = textureSampleLevel(src_b, lin, t.xy, 0.0);
    let confidence = t.w * attenuate(t.xy) * vignette(t.xy);
    c = vec4<f32>(c.rgb * confidence, t.z);
    return finite4(c);
}

// src_a = resolve, src_b = history.
@fragment
fn reproject(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let mv = textureSampleLevel(motion, lin, in.uv, 0.0).xy;
    let uv = in.uv - mv;
    let k = 1.0 / ssr.sizes.xy;
    var color = textureSampleLevel(src_a, lin, in.uv, 0.0);
    var mn = color;
    var mx = color;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            if x == 0 && y == 0 {
                continue;
            }
            let s = textureSampleLevel(src_a, lin, in.uv + vec2<f32>(f32(x), f32(y)) * k, 0.0);
            mn = min(mn, s);
            mx = max(mx, s);
        }
    }
    var history = textureSampleLevel(src_b, lin, uv, 0.0);
    history = clamp(history, mn, mx);
    color.a = clamp(smoothstep(0.002 * ssr.sizes.x, 0.0035 * ssr.sizes.x, length(mv)), 0.0, 1.0);
    let weight = clamp(mix(0.95, 0.7, history.a * 100.0), 0.7, 0.95);
    color.a *= 0.85;
    return finite4(mix(finite4(color), finite4(history), weight));
}

// Composite (PPv2 FragComposite, written to the other post-process texture): the probe reflection is estimated from
// the camera's environment map (Bevy's lit colour has no separate reflections buffer). src_a = resolve pyramid, src_b = scene.
@fragment
fn composite(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    // src_b = the scene colour; the output replaces it.
    let scene = textureLoad(src_b, vec2<i32>(in.position.xy), 0);
    let d = load_depth(in.uv);
    if d <= 0.0 {
        return scene;
    }
    let g = textureLoad(gbuffer, vec2<i32>(in.position.xy), 0);
    let base_rough = unpack_unorm4x8_(g.r);
    let props = unpack_unorm4x8_(g.b);
    let base = pow(base_rough.rgb, vec3(2.2));
    let perceptual_roughness = base_rough.a;
    let metallic = props.g;
    let reflectance = props.r;
    let occlusion = props.b;
    let n = octahedral_decode(unpack_24bit_normal(g.a));
    // Unity Standard: specColor = lerp(dielectricSpec 0.04, albedo, metallic); oneMinusReflectivity = 0.96 (1 - metallic).
    let f0 = mix(vec3(0.16 * reflectance * reflectance), base, metallic);
    // EnergyConservationBetweenDiffuseAndSpecular: oneMinusReflectivity = 1 - SpecularStrength(specColor) (max of rgb).
    let one_minus_refl = 1.0 - max(f0.r, max(f0.g, f0.b));
    let smoothness = 1.0 - perceptual_roughness;
    let pv = view_position(in.uv);
    let eye_v = normalize(pv);
    let eye = (ssr.world_from_view * vec4<f32>(eye_v, 0.0)).xyz;
    let lod = perceptual_roughness * (ssr.params.w - 1.0) + 1.0; // SmoothnessToRoughness = perceptual roughness
    let res = textureSampleLevel(src_a, lin, in.uv, lod);
    let confidence = clamp(2.0 * dot(-eye, normalize(reflect(-eye, n))), 0.0, 1.0);
    // BRDF1 indirect specular (linear colour space): surfaceReduction / (r^2 + 1), FresnelLerp(spec, grazing, nv).
    let roughness = perceptual_roughness * perceptual_roughness;
    let surface_reduction = 1.0 / (roughness * roughness + 1.0);
    let grazing = clamp(smoothness + (1.0 - one_minus_refl), 0.0, 1.0);
    let nv = abs(dot(n, -eye));
    let fres = mix(f0, vec3(grazing), pow(1.0 - nv, 5.0));
    let brdf = surface_reduction * res.rgb * fres;
    // _CameraReflectionsTexture: the reflection-probe specular already in the lit colour (Unity deferred reflections:
    // same BRDF with the probe sample at mip perceptualRoughness * (1.7 - 0.7 r) * UNITY_SPECCUBE_LOD_STEPS (6)).
    // Internal-DeferredReflections: UNITY_BRDF_PBS(0, specColor, ..., indirect.specular = UnityGI_IndirectSpecular(...)
    // = probe * occlusion).
    var probes = vec3<f32>(0.0);
    if ssr.misc.z > 0.5 {
        let r_dir = reflect(eye, n);
        let mip = min(perceptual_roughness * (1.7 - 0.7 * perceptual_roughness) * 6.0, ssr.misc.y - 1.0);
        probes = surface_reduction * textureSampleLevel(env_cube, lin, r_dir, mip).rgb * occlusion * fres;
    }
    var a = res.a * 2.0 * res.a;
    let fade = 1.0 - clamp(1.5 * a * smoothstep(0.5, 1.0, 1.5 * a) * ssr.params.y, 0.0, 1.0);
    // FragComposite: color = max(0, color - probes); color += lerp(probes, BRDF(resolve), confidence * fade) * gbuffer0.a.
    let w = confidence * fade;
    let refl = finite4(vec4<f32>(mix(probes, brdf, w) * occlusion, 0.0)).rgb;
    return vec4<f32>(max(vec3<f32>(0.0), scene.rgb - probes) + refl, scene.a);
}

// PPv2 GaussianDownsample.compute (MiniEngine BlurCS): D(p) = bilinear sample of the source at destination texel
// centre (p + 0.5) / dstSize (2x2 average), then a separable 9-tap binomial blur over destination texels
// (0.00390625, 0.03125, 0.109375, 0.21875, 0.2734375, ...). src_a: single-mip view of the previous level.
const BLUR_W = array<f32, 9>(0.00390625, 0.03125, 0.109375, 0.21875, 0.2734375, 0.21875, 0.109375, 0.03125, 0.00390625);

@fragment
fn downsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let dst = vec2<f32>(textureDimensions(src_a)) * 0.5;
    let p = floor(in.position.xy);
    var c = vec4<f32>(0.0);
    for (var j = 0; j < 9; j++) {
        for (var i = 0; i < 9; i++) {
            let q = p + vec2<f32>(f32(i - 4), f32(j - 4));
            c += BLUR_W[i] * BLUR_W[j] * textureSampleLevel(src_a, lin, (q + 0.5) / dst, 0.0);
        }
    }
    return finite4(c);
}
