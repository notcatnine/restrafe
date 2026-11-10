// Camera post-processing: S_PP_Damage00 (CameraEffect) + PPv2 LensDistortion / ChromaticAberration / ColorGrading
// gamma / Vignette / Grain (Camera Profile blended with local effect volumes on the CPU, see postfx.rs).
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct PostFx {
    // x = _Damage, y = _Suppression, z = lens intensity (-100..100), w = saturation (-100..100)
    a: vec4<f32>,
    // xyz = ColorGrading inverse gamma, w = time % 10 (grain phase)
    b: vec4<f32>,
    // Grain: x = intensity * 20 (0 = off), y = lumContrib, z = size, w = frame
    c: vec4<f32>,
    // Vignette: x = intensity * 3, y = smoothness * 5, z = roundness; w = ChromaticAberration amount
    d: vec4<f32>,
    // Vignette colour
    e: vec4<f32>,
}

@group(0) @binding(0) var screen_tex: texture_2d<f32>;
@group(0) @binding(1) var screen_samp: sampler;
@group(0) @binding(2) var<uniform> fx: PostFx;

// PPv2 LensDistortion.shader (centre 0,0, intensityX/Y 1, scale 1).
fn distort(uv_in: vec2<f32>, intensity: f32) -> vec2<f32> {
    let amount = 1.6 * max(abs(intensity), 1.0);
    let theta = radians(min(160.0, amount));
    let sigma = 2.0 * tan(theta * 0.5);
    let ax = select(1.0 / theta, theta, intensity >= 0.0);
    var uv = (uv_in - 0.5) * 1.0 + 0.5; // 1 / scale
    let ruv = uv - 0.5;
    var ru = length(ruv);
    if (intensity > 0.0) {
        let wu = ru * ax;
        ru = tan(wu) * (1.0 / (ru * sigma));
    } else {
        ru = (1.0 / ru) * ax * atan(ru * sigma);
    }
    uv = uv + ruv * (ru - 1.0);
    return uv;
}

// PPv2 GrainBaker.shader: hash noise, 3x3 high-pass (Step1) then 3x3 blur (Step2) at texel coords of the 192^2
// lookup texture.
fn grain_noise(n_in: vec2<f32>, x: f32) -> f32 {
    let n = n_in + x;
    return fract(sin(dot(n, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

fn grain_step1(uv: vec2<f32>, n: f32) -> f32 {
    let b = 2.0;
    let c = -12.0;
    return (1.0 / (4.0 + b * 4.0 + abs(c))) * (
        grain_noise(uv + vec2<f32>(-1.0, -1.0), n) + grain_noise(uv + vec2<f32>(0.0, -1.0), n) * b
        + grain_noise(uv + vec2<f32>(1.0, -1.0), n) + grain_noise(uv + vec2<f32>(-1.0, 0.0), n) * b
        + grain_noise(uv, n) * c + grain_noise(uv + vec2<f32>(1.0, 0.0), n) * b
        + grain_noise(uv + vec2<f32>(-1.0, 1.0), n) + grain_noise(uv + vec2<f32>(0.0, 1.0), n) * b
        + grain_noise(uv + vec2<f32>(1.0, 1.0), n));
}

fn grain_step2(uv: vec2<f32>, n: f32) -> f32 {
    let b = 2.0;
    let c = 4.0;
    return (1.0 / (4.0 + b * 4.0 + abs(c))) * (
        grain_step1(uv + vec2<f32>(-1.0, -1.0), n) + grain_step1(uv + vec2<f32>(0.0, -1.0), n) * b
        + grain_step1(uv + vec2<f32>(1.0, -1.0), n) + grain_step1(uv + vec2<f32>(-1.0, 0.0), n) * b
        + grain_step1(uv, n) * c + grain_step1(uv + vec2<f32>(1.0, 0.0), n) * b
        + grain_step1(uv + vec2<f32>(-1.0, 1.0), n) + grain_step1(uv + vec2<f32>(0.0, 1.0), n) * b
        + grain_step1(uv + vec2<f32>(1.0, 1.0), n));
}

// Radical inverse (HaltonSeq.Get) for the per-frame grain offset.
fn halton(index_in: u32, base: u32) -> f32 {
    var result = 0.0;
    var f = 1.0 / f32(base);
    var i = index_in;
    loop {
        if (i == 0u) {
            break;
        }
        result = result + f * f32(i % base);
        i = i / base;
        f = f / f32(base);
    }
    return result;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    var uv = in.uv;
    let lens = fx.a.z;
    if (abs(lens) > 0.0001) {
        uv = distort(uv, lens);
    }
    var col = textureSample(screen_tex, screen_samp, uv).rgb;
    // ChromaticAberration (non-fast, default 3-texel spectral LUT red / green / blue).
    if (fx.d.w > 0.0) {
        let dims = vec2<f32>(textureDimensions(screen_tex));
        let coords = 2.0 * uv - 1.0;
        let end = uv - coords * dot(coords, coords) * fx.d.w;
        let diff = end - uv;
        let samples = clamp(i32(length(dims * diff / 2.0)), 3, 16);
        let delta = diff / f32(samples);
        var pos = uv;
        var sum = vec3<f32>(0.0);
        var fsum = vec3<f32>(0.0);
        for (var i = 0; i < samples; i = i + 1) {
            let t = (f32(i) + 0.5) / f32(samples);
            // bilinear lookup of the 3x1 LUT (texel centres 1/6, 1/2, 5/6, clamped)
            let x = clamp(t * 3.0 - 0.5, 0.0, 2.0);
            let filt = vec3<f32>(clamp(1.0 - x, 0.0, 1.0), clamp(1.0 - abs(x - 1.0), 0.0, 1.0), clamp(x - 1.0, 0.0, 1.0));
            sum = sum + textureSampleLevel(screen_tex, screen_samp, pos, 0.0).rgb * filt;
            fsum = fsum + filt;
            pos = pos + delta;
        }
        col = sum / fsum;
    }

    // ColorGrading lift-gamma-gain gamma (Camera Profile Vector4(1,1,1,brightness-1) blended with volume gammas):
    // sign(c) * pow(|c|, invgamma).
    if (any(abs(fx.b.xyz - vec3<f32>(1.0)) > vec3<f32>(0.00001))) {
        col = pow(max(col, vec3<f32>(0.0)), fx.b.xyz);
    }
    // ColorGrading saturation (-100..100).
    let luma = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
    col = mix(vec3<f32>(luma), col, (fx.a.w + 100.0) / 100.0);

    // PPv2 classic Vignette (Camera Profile: intensity 0.43, smoothness 0.11, roundness 1, rounded off,
    // centre 0.5/0.5, colour black; blended with local volumes): settings = (intensity*3, smoothness*5, roundness).
    {
        var d = abs(in.uv - vec2<f32>(0.5)) * fx.d.x;
        d = pow(clamp(d, vec2<f32>(0.0), vec2<f32>(1.0)), vec2<f32>(fx.d.z));
        let vfactor = pow(clamp(1.0 - dot(d, d), 0.0, 1.0), fx.d.y);
        col = col * mix(fx.e.rgb, vec3<f32>(1.0), vfactor);
    }

    // Grain (Uber: after grading): lookup texel = screen pixel / size + Halton offset * 192 (Repeat),
    // colour += colour * grain * intensity * 20 * lerp(1, 1 - sqrt(luma(saturate(colour))), lumContrib).
    if (fx.c.x > 0.0) {
        let dims = vec2<f32>(textureDimensions(screen_tex));
        let frame = u32(fx.c.w);
        let off = vec2<f32>(halton(frame & 1023u, 2u), halton(frame & 1023u, 3u)) * 192.0;
        let texel = floor(in.uv * dims / fx.c.z + off) % vec2<f32>(192.0);
        let g = grain_step2(texel, 0.07 * fract(fx.b.w));
        var lum = 1.0 - sqrt(dot(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(0.2126729, 0.7151522, 0.072175)));
        lum = mix(1.0, lum, fx.c.y);
        col = col + col * g * fx.c.x * lum;
    }

    // S_PP_Damage00: red screen-edge vignette scaled by _Damage (_Color1 = 0.915,0.177,0.177),
    // _Suppression darkens/desaturates the edges (original mask texture not reproduced).
    let r = length((in.uv - 0.5) * 2.0) * 0.7071;
    let edge = smoothstep(0.25, 1.0, r);
    let dmg = clamp(fx.a.x, 0.0, 1.0);
    col = mix(col, vec3<f32>(0.915, 0.177, 0.177), clamp(edge * edge * dmg * 0.85, 0.0, 1.0));
    let sup = clamp(fx.a.y / 3.0, 0.0, 1.0);
    let l2 = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
    col = mix(col, vec3<f32>(l2) * (1.0 - 0.6 * edge), sup * (0.3 + 0.7 * edge));
    // Resolve to the 8-bit back buffer (next stacked camera starts from the clamped composite).
    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
