// Emission of two STRAFTAT menu shaders, ported from their compiled D3D11 deferred-pass pixel shaders (albedo 0,
// specular 0.04, smoothness 0: only the emission is visible). Project-authored transcription of the math.
//
// S_Emissive_Mask_00 (kind 0): emission = lerp(_MaskedColor, _LightColor, pow(tex.r, _Power)) * _EmissiveBoost.
// S_TVScanlines_00 (kind 1), y = world-space Y (Unity: overlay canvas at (w/2, h/2) scaled by the scale factor):
//   a = saturate(f * (1 - f) * _Float0), f = frac(y * _lines1_number * _wavelength + _time * _Time.y)
//   b = saturate(g * (1 - g) * _Float1), g = frac(y * _lines2_number * _wavelength2 + _time2 * _Time.y)
//   emission = lerp(_Color0, _Color1, a * b)
// Colour properties (serialized m_Type 0 = Color; AssetRipper prints them as Vector) arrive gamma -> linear converted
// (ugui_assets.rs build_fx), like Unity in this Linear project.

#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::globals

struct UnityFxParams {
    kind: vec4<f32>,
    color_a: vec4<f32>,
    color_b: vec4<f32>,
    p0: vec4<f32>,
    p1: vec4<f32>,
    st: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> p: UnityFxParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var tex_sampler: sampler;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var c: vec3<f32>;
    if (p.kind.x < 0.5) {
        // Mesh UVs were flipped (v' = 1 - v) on import: apply _ST in Unity UV space.
        let uv_u = vec2<f32>(in.uv.x, 1.0 - in.uv.y) * p.st.xy + p.st.zw;
        let m = textureSample(tex, tex_sampler, vec2<f32>(uv_u.x, 1.0 - uv_u.y)).r;
        c = mix(p.color_a.rgb, p.color_b.rgb, pow(m, p.p0.x)) * p.p0.y;
    } else {
        let y = p.kind.z + p.kind.y * in.world_position.y;
        let t = globals.time;
        let f = fract(y * p.p0.x * p.p0.y + p.p0.z * t);
        let a = saturate(f * (1.0 - f) * p.p0.w);
        let g = fract(y * p.p1.x * p.p1.y + p.p1.z * t);
        let b = saturate(g * (1.0 - g) * p.p1.w);
        c = mix(p.color_a.rgb, p.color_b.rgb, a * b);
    }
    return vec4<f32>(c, 1.0);
}
