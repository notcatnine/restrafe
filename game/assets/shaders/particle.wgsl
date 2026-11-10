// Particle shaders recovered from the game's DXBC (project-authored port, see src/vfx/mat.rs `ParticleMat`).
// One fragment entry point; the formula is picked by the PMODE_* shader def, the blend state is set by the
// pipeline (exact Unity Blend factors). Inputs: vertex colour (raw or linearised per renderer, like Unity's
// ParticleSystemRenderer.applyActiveColorSpace), material tint in linear space, texture sampled as in Unity
// (sRGB or linear per the texture importer).
#import bevy_pbr::{
    forward_io::VertexOutput,
    mesh_view_bindings::{globals, view, lights, view_transmission_texture, view_transmission_sampler},
    view_transformations::{depth_ndc_to_view_z, position_world_to_view},
}
#import bevy_pbr::mesh_view_bindings as vb
#import bevy_pbr::clustered_forward as clustering
#ifdef DEPTH_PREPASS
#import bevy_pbr::prepass_utils::prepass_depth
#endif

struct ParticleParams {
    // _TintColor / _Color0 (linear)
    tint: vec4<f32>,
    // texture _ST (scale.xy, offset.zw)
    st: vec4<f32>,
    // x boost (_EmissiveBoost / _EmisiveBoost), y _ScrollSpeed, z _InvFade, w opacity (_Opacity)
    p0: vec4<f32>,
    // x depth-fade length (_Float1), y depth-fade offset (_Float2), z camera near plane
    // (blood: x _EdgeDarken, y _Columns, z _Rows)
    p1: vec4<f32>,
    // blood: _ChannelMask
    cm: vec4<f32>,
    // blood: _WarpTex_ST, _NoiseTex_ST, (_WarpStr, _NoiseAlphaStr, _NoiseColorStr, _Randomize), _ChannelMask2
    st2: vec4<f32>,
    st3: vec4<f32>,
    p2: vec4<f32>,
    cm2: vec4<f32>,
    // blood: _SpecularColor (a = specular weight), _ReflectionTex_ST, (_ReflectionSat, _FlattenNormal, _LightStr, 0)
    spec: vec4<f32>,
    st4: vec4<f32>,
    p3: vec4<f32>,
    // blood: vertex light factor (rgb)
    light: vec4<f32>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> m: ParticleParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var tex_s: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var tex2: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var tex2_s: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var tex3: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var tex3_s: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(7) var tex4: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(8) var tex4_s: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(9) var tex5: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(10) var tex5_s: sampler;

fn eye_depth(world: vec3<f32>) -> f32 {
    return -position_world_to_view(world).z;
}

// Unity soft particles (SOFTPARTICLES_ON): saturate(_InvFade * (sceneZ - partZ)).
fn soft_fade(in: VertexOutput, sample_index: u32) -> f32 {
#ifdef SOFT
#ifdef DEPTH_PREPASS
    let scene_d = prepass_depth(in.position, sample_index);
    if scene_d > 0.0 {
        let scene_z = -depth_ndc_to_view_z(scene_d);
        return clamp(m.p0.z * (scene_z - eye_depth(in.world_position.xyz)), 0.0, 1.0);
    }
#endif
#endif
    return 1.0;
}

@fragment
fn fragment(in: VertexOutput, @builtin(sample_index) sample_index: u32) -> @location(0) vec4<f32> {
#ifdef VERTEX_COLORS
    var vc = in.color;
#else
    var vc = vec4<f32>(1.0);
#endif
#ifdef VERTEX_UVS_A
    let uv0 = in.uv;
#else
    let uv0 = vec2<f32>(0.0);
#endif
    let uv = uv0 * m.st.xy + m.st.zw;
    vc.a = vc.a * soft_fade(in, sample_index);
    var o = vec4<f32>(0.0);

#ifdef PMODE_AMP00
    // S_Particle_00 / S_ParticleAdditive_00: rgb = vcol * boost, a = tex.r * vcol.a
    let t = textureSample(tex, tex_s, uv);
    o = vec4<f32>(vc.rgb * m.p0.x, t.r * vc.a);
#endif
#ifdef PMODE_AMP03
    // S_Particle_03: rgb = vcol * boost * vcol.a, a = tex.r * vcol.a
    let t = textureSample(tex, tex_s, uv);
    o = vec4<f32>(vc.rgb * m.p0.x * vc.a, t.r * vc.a);
#endif
#ifdef PMODE_AMP04
    // S_Particle_04: rgb = vcol * boost * tex.rgb * vcol.a, a = tex.r * vcol.a
    let t = textureSample(tex, tex_s, uv);
    o = vec4<f32>(vc.rgb * m.p0.x * t.rgb * vc.a, t.r * vc.a);
#endif
#ifdef PMODE_DEPTHFADE
    // S_Particle_depthfade__00: rgb = vcol, a = tex.r * vcol.a * saturate((eyeZ - near - _Float2) / _Float1)
    let t = textureSample(tex, tex_s, uv);
    let f = clamp((eye_depth(in.world_position.xyz) - m.p1.z - m.p1.y) / m.p1.x, 0.0, 1.0);
    o = vec4<f32>(vc.rgb, t.r * vc.a * f);
#endif
#ifdef PMODE_TRAIL01
    // S_Particle_Trail_01 (emission part; black albedo): rgb = vcol * boost * vcol.a, a = vcol.a
    o = vec4<f32>(vc.rgb * m.p0.x * vc.a, vc.a);
#endif
#ifdef PMODE_TRAIL00
    // S_Particle_Trail_00: Standard (albedo = vcol, metallic 0, smoothness 0) + emission vcol * boost, a = vcol.a.
    // The lit part is approximated by the emission alone (camera-facing ribbons, dim map light).
    o = vec4<f32>(vc.rgb * m.p0.x, vc.a);
#endif
#ifdef PMODE_STD_UNLIT
    // Particles/Standard Unlit: albedo = tex * _Color * vcol, a = albedo.a (blend state per _Mode).
    o = textureSample(tex, tex_s, uv) * m.tint * vc;
#endif
#ifdef PMODE_CONST_EMISSIVE
    // S_DoubleSided_Emissive_00 (emission part): rgb = _Color0 * _EmissiveBoost, a = _Opacity
    o = vec4<f32>(m.tint.rgb * m.p0.x, m.p0.w);
#endif
#ifdef PMODE_LEGACY
    // Legacy Particles/Additive, Alpha Blended (+ WFX Alpha Blended): 2 * vcol * _TintColor * tex
    let t = textureSample(tex, tex_s, uv);
    o = 2.0 * vc * m.tint * t;
    o.a = clamp(o.a, 0.0, 1.0);
#endif
#ifdef PMODE_MOBILE
    // Mobile/Particles/Additive, Alpha Blended, VertexLit Blended (vertex colour pre-lit on the CPU): tex * vcol
    var lc = vc;
    if m.p1.w > 0.5 {
        // Particles/VertexLit Blended: vertex colour = saturate(vcol * ambient + _EmisColor), a = vcol.a
        lc = vec4<f32>(clamp(vc.rgb * m.tint.w + m.tint.rgb, vec3<f32>(0.0), vec3<f32>(1.0)), vc.a);
    }
    o = textureSample(tex, tex_s, uv) * lc;
#endif
#ifdef PMODE_MOBILE_MUL
    // Mobile/Particles/Multiply: lerp(1, tex * vcol, tex.a * vcol.a)
    let t = textureSample(tex, tex_s, uv);
    o = mix(vec4<f32>(1.0), t * vc, t.a * vc.a);
#endif
#ifdef PMODE_WFX_ADD8
    // WFX/Additive Alpha8 (Blend One One): 2 * vcol * tint * dot(tex.aa, vcol.aa)
    let t = textureSample(tex, tex_s, uv);
    o = 2.0 * vc * m.tint * (2.0 * t.a * vc.a);
#endif
#ifdef PMODE_WFX_ADDSOFT8
    // WFX/Additive (Soft) Alpha8 (Blend OneMinusDstColor One): 2 * vcol * tint * tex.a
    let t = textureSample(tex, tex_s, uv);
    o = 2.0 * vc * m.tint * t.a;
#endif
#ifdef PMODE_WFX_MUL8
    // WFX/Multiply Alpha8 (Blend Zero SrcColor): tex.a * vcol
    let t = textureSample(tex, tex_s, uv);
    o = t.a * vc;
#endif
#ifdef PMODE_WFX_MULSOFT
    // WFX/Multiply Soft Tint, WFX/Scroll/* (Blend DstColor SrcColor | DstColor SrcAlpha):
    // c = tex(scrolled) * (vcol * tint) [rgb], mask = tex(uv).a * vcol.a, out = lerp(0.5, c, mask)
#ifdef SCROLL
    // _Time.x = t / 20; scroll = sign-preserving frac(_Time.x * _ScrollSpeed)
    let s = globals.time / 20.0 * m.p0.y;
    let fs = select(-fract(abs(s)), fract(abs(s)), s >= -s);
    let ts = textureSample(tex, tex_s, vec2<f32>(uv.x, uv.y - fs));
#else
    let ts = textureSample(tex, tex_s, uv);
#endif
    let t0 = textureSample(tex, tex_s, uv);
    let mask = t0.a * vc.a;
#ifdef SMOKE
    // WFX/Scroll/Smoke: alpha channel = mask (blend DstColor SrcAlpha -> dst * (out.rgb + out.a))
    let c = vec4<f32>(ts.rgb * vc.rgb * m.tint.rgb, mask);
#else
    let c = vec4<f32>(ts.rgb * vc.rgb * m.tint.rgb, ts.a);
#endif
    o = mix(vec4<f32>(0.5), c, mask);
#endif
#ifdef PMODE_BLOOD
    // FXVille Blood 2018, forward base (DXBC blobs 3 / 211: SPECULAR_REFLECTION_ON DIRECTIONAL LIGHTPROBE_SH).
    // p0 = (_MaskStr, _ProcMask, _AlphaMin, _AlphaSoft), p1.x _EdgeDarken, p1.yz (_Columns, _Rows), cm _ChannelMask,
    // st2 _WarpTex_ST, st3 _NoiseTex_ST, p2 = (_WarpStr, _NoiseAlphaStr, _NoiseColorStr, _Randomize), cm2 _ChannelMask2.
    // Custom streams: uv_b = Custom1.xy (x: noise / warp scroll, y: warp amount), tangent.w = StableRandomX.
    // The flip / fall of the vertex program are applied on the CPU (sim.rs); vc.a arrives as the stream colour
    // alpha, the vertex program's a^2 + _AlphaMin is done here. Specular (2026-11-05, PS blob 211; names from the
    // variant's parameter blob 206): _Normal (DXT5nm, at the noise UV) in the particle's tangent frame, reflection
    // R = reflect(-V, N) looked up equirectangularly in _ReflectionTex. m.light = the vertex SH light factor
    // v4 = 1 + _LightStr * (max(ShadeSH9(n), 0.15 * ambient) - 1), one value per renderer (sim.rs).
#ifdef VERTEX_UVS_B
    let cust = in.uv_b;
#else
    let cust = vec2<f32>(0.0);
#endif
#ifdef VERTEX_TANGENTS
    let rnd = in.world_tangent.w;
#else
    let rnd = 0.0;
#endif
    // o0.zw: cell UV + per-particle offset
    let zw = uv * m.p1.yz + rnd * vec2<f32>(3.0, 8.0) * m.p2.w;
    let cell = fract(uv * m.p1.yz) - vec2<f32>(0.5);
    let r = clamp(4.0 * dot(cell * cell, vec2<f32>(1.0)), 0.0, 1.0);
    let proc = 1.0 - m.p0.y * r * r;
    let wuv = zw * m.st2.xy + (cust.x + 1.0) * m.st2.zw + rnd * vec2<f32>(5.0, 8.0);
    let warp = textureSample(tex2, tex2_s, wuv).xy * 2.0 - vec2<f32>(1.0);
    let wamt = cust.y * m.p2.x;
    let t = clamp(vec4<f32>(1.0) + m.p0.x * (textureSample(tex, tex_s, uv * m.st.xy + warp * wamt) - vec4<f32>(1.0)), vec4<f32>(0.0), vec4<f32>(1.0)) * proc;
    var mask = clamp(dot(t, m.cm), 0.0, 1.0);
    let nuv = zw * m.st3.xy + cust.x * m.st3.zw + warp * wamt;
    let na = clamp(1.0 + m.p2.y * (dot(textureSample(tex3, tex3_s, nuv), m.cm2) - 1.0), 0.0, 1.0);
    mask = mask * na;
    let amin = m.p0.z;
    let z = mask * (vc.a * vc.a + amin) - amin;
    let a = clamp(z / m.p0.w, 0.0, 1.0);
    var x = 1.0 - mask * a;
    x = 1.0 - x * x;
    x = x + m.p2.z * (na - 0.5);
    x = clamp(0.71 + m.p1.x * (x * x - 0.71), 0.0, 1.0);
    let c = max(vc.rgb, vec3<f32>(0.001));
    let dark = min(0.3 * c * c * c, vec3<f32>(1.0));
    let shade = mix(dark, vec3<f32>(0.71), x);
    let fa = clamp(x * (m.tint.a - 1.25) + 1.25, 0.0, 1.0);
    var spec = vec3<f32>(0.0);
    if m.spec.w > 0.0 {
        // Tangent-space normal: xy from the DXT5nm map (w * x, y), z = (sat(z / (_AlphaSoft + 0.2)) - 0.1) * _FlattenNormal * 1.2.
        let nm = textureSample(tex5, tex5_s, nuv);
        let nt = normalize(vec3<f32>(nm.w * nm.x * 2.0 - 1.0, nm.y * 2.0 - 1.0, (clamp(z / (m.p0.w + 0.2), 0.0, 1.0) - 0.1) * m.p3.y * 1.2));
        let nw = normalize(in.world_normal);
#ifdef VERTEX_TANGENTS
        let tw = in.world_tangent.xyz;
#else
        let tw = vec3<f32>(1.0, 0.0, 0.0);
#endif
        // Binormal = the quad's +v (texture up) direction.
        let bw = cross(nw, tw);
        let n = normalize(tw * nt.x + bw * nt.y + nw * nt.z + nw);
        let v = normalize(view.world_position - in.world_position.xyz);
        let r = reflect(-v, n);
        let fres = clamp(1.0 - dot(v, n) * m.spec.w, 0.2, 1.0);
        // Equirectangular lookup in Unity space (z mirrored): u = atan2(R.x, R.z) / 2pi, v = R.y / 2, + (random + _Time.xy) scroll.
        let ang = atan2(r.x, -r.z);
        let ru = vec2<f32>(ang * 0.159155 * m.st4.x, r.y * 0.5 * m.st4.y) + (vec2<f32>(rnd) + vec2<f32>(globals.time / 20.0, globals.time)) * m.st4.zw;
        let rc = textureSample(tex4, tex4_s, vec2<f32>(ru.x, 1.0 - ru.y)).rgb;
        let lum = dot(rc, vec3<f32>(1.0)) * 0.333;
        let sc = m.p3.x * (rc - vec3<f32>(lum)) + vec3<f32>(lum);
        let s4 = sc * sc;
        spec = clamp(sc + mask * m.spec.w * (s4 * s4 - sc), vec3<f32>(0.0), vec3<f32>(10.0)) * fres * m.light.rgb * m.spec.rgb;
    }
    o = vec4<f32>(c * shade * max(m.light.rgb * m.tint.rgb, vec3<f32>(0.0)) + spec, fa * a);
#endif
#ifdef PMODE_DISTORTION
    // S_DistortionRefraction_00 (Amplify standard surface, black albedo, smoothness 0, metallic 0, _USENORMAL_ON,
    // ForwardBase) from the DXBC. p0 = (_Float0 opacity, _ChromaticAberration, _Float1 refraction index).
    // Normal: _TextureSample0 sampled raw (sRGB-decoded, no UnpackNormal) through the vertex TBN. The particle
    // renderer streams (Position, Normal, Color, UV; m_UseCustomVertexStreams 0) have no Tangent: the missing channel is
    // taken as (1,0,0,1) in world space (particle vertices are baked in world space). Not documented by Unity; it cannot
    // be zero: the DXBC vertex shader normalizes the tangent with rsq, so a zero one would make every pixel NaN.
    // Binormal (DXBC) = cross(N, T) * tangent.w * unity_WorldTransformParams.w in Unity space; the port's world is
    // Unity's z mirror (det -1), which negates a cross product: mirrored binormal = cross(T, N) in port space.
    let n_ts = textureSample(tex, tex_s, uv).rgb;
    let nv = in.world_normal;
    let tv = vec3<f32>(1.0, 0.0, 0.0);
    let bv = cross(tv, nv);
    let wn = normalize(tv * n_ts.x + bv * n_ts.y + nv * n_ts.z);
    let vdir = normalize(view.world_position - in.world_position.xyz);
    let ndv = dot(wn, vdir);
    // Amplify Refraction(): offset = (ior - 1) * (V * N).xy * (1 - N.V), added to the (top-down) grab UV;
    // r from +offset, g from offset * (1 - CA), b from offset * (1 + CA).
    let vn = (view.view_from_world * vec4<f32>(wn, 0.0)).xy;
    let refr = (m.p0.z - 1.0) * vn * (1.0 - ndv);
    let suv = in.position.xy / vec2<f32>(textureDimensions(view_transmission_texture));
    let ca = m.p0.y;
    let rr = textureSampleLevel(view_transmission_texture, view_transmission_sampler, suv + refr, 0.0).r;
    let rg = textureSampleLevel(view_transmission_texture, view_transmission_sampler, suv + refr * (1.0 - ca), 0.0).g;
    let rb = textureSampleLevel(view_transmission_texture, view_transmission_sampler, suv + refr * (1.0 + ca), 0.0).b;
    // BRDF1 with roughness 1 (D * pi = 1), specColor 0.04: direct specular of the main directional light
    // (_LightColor0 = Bevy colour * exposure / pi, the port's lux -> Unity light colour mapping).
    var spec = vec3<f32>(0.0);
    if lights.n_directional_lights > 0u {
        let ldir = lights.directional_lights[0].direction_to_light;
        let lc = lights.directional_lights[0].color.rgb * view.exposure / 3.14159265;
        let h = normalize(ldir + vdir);
        let nl = clamp(dot(wn, ldir), 0.0, 1.0);
        let lh = clamp(dot(ldir, h), 0.0, 1.0);
        let vis = 0.5 / (abs(ndv) + nl + 1e-5);
        let f = 0.04 + 0.96 * pow(1.0 - lh, 5.0);
        spec = lc * (vis * nl) * f;
    }
    // Indirect specular (same ForwardBase DXBC): unity_SpecCube0 at the reflection vector, LOD 6 (perceptual roughness 1
    // x UNITY_SPECCUBE_LOD_STEPS; the map's probes are 128 px, m_BoxProjection 0, HDR decode = x 1) x surfaceReduction
    // 0.5 x FresnelLerp(0.04, grazing 0.04) = 0.02 x env. The port's probe is the camera's EnvironmentMapLight specular
    // cube (scene_map.rs picks the probe box around the camera); intensity_for_view x exposure undoes the port's 1/exposure.
#ifdef ENVIRONMENT_MAP
    if vb::light_probes.view_cubemap_index >= 0 {
        let rdir = reflect(-vdir, wn);
#ifdef MULTIPLE_LIGHT_PROBES_IN_ARRAY
        let env = textureSampleLevel(vb::specular_environment_maps[vb::light_probes.view_cubemap_index], vb::environment_map_sampler, rdir, 6.0).rgb;
#else
        let env = textureSampleLevel(vb::specular_environment_map, vb::environment_map_sampler, rdir, 6.0).rgb;
#endif
        spec += env * (vb::light_probes.intensity_for_view * view.exposure * 0.5 * 0.04);
    }
#endif
    // ForwardAdd pass (Blend One One, DXBC of the POINT variant): per pixel light, the same direct specular with the
    // light's _LightTexture0 falloff. Unity's forward path renders a light per pixel when it is Important, or while the
    // object's per-pixel lights stay under QualitySettings.pixelLightCount (Low 0 / Medium 1 / High 2 / Ultra 4), the
    // brightest directional light (ForwardBase) counting as one. Ported: the Important point lights (the explosions'
    // own lights, tagged by unity_light.rs UNITY_LIGHT_RADIUS_IMPORTANT), exact for Low / Medium where the base
    // directional light uses the only slot; extra Auto lights on High / Ultra are not ported.
    let view_z = dot(vec4<f32>(view.view_from_world[0].z, view.view_from_world[1].z, view.view_from_world[2].z, view.view_from_world[3].z), in.world_position);
    let ortho = view.clip_from_view[3].w == 1.0;
    let ranges = clustering::unpack_clusterable_object_index_ranges(clustering::view_fragment_cluster_index(in.position.xy, view_z, ortho));
    for (var i: u32 = ranges.first_point_light_index_offset; i < ranges.first_spot_light_index_offset; i = i + 1u) {
        let light = &vb::clustered_lights.data[clustering::get_clusterable_object_id(i)];
        if abs((*light).position_radius.w - 2.0e-5) > 2.0e-6 {
            continue;
        }
        let to_light = (*light).position_radius.xyz - in.world_position.xyz;
        let x = dot(to_light, to_light) * (*light).color_inverse_square_range.w;
        var atten = 1.0 / (1.0 + 25.0 * x);
        if x >= 0.64 {
            atten *= clamp(1.0 - (x - 0.64) / 0.36, 0.0, 1.0);
        }
        let ldir = normalize(to_light);
        let lc = (*light).color_inverse_square_range.rgb * view.exposure / 3.14159265;
        let h = normalize(ldir + vdir);
        let nl = clamp(dot(wn, ldir), 0.0, 1.0);
        let lh = clamp(dot(ldir, h), 0.0, 1.0);
        let vis = 0.5 / (abs(ndv) + nl + 1e-5);
        spec += lc * atten * (vis * nl) * (0.04 + 0.96 * pow(1.0 - lh, 5.0));
    }
    // _ALPHAPREMULTIPLY_ON: out alpha = 0.04 + 0.96 * opacity; refraction added * (1 - alpha), alpha = 1.
    let oa = 0.04 + 0.96 * m.p0.x;
    o = vec4<f32>(vec3<f32>(rr, rg, rb) * (1.0 - oa) + spec, 1.0);
#endif
    return o;
}
