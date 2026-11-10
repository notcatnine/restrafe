//! Particle materials. AssetRipper exports dummy shaders, so the real logic was recovered from the DXBC in
//! `ripped_gltf/Assets/Shader/<shader>.json` (forward-base, no-fog variants) and ported to
//! `assets/shaders/particle.wgsl` (`ParticleMat`): per-shader formula + the exact Unity blend factors.
//!
//! | shader (materials)                      | blend                    | formula                                        |
//! |-----------------------------------------|--------------------------|------------------------------------------------|
//! | S_Particle_00                           | SrcAlpha OneMinusSrcAlpha| rgb vcol*boost, a tex.r*vcol.a                 |
//! | S_ParticleAdditive_00                   | SrcAlpha One, ZWrite On  | same as S_Particle_00                          |
//! | S_Particle_03                           | SrcAlpha OneMinusSrcAlpha| rgb vcol*boost*vcol.a, a tex.r*vcol.a          |
//! | S_Particle_04                           | SrcAlpha OneMinusSrcAlpha| rgb vcol*boost*tex.rgb*vcol.a, a tex.r*vcol.a  |
//! | S_Particle_depthfade__00                | SrcAlpha OneMinusSrcAlpha| rgb vcol, a tex.r*vcol.a*cameraDepthFade       |
//! | S_Particle_Trail_01 (emission part)     | SrcAlpha OneMinusSrcAlpha| rgb vcol*boost*vcol.a, a vcol.a, ZWrite On     |
//! | S_DoubleSided_Emissive_00 (emission)    | SrcAlpha OneMinusSrcAlpha| rgb _Color0*boost, a _Opacity, ZWrite On       |
//! | Particles/Additive (builtin 200)        | SrcAlpha One             | 2*vcol*_TintColor*tex (+ soft particles)       |
//! | Particles/Alpha Blended (203), WFX AB   | SrcAlpha OneMinusSrcAlpha| 2*vcol*_TintColor*tex (+ soft particles)       |
//! | Mobile/Particles/Additive (10720)       | SrcAlpha One             | tex*vcol                                       |
//! | Mobile/Particles/Multiply (10723)       | Zero SrcColor            | lerp(1, tex*vcol, tex.a*vcol.a)                |
//! | Particles/VertexLit Blended (208)       | SrcAlpha OneMinusSrcAlpha| tex*sat(vcol*ambient+_EmisColor), a vcol.a     |
//! | WFX/Additive Alpha8                     | One One                  | 2*vcol*tint*(2*tex.a*vcol.a)                   |
//! | WFX/Additive (Soft) Alpha8              | OneMinusDstColor One     | 2*vcol*tint*tex.a                              |
//! | WFX/Multiply Alpha8                     | Zero SrcColor            | tex.a*vcol                                     |
//! | WFX/Multiply Soft Tint (+Scroll)        | DstColor SrcColor        | lerp(0.5, tex*vcol*tint, tex.a*vcol.a)         |
//! | WFX/Scroll/Smoke                        | DstColor SrcAlpha        | lerp(0.5, (tex*vcol*tint, mask), mask)         |
//!
//! FXVille blood is its own mode (PMODE_BLOOD, recovered from DXBC). Lit shaders stay on `StandardMaterial`: S_Surfaces debris, Standard gibs,
//! S_Particle_02_Albedo (lit Standard, alpha = tex.r), WFX Transparent Diffuse/Specular bullet holes, Easy Decal
//! multiply; decals always use `StandardMaterial` (their alpha fades per instance).
//!
//! Colour spaces (project is Linear): material colours are linearised; textures are sampled sRGB or linear as
//! their importer says (`sRGBTexture` in the ripped .meta); vertex colours are linearised only when the
//! renderer has `m_ApplyActiveColorSpace` (see `sim.rs`).

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::EntityCommands;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Extent3d, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError, TextureDimension, TextureFormat,
};
use bevy::shader::ShaderRef;
use serde_json::Value;
use std::collections::HashMap;


/// Camera near clip plane (Player prefab cameras: 0.01), used by the Amplify camera depth fade.
const NEAR_CLIP: f32 = 0.01;

/// `glstate.lightmodel.ambient` stand-in for Particles/VertexLit Blended (no per-vertex lights are applied).
const VERTEXLIT_AMBIENT: f32 = 0.4;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Family {
    Additive,
    Blend,
    Multiply,
    Opaque,
    Blood,
}

#[derive(Clone)]
pub enum MatHandle {
    Std(Handle<StandardMaterial>),
    Particle(Handle<ParticleMat>),
}

#[derive(Clone)]
pub struct MatInfo {
    pub handle: MatHandle,
    pub family: Family,
    /// Atlas frames baked into the texture mapping (`_Columns`/`_Rows` of blood flipbooks).
    pub flipbook: (i32, i32),
    /// FXVille Blood vertex-shader material values the CPU applies (UV flip, screen-space fall).
    pub blood: Option<BloodVs>,
}

/// FXVille Blood 2018 vertex program inputs (DXBC blob 3): `_FlipU` / `_FlipV` (per-particle random UV mirror),
/// `_FallOffset` / `_FallRandomness` (clip-space fall of TEXCOORD1.z).
#[derive(Clone, Copy, Debug)]
pub struct BloodVs {
    pub flip: Vec2,
    pub fall_offset: f32,
    pub fall_random: f32,
}

impl MatInfo {
    /// Inserts the `MeshMaterial3d` for this material.
    pub fn insert(&self, ec: &mut EntityCommands) {
        match &self.handle {
            MatHandle::Std(h) => ec.insert(MeshMaterial3d(h.clone())),
            MatHandle::Particle(h) => ec.insert(MeshMaterial3d(h.clone())),
        };
    }
}

// ---------------------------------------------------------------- ParticleMat

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PMode {
    Amp00,
    Amp03,
    Amp04,
    DepthFade,
    Trail01,
    /// S_Particle_Trail_00: lit Standard (albedo = vcol, smoothness 0) + emission vcol * boost, a = vcol.a.
    Trail00,
    /// Particles/Standard Unlit (builtin 211): tex * _Color * vcol (_ColorMode multiply), blend from _Mode.
    StdUnlit,
    ConstEmissive,
    Legacy,
    Mobile,
    MobileMul,
    WfxAdd8,
    WfxAddSoft8,
    WfxMul8,
    WfxMulSoft,
    WfxScrollMul,
    WfxScrollSmoke,
    Blood,
    /// S_DistortionRefraction_00: GrabPass refraction (opaque write, rendered after the transparent pass).
    Distortion,
}

/// Unity `Blend` factors of the shader's forward pass.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PBlend {
    /// SrcAlpha OneMinusSrcAlpha
    Alpha,
    /// SrcAlpha One
    AddAlpha,
    /// One One
    AddOne,
    /// OneMinusDstColor One
    SoftAdd,
    /// Zero SrcColor
    Mul,
    /// DstColor SrcColor
    Mul2x,
    /// DstColor SrcAlpha
    Smoke,
    /// SrcAlpha One, BlendOp RevSub (Particles/Standard Unlit `_Mode` 5 Subtractive: dst - src * a)
    RevSub,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ParticleKey {
    pub mode: PMode,
    pub blend: PBlend,
    pub soft: bool,
    pub zwrite: bool,
}

#[derive(ShaderType, Clone, Debug)]
pub struct ParticleParams {
    pub tint: Vec4,
    pub st: Vec4,
    pub p0: Vec4,
    pub p1: Vec4,
    pub cm: Vec4,
    /// blood: _WarpTex_ST
    pub st2: Vec4,
    /// blood: _NoiseTex_ST
    pub st3: Vec4,
    /// blood: (_WarpStr, _NoiseAlphaStr, _NoiseColorStr, _Randomize)
    pub p2: Vec4,
    /// blood: _ChannelMask2
    pub cm2: Vec4,
    /// blood: _SpecularColor (linear rgb, a = specular weight)
    pub spec: Vec4,
    /// blood: _ReflectionTex_ST
    pub st4: Vec4,
    /// blood: (_ReflectionSat, _FlattenNormal, _LightStr, 0)
    pub p3: Vec4,
    /// blood: the vertex light factor 1 + _LightStr * (max(ShadeSH9(n), 0.15 * ambient) - 1) (rgb; set per emitter by
    /// sim.rs when _LightStr != 0, else 1).
    pub light: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
#[bind_group_data(ParticleKey)]
pub struct ParticleMat {
    #[uniform(0)]
    pub params: ParticleParams,
    #[texture(1)]
    #[sampler(2)]
    pub texture: Option<Handle<Image>>,
    /// blood: _WarpTex
    #[texture(3)]
    #[sampler(4)]
    pub texture2: Option<Handle<Image>>,
    /// blood: _NoiseTex
    #[texture(5)]
    #[sampler(6)]
    pub texture3: Option<Handle<Image>>,
    /// blood: _ReflectionTex
    #[texture(7)]
    #[sampler(8)]
    pub texture4: Option<Handle<Image>>,
    /// blood: _Normal
    #[texture(9)]
    #[sampler(10)]
    pub texture5: Option<Handle<Image>>,
    pub key: ParticleKey,
    /// The shader has a SOFTPARTICLES_ON variant; `key.soft` = this && QualitySettings.softParticles (Ultra only).
    pub soft_variant: bool,
}

/// QualitySettings.softParticles follows the quality level at once (SetQualityLevel), also for live materials.
pub fn sync_soft_particles(settings: Res<crate::settings::Settings>, mut mats: ResMut<Assets<ParticleMat>>, mut last: Local<Option<bool>>) {
    let soft_q = settings.quality_setting >= 3;
    if *last == Some(soft_q) {
        return;
    }
    let first = last.is_none();
    *last = Some(soft_q);
    if first {
        return;
    }
    let ids: Vec<_> = mats.iter().filter(|(_, m)| m.soft_variant && m.key.soft != soft_q).map(|(id, _)| id).collect();
    for id in ids {
        if let Some(mut m) = mats.get_mut(id) {
            m.key.soft = soft_q;
        }
    }
}

impl From<&ParticleMat> for ParticleKey {
    fn from(m: &ParticleMat) -> Self {
        m.key
    }
}

impl Material for ParticleMat {
    fn fragment_shader() -> ShaderRef {
        "shaders/particle.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        if self.key.mode == PMode::Distortion {
            // Blend One Zero, ZWrite On: Opaque + reads_view_transmission_texture -> Transmissive3d phase, which
            // `distortion.rs` renders after the transparent pass (Unity queue 3000/3001 GrabPass).
            AlphaMode::Opaque
        } else {
            AlphaMode::Blend
        }
    }
    fn reads_view_transmission_texture(&self) -> bool {
        self.key.mode == PMode::Distortion
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(_: &MaterialPipeline, descriptor: &mut RenderPipelineDescriptor, _: &MeshVertexBufferLayoutRef, key: MaterialPipelineKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        let k = key.bind_group_data;
        let op = if k.blend == PBlend::RevSub { BlendOperation::ReverseSubtract } else { BlendOperation::Add };
        let c = |src: BlendFactor, dst: BlendFactor| BlendState {
            color: BlendComponent { src_factor: src, dst_factor: dst, operation: op },
            alpha: BlendComponent { src_factor: src, dst_factor: dst, operation: op },
        };
        use BlendFactor as F;
        let blend = match k.blend {
            PBlend::Alpha => c(F::SrcAlpha, F::OneMinusSrcAlpha),
            PBlend::AddAlpha => c(F::SrcAlpha, F::One),
            PBlend::AddOne => c(F::One, F::One),
            PBlend::SoftAdd => c(F::OneMinusDst, F::One),
            PBlend::Mul => c(F::Zero, F::Src),
            PBlend::Mul2x => c(F::Dst, F::Src),
            PBlend::Smoke => c(F::Dst, F::SrcAlpha),
            PBlend::RevSub => c(F::SrcAlpha, F::One),
        };
        let mode = match k.mode {
            PMode::Amp00 => "PMODE_AMP00",
            PMode::Amp03 => "PMODE_AMP03",
            PMode::Amp04 => "PMODE_AMP04",
            PMode::DepthFade => "PMODE_DEPTHFADE",
            PMode::Trail01 => "PMODE_TRAIL01",
            PMode::Trail00 => "PMODE_TRAIL00",
            PMode::StdUnlit => "PMODE_STD_UNLIT",
            PMode::ConstEmissive => "PMODE_CONST_EMISSIVE",
            PMode::Legacy => "PMODE_LEGACY",
            PMode::Mobile => "PMODE_MOBILE",
            PMode::MobileMul => "PMODE_MOBILE_MUL",
            PMode::WfxAdd8 => "PMODE_WFX_ADD8",
            PMode::WfxAddSoft8 => "PMODE_WFX_ADDSOFT8",
            PMode::WfxMul8 => "PMODE_WFX_MUL8",
            PMode::WfxMulSoft | PMode::WfxScrollMul | PMode::WfxScrollSmoke => "PMODE_WFX_MULSOFT",
            PMode::Blood => "PMODE_BLOOD",
            PMode::Distortion => "PMODE_DISTORTION",
        };
        if let Some(frag) = descriptor.fragment.as_mut() {
            frag.shader_defs.push(mode.into());
            if matches!(k.mode, PMode::WfxScrollMul | PMode::WfxScrollSmoke) {
                frag.shader_defs.push("SCROLL".into());
            }
            if k.mode == PMode::WfxScrollSmoke {
                frag.shader_defs.push("SMOKE".into());
            }
            if k.soft {
                frag.shader_defs.push("SOFT".into());
            }
            for t in frag.targets.iter_mut().flatten() {
                t.blend = if k.mode == PMode::Distortion { None } else { Some(blend) };
            }
        }
        descriptor.primitive.cull_mode = None;
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_write_enabled = Some(k.zwrite);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- library

#[derive(Default)]
pub struct MatLib {
    cache: HashMap<String, Option<MatInfo>>,
    images: HashMap<String, Handle<Image>>,
    soft: Option<Handle<Image>>,
    srgb_tex: HashMap<String, bool>,
}

fn arr4(v: Option<&Value>, d: Vec4) -> Vec4 {
    match v.and_then(|v| v.as_array()) {
        Some(a) if a.len() == 4 => Vec4::new(a[0].as_f64().unwrap_or(0.0) as f32, a[1].as_f64().unwrap_or(0.0) as f32, a[2].as_f64().unwrap_or(0.0) as f32, a[3].as_f64().unwrap_or(0.0) as f32),
        _ => d,
    }
}

/// Unity colour property -> linear (rgb converted, alpha kept).
fn lin(c: Vec4) -> Vec4 {
    Vec4::new(srgb(c.x), srgb(c.y), srgb(c.z), c.w)
}

fn pick_tex(tex: &serde_json::Map<String, Value>) -> Option<(String, Vec4)> {
    let st = |t: &Value| {
        let s = t["scale"].as_array().map(|a| (a[0].as_f64().unwrap_or(1.0) as f32, a[1].as_f64().unwrap_or(1.0) as f32)).unwrap_or((1.0, 1.0));
        let o = t["offset"].as_array().map(|a| (a[0].as_f64().unwrap_or(0.0) as f32, a[1].as_f64().unwrap_or(0.0) as f32)).unwrap_or((0.0, 0.0));
        Vec4::new(s.0, s.1, o.0, o.1)
    };
    for k in ["_Emissive", "_MainTex", "_Opacity", "_TextureSample0", "_Albedo", "_BaseMap", "_Tex"] {
        if let Some(t) = tex.get(k) {
            if let Some(p) = t["path"].as_str() {
                return Some((p.to_string(), st(t)));
            }
        }
    }
    tex.iter()
        .filter(|(k, _)| !["Normal", "Noise", "Warp", "Reflection", "Bump"].iter().any(|bad| k.contains(bad)))
        .find_map(|(_, t)| t["path"].as_str().map(|p| (p.to_string(), st(t))))
}

/// CPU texture conversions.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TexOp {
    None,
    /// Blood: channel `ch` of the mask -> alpha (white colour) with the shader's alpha clip.
    Blood(usize, f32, f32),
    /// Alpha := red channel (S_Particle_02_Albedo opacity on StandardMaterial).
    RedToAlpha,
    /// Unchanged, sampled with the importer's wrap mode (`wrapU` 0 = Repeat).
    Wrap,
}

impl MatLib {
    fn soft_circle(&mut self, images: &mut Assets<Image>) -> Handle<Image> {
        if let Some(h) = &self.soft {
            return h.clone();
        }
        let n = 64usize;
        let mut data = Vec::with_capacity(n * n * 4);
        for y in 0..n {
            for x in 0..n {
                let (dx, dy) = ((x as f32 + 0.5) / n as f32 * 2.0 - 1.0, (y as f32 + 0.5) / n as f32 * 2.0 - 1.0);
                let a = (1.0 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
                let a = a * a * (3.0 - 2.0 * a);
                data.extend_from_slice(&[255, 255, 255, (a * 255.0) as u8]);
            }
        }
        let h = images.add(Image::new(Extent3d { width: n as u32, height: n as u32, depth_or_array_layers: 1 }, TextureDimension::D2, data, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default()));
        self.soft = Some(h.clone());
        h
    }

    /// Texture colour space (the importer's sRGBTexture, stored as the Texture2D's m_ColorSpace; default sRGB).
    fn is_srgb(&mut self, file: &str) -> bool {
        if let Some(v) = self.srgb_tex.get(file) {
            return *v;
        }
        let v = crate::owned::Owned::global().and_then(|o| o.texture_header(file)).map(|t| t.color_space != 0).unwrap_or(true);
        self.srgb_tex.insert(file.to_string(), v);
        v
    }

    /// Decode `assets/vfx/tex/<file>` (Unity path `Texture2D/<file>`).
    fn image(&mut self, path: &str, op: TexOp, images: &mut Assets<Image>) -> Option<Handle<Image>> {
        let key = format!("{path}|{op:?}");
        if let Some(h) = self.images.get(&key) {
            return Some(h.clone());
        }
        let name = path.rsplit('/').next()?;
        let file = format!("assets/vfx/tex/{name}");
        // A file under assets/vfx/tex if there is one, else the Texture2D read in place from the installation.
        let (w, h, mut data, owned) = match std::fs::read(&file) {
            Ok(bytes) => {
                let img = Image::from_buffer(&bytes, ImageType::Extension("png"), CompressedImageFormats::NONE, true, ImageSampler::Default, RenderAssetUsages::default()).ok()?;
                let rgba = img.try_into_dynamic().ok()?.to_rgba8();
                (rgba.width(), rgba.height(), rgba.into_raw(), None)
            }
            Err(_) => {
                let stem = name.rsplit_once('.').map(|(a, _)| a).unwrap_or(name);
                let (w, h, d, t) = crate::owned::Owned::global()?.rgba_texture(stem).map_err(|e| if !name.starts_with("guid:") { warn!("vfx texture {name}: {e}") }).ok()?;
                (w, h, d, Some(t))
            }
        };
        let mut srgb_fmt = owned.as_ref().map(|t| t.color_space == 1).unwrap_or_else(|| self.is_srgb(name));
        match op {
            TexOp::None => {}
            TexOp::Blood(ch, amin, soft) => {
                for px in data.chunks_exact_mut(4) {
                    let m = px[ch.min(3)] as f32 / 255.0;
                    let a = ((m - amin) / soft.max(0.05)).clamp(0.0, 1.0);
                    let a = a * a * (3.0 - 2.0 * a);
                    px[0] = 255;
                    px[1] = 255;
                    px[2] = 255;
                    px[3] = (a * 255.0) as u8;
                }
                srgb_fmt = true;
            }
            TexOp::Wrap => {}
            TexOp::RedToAlpha => {
                for px in data.chunks_exact_mut(4) {
                    // the shader reads .r after the sRGB decode; alpha is never decoded, so store decoded red
                    let r = if srgb_fmt { srgb(px[0] as f32 / 255.0) } else { px[0] as f32 / 255.0 };
                    px[3] = (r * 255.0).round() as u8;
                }
            }
        }
        let fmt = if srgb_fmt { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm };
        let mut out = Image::new(Extent3d { width: w, height: h, depth_or_array_layers: 1 }, TextureDimension::D2, data, fmt, RenderAssetUsages::default());
        out.sampler = ImageSampler::linear();
        let wrap_repeat = match &owned {
            Some(t) => t.wrap.0 == 0,
            None => crate::owned::Owned::global().and_then(|o| o.texture_header(name)).is_some_and(|t| t.wrap.0 == 0),
        };
        if op == TexOp::Wrap && wrap_repeat {
            out.sampler = ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
                address_mode_u: bevy::image::ImageAddressMode::Repeat,
                address_mode_v: bevy::image::ImageAddressMode::Repeat,
                ..bevy::image::ImageSamplerDescriptor::linear()
            });
        }
        let handle = images.add(out);
        // QualitySettings.masterTextureLimit (Low) halves mipmapped textures (68 / 72 VFX textures; texquality.rs).
        let mipmapped = match &owned {
            Some(t) => t.mip_count > 1,
            None => crate::owned::Owned::global().and_then(|o| o.texture_header(name)).map(|t| t.mip_count > 1).unwrap_or(true),
        };
        if mipmapped {
            crate::texquality::mark(&handle);
        }
        self.images.insert(key, handle.clone());
        Some(handle)
    }

    /// Material for a `m_Materials` entry (`asset:Material/X.mat` or `asset-guid:...:<builtin id>`).
    pub fn get(&mut self, key: &str, mats: &mut Assets<StandardMaterial>, pmats: &mut Assets<ParticleMat>, images: &mut Assets<Image>) -> Option<MatInfo> {
        if let Some(m) = self.cache.get(key) {
            return m.clone();
        }
        let m = self.build(key, false, mats, pmats, images);
        self.cache.insert(key.to_string(), m.clone());
        m
    }

    /// `StandardMaterial` variant (decals: per-instance alpha fade).
    pub fn get_std(&mut self, key: &str, mats: &mut Assets<StandardMaterial>, pmats: &mut Assets<ParticleMat>, images: &mut Assets<Image>) -> Option<MatInfo> {
        let ck = format!("std|{key}");
        if let Some(m) = self.cache.get(&ck) {
            return m.clone();
        }
        let m = self.build(key, true, mats, pmats, images);
        self.cache.insert(ck, m.clone());
        m
    }

    fn build(&mut self, key: &str, force_std: bool, mats: &mut Assets<StandardMaterial>, pmats: &mut Assets<ParticleMat>, images: &mut Assets<Image>) -> Option<MatInfo> {
        let (name, json): (String, Option<Value>) = match key.strip_prefix("asset:").filter(|k| k.ends_with(".mat")) {
            Some(n) => {
                let stem = n.rsplit('/').next().unwrap_or(n).trim_end_matches(".mat");
                (
                    stem.to_string(),
                    // the exported file, else the Material read from the installation
                    std::fs::read_to_string(format!("assets/data/vfx/mat/{stem}.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).or_else(|| crate::owned::Owned::global()?.material_json(stem)),
                )
            }
            None => (key.to_string(), None),
        };
        let json = json.unwrap_or(Value::Null);
        let shader = json["shader"].as_str().unwrap_or(key).to_string();
        let floats = json["floats"].as_object().cloned().unwrap_or_default();
        let fl = |k: &str, d: f32| floats.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d);
        let colors = json["colors"].as_object().cloned().unwrap_or_default();
        let tex = json["tex"].as_object().cloned().unwrap_or_default();
        let lname = name.to_lowercase();
        let builtin = |id: &str| shader.contains(&format!("builtin:{id}")) || key.ends_with(&format!(":{id}"));
        let sh = |s: &str| shader.contains(s);
        if sh("Distortion") && !sh("S_DistortionRefraction_00") {
            return None;
        }
        // Debug: RESTRAFE_VFX_SKIP=<name substr>[,..] hides matching particle materials.
        if std::env::var("RESTRAFE_VFX_SKIP").is_ok_and(|v| v.split(',').any(|x| !x.is_empty() && name.contains(x))) {
            return None;
        }
        let tex_path = pick_tex(&tex);
        let tint = lin(arr4(colors.get("_TintColor"), Vec4::splat(0.5)));
        // Soft particles: QualitySettings Ultra (the default level 3) has softParticles on.
        let soft_q = crate::settings::Settings::load().quality_setting >= 3;

        // ---- recovered particle shaders ----
        // (mode, blend, zwrite, has SOFTPARTICLES variant)
        let pm: Option<(PMode, PBlend, bool, bool)> = if force_std {
            None
        } else if sh("S_DistortionRefraction_00") {
            Some((PMode::Distortion, PBlend::Alpha, true, false))
        } else if sh("FXVille Blood") {
            Some((PMode::Blood, PBlend::Alpha, false, false))
        } else if sh("S_ParticleAdditive_00") {
            Some((PMode::Amp00, PBlend::AddAlpha, true, false))
        } else if sh("S_Particle_00") {
            Some((PMode::Amp00, PBlend::Alpha, false, false))
        } else if sh("S_Particle_03") {
            Some((PMode::Amp03, PBlend::Alpha, false, false))
        } else if sh("S_Particle_04") {
            Some((PMode::Amp04, PBlend::Alpha, false, false))
        } else if sh("S_Particle_depthfade") {
            Some((PMode::DepthFade, PBlend::Alpha, false, false))
        } else if sh("S_Particle_Trail_01") {
            Some((PMode::Trail01, PBlend::Alpha, true, false))
        } else if sh("S_Particle_Trail_00") {
            Some((PMode::Trail00, PBlend::Alpha, true, false))
        } else if builtin("211") {
            // Particles/Standard Unlit: StandardParticlesShaderGUI blend presets per _Mode (2 Fade, 3 Transparent
            // (premultiplied: approximated as Fade), 4 Additive, 5 Subtractive).
            let blend = match fl("_Mode", 2.0) as i32 {
                4 => PBlend::AddAlpha,
                5 if std::env::var_os("RESTRAFE_DEBUG_REVSUB_ALPHA").is_none() => PBlend::RevSub,
                _ => PBlend::Alpha,
            };
            Some((PMode::StdUnlit, blend, fl("_ZWrite", 0.0) > 0.5, false))
        } else if sh("S_DoubleSided_Emissive_00") {
            Some((PMode::ConstEmissive, PBlend::Alpha, true, false))
        } else if sh("WFX_Scroll_Smoke") {
            Some((PMode::WfxScrollSmoke, PBlend::Smoke, false, false))
        } else if sh("WFX_Scroll_Multiply Soft Tint") {
            Some((PMode::WfxScrollMul, PBlend::Mul2x, false, false))
        } else if sh("WFX_Multiply Soft Tint") {
            Some((PMode::WfxMulSoft, PBlend::Mul2x, false, false))
        } else if sh("WFX_Multiply Alpha8") {
            Some((PMode::WfxMul8, PBlend::Mul, false, true))
        } else if sh("WFX_Additive (Soft) Alpha8") {
            Some((PMode::WfxAddSoft8, PBlend::SoftAdd, false, true))
        } else if sh("WFX_Additive Alpha8") {
            Some((PMode::WfxAdd8, PBlend::AddOne, false, true))
        } else if sh("WFX_Alpha Blended") {
            Some((PMode::Legacy, PBlend::Alpha, false, true))
        } else if builtin("200") {
            Some((PMode::Legacy, PBlend::AddAlpha, false, true))
        } else if builtin("203") {
            Some((PMode::Legacy, PBlend::Alpha, false, true))
        } else if builtin("10720") {
            Some((PMode::Mobile, PBlend::AddAlpha, false, false))
        } else if builtin("10721") {
            Some((PMode::Mobile, PBlend::Alpha, false, false))
        } else if builtin("10723") {
            Some((PMode::MobileMul, PBlend::Mul, false, false))
        } else if builtin("208") {
            // VertexLit Blended: tex * saturate(vcol * ambient + _EmisColor); the vertex term is folded into the
            // Mobile formula via the tint (applied on the CPU side: vcol already multiplied in the shader).
            Some((PMode::Mobile, PBlend::Alpha, false, false))
        } else {
            None
        };
        if let Some((mode, blend, zwrite, soft_variant)) = pm {
            let blood = mode == PMode::Blood;
            let (texture, st) = match &tex_path {
                Some((p, st)) => (self.image(p, if blood { TexOp::Wrap } else { TexOp::None }, images), *st),
                None => (None, Vec4::new(1.0, 1.0, 0.0, 0.0)),
            };
            // FXVille blood: _WarpTex / _NoiseTex (+ _ST), repeat-wrapped like their importers.
            let mut side = |k: &str| -> (Option<Handle<Image>>, Vec4) {
                let Some(t) = tex.get(k).filter(|_| blood) else { return (None, Vec4::new(1.0, 1.0, 0.0, 0.0)) };
                let s = t["scale"].as_array().map(|a| (a[0].as_f64().unwrap_or(1.0) as f32, a[1].as_f64().unwrap_or(1.0) as f32)).unwrap_or((1.0, 1.0));
                let o = t["offset"].as_array().map(|a| (a[0].as_f64().unwrap_or(0.0) as f32, a[1].as_f64().unwrap_or(0.0) as f32)).unwrap_or((0.0, 0.0));
                (t["path"].as_str().and_then(|p| self.image(p, TexOp::Wrap, images)), Vec4::new(s.0, s.1, o.0, o.1))
            };
            let (texture2, st2) = side("_WarpTex");
            let (texture3, st3) = side("_NoiseTex");
            let (texture4, st4) = side("_ReflectionTex");
            let (texture5, _) = side("_Normal");
            let boost = fl("_EmissiveBoost", fl("_EmisiveBoost", 1.0));
            let ptint = match mode {
                PMode::ConstEmissive => lin(arr4(colors.get("_Color0"), Vec4::ONE)),
                PMode::Blood => lin(arr4(colors.get("_BaseColor"), Vec4::ONE)),
                PMode::StdUnlit => lin(arr4(colors.get("_Color"), Vec4::ONE)),
                PMode::Mobile if builtin("208") => {
                    let e = lin(arr4(colors.get("_EmisColor"), Vec4::new(0.2, 0.2, 0.2, 0.0)));
                    Vec4::new(e.x, e.y, e.z, VERTEXLIT_AMBIENT)
                }
                _ => tint,
            };
            let family = match blend {
                _ if mode == PMode::Distortion => Family::Opaque,
                PBlend::AddAlpha | PBlend::AddOne | PBlend::SoftAdd => Family::Additive,
                PBlend::Mul | PBlend::Mul2x | PBlend::Smoke => Family::Multiply,
                PBlend::Alpha if mode == PMode::Blood => Family::Blood,
                PBlend::Alpha | PBlend::RevSub => Family::Blend,
            };
            let (columns, rows) = (fl("_Columns", 1.0).max(1.0), fl("_Rows", 1.0).max(1.0));
            let handle = pmats.add(ParticleMat {
                params: ParticleParams {
                    tint: ptint,
                    st,
                    p0: if mode == PMode::Distortion {
                        // (_Float0 = Opacity, _ChromaticAberration, _Float1 = refraction index), cb0[5].xyz
                        Vec4::new(fl("_Float0", 0.0), fl("_ChromaticAberration", 0.1), fl("_Float1", 1.0), 0.0)
                    } else if blood {
                        Vec4::new(fl("_MaskStr", 0.7), fl("_ProcMask", 1.0), fl("_AlphaMin", 0.1), fl("_AlphaSoft", 0.1).max(1e-4))
                    } else {
                        Vec4::new(boost, fl("_ScrollSpeed", 2.0), fl("_InvFade", 1.0), fl("_Opacity", 1.0))
                    },
                    p1: if blood {
                        Vec4::new(fl("_EdgeDarken", 1.0), columns, rows, 0.0)
                    } else {
                        Vec4::new(fl("_Float1", 1.0).max(1e-4), fl("_Float2", 0.0), NEAR_CLIP, if builtin("208") { 1.0 } else { 0.0 })
                    },
                    cm: arr4(colors.get("_ChannelMask"), Vec4::new(1.0, 0.0, 0.0, 0.0)),
                    st2,
                    st3,
                    // Without a _WarpTex / _NoiseTex the fallback white texture would bias the warp: zero its strength.
                    p2: Vec4::new(if texture2.is_some() { fl("_WarpStr", 0.0) } else { 0.0 }, if texture3.is_some() { fl("_NoiseAlphaStr", 0.0) } else { 0.0 }, if texture3.is_some() { fl("_NoiseColorStr", 0.0) } else { 0.0 }, fl("_Randomize", 0.0)),
                    cm2: arr4(colors.get("_ChannelMask2"), Vec4::new(1.0, 0.0, 0.0, 0.0)),
                    // Without a _ReflectionTex the specular term is off (weight 0 keeps the fresnel at 1).
                    spec: if texture4.is_some() { lin(arr4(colors.get("_SpecularColor"), Vec4::ZERO)) } else { Vec4::ZERO },
                    st4,
                    p3: Vec4::new(fl("_ReflectionSat", 1.0), fl("_FlattenNormal", 0.0), fl("_LightStr", 0.0), 0.0),
                    light: Vec4::ONE,
                },
                texture,
                texture2,
                texture3,
                texture4,
                texture5,
                key: ParticleKey { mode, blend, soft: soft_variant && soft_q, zwrite },
                soft_variant,
            });
            let blood_vs = blood.then(|| BloodVs { flip: Vec2::new(fl("_FlipU", 0.0), fl("_FlipV", 0.0)), fall_offset: fl("_FallOffset", 0.0), fall_random: fl("_FallRandomness", 0.0) });
            return Some(MatInfo { handle: MatHandle::Particle(handle), family, flipbook: if blood { (columns as i32, rows as i32) } else { (1, 1) }, blood: blood_vs });
        }

        // ---- StandardMaterial (lit / decal / approximated) ----
        let family = if sh("Blood") {
            Family::Blood
        } else if sh("Multiply") || builtin("10723") {
            Family::Multiply
        } else if sh("S_Surfaces") || builtin("46") {
            Family::Opaque
        } else if sh("Additive") || builtin("200") || builtin("10720") || (sh("S_Particle") && !sh("Albedo") && !sh("_00") && !sh("_03") && !sh("_04")) {
            Family::Additive
        } else if lname.contains("add") && !sh("Shader/") {
            Family::Additive
        } else {
            Family::Blend
        };
        let lit = sh("WFX_Transparent") || sh("S_Particle_02_Albedo") || builtin("46") || sh("S_Surfaces");
        let (columns, rows) = (fl("_Columns", 1.0).max(1.0) as i32, fl("_Rows", 1.0).max(1.0) as i32);
        let image = match (family, &tex_path) {
            (Family::Blood, Some((p, _))) => {
                let cm = arr4(colors.get("_ChannelMask"), Vec4::new(1.0, 0.0, 0.0, 0.0));
                let ch = if cm.y > 0.5 { 1 } else if cm.z > 0.5 { 2 } else if cm.w > 0.5 { 3 } else { 0 };
                let amin = fl("_AlphaMin", 0.1);
                self.image(p, TexOp::Blood(ch, amin, (fl("_AlphaSoft", 0.1) + 0.2).min(0.6)), images)
            }
            (_, Some((p, _))) if sh("S_Particle_02_Albedo") => self.image(p, TexOp::RedToAlpha, images),
            (_, Some((p, _))) => self.image(p, TexOp::None, images),
            (Family::Blend, None) | (Family::Blood, None) if !lit => Some(self.soft_circle(images)),
            _ => None,
        };
        let legacy = sh("WFX_Alpha Blended") || builtin("200") || builtin("203");
        let color = match family {
            Family::Blood => {
                let b = arr4(colors.get("_BaseColor"), Vec4::ONE);
                Color::linear_rgba(srgb(b.x), srgb(b.y), srgb(b.z), 1.0)
            }
            _ if legacy => Color::linear_rgba(tint.x * 2.0, tint.y * 2.0, tint.z * 2.0, (tint.w * 2.0).min(1.0)),
            _ if sh("WFX_Transparent") => {
                let t = lin(arr4(colors.get("_TintColor"), Vec4::ONE));
                Color::linear_rgba(t.x, t.y, t.z, t.w)
            }
            Family::Multiply => {
                let c = lin(arr4(colors.get("_Tint"), Vec4::ONE));
                Color::linear_rgba(c.x, c.y, c.z, c.w)
            }
            Family::Additive => {
                let c = lin(arr4(colors.get("_Color"), Vec4::ONE));
                Color::linear_rgba(c.x, c.y, c.z, 1.0)
            }
            Family::Blend => {
                let c = lin(arr4(colors.get("_Tint"), arr4(colors.get("_Color"), Vec4::ONE)));
                Color::linear_rgba(c.x, c.y, c.z, c.w.clamp(0.0, 1.0))
            }
            Family::Opaque => {
                let c = lin(arr4(colors.get("_Color"), Vec4::ONE));
                Color::linear_rgba(c.x, c.y, c.z, 1.0)
            }
        };
        let handle = mats.add(StandardMaterial {
            base_color: color,
            base_color_texture: image,
            unlit: !lit,
            perceptual_roughness: if sh("WFX_Transparent Specular") { 0.6 } else { 1.0 },
            reflectance: if lit && !sh("Specular") { 0.0 } else { 0.5 },
            double_sided: true,
            cull_mode: None,
            fog_enabled: false,
            alpha_mode: match family {
                Family::Additive => AlphaMode::Add,
                Family::Blend | Family::Blood => AlphaMode::Blend,
                Family::Multiply => AlphaMode::Multiply,
                Family::Opaque => AlphaMode::Opaque,
            },
            ..default()
        });
        Some(MatInfo { handle: MatHandle::Std(handle), family, flipbook: if family == Family::Blood { (columns, rows) } else { (1, 1) }, blood: None })
    }
}

/// sRGB -> linear (project colour space is Linear).
pub fn srgb(c: f32) -> f32 {
    let c = c.max(0.0);
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}
