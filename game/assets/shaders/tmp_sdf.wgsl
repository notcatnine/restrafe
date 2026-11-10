// Port of TextMeshPro's "TextMeshPro/Distance Field" shader (TMP_SDF.shader + TMPro.cginc, TMP 3.0.x vertex layout:
// texcoord1.y = character scale * |lossyScale.y|, sign = bold). Used for the 3D HUD texts (perspective UICam) and the
// grab popup (orthographic overlay camera, 1 world unit = 1 canvas unit).
//
// Mesh attributes: position (TMP local, already converted to our space), normal (TMP: 0,0,-1), uv0 = atlas UV (Bevy
// convention, v down), uv1.y = character scale WITHOUT the lossy scale (applied here from the model matrix, like TMP
// re-generating uv2 whenever lossyScale.y changes), color = TMP vertex colour (raw Color32/255, not linearised).
// Material colours arrive as serialized (gamma values, NOT linearised) - see `tmp_text::tmp_color` for the
// measurements against the original game.
// Output is premultiplied (Blend One OneMinusSrcAlpha), no fog, no tonemapping (the HUD/overlay output is clamped
// to [0,1] by the 8-bit target, as in Unity).

#import bevy_pbr::mesh_functions::{get_world_from_local, mesh_normal_local_to_world}
#import bevy_pbr::mesh_view_bindings::view

struct TmpSdfParams {
    face_color: vec4<f32>,
    outline_color: vec4<f32>,
    underlay_color: vec4<f32>,
    glow_color: vec4<f32>,
    specular_color: vec4<f32>,
    // x _FaceDilate, y _OutlineWidth, z _OutlineSoftness, w _WeightNormal
    shape: vec4<f32>,
    // x _ScaleRatioA, y _ScaleRatioB, z _ScaleRatioC, w _GradientScale
    ratios: vec4<f32>,
    // x _UnderlayOffsetX, y _UnderlayOffsetY, z _UnderlayDilate, w _UnderlaySoftness
    underlay: vec4<f32>,
    // x _GlowOffset, y _GlowInner, z _GlowOuter, w _GlowPower
    glow: vec4<f32>,
    // x _Bevel, y _BevelOffset, z _BevelWidth, w _BevelClamp
    bevel: vec4<f32>,
    // x _BevelRoundness, y _LightAngle, z _SpecularPower, w _Reflectivity
    light: vec4<f32>,
    // x _Diffuse, y _Ambient, z _Sharpness, w _PerspectiveFilter
    misc: vec4<f32>,
    // x _TextureWidth, y _TextureHeight, z _ScaleX, w _ScaleY
    tex: vec4<f32>,
    // x UNDERLAY_ON, y BEVEL_ON, z GLOW_ON, w _WeightBold
    flags: vec4<f32>,
    // x _ShaderFlags
    extra: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> p: TmpSdfParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var atlas: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var atlas_sampler: sampler;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) uv1: vec2<f32>,
    @location(4) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) atlas_uv: vec2<f32>,
    // alphaClip, scale, bias, weight
    @location(1) param: vec4<f32>,
    // underlay uv, bScale, bBias
    @location(2) underlay: vec4<f32>,
    @location(3) color: vec4<f32>,
};

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = get_world_from_local(v.instance_index);
    let world = world_from_local * vec4<f32>(v.position, 1.0);
    let clip = view.clip_from_world * world;

    // pixelSize = w / (float2(_ScaleX, _ScaleY) * abs(mul((float2x2)UNITY_MATRIX_P, _ScreenParams.xy)))
    let proj = view.clip_from_view;
    let screen = view.viewport.zw;
    let pp = abs(vec2<f32>(proj[0][0] * screen.x + proj[1][0] * screen.y, proj[0][1] * screen.x + proj[1][1] * screen.y));
    let pixel_size = vec2<f32>(clip.w, clip.w) / (p.tex.zw * pp);
    var scale = inverseSqrt(dot(pixel_size, pixel_size));
    // TMP: texcoord1.y = characterInfo.scale * |transform.lossyScale.y|.
    let lossy_y = length(world_from_local[1].xyz);
    scale *= abs(v.uv1.y) * lossy_y * p.ratios.w * (p.misc.z + 1.0);
    if (proj[3][3] == 0.0) {
        // Perspective: soften when the text is seen at a grazing angle.
        let n = normalize(mesh_normal_local_to_world(v.normal, v.instance_index));
        let view_dir = normalize(view.world_position - world.xyz);
        scale = mix(abs(scale) * (1.0 - p.misc.w), scale, abs(dot(n, view_dir)));
    }

    let bold = select(0.0, 1.0, v.uv1.y <= 0.0);
    var weight = mix(p.shape.w, p.flags.w, bold) / 4.0;
    weight = (weight + p.shape.x) * p.ratios.x * 0.5;

    let bias = (0.5 - weight) + (0.5 / scale);

    var alpha_clip = 1.0 - p.shape.y * p.ratios.x - p.shape.z * p.ratios.x;
    if (p.flags.z > 0.5) {
        alpha_clip = min(alpha_clip, 1.0 - p.glow.x * p.ratios.y - p.glow.z * p.ratios.y);
    }
    alpha_clip = alpha_clip / 2.0 - (0.5 / scale) - weight;

    // Underlay
    var b_scale = scale;
    b_scale /= 1.0 + ((p.underlay.w * p.ratios.z) * b_scale);
    let b_bias = (0.5 - weight) * b_scale - 0.5 - ((p.underlay.z * p.ratios.z) * 0.5 * b_scale);
    let ox = -(p.underlay.x * p.ratios.z) * p.ratios.w / p.tex.x;
    let oy = -(p.underlay.y * p.ratios.z) * p.ratios.w / p.tex.y;

    out.clip_position = clip;
    out.atlas_uv = v.uv;
    out.param = vec4<f32>(alpha_clip, scale, bias, weight);
    // Unity UV v points up, ours down.
    out.underlay = vec4<f32>(v.uv + vec2<f32>(ox, -oy), b_scale, b_bias);
    out.color = v.color;
    return out;
}

fn sdf(uv: vec2<f32>) -> f32 {
    return textureSampleLevel(atlas, atlas_sampler, uv, 0.0).a;
}

// TMPro.cginc GetColor
fn get_color(d: f32, face_in: vec4<f32>, outline_in: vec4<f32>, outline: f32, softness: f32) -> vec4<f32> {
    let face_alpha = 1.0 - saturate((d - outline * 0.5 + softness * 0.5) / (1.0 + softness));
    let outline_alpha = saturate(d + outline * 0.5) * sqrt(min(1.0, outline));
    var face = vec4<f32>(face_in.rgb * face_in.a, face_in.a);
    let oc = vec4<f32>(outline_in.rgb * outline_in.a, outline_in.a);
    face = mix(face, oc, outline_alpha);
    return face * face_alpha;
}

// TMPro_Surface / TMPro.cginc GetSurfaceNormal(float4 h, float bias)
fn surface_normal_h(h_in: vec4<f32>, bias: f32) -> vec3<f32> {
    let raised = (p.extra.x % 2.0) >= 1.0;
    var h = h_in + vec4<f32>(bias + p.bevel.y);
    let bevel_width = max(0.01, p.shape.y + p.bevel.z);
    // Track outline
    h -= vec4<f32>(0.5);
    h /= bevel_width;
    h = saturate(h + vec4<f32>(0.5));
    if (raised) {
        h = vec4<f32>(1.0) - abs(h * 2.0 - vec4<f32>(1.0));
    }
    h = mix(h, sin(h * 3.141592 / 2.0), vec4<f32>(p.light.x));
    h = min(h, vec4<f32>(1.0 - p.bevel.w));
    h *= p.bevel.x * bevel_width * p.ratios.w * -2.0;
    let va = normalize(vec3<f32>(1.0, 0.0, h.y - h.x));
    let vb = normalize(vec3<f32>(0.0, -1.0, h.w - h.z));
    return cross(va, vb);
}

fn surface_normal(uv: vec2<f32>, bias: f32, delta: vec2<f32>) -> vec3<f32> {
    // Unity: h = {uv - dx, uv + dx, uv - dy, uv + dy} with v up; our v points down.
    let h = vec4<f32>(
        sdf(uv - vec2<f32>(delta.x, 0.0)),
        sdf(uv + vec2<f32>(delta.x, 0.0)),
        sdf(uv + vec2<f32>(0.0, delta.y)),
        sdf(uv - vec2<f32>(0.0, delta.y)),
    );
    return surface_normal_h(h, bias);
}

fn get_specular(n: vec3<f32>, l: vec3<f32>) -> vec3<f32> {
    let spec = pow(max(0.0, dot(n, l)), p.light.w);
    return p.specular_color.rgb * spec * p.light.z;
}

fn get_glow_color(d: f32, scale: f32) -> vec4<f32> {
    var glow = d - (p.glow.x * p.ratios.y) * 0.5 * scale;
    let t = mix(p.glow.y, p.glow.z * p.ratios.y, step(0.0, glow)) * 0.5 * scale;
    glow = saturate(abs(glow / (1.0 + t)));
    glow = 1.0 - pow(glow, p.glow.w);
    glow *= sqrt(min(1.0, t)); // Fade off glow thinner than 1 screen pixel
    return vec4<f32>(p.glow_color.rgb, saturate(p.glow_color.a * glow * 2.0));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let c = sdf(in.atlas_uv);
    if (p.flags.x < 0.5 && c - in.param.x < 0.0) {
        discard;
    }
    let scale = in.param.y;
    let bias = in.param.z;
    let weight = in.param.w;
    let sd = (bias - c) * scale;

    let outline = (p.shape.y * p.ratios.x) * scale;
    let softness = (p.shape.z * p.ratios.x) * scale;

    var face_color = p.face_color;
    let outline_color = p.outline_color;
    face_color = vec4<f32>(face_color.rgb * in.color.rgb, face_color.a);

    face_color = get_color(sd, face_color, outline_color, outline, softness);

    if (p.flags.y > 0.5) {
        let dxy = vec2<f32>(0.5 / p.tex.x, 0.5 / p.tex.y);
        // _BumpMap is the default "bump" texture and _BumpFace/_BumpOutline are 0: no bump.
        let n = surface_normal(in.atlas_uv, weight, dxy);
        let light = normalize(vec3<f32>(sin(p.light.y), cos(p.light.y), -1.0));
        let col = get_specular(n, light);
        face_color = vec4<f32>(face_color.rgb + col * face_color.a, face_color.a);
        face_color = vec4<f32>(face_color.rgb * (1.0 - dot(n, light) * p.misc.x), face_color.a);
        face_color = vec4<f32>(face_color.rgb * mix(p.misc.y, 1.0, n.z * n.z), face_color.a);
        // _Cube is the default black cubemap: no reflection term.
    }

    if (p.flags.x > 0.5) {
        let d = sdf(in.underlay.xy) * in.underlay.z;
        let uc = vec4<f32>(p.underlay_color.rgb * p.underlay_color.a, p.underlay_color.a);
        face_color += uc * saturate(d - in.underlay.w) * (1.0 - face_color.a);
    }

    if (p.flags.z > 0.5) {
        let glow_color = get_glow_color(sd, scale);
        face_color = vec4<f32>(face_color.rgb + glow_color.rgb * glow_color.a, face_color.a);
    }

    // Overlay canvases (orthographic camera) draw into Unity's 8-bit back buffer: the output is clamped before
    // blending. The world-space HUD (perspective UICam) renders into UICam's HDR target (PPv2 bloom sees > 1).
    let out = face_color * in.color.a;
    if (view.clip_from_view[3][3] == 1.0) {
        return clamp(out, vec4<f32>(0.0), vec4<f32>(1.0));
    }
    return out;
}
