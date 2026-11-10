// Port of Unity Post Processing Stack v2 Bloom (Bloom.shader, Sampling.hlsl DownsampleBox13Tap / UpsampleTent,
// Colors.hlsl QuadraticThreshold) and the bloom block of Uber.shader. Project-authored (no game data).
//
// Entry points (one pipeline each):
//   copy       : src -> target (the frame before the camera draws)
//   prefilter  : FragPrefilter13   src = camera colour (full res), aux = copy -> pyramid down[0] (half res); only the
//                pixels this camera changed count (its own content)
//   downsample : FragDownsample13  src = down[i-1]                  -> down[i]
//   upsample   : FragUpsampleTent  src = last up (lower mip), aux = down[i] -> up[i]
//   uber       : Uber.shader BLOOM src = camera colour, aux = last up -> camera colour (other ping-pong texture)
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct Ppv2BloomUniform {
    // _Threshold: x threshold (linear), y threshold - knee, z knee * 2, w 0.25 / knee
    threshold: vec4<f32>,
    // x _Params.x (clamp, linear), y _SampleScale, z bloom intensity (Exp2(i/10)-1), w 1 = saturate the output
    params: vec4<f32>,
    // _Bloom_Color (linear)
    color: vec4<f32>,
}

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var aux_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> u: Ppv2BloomUniform;

const HALF_MAX: f32 = 65504.0;
const EPSILON: f32 = 1.0e-4;

fn texel(t: texture_2d<f32>) -> vec2<f32> {
    return 1.0 / vec2<f32>(textureDimensions(t));
}

fn s(t: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(t, samp, uv, 0.0);
}

// StdLib.hlsl SafeHDR + PostProcessLayer.stopNaNPropagation (NaN/Inf -> 0).
fn safe_hdr(c: vec4<f32>) -> vec4<f32> {
    var o = c;
    if (any(c != c) || any(abs(c) > vec4<f32>(3.0e38))) {
        o = vec4<f32>(0.0);
    }
    return min(o, vec4<f32>(HALF_MAX));
}

// Sampling.hlsl DownsampleBox13Tap ([Jimenez14]).
fn downsample_box13(t: texture_2d<f32>, uv: vec2<f32>, ts: vec2<f32>) -> vec4<f32> {
    let A = s(t, uv + ts * vec2<f32>(-1.0, -1.0));
    let B = s(t, uv + ts * vec2<f32>(0.0, -1.0));
    let C = s(t, uv + ts * vec2<f32>(1.0, -1.0));
    let D = s(t, uv + ts * vec2<f32>(-0.5, -0.5));
    let E = s(t, uv + ts * vec2<f32>(0.5, -0.5));
    let F = s(t, uv + ts * vec2<f32>(-1.0, 0.0));
    let G = s(t, uv);
    let H = s(t, uv + ts * vec2<f32>(1.0, 0.0));
    let I = s(t, uv + ts * vec2<f32>(-0.5, 0.5));
    let J = s(t, uv + ts * vec2<f32>(0.5, 0.5));
    let K = s(t, uv + ts * vec2<f32>(-1.0, 1.0));
    let L = s(t, uv + ts * vec2<f32>(0.0, 1.0));
    let M = s(t, uv + ts * vec2<f32>(1.0, 1.0));
    let div = (1.0 / 4.0) * vec2<f32>(0.5, 0.125);
    var o = (D + E + I + J) * div.x;
    o += (A + B + G + F) * div.y;
    o += (B + C + H + G) * div.y;
    o += (F + G + L + K) * div.y;
    o += (G + H + M + L) * div.y;
    return o;
}

// Sampling.hlsl UpsampleTent (9-tap bilinear tent).
fn upsample_tent(t: texture_2d<f32>, uv: vec2<f32>, ts: vec2<f32>, scale: f32) -> vec4<f32> {
    let d = ts.xyxy * vec4<f32>(1.0, 1.0, -1.0, 0.0) * scale;
    var r = s(t, uv - d.xy);
    r += s(t, uv - d.wy) * 2.0;
    r += s(t, uv - d.zy);
    r += s(t, uv + d.zw) * 2.0;
    r += s(t, uv) * 4.0;
    r += s(t, uv + d.xw) * 2.0;
    r += s(t, uv + d.zy);
    r += s(t, uv + d.wy) * 2.0;
    r += s(t, uv + d.xy);
    return r * (1.0 / 16.0);
}

// Colors.hlsl QuadraticThreshold. curve = (threshold - knee, knee * 2, 0.25 / knee)
fn quadratic_threshold(color: vec4<f32>, threshold: f32, curve: vec3<f32>) -> vec4<f32> {
    let br = max(max(color.r, color.g), color.b);
    var rq = clamp(br - curve.x, 0.0, curve.y);
    rq = curve.z * rq * rq;
    return color * (max(rq, br - threshold) / max(br, EPSILON));
}

// Camera colour where this camera drew (differs from the frame before it), else 0.
fn own(uv: vec2<f32>) -> vec4<f32> {
    let c = s(src_tex, uv);
    let b = s(aux_tex, uv);
    return select(vec4<f32>(0.0), c, any(abs(c.rgb - b.rgb) > vec3<f32>(0.002)));
}

fn own_box13(uv: vec2<f32>, ts: vec2<f32>) -> vec4<f32> {
    let A = own(uv + ts * vec2<f32>(-1.0, -1.0));
    let B = own(uv + ts * vec2<f32>(0.0, -1.0));
    let C = own(uv + ts * vec2<f32>(1.0, -1.0));
    let D = own(uv + ts * vec2<f32>(-0.5, -0.5));
    let E = own(uv + ts * vec2<f32>(0.5, -0.5));
    let F = own(uv + ts * vec2<f32>(-1.0, 0.0));
    let G = own(uv);
    let H = own(uv + ts * vec2<f32>(1.0, 0.0));
    let I = own(uv + ts * vec2<f32>(-0.5, 0.5));
    let J = own(uv + ts * vec2<f32>(0.5, 0.5));
    let K = own(uv + ts * vec2<f32>(-1.0, 1.0));
    let L = own(uv + ts * vec2<f32>(0.0, 1.0));
    let M = own(uv + ts * vec2<f32>(1.0, 1.0));
    let div = (1.0 / 4.0) * vec2<f32>(0.5, 0.125);
    var o = (D + E + I + J) * div.x;
    o += (A + B + G + F) * div.y;
    o += (B + C + H + G) * div.y;
    o += (F + G + L + K) * div.y;
    o += (G + H + M + L) * div.y;
    return o;
}

@fragment
fn copy(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return s(src_tex, in.uv);
}

@fragment
fn prefilter(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    var color = safe_hdr(own_box13(in.uv, texel(src_tex)));
    // autoExposure = 1 (no AutoExposure effect in the profile).
    color = min(vec4<f32>(u.params.x), color);
    return quadratic_threshold(color, u.threshold.x, u.threshold.yzw);
}

@fragment
fn downsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return downsample_box13(src_tex, in.uv, texel(src_tex));
}

@fragment
fn upsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let bloom = upsample_tent(src_tex, in.uv, texel(src_tex), u.params.y);
    return bloom + s(aux_tex, in.uv);
}

@fragment
fn uber(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    var color = safe_hdr(s(src_tex, in.uv));
    var bloom = upsample_tent(aux_tex, in.uv, texel(aux_tex), u.params.y);
    bloom *= u.params.z;
    color += bloom * vec4<f32>(u.color.rgb, 1.0);
    if (u.params.w > 0.5) {
        // The stacked camera resolves into the 8-bit back buffer before the next camera copies it.
        color = vec4<f32>(clamp(color.rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
    }
    return color;
}
