// Port of Unity Post Processing Stack v2 AmbientOcclusion, mode ScalableAmbientObscurance (ScalableAO.cs +
// ScalableAO.hlsl): occlusion estimation, geometry-aware separable blur, composition (BlurSmall, multiply blend
// "Blend Zero OneMinusSrcColor, Zero OneMinusSrcAlpha"). Project-authored (no game data).
//
// Space convention follows Unity's shader: camera space with +z forward (positive linear depth), y up.
// Bevy: prepass depth is infinite reversed-Z (linear depth = near / d), prepass normal = world normal * 0.5 + 0.5.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct Ppv2SaoUniform {
    // _AOParams: x intensity, y radius, z downsample (0.5 or 1), w sample count
    ao_params: vec4<f32>,
    // _AOColor = white - settings.color (rgb)
    ao_color: vec4<f32>,
    // x p11, y p22, z near, w unused
    proj: vec4<f32>,
    // xy full-res screen size (_ScreenParams.xy), zw unused
    screen: vec4<f32>,
    // world -> view rotation (Bevy view space), columns
    view_from_world: mat4x4<f32>,
}

@group(0) @binding(0) var tex_a: texture_2d<f32>;
@group(0) @binding(1) var tex_b: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> u: Ppv2SaoUniform;

@group(1) @binding(0) var depth_tex: texture_depth_2d;

const K_CONTRAST: f32 = 0.6;
const K_GEOMETRY: f32 = 0.8;
const K_BETA: f32 = 0.002;
const EPSILON: f32 = 1.0e-4;
const TWO_PI: f32 = 6.28318530718;

fn s(t: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(t, samp, uv, 0.0);
}

// ---------------- estimation ----------------

// Linear eye depth + CheckBounds (outside the screen or at the far plane -> +1e8).
fn sample_depth(uv: vec2<f32>) -> f32 {
    let dims = vec2<f32>(textureDimensions(depth_tex));
    let p = vec2<i32>(clamp(uv * dims, vec2<f32>(0.0), dims - 1.0));
    let d = textureLoad(depth_tex, p, 0);
    var ob = 0.0;
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        ob += 1.0;
    }
    if (d <= 0.00001) {
        ob += 1.0;
        return 1.0e8 * ob;
    }
    return u.proj.z / d + ob * 1.0e8;
}

// View-space normal, Unity camera space (z forward).
fn sample_normal(uv: vec2<f32>) -> vec3<f32> {
    let n_world = normalize(s(tex_b, uv).xyz * 2.0 - 1.0);
    let n_view = (u.view_from_world * vec4<f32>(n_world, 0.0)).xyz;
    return vec3<f32>(n_view.x, n_view.y, -n_view.z);
}

// Bevy uv (y down) -> Unity-style NDC (y up).
fn uv_to_ndc(uv: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
}

fn reconstruct_view_pos(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    return vec3<f32>(uv_to_ndc(uv) / u.proj.xy * depth, depth);
}

fn uv_random(a: f32, b: f32) -> f32 {
    let f = dot(vec2<f32>(a, b), vec2<f32>(12.9898, 78.233));
    return fract(43758.5453 * sin(f));
}

fn gradient_noise(uv_in: vec2<f32>) -> f32 {
    let uv = floor(uv_in * u.screen.xy);
    let f = dot(vec2<f32>(0.06711056, 0.00583715), uv);
    return fract(52.9829189 * fract(f));
}

fn pick_sample_point(uv: vec2<f32>, index: f32) -> vec3<f32> {
    let gn = gradient_noise(uv * u.ao_params.z);
    let uu = fract(uv_random(0.0, index + uv.x * 1e-10) + gn) * 2.0 - 1.0;
    let theta = (uv_random(1.0, index + uv.x * 1e-10) + gn) * TWO_PI;
    let v = vec3<f32>(vec2<f32>(cos(theta), sin(theta)) * sqrt(1.0 - uu * uu), uu);
    let l = sqrt((index + 1.0) / u.ao_params.w) * u.ao_params.y;
    return v * l;
}

fn pack_ao_normal(ao: f32, n: vec3<f32>) -> vec4<f32> {
    return vec4<f32>(ao, n * 0.5 + 0.5);
}

@fragment
fn estimate(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let norm_o = sample_normal(uv);
    let depth_o = sample_depth(uv);
    let vpos_o = reconstruct_view_pos(uv, depth_o);

    var ao = 0.0;
    let count = i32(u.ao_params.w);
    for (var i = 0; i < count; i++) {
        let v_s1 = pick_sample_point(uv, f32(i));
        let vpos_s1 = vpos_o + v_s1;
        // Re-project (unity_CameraProjection 3x3, off-centre terms 0).
        let ndc = vec2<f32>(vpos_s1.x * u.proj.x, vpos_s1.y * u.proj.y) / vpos_s1.z;
        let uv_s1 = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        let depth_s1 = sample_depth(uv_s1);
        let vpos_s2 = reconstruct_view_pos(uv_s1, depth_s1);
        let v_s2 = vpos_s2 - vpos_o;
        let a1 = max(dot(v_s2, norm_o) - K_BETA * depth_o, 0.0);
        let a2 = dot(v_s2, v_s2) + EPSILON;
        ao += a1 / a2;
    }
    ao *= u.ao_params.y;
    ao = pow(max(ao * u.ao_params.x / max(u.ao_params.w, 1.0), 0.0), K_CONTRAST);
    // Non-finite (degenerate sample / normal) -> no occlusion: the multiply blend would otherwise blacken the pixel and
    // the blurs spread it.
    if !(ao == ao) || ao > 1.0e30 {
        ao = 0.0;
    }
    return pack_ao_normal(saturate(ao), norm_o);
}

// ---------------- blur ----------------

fn packed_ao(p: vec4<f32>) -> f32 {
    return p.r;
}

fn packed_normal(p: vec4<f32>) -> vec3<f32> {
    return p.gba * 2.0 - 1.0;
}

fn compare_normal(d1: vec3<f32>, d2: vec3<f32>) -> f32 {
    return smoothstep(K_GEOMETRY, 1.0, dot(d1, d2));
}

fn blur5(uv: vec2<f32>, delta: vec2<f32>) -> vec4<f32> {
    let p0 = s(tex_a, uv);
    let p1a = s(tex_a, uv - delta * 1.3846153846);
    let p1b = s(tex_a, uv + delta * 1.3846153846);
    let p2a = s(tex_a, uv - delta * 3.2307692308);
    let p2b = s(tex_a, uv + delta * 3.2307692308);
    let n0 = packed_normal(p0);
    let w0 = 0.2270270270;
    let w1a = compare_normal(n0, packed_normal(p1a)) * 0.3162162162;
    let w1b = compare_normal(n0, packed_normal(p1b)) * 0.3162162162;
    let w2a = compare_normal(n0, packed_normal(p2a)) * 0.0702702703;
    let w2b = compare_normal(n0, packed_normal(p2b)) * 0.0702702703;
    var acc = packed_ao(p0) * w0;
    acc += packed_ao(p1a) * w1a;
    acc += packed_ao(p1b) * w1b;
    acc += packed_ao(p2a) * w2a;
    acc += packed_ao(p2b) * w2b;
    acc /= w0 + w1a + w1b + w2a + w2b;
    return pack_ao_normal(acc, n0);
}

// Horizontal pass: always 2 texels of the (possibly downsampled) occlusion buffer, to match the dither pattern.
@fragment
fn blur_h(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let ts = 1.0 / vec2<f32>(textureDimensions(tex_a));
    return blur5(in.uv, vec2<f32>(ts.x * 2.0, 0.0));
}

// Vertical pass: source is full res; apply DOWNSAMPLE to match the dither pattern.
@fragment
fn blur_v(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let ts = 1.0 / vec2<f32>(textureDimensions(tex_a));
    return blur5(in.uv, vec2<f32>(0.0, ts.y / u.ao_params.z * 2.0));
}

// Composition: BlurSmall (geometry-aware 5-tap), EncodeAO (identity in linear colour space).
fn composite_ao(uv: vec2<f32>) -> vec4<f32> {
    let delta = 1.0 / vec2<f32>(textureDimensions(tex_a)) / u.ao_params.z;
    let p0 = s(tex_a, uv);
    let p1 = s(tex_a, uv + vec2<f32>(-delta.x, -delta.y));
    let p2 = s(tex_a, uv + vec2<f32>(delta.x, -delta.y));
    let p3 = s(tex_a, uv + vec2<f32>(-delta.x, delta.y));
    let p4 = s(tex_a, uv + vec2<f32>(delta.x, delta.y));
    let n0 = packed_normal(p0);
    let w0 = 1.0;
    let w1 = compare_normal(n0, packed_normal(p1));
    let w2 = compare_normal(n0, packed_normal(p2));
    let w3 = compare_normal(n0, packed_normal(p3));
    let w4 = compare_normal(n0, packed_normal(p4));
    var ao = packed_ao(p0) * w0 + packed_ao(p1) * w1 + packed_ao(p2) * w2 + packed_ao(p3) * w3 + packed_ao(p4) * w4;
    ao /= w0 + w1 + w2 + w3 + w4;
    if !(ao == ao) {
        ao = 0.0;
    }
    ao = clamp(ao, 0.0, 1.0);
    return vec4<f32>(ao * u.ao_color.rgb, ao);
}

@fragment
fn composite(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return composite_ao(in.uv);
}

// Debug view (RESTRAFE_SAO_VIEW=1): replaces the camera colour with 1 - occlusion.
@fragment
fn composite_debug(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let c = composite_ao(in.uv);
    return vec4<f32>(vec3<f32>(1.0 - c.a), 1.0);
}
