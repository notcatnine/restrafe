//! Unity asset loaders for the UGUI importer (`ugui.rs`) and the material layer: guid tokens of objects in the
//! installation (owned_docs.rs), Texture2D (+ import settings, CPU mip chain, normal maps unpacked), Sprite (rect,
//! 9-slice border, pixels per unit, incl. Unity's built-in UI sprites), Mesh (vertex/index buffers, Z-mirrored to
//! Bevy) and Material (-> `StandardMaterial`). Everything is read in place from the installation.

use crate::ugui_scene::{f, file_ref, guid_ref, int, rgba};
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor, TextureFormatPixelInfo};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use serde_yaml::{Mapping, Value};
use std::collections::HashMap;

pub const BUILTIN_EXTRA: &str = "0000000000000000f000000000000000";

/// Built-in UI sprites (`unity_builtin_extra`) by local file id.
pub fn builtin_sprite_name(file_id: i64) -> Option<&'static str> {
    Some(match file_id {
        10901 => "Checkmark",
        10905 => "UISprite",
        10907 => "Background",
        10911 => "InputFieldBackground",
        10913 => "Knob",
        10915 => "DropdownArrow",
        10917 => "UIMask",
        _ => return None,
    })
}

#[derive(Clone)]
pub struct SpriteInfo {
    pub texture: Handle<Image>,
    pub tex_size: Vec2,
    /// Sprite rect in texture pixels (origin bottom-left).
    pub rect: Rect,
    /// Border in pixels: x left, y bottom, z right, w top.
    pub border: Vec4,
    pub ppu: f32,
    /// Texture wrapMode (U) is Repeat: Image Tiled without border then tiles with UVs > 1 (`GenerateTiledSprite`).
    pub repeat: bool,
    /// `DataUtility.GetPadding`: transparent margin between the sprite rect and its tight texture rect (`rect` above),
    /// in pixels: x left, y bottom, z right, w top.
    pub padding: Vec4,
}

impl SpriteInfo {
    /// `sprite.rect.size` (the texture rect plus its padding).
    pub fn full_size(&self) -> Vec2 {
        self.rect.size() + Vec2::new(self.padding.x + self.padding.z, self.padding.y + self.padding.w)
    }

    /// Outer UV (Unity convention, v up).
    pub fn outer_uv(&self) -> Rect {
        Rect::from_corners(self.rect.min / self.tex_size, self.rect.max / self.tex_size)
    }
}

/// Loaded Unity assets, by GUID.
#[derive(Resource, Default)]
pub struct UnityAssets {
    by_guid: HashMap<String, String>,
    textures: HashMap<String, Option<(Handle<Image>, Vec2)>>,
    sprites: HashMap<(String, i64), Option<SpriteInfo>>,
    meshes: HashMap<String, Option<Vec<Handle<Mesh>>>>,
    materials: HashMap<String, Option<UMat>>,
    pub white: Option<Handle<Image>>,
    /// Custom-shader materials (canvas scale / screen height updated every frame).
    pub fx_mats: Vec<Handle<UnityFxMaterial>>,
}

/// A Unity material converted to a Bevy material.
#[derive(Clone, Debug)]
pub enum UMat {
    Std(Handle<StandardMaterial>),
    Fx(Handle<UnityFxMaterial>),
}

/// Uniforms of `ugui_fx.wgsl`.
#[derive(bevy::render::render_resource::ShaderType, Clone, Copy, Debug, Default, Reflect)]
pub struct UnityFxParams {
    /// x shader (0 S_Emissive_Mask_00, 1 S_TVScanlines_00), y canvas scale factor, z screen height / 2 (pixels).
    pub kind: Vec4,
    pub color_a: Vec4,
    pub color_b: Vec4,
    pub p0: Vec4,
    pub p1: Vec4,
    /// _TextureSample0_ST
    pub st: Vec4,
}

/// Emission-only port of the menu shaders S_Emissive_Mask_00 / S_TVScanlines_00 (see `ugui_fx.wgsl`).
#[derive(Asset, bevy::render::render_resource::AsBindGroup, Reflect, Debug, Clone)]
pub struct UnityFxMaterial {
    #[uniform(0)]
    pub p: UnityFxParams,
    #[texture(1)]
    #[sampler(2)]
    pub tex: Handle<Image>,
}

impl Material for UnityFxMaterial {
    fn fragment_shader() -> bevy::shader::ShaderRef {
        "shaders/ugui_fx.wgsl".into()
    }
}

fn yaml_inner(text: &str) -> Option<Mapping> {
    // First document of a Unity asset file.
    let body = text.split("\n--- !u!").nth(1)?;
    let body = body.split_once('\n')?.1;
    match serde_yaml::from_str::<Value>(body).ok()? {
        Value::Mapping(m) => m.into_iter().next().and_then(|(_, v)| match v {
            Value::Mapping(m) => Some(m),
            _ => None,
        }),
        _ => None,
    }
}

/// A normal-map Texture2D of the installation as an RGB-normal image (linear, its wrap, mipmapped when it has mips).
pub fn normal_image(n: usize, p: i64) -> Option<Image> {
    let (w, h, rgba) = normal_rgba(n, p)?;
    let t = crate::owned::Owned::global()?.texture_header_at(n, p)?;
    let mips = t.mip_count > 1;
    let mut img = Image::new(Extent3d { width: w, height: h, depth_or_array_layers: 1 }, TextureDimension::D2, rgba, TextureFormat::Rgba8Unorm, if mips { RenderAssetUsages::default() } else { RenderAssetUsages::RENDER_WORLD });
    let mode = if t.wrap.0 == 1 { ImageAddressMode::ClampToEdge } else { ImageAddressMode::Repeat };
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor { address_mode_u: mode, address_mode_v: mode, mag_filter: ImageFilterMode::Linear, min_filter: ImageFilterMode::Linear, mipmap_filter: ImageFilterMode::Linear, ..default() });
    if mips {
        with_mips(&mut img);
    }
    Some(img)
}

/// RGBA8 RGB-normal pixels of a normal-map texture (see `UnityAssets::normal_texture`), unpacked like Unity's
/// UnpackNormal (UnpackNormalmapRGorAG): x = r * a, y = g, so the DXT5nm layout of desktop normal maps (x in alpha,
/// r = 1, any format) and plain RG / RGB maps (a = 1) both decode; z = sqrt(1 - x^2 - y^2).
fn normal_rgba(n: usize, p: i64) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h, mut rgba, _) = crate::owned::Owned::global()?.rgba_texture_at(n, p).ok()?;
    for px in rgba.chunks_exact_mut(4) {
        let x = px[0] as f32 * px[3] as f32 / 255.0;
        let y = px[1] as f32;
        let (nx, ny) = (x / 255.0 * 2.0 - 1.0, y / 255.0 * 2.0 - 1.0);
        let nz = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
        px.copy_from_slice(&[x.round() as u8, px[1], ((nz * 0.5 + 0.5) * 255.0).round() as u8, 255]);
    }
    Some((w, h, rgba))
}

fn rect_of(v: Option<&Value>) -> Rect {
    let g = |k: &str| f(v.and_then(|v| v.get(k)));
    Rect::new(g("x"), g("y"), g("x") + g("width"), g("y") + g("height"))
}

/// Box-filtered RGBA8 mip chain (sRGB data is averaged as stored, close enough for UI/menu textures).
fn with_mips(img: &mut Image) {
    let (mut w, mut h) = (img.width() as usize, img.height() as usize);
    let Some(data) = img.data.as_ref() else { return };
    if img.texture_descriptor.format.pixel_size().ok() != Some(4) || w == 0 || h == 0 {
        return;
    }
    let mut all = data.clone();
    let mut prev = data.clone();
    let mut levels = 1;
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0u8; nw * nh * 4];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let mut s = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(w - 1);
                        let sy = (y * 2 + dy).min(h - 1);
                        s += prev[(sy * w + sx) * 4 + c] as u32;
                    }
                    next[(y * nw + x) * 4 + c] = ((s + 2) / 4) as u8;
                }
            }
        }
        all.extend_from_slice(&next);
        prev = next;
        w = nw;
        h = nh;
        levels += 1;
    }
    img.data = Some(all);
    img.texture_descriptor.mip_level_count = levels;
}

impl UnityAssets {
    /// The asset path the old exports gave a guid token's object ("Material/<name>.mat", "Shader/<name>.shader" ...).
    pub fn path(&mut self, guid: &str) -> Option<String> {
        if let Some(p) = self.by_guid.get(guid) {
            return Some(p.clone());
        }
        let p = crate::owned_docs::path(guid)?;
        self.by_guid.insert(guid.to_string(), p.clone());
        Some(p)
    }

    /// An asset's document (Material, Sprite, RenderTexture ...) by guid token.
    pub fn doc(&mut self, guid: &str) -> Option<Mapping> {
        crate::owned_docs::asset(guid).map(|(_, m)| (*m).clone())
    }

    /// A texture's header (colour space, filter, wrap, mips) by guid token.
    fn tex_header(&mut self, guid: &str) -> Option<crate::unity_fs::Texture2D> {
        let (n, p) = crate::owned_docs::parse_token(guid)?;
        crate::owned::Owned::global()?.texture_header_at(n, p)
    }

    /// A Texture2D of the installation as a UI image with its import settings (sRGB = colour space, point filter,
    /// clamp wrap, mipmaps).
    fn texture_at(&mut self, node: usize, pid: i64, images: &mut Assets<Image>) -> Option<(Handle<Image>, Vec2)> {
        let key = format!("u:{node}:{pid}");
        if let Some(t) = self.textures.get(&key) {
            return t.clone();
        }
        let out = (|| {
            let (w, h, rgba, t) = crate::owned::Owned::global()?.rgba_texture_at(node, pid).map_err(|e| warn!("ugui: {e}")).ok()?;
            let (srgb, point, clamp, mips) = (t.color_space != 0, t.filter == 0, t.wrap.0 == 1, t.mip_count > 1);
            let mode = if clamp { ImageAddressMode::ClampToEdge } else { ImageAddressMode::Repeat };
            let filt = if point { ImageFilterMode::Nearest } else { ImageFilterMode::Linear };
            let usage = if mips { RenderAssetUsages::default() } else { RenderAssetUsages::RENDER_WORLD };
            let mut img = Image::new(Extent3d { width: w, height: h, depth_or_array_layers: 1 }, TextureDimension::D2, rgba, if srgb { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm }, usage);
            img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor { address_mode_u: mode, address_mode_v: mode, mag_filter: filt, min_filter: filt, mipmap_filter: filt, ..default() });
            if mips {
                with_mips(&mut img);
            }
            let h_img = images.add(img);
            if mips {
                crate::texquality::mark(&h_img);
            }
            Some((h_img, Vec2::new(w as f32, h as f32)))
        })();
        if out.is_none() {
            warn!("ugui: texture {key} failed to load");
        }
        self.textures.insert(key, out.clone());
        out
    }

    pub fn white(&mut self, images: &mut Assets<Image>) -> Handle<Image> {
        if let Some(h) = &self.white {
            return h.clone();
        }
        let img = Image::new_fill(Extent3d { width: 1, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, &[255, 255, 255, 255], TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD);
        let h = images.add(img);
        self.white = Some(h.clone());
        h
    }

    /// A normal map as RGB normals (what the old exports' PNGs held): Unity stores DXT5 normal maps as DXT5nm
    /// (x in alpha, y in green) and BC5 ones as x / y in red / green; z is reconstructed (sqrt(1 - x^2 - y^2)).
    pub fn normal_texture(&mut self, guid: &str, images: &mut Assets<Image>) -> Option<Handle<Image>> {
        let key = format!("normal|{guid}");
        if let Some(t) = self.textures.get(&key) {
            return t.clone().map(|t| t.0);
        }
        let out = (|| {
            let (n, p) = crate::owned_docs::parse_token(guid)?;
            let img = normal_image(n, p)?;
            let (size, mips) = (Vec2::new(img.width() as f32, img.height() as f32), img.texture_descriptor.mip_level_count > 1);
            let hd = images.add(img);
            if mips {
                crate::texquality::mark(&hd);
            }
            Some((hd, size))
        })();
        self.textures.insert(key, out.clone());
        out.map(|t| t.0)
    }

    pub fn texture(&mut self, guid: &str, images: &mut Assets<Image>) -> Option<(Handle<Image>, Vec2)> {
        let (n, p) = crate::owned_docs::parse_token(guid)?;
        self.texture_at(n, p, images)
    }

    /// A Sprite reference (`{fileID, guid}`).
    pub fn sprite(&mut self, guid: &str, file_id: i64, images: &mut Assets<Image>) -> Option<SpriteInfo> {
        let key = (guid.to_string(), file_id);
        if let Some(s) = self.sprites.get(&key) {
            return s.clone();
        }
        let mut s = if guid == BUILTIN_EXTRA { self.builtin_sprite(file_id, images) } else { self.asset_sprite(guid, images) };
        if let Some(sp) = s.as_mut() {
            sp.repeat = images.get(&sp.texture).is_some_and(|i| match &i.sampler {
                ImageSampler::Descriptor(d) => d.address_mode_u == ImageAddressMode::Repeat,
                _ => false,
            });
        }
        if s.is_none() {
            warn!("ugui: sprite {guid}:{file_id} not found");
        }
        self.sprites.insert(key, s.clone());
        s
    }

    fn builtin_sprite(&mut self, file_id: i64, images: &mut Assets<Image>) -> Option<SpriteInfo> {
        // Unity's built-in sprite of that id (UISprite, Background, ...): in the installation's unity_builtin_extra, or
        // the copy the build made of it (by name)
        let name = builtin_sprite_name(file_id)?;
        let o = crate::owned::Owned::global()?;
        let extra = o.builtin_extra_node()?;
        let (node, m) = match crate::owned_docs::asset_at(extra, file_id).filter(|(c, _)| *c == 213) {
            Some((_, m)) => (extra, m),
            None => {
                let (n, p) = o.find_named(213, name)?;
                (n, crate::owned_docs::asset_at(n, p)?.1)
            }
        };
        let rd = m.get("m_RD");
        let tex = rd.and_then(|r| r.get("texture"));
        // the texture: a token, or an id in the same file
        let (tn, tp) = guid_ref(tex).and_then(|g| crate::owned_docs::parse_token(&g)).unwrap_or((node, file_ref(tex)));
        let (texture, tex_size) = self.texture_at(tn, tp, images)?;
        // Built-in sprites are full-texture sprites; m_RD.textureRect is the authoritative rect.
        let tr = rect_of(rd.and_then(|r| r.get("textureRect")));
        let rect = if tr.width() > 0.0 { tr } else { Rect::new(0.0, 0.0, tex_size.x, tex_size.y) };
        let b = m.get("m_Border");
        let bv = |k: &str| f(b.and_then(|b| b.get(k)));
        Some(SpriteInfo {
            texture,
            tex_size,
            rect,
            border: Vec4::new(bv("x"), bv("y"), bv("z"), bv("w")),
            ppu: m.get("m_PixelsToUnits").and_then(Value::as_f64).unwrap_or(100.0) as f32,
            repeat: false,
            padding: Vec4::ZERO,
        })
    }

    fn asset_sprite(&mut self, guid: &str, images: &mut Assets<Image>) -> Option<SpriteInfo> {
        let m = self.doc(guid)?;
        let rd = m.get("m_RD");
        let tex_guid = guid_ref(rd.and_then(|r| r.get("texture")))?;
        let (texture, tex_size) = self.texture(&tex_guid, images)?;
        let tr = rect_of(rd.and_then(|r| r.get("textureRect")));
        let rect = if tr.width() > 0.0 { tr } else { rect_of(m.get("m_Rect")) };
        let b = m.get("m_Border");
        let bv = |k: &str| f(b.and_then(|b| b.get(k)));
        // A Single-mode sprite (texture importer spriteMode 1) covers its whole texture, but with the Tight mesh type
        // its texture rect is the tight bounds of the opaque pixels. AssetRipper writes that tight rect as m_Rect too
        // (and textureRectOffset 0); the build has m_Rect = the full texture and textureRectOffset = the tight rect's
        // origin (T_Jumpsuit_Icon_00: rect 0,0,512,512, textureRect 152,3,200x509, read with UnityPy). Image then
        // draws the tight rect inside the padded rect (DataUtility.GetPadding), not stretched over it.
        let full = Rect::new(0.0, 0.0, tex_size.x, tex_size.y);
        // single-mode sprites: the build's m_Rect is the whole texture
        let single = rect_of(m.get("m_Rect")) == full;
        let padding = if single && rect != full && rect.min.cmpge(Vec2::ZERO).all() && rect.max.cmple(tex_size + 0.01).all() {
            Vec4::new(rect.min.x, rect.min.y, tex_size.x - rect.max.x, tex_size.y - rect.max.y)
        } else {
            Vec4::ZERO
        };
        // The build's border already includes the padding (T_Item_Frame_00: 22 / 16 / 26 / 30; the old exports wrote it
        // relative to the tight rect: 2.99 / 1.92 / 1.99 / 1.93 + padding 19.01 / 14.08 / 24.01 / 28.07).
        let border = Vec4::new(bv("x"), bv("y"), bv("z"), bv("w"));
        Some(SpriteInfo { texture, tex_size, rect, border, ppu: f(m.get("m_PixelsToUnits")).max(0.0001), repeat: false, padding })
    }

    /// `Mesh/*.asset` -> one Bevy mesh per sub-mesh (Unity left-handed -> Bevy: z mirrored, winding flipped,
    /// UV v flipped).
    pub fn mesh(&mut self, guid: &str, meshes: &mut Assets<Mesh>) -> Option<Vec<Handle<Mesh>>> {
        if let Some(m) = self.meshes.get(guid) {
            return m.clone();
        }
        let out = crate::owned_docs::parse_token(guid)
            .and_then(|(n, p)| crate::owned::Owned::global()?.mesh_at(n, p))
            .map(|m| meshes_from_unity(&m).into_iter().map(|m| meshes.add(m)).collect::<Vec<_>>());
        if out.is_none() {
            warn!("ugui: mesh {guid} failed to decode");
        }
        self.meshes.insert(guid.to_string(), out.clone());
        out
    }

    /// `.mat` -> StandardMaterial (main texture + colour, smoothness, metallic, emission; unlit shaders unlit), or
    /// [`UnityFxMaterial`] for the menu shaders ported from their compiled D3D11 bytecode.
    pub fn material(&mut self, guid: &str, images: &mut Assets<Image>, mats: &mut Assets<StandardMaterial>, fx: &mut Assets<UnityFxMaterial>) -> Option<UMat> {
        if let Some(m) = self.materials.get(guid) {
            return m.clone();
        }
        let out = match self.build_fx(guid, images) {
            Some(f) => {
                let h = fx.add(f);
                self.fx_mats.push(h.clone());
                Some(UMat::Fx(h))
            }
            None => self.build_material(guid, images).map(|m| UMat::Std(mats.add(m))),
        };
        self.materials.insert(guid.to_string(), out.clone());
        out
    }

    /// S_Emissive_Mask_00 / S_TVScanlines_00 (Amplify surface shaders: albedo 0, specular 0.04, smoothness 0,
    /// emission as below; the specular term is negligible and dropped).
    fn build_fx(&mut self, guid: &str, images: &mut Assets<Image>) -> Option<UnityFxMaterial> {
        let m = self.doc(guid)?;
        let shader = guid_ref(m.get("m_Shader")).and_then(|g| self.path(&g)).unwrap_or_default();
        let kind = if shader.ends_with("S_Emissive_Mask_00.shader") {
            0.0
        } else if shader.ends_with("S_TVScanlines_00.shader") {
            1.0
        } else {
            return None;
        };
        let props = m.get("m_SavedProperties");
        let fl = |k: &str| props.and_then(|p| p.get("m_Floats")).and_then(|t| t.get(k)).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        // Colour properties: AssetRipper writes them as `Vector` in the .shader text, but the build's serialized shader
        // (ripped_gltf/Assets/Shader/<shader>.json m_PropInfo) has m_Type 0 = Color, m_Flags 0 for _MaskedColor /
        // _LightColor (S_Emissive_Mask_00) and _Color0 / _Color1 (S_TVScanlines_00): Unity converts them gamma -> linear
        // in this Linear project. RESTRAFE_FX_RAW_COLORS=1 = old raw behaviour (comparisons).
        let raw = std::env::var_os("RESTRAFE_FX_RAW_COLORS").is_some();
        let v = |k: &str| {
            let c = props.and_then(|p| p.get("m_Colors")).and_then(|t| t.get(k)).map(|c| rgba(Some(c))).unwrap_or([0.0; 4]);
            if raw { Vec4::from_array(c) } else { crate::tmp_text::linear_color(c) }
        };
        let texenv = props.and_then(|p| p.get("m_TexEnvs")).and_then(|t| t.get("_TextureSample0"));
        let st = texenv.map(|t| {
            let s = t.get("m_Scale");
            let o = t.get("m_Offset");
            Vec4::new(f(s.and_then(|s| s.get("x"))), f(s.and_then(|s| s.get("y"))), f(o.and_then(|s| s.get("x"))), f(o.and_then(|s| s.get("y"))))
        });
        let tex = texenv.and_then(|t| guid_ref(t.get("m_Texture"))).and_then(|g| self.texture(&g, images)).map(|t| t.0);
        let white = self.white(images);
        let p = if kind == 0.0 {
            UnityFxParams { kind: Vec4::new(0.0, 1.0, 540.0, 0.0), color_a: v("_MaskedColor"), color_b: v("_LightColor"), p0: Vec4::new(fl("_Power"), fl("_EmissiveBoost"), 0.0, 0.0), p1: Vec4::ZERO, st: st.unwrap_or(Vec4::new(1.0, 1.0, 0.0, 0.0)) }
        } else {
            UnityFxParams {
                kind: Vec4::new(1.0, 1.0, 540.0, 0.0),
                color_a: v("_Color0"),
                color_b: v("_Color1"),
                p0: Vec4::new(fl("_lines1_number"), fl("_wavelength"), fl("_time"), fl("_Float0")),
                p1: Vec4::new(fl("_lines2_number"), fl("_wavelength2"), fl("_time2"), fl("_Float1")),
                st: Vec4::new(1.0, 1.0, 0.0, 0.0),
            }
        };
        Some(UnityFxMaterial { p, tex: tex.unwrap_or(white) })
    }

    /// Unity's built-in Default-Material (Standard: white, smoothness 0.5, metallic 0).
    pub fn default_material(&mut self, mats: &mut Assets<StandardMaterial>) -> UMat {
        if let Some(Some(m)) = self.materials.get("builtin:Default-Material") {
            return m.clone();
        }
        let m = UMat::Std(mats.add(StandardMaterial { base_color: Color::WHITE, perceptual_roughness: 0.5, metallic: 0.0, reflectance: 0.5, ..default() }));
        self.materials.insert("builtin:Default-Material".to_string(), Some(m.clone()));
        m
    }

    /// A material by its asset name (`Material/<name>.mat`), e.g. a glTF material name exported by AssetRipper.
    pub fn material_by_name(&mut self, name: &str, images: &mut Assets<Image>, mats: &mut Assets<StandardMaterial>, fx: &mut Assets<UnityFxMaterial>) -> Option<UMat> {
        let (n, p) = crate::owned::Owned::global()?.find_named(21, name)?;
        let guid = crate::owned_docs::token(n, p);
        self.material(&guid, images, mats, fx)
    }

    /// Shader file name (e.g. "S_Surfaces_00.shader") of `Material/<name>.mat`.
    pub fn material_shader(&mut self, name: &str) -> Option<String> {
        let (n, p) = crate::owned::Owned::global()?.find_named(21, name)?;
        let m = self.doc(&crate::owned_docs::token(n, p))?;
        let g = guid_ref(m.get("m_Shader"))?;
        self.path(&g).map(|p| p.rsplit('/').next().unwrap_or("").to_string())
    }

    /// Decoded RGBA8 pixels of a texture (no colour-space conversion), for channel packing.
    fn pixels(&mut self, guid: &str) -> Option<(Vec<u8>, u32, u32)> {
        let (n, p) = crate::owned_docs::parse_token(guid)?;
        let (w, h, rgba, _) = crate::owned::Owned::global()?.rgba_texture_at(n, p).ok()?;
        Some((rgba, w, h))
    }

    /// Repeat-wrapped, trilinear, mipmapped texture from packed RGBA8 pixels.
    fn packed_texture(data: Vec<u8>, w: u32, h: u32, srgb: bool, images: &mut Assets<Image>) -> Handle<Image> {
        let fmt = if srgb { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm };
        // Main-world copy kept: texquality.rs re-uploads it with a clamped sampler on the Low quality level.
        let mut img = Image::new(Extent3d { width: w, height: h, depth_or_array_layers: 1 }, TextureDimension::D2, data, fmt, RenderAssetUsages::default());
        img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            mag_filter: ImageFilterMode::Linear,
            min_filter: ImageFilterMode::Linear,
            mipmap_filter: ImageFilterMode::Linear,
            anisotropy_clamp: 1,
            ..default()
        });
        with_mips(&mut img);
        let h = images.add(img);
        crate::texquality::mark(&h);
        h
    }

    /// Channel `c` of texture `guid` resampled (nearest) to `w` x `h`, or None.
    /// Values of an sRGB-imported texture are decoded to linear (as the GPU sampler would in Unity).
    /// Bevy metallic/roughness texture (G = perceptual roughness) from a Unity "roughness" map read as smoothness =
    /// 1 - tex.r, i.e. perceptual roughness = tex.r (sRGB-decoded per the importer).
    fn roughness_r_texture(&mut self, guid: Option<String>, images: &mut Assets<Image>) -> Option<Handle<Image>> {
        let (r, w, h) = self.channel(guid, 0, None)?;
        let mut d = vec![0u8; (w * h * 4) as usize];
        for (i, v) in r.into_iter().enumerate() {
            d[i * 4 + 1] = v;
            d[i * 4 + 3] = 255;
        }
        Some(Self::packed_texture(d, w, h, false, images))
    }

    fn channel(&mut self, guid: Option<String>, c: usize, size: Option<(u32, u32)>) -> Option<(Vec<u8>, u32, u32)> {
        let guid = guid?;
        let (d, w, h) = self.pixels(&guid)?;
        let srgb = self.tex_header(&guid).is_some_and(|t| t.color_space != 0);
        let lut: Vec<u8> = (0..256).map(|v| if srgb { (crate::tmp_text::gamma_to_linear(v as f32 / 255.0) * 255.0).round() as u8 } else { v as u8 }).collect();
        let (tw, th) = size.unwrap_or((w, h));
        let mut out = vec![0u8; (tw * th) as usize];
        for y in 0..th {
            for x in 0..tw {
                let (sx, sy) = (x * w / tw, y * h / th);
                out[(y * tw + x) as usize] = lut[d[((sy * w + sx) * 4) as usize + c] as usize];
            }
        }
        Some((out, tw, th))
    }

    /// Amplify surface shaders S_Surfaces_00 / S_Surfaces_00_NoFog (Standard metallic lighting), recovered from the
    /// compiled deferred/forward pixel shaders (all three keywords on):
    ///   Albedo = _TextureSample0 (uv_texcoord, no _ST), Normal = UnpackNormal(_Normal) (_USENORMAL_ON),
    ///   Metallic = 1 - _roughness.r (_USEROUGHNESS_ON, else _Metallic), Smoothness = _MEtallic.r (_USEMETALLIC_ON,
    ///   else _Smoothness). (The graph wires the "roughness" map into Metallic and the "MEtallic" map into
    ///   Smoothness.) AssetRipper's glTF export instead used the first texture (_MEtallic) as base colour.
    fn build_surfaces(&mut self, m: &Mapping, images: &mut Assets<Image>) -> StandardMaterial {
        let props = m.get("m_SavedProperties");
        let tex = |k: &str| props.and_then(|p| p.get("m_TexEnvs")).and_then(|t| t.get(k)).and_then(|t| guid_ref(t.get("m_Texture")));
        let fl = |k: &str| props.and_then(|p| p.get("m_Floats")).and_then(|t| t.get(k)).and_then(Value::as_f64).map(|v| v as f32);
        let kw: Vec<String> = m.get("m_ValidKeywords").and_then(Value::as_sequence).map(|s| s.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        let on = |k: &str| kw.iter().any(|x| x == k);
        let base = tex("_TextureSample0").and_then(|g| self.texture(&g, images)).map(|t| t.0);
        let normal = if on("_USENORMAL_ON") && std::env::var_os("RESTRAFE_SURF_NONORMAL").is_none() { tex("_Normal").and_then(|g| self.normal_texture(&g, images)) } else { None };
        let metal_tex = if on("_USEROUGHNESS_ON") { self.channel(tex("_roughness"), 0, None) } else { None };
        let size = metal_tex.as_ref().map(|t| (t.1, t.2));
        let smooth_tex = if on("_USEMETALLIC_ON") { self.channel(tex("_MEtallic"), 0, size) } else { None };
        let metallic = fl("_Metallic").unwrap_or(0.0).clamp(0.0, 1.0);
        let smoothness = fl("_Smoothness").unwrap_or(0.0).clamp(0.0, 1.0);
        let mut mat = StandardMaterial { base_color_texture: base, normal_map_texture: normal, reflectance: 0.5, ..default() };
        if (metal_tex.is_some() || smooth_tex.is_some()) && std::env::var_os("RESTRAFE_SURF_NOMR").is_none() {
            let (w, h) = size.or(smooth_tex.as_ref().map(|t| (t.1, t.2))).unwrap_or((1, 1));
            let mut data = vec![255u8; (w * h * 4) as usize];
            for i in 0..(w * h) as usize {
                // Bevy: G = perceptual roughness (1 - smoothness), B = metallic.
                let s = smooth_tex.as_ref().map(|t| t.0[i]).unwrap_or((smoothness * 255.0).round() as u8);
                let mt = metal_tex.as_ref().map(|t| 255 - t.0[i]).unwrap_or((metallic * 255.0).round() as u8);
                data[i * 4] = 0;
                data[i * 4 + 1] = 255 - s;
                data[i * 4 + 2] = mt;
            }
            mat.metallic_roughness_texture = Some(Self::packed_texture(data, w, h, false, images));
            mat.metallic = 1.0;
            mat.perceptual_roughness = 1.0;
        } else {
            mat.metallic = metallic;
            mat.perceptual_roughness = 1.0 - smoothness;
        }
        mat
    }

    /// Amplify S_TransparentWindows_00 (Standard specular-less dielectric, alpha "fade": Blend SrcAlpha
    /// OneMinusSrcAlpha, ZWrite off), recovered from its compiled FORWARDBASE pixel shader:
    ///   Albedo = _BC, Smoothness = 1 - _Roughness.r, Metallic 0, Alpha = _Opacity.r.
    /// Amplify S_Windows_00 (alpha blend "fade", Standard metallic), recovered from the forward-base DXBC:
    ///   o = _Opacity(uv * _Opacity_ST).r;  Albedo = _Color;  Metallic 0;
    ///   Smoothness = saturate(_Smoothness * o' + _AddSmoothness)   (o' = 1 - o with _INVERTSMOOTHNESS_ON)
    ///   Alpha      = saturate(o'' * _OpacityMult)                   (o'' = 1 - o with _INVERTOPACITY_ON)
    ///   Normal     = UnpackNormal(_Normal) with _USENORMAL_ON, else the vertex normal.
    fn build_glass(&mut self, p: &Props, images: &mut Assets<Image>) -> Option<StandardMaterial> {
        let c = p.color("_Color", [1.0, 1.0, 1.0, 0.0]);
        let (inv_o, inv_s) = (p.on("_INVERTOPACITY_ON"), p.on("_INVERTSMOOTHNESS_ON"));
        let (sm, add, om) = (p.fl("_Smoothness", 1.0), p.fl("_AddSmoothness", 0.0), p.fl("_OpacityMult", 0.0));
        // Unassigned _Opacity = Unity default "white" (o = 1).
        let (op, w, h) = self.channel(p.tex("_Opacity"), 0, None).unwrap_or((vec![255], 1, 1));
        let mut bc = vec![255u8; (w * h * 4) as usize];
        let mut mr = vec![255u8; (w * h * 4) as usize];
        for (i, v) in op.into_iter().enumerate() {
            let o = v as f32 / 255.0;
            let oo = if inv_o { 1.0 - o } else { o };
            let os = if inv_s { 1.0 - o } else { o };
            bc[i * 4 + 3] = ((oo * om).clamp(0.0, 1.0) * 255.0).round() as u8;
            let smooth = (sm * os + add).clamp(0.0, 1.0);
            mr[i * 4] = 0;
            mr[i * 4 + 1] = ((1.0 - smooth) * 255.0).round() as u8;
            mr[i * 4 + 2] = 0;
        }
        let normal = if p.on("_USENORMAL_ON") { p.tex("_Normal").and_then(|g| self.normal_texture(&g, images)) } else { None };
        Some(StandardMaterial {
            base_color: Color::LinearRgba(LinearRgba::new(c.x, c.y, c.z, 1.0)),
            base_color_texture: Some(Self::packed_texture(bc, w, h, false, images)),
            metallic_roughness_texture: Some(Self::packed_texture(mr, w, h, false, images)),
            normal_map_texture: normal,
            metallic: 0.0,
            perceptual_roughness: 1.0,
            reflectance: 0.5,
            uv_transform: st_affine(p.st("_Opacity")),
            alpha_mode: AlphaMode::Blend,
            ..default()
        })
    }

    fn build_windows(&mut self, m: &Mapping, images: &mut Assets<Image>) -> Option<StandardMaterial> {
        let props = m.get("m_SavedProperties");
        let tex = |k: &str| props.and_then(|p| p.get("m_TexEnvs")).and_then(|t| t.get(k)).and_then(|t| guid_ref(t.get("m_Texture")));
        let (mut bc, w, h) = self.pixels(&tex("_BC")?)?;
        if let Some((op, _, _)) = self.channel(tex("_Opacity"), 0, Some((w, h))) {
            for (i, a) in op.into_iter().enumerate() {
                bc[i * 4 + 3] = a;
            }
        }
        let rough = self.channel(tex("_Roughness"), 0, Some((w, h))).map(|(r, _, _)| {
            let mut d = vec![0u8; (w * h * 4) as usize];
            for (i, v) in r.into_iter().enumerate() {
                d[i * 4 + 1] = v;
                d[i * 4 + 3] = 255;
            }
            Self::packed_texture(d, w, h, false, images)
        });
        Some(StandardMaterial {
            base_color_texture: Some(Self::packed_texture(bc, w, h, true, images)),
            metallic_roughness_texture: rough,
            metallic: 0.0,
            perceptual_roughness: 1.0,
            reflectance: 0.5,
            alpha_mode: AlphaMode::Blend,
            ..default()
        })
    }

    fn build_material(&mut self, guid: &str, images: &mut Assets<Image>) -> Option<StandardMaterial> {
        let p = self.path(guid)?;
        let m = self.doc(guid)?;
        let shader = guid_ref(m.get("m_Shader")).and_then(|g| self.path(&g)).unwrap_or_default();
        match shader.rsplit('/').next().unwrap_or("") {
            "S_Surfaces_00.shader" | "S_Surfaces_00_NoFog.shader" => return Some(self.build_surfaces(&m, images)),
            "S_TransparentWindows_00.shader" => {
                if let Some(w) = self.build_windows(&m, images) {
                    return Some(w);
                }
            }
            // Weapon shader family: the StandardMaterial part of the port (no AO / emission terms).
            "S_WeaponOutline_00.shader" | "S_WeaponOutline_00_Special.shader" | "S_WeaponOutline_01_Emissive.shader" | "S_EnviroSurface00.shader" => {
                if let Some(s) = self.surface(&m, images) {
                    return Some(s.std);
                }
            }
            _ => {}
        }
        self.build_std(&m, &p, &shader, images)
    }

    /// Unity Standard (and approximated unknown) shaders -> StandardMaterial.
    fn build_std(&mut self, m: &Mapping, p: &str, shader: &str, images: &mut Assets<Image>) -> Option<StandardMaterial> {
        let props = m.get("m_SavedProperties");
        let tex = |k: &str| props.and_then(|p| p.get("m_TexEnvs")).and_then(|t| t.get(k)).and_then(|t| guid_ref(t.get("m_Texture")));
        let fl = |k: &str| props.and_then(|p| p.get("m_Floats")).and_then(|t| t.get(k)).and_then(Value::as_f64).map(|v| v as f32);
        let col = |k: &str| props.and_then(|p| p.get("m_Colors")).and_then(|t| t.get(k)).map(|c| rgba(Some(c)));
        let keywords: Vec<String> = m.get("m_ValidKeywords").and_then(Value::as_sequence).map(|s| s.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        let main = ["_MainTex", "_BaseMap", "_TextureSample0", "_Albedo", "_BaseColorMap"].iter().find_map(|k| tex(k));
        let c = col("_Color").or(col("_BaseColor")).unwrap_or([1.0; 4]);
        let mut mat = StandardMaterial {
            base_color: Color::srgba(c[0], c[1], c[2], c[3]),
            base_color_texture: main.and_then(|g| self.texture(&g, images)).map(|t| t.0),
            perceptual_roughness: 1.0 - fl("_Smoothness").or(fl("_Glossiness")).unwrap_or(0.5),
            metallic: fl("_Metallic").unwrap_or(0.0),
            unlit: shader.to_lowercase().contains("unlit"),
            ..default()
        };
        if keywords.iter().any(|k| k == "_EMISSION") {
            if let Some(e) = col("_EmissionColor") {
                mat.emissive = LinearRgba::rgb(crate::tmp_text::gamma_to_linear(e[0]), crate::tmp_text::gamma_to_linear(e[1]), crate::tmp_text::gamma_to_linear(e[2]));
            }
            mat.emissive_texture = tex("_EmissionMap").and_then(|g| self.texture(&g, images)).map(|t| t.0);
        }
        if fl("_Mode").unwrap_or(0.0) >= 2.0 {
            mat.alpha_mode = AlphaMode::Blend;
        }
        let known = ["S_Surfaces_00", "Standard", "Unlit"].iter().any(|s| shader.contains(s));
        if !known {
            info!("ugui: material {p} uses shader {shader}; approximated as Standard");
        }
        Some(mat)
    }
}

/// A Unity material ported for a 3D renderer: Standard lighting part + the `unity_surface.wgsl` extension
/// (see `unity_pbr.rs`).
pub struct SurfaceMat {
    pub std: StandardMaterial,
    pub ext: crate::unity_pbr::UnityExtParams,
    pub ext_tex: Option<Handle<Image>>,
}

impl SurfaceMat {
    fn plain(std: StandardMaterial) -> Self {
        SurfaceMat { std, ext: default(), ext_tex: None }
    }
}

/// Read access to a material's `m_SavedProperties` (missing values fall back to the shader defaults given).
struct Props<'a>(Option<&'a Value>, Vec<String>);

impl Props<'_> {
    fn new(m: &Mapping) -> Props<'_> {
        let kw = m.get("m_ValidKeywords").and_then(Value::as_sequence).map(|s| s.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        Props(m.get("m_SavedProperties"), kw)
    }
    fn on(&self, k: &str) -> bool {
        self.1.iter().any(|x| x == k)
    }
    fn tex(&self, k: &str) -> Option<String> {
        self.0.and_then(|p| p.get("m_TexEnvs")).and_then(|t| t.get(k)).and_then(|t| guid_ref(t.get("m_Texture")))
    }
    /// `_ST` of a texture slot (scale xy, offset zw).
    fn st(&self, k: &str) -> Vec4 {
        let t = self.0.and_then(|p| p.get("m_TexEnvs")).and_then(|t| t.get(k));
        let g = |a: &str, b: &str| t.and_then(|t| t.get(a)).and_then(|v| v.get(b)).and_then(Value::as_f64).map(|v| v as f32);
        Vec4::new(g("m_Scale", "x").unwrap_or(1.0), g("m_Scale", "y").unwrap_or(1.0), g("m_Offset", "x").unwrap_or(0.0), g("m_Offset", "y").unwrap_or(0.0))
    }
    fn fl(&self, k: &str, default: f32) -> f32 {
        self.0.and_then(|p| p.get("m_Floats")).and_then(|t| t.get(k)).and_then(Value::as_f64).map(|v| v as f32).unwrap_or(default)
    }
    /// A Color property as the shader sees it (gamma -> linear, Linear colour space project; HDR values too).
    fn color(&self, k: &str, default: [f32; 4]) -> Vec4 {
        let c = self.0.and_then(|p| p.get("m_Colors")).and_then(|t| t.get(k)).map(|c| rgba(Some(c))).unwrap_or(default);
        crate::tmp_text::linear_color(c)
    }
}

/// Unity `_ST` (Unity uv space, v up) -> Bevy `uv_transform` for AssetRipper glTF uvs (v down).
fn st_affine(st: Vec4) -> bevy::math::Affine2 {
    bevy::math::Affine2::from_scale_angle_translation(Vec2::new(st.x, st.y), 0.0, Vec2::new(st.z, 1.0 - st.y - st.w))
}

impl UnityAssets {
    /// `Material/<name>.mat` converted for a mesh renderer, or None when its shader has no port (callers keep the
    /// glTF material then).
    pub fn surface_material(&mut self, name: &str, images: &mut Assets<Image>) -> Option<SurfaceMat> {
        let (n, p) = crate::owned::Owned::global()?.find_named(21, name)?;
        let m = self.doc(&crate::owned_docs::token(n, p))?;
        let s = self.surface(&m, images);
        if s.is_none() {
            let sh = guid_ref(m.get("m_Shader")).and_then(|g| self.path(&g)).unwrap_or_else(|| format!("builtin {}", file_id(m.get("m_Shader"))));
            debug!("unity material {name}: shader {sh} has no port, keeping the glTF material");
        }
        s
    }

    fn surface(&mut self, m: &Mapping, images: &mut Assets<Image>) -> Option<SurfaceMat> {
        let shader_guid = guid_ref(m.get("m_Shader"));
        if shader_guid.as_deref() == Some(BUILTIN_EXTRA) {
            // unity_builtin_extra: 46 = Standard.
            return match file_id(m.get("m_Shader")) {
                46 => self.build_std(m, "", "Standard", images).map(SurfaceMat::plain),
                // Unlit/Texture (DF_Cyst "Lights"): colour = _MainTex, opaque, no lighting.
                10752 => self.build_std(m, "", "Unlit/Texture", images).map(|mut s| {
                    s.base_color = Color::WHITE; // Unlit/Texture has no _Color
                    SurfaceMat::plain(s)
                }),
                // Particles/Standard Unlit (DF_Blister "Screen"): _MainTex * _Color * vertex colour, _Mode 0 opaque.
                211 => self.build_std(m, "", "Particles/Standard Unlit", images).map(SurfaceMat::plain),
                _ => None,
            };
        }
        let shader = shader_guid.and_then(|g| self.path(&g)).unwrap_or_default();
        let p = Props::new(m);
        match shader.rsplit('/').next().unwrap_or("") {
            "S_WeaponOutline_00.shader" => Some(self.weapon_outline(&p, images)),
            "S_WeaponOutline_00_Special.shader" => {
                // Same surface as S_WeaponOutline_00 (no _MS/_AO variants in the build) + emission bands.
                let mut s = self.weapon_outline(&p, images);
                s.ext.kind = crate::unity_pbr::kind::SPECIAL;
                s.ext.color = p.color("_special", [1.0, 1.0, 1.0, 0.0]);
                s.ext.p0 = Vec4::new(p.fl("_lines1_number", 0.1), p.fl("_wavelength", 5.0), p.fl("_time", 1.0), p.fl("_Float1", 4.0));
                s.ext.p1 = Vec4::new(p.fl("_specialPower", 1.0), p.fl("_emissiveBoost", 1.0), 0.0, 0.0);
                Some(s)
            }
            "S_WeaponOutline_01_Emissive.shader" => {
                let mut s = self.weapon_outline(&p, images);
                s.ext.kind = crate::unity_pbr::kind::HANDCANON;
                s.ext.boost = p.fl("_EmissiveBoost", 1.0);
                s.ext.p0 = Vec4::new(p.fl("_timeglow", 1.0), 0.0, 0.0, 0.0);
                s.ext.st = p.st("_Emissive");
                s.ext_tex = p.tex("_Emissive").and_then(|g| self.texture(&g, images)).map(|t| t.0);
                Some(s)
            }
            "S_EnviroSurface00.shader" | "S_EnviroSurface01.shader" | "S_EnviroSurface01_DoubleSided.shader" | "S_Hats_00.shader" => {
                // S_Hats_00 (all hats, recovered from DXBC 2026-11-05: deferred pass = Standard metallic with albedo _BC, no tint,
                // no normal map, occlusion 1) is the same surface.
                // Albedo = _BC, Metallic = _Metallic, Smoothness = _Smoothness (S_EnviroSurface01 recovered from DXBC: the
                // same; the DoubleSided variant is Cull Off).
                let double = shader.ends_with("_DoubleSided.shader");
                let std = StandardMaterial {
                    base_color_texture: p.tex("_BC").and_then(|g| self.texture(&g, images)).map(|t| t.0),
                    metallic: p.fl("_Metallic", 0.0),
                    perceptual_roughness: 1.0 - p.fl("_Smoothness", 0.0),
                    reflectance: 0.5,
                    uv_transform: st_affine(p.st("_BC")),
                    cull_mode: if double { None } else { Some(bevy::render::render_resource::Face::Back) },
                    double_sided: double,
                    ..default()
                };
                Some(SurfaceMat::plain(std))
            }
            "S_Emissive00.shader" => {
                // Recovered from DXBC 2026-11-05 (deferred pass): albedo 0, specular 0.04 (metallic 0), smoothness
                // _Smoothness, emission = _TextureSample0(uv * _ST) * _EmissiveBoost (Saint's halo, M_Lava, TableTopMEtal00 7).
                let boost = p.fl("_EmissiveBoost", 1.0);
                let std = StandardMaterial {
                    base_color: Color::BLACK,
                    emissive: LinearRgba::rgb(boost, boost, boost),
                    emissive_texture: p.tex("_TextureSample0").and_then(|g| self.texture(&g, images)).map(|t| t.0),
                    metallic: 0.0,
                    perceptual_roughness: 1.0 - p.fl("_Smoothness", 0.0),
                    reflectance: 0.5,
                    uv_transform: st_affine(p.st("_TextureSample0")),
                    ..default()
                };
                Some(SurfaceMat::plain(std))
            }
            // Bakery's Standard clone (Shady / Blood / Morve hats): Standard's properties and lighting; its keyword
            // BAKERY_SHNONLINEAR only changes the lightmap SH decoding (these renderers are dynamic).
            "Bakery_Standard.shader" => self.build_std(m, "", "Standard", images).map(SurfaceMat::plain),
            "S_MaskedSurface_01.shader" if p.fl("_RGBA", 0.0) == 0.0 => {
                // Recovered from DXBC 2026-11-05 (PathID 1994, deferred blob 1552, no _RGBA_ON): albedo _MainTex, clip
                // _O.r < _Cutoff, metallic 0, smoothness 0, Cull Off, AlphaTest queue (NoChill's hair).
                let main = p.tex("_MainTex")?;
                let (mut bc, w, h) = self.pixels(&main)?;
                if let Some((o, _, _)) = self.channel(p.tex("_O"), 0, Some((w, h))) {
                    for (i, a) in o.into_iter().enumerate() {
                        bc[i * 4 + 3] = a;
                    }
                }
                let srgb = self.tex_header(&main).is_none_or(|t| t.color_space != 0);
                let std = StandardMaterial {
                    base_color_texture: Some(Self::packed_texture(bc, w, h, srgb, images)),
                    alpha_mode: AlphaMode::Mask(p.fl("_Cutoff", 0.5)),
                    metallic: 0.0,
                    perceptual_roughness: 1.0,
                    reflectance: 0.5,
                    uv_transform: st_affine(p.st("_MainTex")),
                    cull_mode: None,
                    double_sided: true,
                    ..default()
                };
                Some(SurfaceMat::plain(std))
            }
            "S_StunnedAboubi_00.shader" if p.fl("_Stunned", 0.0) == 0.0 => {
                // Recovered from DXBC 2026-11-05 (PathID 1900, ForwardBase blob 156; no deferred pass): Standard metallic 0,
                // smoothness 0, albedo _BC (_BC_ST), Cull Off. The ASE outline pass has _ASEOutlineWidth 0 (M_Pipe).
                let std = StandardMaterial {
                    base_color_texture: p.tex("_BC").and_then(|g| self.texture(&g, images)).map(|t| t.0),
                    metallic: 0.0,
                    perceptual_roughness: 1.0,
                    reflectance: 0.5,
                    uv_transform: st_affine(p.st("_BC")),
                    cull_mode: None,
                    double_sided: true,
                    opaque_render_method: bevy::material::OpaqueRendererMethod::Forward,
                    ..default()
                };
                Some(SurfaceMat::plain(std))
            }
            "S_Roughness_BC_00.shader" => {
                // Recovered from DXBC 2026-11-05 (data.unity3d PathID 35, deferred blob 587): albedo _BC (_BC_ST),
                // smoothness 1 - _Roughness.r (_Roughness_ST), metallic 0, no normal map (M_Circus_00).
                let rough = self.roughness_r_texture(p.tex("_Roughness"), images);
                let std = StandardMaterial {
                    base_color_texture: p.tex("_BC").and_then(|g| self.texture(&g, images)).map(|t| t.0),
                    metallic_roughness_texture: rough,
                    metallic: 0.0,
                    perceptual_roughness: 1.0,
                    reflectance: 0.5,
                    uv_transform: st_affine(p.st("_BC")),
                    ..default()
                };
                Some(SurfaceMat::plain(std))
            }
            "S_Car_00.shader" if p.fl("_UseMetallic", 0.0) == 0.0 && (p.fl("_UseEmission", 0.0) != 0.0) != (p.fl("_useRoughness", 0.0) != 0.0) => {
                // Recovered from DXBC 2026-11-05 (PathID 1955): _USEEMISSION_ON (blob 1650): normal _N, albedo _BC,
                // emission = _EmissionMap, metallic 0, smoothness 0. _USEROUGHNESS_ON (blob 1651): normal _N, albedo
                // _BC, smoothness 1 - _Roughness.r, metallic 0 (M_AE86_00, M_Trashbag_*).
                let emission = p.fl("_UseEmission", 0.0) != 0.0;
                let std = StandardMaterial {
                    base_color_texture: p.tex("_BC").and_then(|g| self.texture(&g, images)).map(|t| t.0),
                    normal_map_texture: p.tex("_N").and_then(|g| self.normal_texture(&g, images)),
                    emissive: if emission { LinearRgba::WHITE } else { LinearRgba::BLACK },
                    emissive_texture: if emission { p.tex("_EmissionMap").and_then(|g| self.texture(&g, images)).map(|t| t.0) } else { None },
                    metallic_roughness_texture: if emission { None } else { self.roughness_r_texture(p.tex("_Roughness"), images) },
                    metallic: 0.0,
                    perceptual_roughness: 1.0,
                    reflectance: 0.5,
                    uv_transform: st_affine(p.st("_BC")),
                    ..default()
                };
                Some(SurfaceMat::plain(std))
            }
            "S_Car_00.shader" if p.fl("_UseMetallic", 0.0) == 0.0 && p.fl("_UseEmission", 0.0) == 0.0 && p.fl("_useRoughness", 0.0) == 0.0 => {
                // Recovered from DXBC (deferred, no keyword): albedo _BC, normal _N, metallic 0, smoothness 0.
                let std = StandardMaterial {
                    base_color_texture: p.tex("_BC").and_then(|g| self.texture(&g, images)).map(|t| t.0),
                    normal_map_texture: p.tex("_N").and_then(|g| self.normal_texture(&g, images)),
                    metallic: 0.0,
                    perceptual_roughness: 1.0,
                    reflectance: 0.5,
                    uv_transform: st_affine(p.st("_BC")),
                    ..default()
                };
                Some(SurfaceMat::plain(std))
            }
            "S_EmissiveColor.shader" => {
                // Albedo = _TextureSample0(uv * _tilingdirt), Metallic 0, Smoothness 0, emission in the extension.
                let k = p.fl("_tilingdirt", 0.0);
                let std = StandardMaterial {
                    base_color_texture: p.tex("_TextureSample0").and_then(|g| self.texture(&g, images)).map(|t| t.0),
                    metallic: 0.0,
                    perceptual_roughness: 1.0,
                    reflectance: 0.5,
                    uv_transform: st_affine(Vec4::new(k, k, 0.0, 0.0)),
                    ..default()
                };
                let ext = crate::unity_pbr::UnityExtParams {
                    kind: crate::unity_pbr::kind::EMISSIVE_COLOR,
                    color: p.color("_Color0", [1.0, 1.0, 1.0, 0.0]),
                    p0: Vec4::new(p.fl("_Float0", 0.0), p.fl("_Power", 0.0), p.fl("_NoiseValue", 0.0), p.fl("_EmissiveBoost", 1.0)),
                    ..default()
                };
                let dirt = p.tex("_DirtTex").and_then(|g| self.texture(&g, images)).map(|t| t.0);
                Some(SurfaceMat { std, ext, ext_tex: dirt })
            }
            "S_FlickeringLight_00.shader" => {
                // Recovered from DXBC (deferred, _FLICKER_ON off): S_EmissiveColor with other texture names: albedo =
                // _EmissionMap(uv * _tilingdirt), emission = saturate(pow(_DirtTex(uv * _Float0), _Power) + _NoiseValue)
                // * _Color0 * _EmissiveBoost * albedo, metallic 0, smoothness 0 (ceiling neon fixtures). 2026-11-05, blobs
                // 1650 / 1651: _FLICKER_ON multiplies the emission by a hash of sin(_Float1) * _Time.y (unity_surface.wgsl),
                // _USETINT_ON multiplies the albedo by _Tint (PunkGoggles / Tv hats).
                let k = p.fl("_tilingdirt", 1.0);
                let tint = if p.fl("_UseTint", 0.0) != 0.0 { p.color("_Tint", [1.0; 4]) } else { Vec4::ONE };
                let std = StandardMaterial {
                    base_color: Color::LinearRgba(LinearRgba::new(tint.x, tint.y, tint.z, 1.0)),
                    base_color_texture: p.tex("_EmissionMap").and_then(|g| self.texture(&g, images)).map(|t| t.0),
                    metallic: 0.0,
                    perceptual_roughness: 1.0,
                    reflectance: 0.5,
                    uv_transform: st_affine(Vec4::new(k, k, 0.0, 0.0)),
                    ..default()
                };
                let ext = crate::unity_pbr::UnityExtParams {
                    kind: crate::unity_pbr::kind::EMISSIVE_COLOR,
                    color: p.color("_Color0", [1.0, 1.0, 1.0, 0.0]),
                    p0: Vec4::new(p.fl("_Float0", 0.0), p.fl("_Power", 0.0), p.fl("_NoiseValue", 0.0), p.fl("_EmissiveBoost", 1.0)),
                    p1: Vec4::new(if p.fl("_Flicker", 0.0) != 0.0 { 1.0 } else { 0.0 }, p.fl("_Float1", 0.0), 0.0, 0.0),
                    ..default()
                };
                let dirt = p.tex("_DirtTex").and_then(|g| self.texture(&g, images)).map(|t| t.0);
                Some(SurfaceMat { std, ext, ext_tex: dirt })
            }
            "S_MeshPreview_00.shader" => {
                // Albedo = _Color0, Emission = _Color0 * _EmissiveBoost, Smoothness 0, Alpha = _Opacity (Blend SrcAlpha
                // OneMinusSrcAlpha). Bevy multiplies emission by alpha before blending; Unity does not -> / alpha.
                let c = p.color("_Color0", [0.502848, 0.9391664, 0.9433962, 0.0]);
                let a = p.fl("_Opacity", 0.5).max(1e-4);
                let e = c * p.fl("_EmissiveBoost", 1.0) / a;
                let std = StandardMaterial {
                    base_color: Color::LinearRgba(LinearRgba::new(c.x, c.y, c.z, a)),
                    emissive: LinearRgba::rgb(e.x, e.y, e.z),
                    metallic: 0.0,
                    perceptual_roughness: 1.0,
                    alpha_mode: AlphaMode::Blend,
                    ..default()
                };
                Some(SurfaceMat::plain(std))
            }
            "S_ProximityMine_00.shader" | "S_ProximityMine_01.shader" => {
                let ring = shader.ends_with("S_ProximityMine_00.shader");
                let std = StandardMaterial {
                    base_color: Color::BLACK,
                    alpha_mode: if ring { AlphaMode::Add } else { AlphaMode::Blend },
                    cull_mode: None,
                    double_sided: true,
                    unlit: true,
                    ..default()
                };
                let ext = crate::unity_pbr::UnityExtParams {
                    kind: if ring { crate::unity_pbr::kind::PROX_RING } else { crate::unity_pbr::kind::PROX_PULSE },
                    color: p.color("_Color0", [0.8867924, 0.2133321, 0.2133321, 0.0]),
                    p0: if ring {
                        Vec4::new(p.fl("_time", 11.46), p.fl("_gradient", 0.06), 0.0, 0.0)
                    } else {
                        Vec4::new(p.fl("_time", 1.0), p.fl("_DepthFade", 1.0), p.fl("_opacitymult", 1.0), 0.0)
                    },
                    ..default()
                };
                Some(SurfaceMat { std, ext, ext_tex: None })
            }
            "S_HandGrenadeRadius_00.shader" => {
                // Recovered from DXBC (forward base): black albedo (specular term dropped), emission _Color0,
                // alpha = saturate(_BaseOpacity * min(pow(max(sin(_time * _Time.y), 0), _power), 1)
                //         + max(1 - |sceneEyeDepth - eyeDepth| / _DepthFade, 0) * _opacitymult).
                let std = StandardMaterial { base_color: Color::BLACK, alpha_mode: AlphaMode::Blend, cull_mode: None, double_sided: true, unlit: true, ..default() };
                let ext = crate::unity_pbr::UnityExtParams {
                    kind: crate::unity_pbr::kind::GRENADE_RADIUS,
                    color: p.color("_Color0", [0.8867924, 0.2133321, 0.2133321, 0.0]),
                    p0: Vec4::new(p.fl("_time", 11.46), p.fl("_DepthFade", 1.0), p.fl("_opacitymult", 1.0), p.fl("_power", 70.0)),
                    p1: Vec4::new(p.fl("_BaseOpacity", 0.0), 0.0, 0.0, 0.0),
                    ..default()
                };
                Some(SurfaceMat { std, ext, ext_tex: None })
            }
            "S_FogCard02.shader" => {
                // Recovered from DXBC (forward base): unlit _Color0, alpha = depth-intersection fade-in (unity_surface kind
                // FOG_CARD). Blend SrcAlpha OneMinusSrcAlpha, Cull Off, ZWrite Off.
                let std = StandardMaterial { base_color: Color::BLACK, alpha_mode: AlphaMode::Blend, cull_mode: None, double_sided: true, unlit: true, ..default() };
                let ext = crate::unity_pbr::UnityExtParams {
                    kind: crate::unity_pbr::kind::FOG_CARD,
                    color: p.color("_Color0", [1.0, 1.0, 1.0, 0.0]),
                    p0: Vec4::new(p.fl("_DepthFade", 1.0), p.fl("_GradientPowr", 0.0), p.fl("_GrtadientMult", 0.0), p.fl("_UseGradient", 0.0)),
                    ..default()
                };
                Some(SurfaceMat { std, ext, ext_tex: None })
            }
            "S_BlackHole_00.shader" => {
                // Recovered from DXBC: Standard metallic, albedo 0, metallic 0, smoothness 0 (cubemap at mip 6),
                // Emission = (1 - N.V)^5 * _SmissiveBoost, Alpha = (1 - (1 - N.V)^5) * _Float0, alpha blend, Cull Off.
                let std = StandardMaterial {
                    base_color: Color::BLACK,
                    metallic: 0.0,
                    perceptual_roughness: 1.0,
                    reflectance: 0.5,
                    alpha_mode: AlphaMode::Blend,
                    cull_mode: None,
                    double_sided: true,
                    ..default()
                };
                let ext = crate::unity_pbr::UnityExtParams {
                    kind: crate::unity_pbr::kind::BLACK_HOLE,
                    // The DXBC reads Emission = f^5 * cb0[4].x, Alpha = (1 - f^5) * cb0[4].y, and the shader's
                    // parameter table puts _Float0 (0.003) at .y, which would make the ball invisible. The original
                    // draws it as an opaque black ball (1.59 m: it caps the 1.43 m DarkTrail tube) whose rim is
                    // covered by the cyan Statics_00 glow (user captures 2026-11-06), so the two are used swapped:
                    // alpha from _SmissiveBoost (1), rim emission from _Float0. RESTRAFE_BLACKHOLE_DXBC=1 = table order.
                    boost: if std::env::var_os("RESTRAFE_BLACKHOLE_DXBC").is_some() { p.fl("_SmissiveBoost", 1.0) } else { p.fl("_Float0", 0.0) },
                    p0: Vec4::new(if std::env::var_os("RESTRAFE_BLACKHOLE_DXBC").is_some() { p.fl("_Float0", 0.0) } else { p.fl("_SmissiveBoost", 1.0) }, 0.0, 0.0, 0.0),
                    ..default()
                };
                Some(SurfaceMat { std, ext, ext_tex: None })
            }
            "S_Emissive_Mask_00.shader" => {
                // Same Amplify surface as the HUD menu screens (build_fx / ugui_fx.wgsl kind 0): albedo 0, specular
                // 0.04, smoothness 0, Emission = lerp(_MaskedColor, _LightColor, pow(_TextureSample0.r, _Power))
                // * _EmissiveBoost (Warden's M_Backrooms_Light_00 tube).
                let std = StandardMaterial { base_color: Color::BLACK, metallic: 0.0, perceptual_roughness: 1.0, reflectance: 0.5, ..default() };
                let ext = crate::unity_pbr::UnityExtParams {
                    kind: crate::unity_pbr::kind::EMISSIVE_MASK,
                    color: p.color("_MaskedColor", [0.0; 4]),
                    p0: Vec4::new(p.fl("_Power", 0.0), p.fl("_EmissiveBoost", 1.0), 0.0, 0.0),
                    p1: p.color("_LightColor", [0.0; 4]),
                    st: p.st("_TextureSample0"),
                    ..default()
                };
                let tex = p.tex("_TextureSample0").and_then(|g| self.texture(&g, images)).map(|t| t.0);
                Some(SurfaceMat { std, ext, ext_tex: Some(tex.unwrap_or_else(|| self.white(images))) })
            }
            "S_Windows_00.shader" => self.build_glass(&p, images).map(SurfaceMat::plain),
            "Shader Graphs_DF_Cyst_Liquid_Shader.shader" => {
                // Recovered from DXBC (Built-in Unlit target, Cull Off): see unity_surface.wgsl kind LIQUID.
                // Custom unlit cut-out (forward only; no G-buffer variant).
                let std = StandardMaterial { base_color: Color::WHITE, cull_mode: None, double_sided: true, unlit: true, opaque_render_method: bevy::material::OpaqueRendererMethod::Forward, ..default() };
                let ext = crate::unity_pbr::UnityExtParams {
                    kind: crate::unity_pbr::kind::LIQUID,
                    color: p.color("_SideColour", [0.0, 0.0, 0.0, 0.0]),
                    p0: Vec4::new(p.fl("_Fill", 0.0), p.fl("_WobbleZ", 0.0), 0.0, 0.0),
                    p1: p.color("_TopColour", [0.0, 0.0, 0.0, 0.0]),
                    ..default()
                };
                Some(SurfaceMat { std, ext, ext_tex: None })
            }
            "S_Surfaces_00.shader" | "S_Surfaces_00_NoFog.shader" => Some(SurfaceMat::plain(self.build_surfaces(m, images))),
            "S_TransparentWindows_00.shader" => self.build_windows(m, images).map(SurfaceMat::plain),
            _ => None,
        }
    }

    /// Amplify S_WeaponOutline_00 (71 weapon materials), recovered from the compiled FORWARDBASE pixel shaders (pass 1;
    /// pass 0 is the `_OutlineWidth` hull, see outline.rs; `_USEOUTLINE_ON` does not change the surface):
    ///   Albedo     = _BC(uv0 * _BC_ST).rgb  [* _Tint.rgb if _USETINT_ON]  [* pow(_AO(uv1).r, _AO_Power) if _USEAO_ON]
    ///   Normal     = UnpackNormal(_Normal(uv0 * _Normal_ST))  if _USENORMAL_ON, else the vertex normal
    ///   Metallic   = _MS(uv0 * _MS_ST).r  if _USEMETALLIC_ON,   else _Metallic
    ///   Smoothness = _MS.a                if _USESMOOTHNESS_ON, else _Smoothness
    ///   Occlusion 1, Emission 0, opaque (Standard metallic BRDF).
    /// AssetRipper's glTF export instead often put `_MS` in base colour and left metallic/roughness at the glTF default
    /// (metallic 1, roughness 1).
    fn weapon_outline(&mut self, p: &Props, images: &mut Assets<Image>) -> SurfaceMat {
        let base = p.tex("_BC").and_then(|g| self.texture(&g, images)).map(|t| t.0);
        let normal = if p.on("_USENORMAL_ON") { p.tex("_Normal").and_then(|g| self.normal_texture(&g, images)) } else { None };
        let tint = if p.on("_USETINT_ON") { p.color("_Tint", [0.0; 4]) } else { Vec4::ONE };
        let (use_m, use_s) = (p.on("_USEMETALLIC_ON"), p.on("_USESMOOTHNESS_ON"));
        let (metal, smooth) = (p.fl("_Metallic", 0.0), p.fl("_Smoothness", 0.0));
        let mut std = StandardMaterial {
            base_color: Color::LinearRgba(LinearRgba::new(tint.x, tint.y, tint.z, 1.0)),
            base_color_texture: base,
            normal_map_texture: normal,
            reflectance: 0.5,
            uv_transform: st_affine(p.st("_BC")),
            metallic: metal.clamp(0.0, 1.0),
            perceptual_roughness: (1.0 - smooth).clamp(0.0, 1.0),
            ..default()
        };
        if use_m || use_s {
            std.metallic_roughness_texture = Some(self.ms_texture(p.tex("_MS"), use_m, metal, use_s, smooth, images));
            std.metallic = 1.0;
            std.perceptual_roughness = 1.0;
        }
        let mut s = SurfaceMat::plain(std);
        if p.on("_USEAO_ON") {
            s.ext.kind = crate::unity_pbr::kind::AO;
            s.ext.ao_power = p.fl("_AO_Power", 0.0);
            // Default texture "white" when unassigned.
            s.ext_tex = Some(p.tex("_AO").and_then(|g| self.texture(&g, images)).map(|t| t.0).unwrap_or_else(|| self.white(images)));
        }
        s
    }

    /// Bevy metallic-roughness texture (G = 1 - smoothness, B = metallic) from a Unity metallic/smoothness map
    /// (R = metallic, sampled through the texture's sRGB setting; A = smoothness, never sRGB). Channels not taken from
    /// the map use the constant. A missing map is Unity's default "white".
    fn ms_texture(&mut self, guid: Option<String>, use_m: bool, metal: f32, use_s: bool, smooth: f32, images: &mut Assets<Image>) -> Handle<Image> {
        let px = guid.as_ref().and_then(|g| self.pixels(g));
        let srgb = guid.as_ref().and_then(|g| self.tex_header(g)).is_some_and(|t| t.color_space != 0);
        let (d, w, h) = px.unwrap_or((vec![255; 4], 1, 1));
        let lin = |v: u8| if srgb { (crate::tmp_text::gamma_to_linear(v as f32 / 255.0) * 255.0).round() as u8 } else { v };
        let mut out = vec![255u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            let m = if use_m { lin(d[i * 4]) } else { (metal.clamp(0.0, 1.0) * 255.0).round() as u8 };
            let s = if use_s { d[i * 4 + 3] } else { (smooth.clamp(0.0, 1.0) * 255.0).round() as u8 };
            out[i * 4] = 0;
            out[i * 4 + 1] = 255 - s;
            out[i * 4 + 2] = m;
        }
        Self::packed_texture(out, w, h, false, images)
    }
}

fn hex_bytes(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect()
}

fn half(b: [u8; 2]) -> f32 {
    let h = u16::from_le_bytes(b) as u32;
    let sign = (h >> 15) & 1;
    let exp = (h >> 10) & 0x1f;
    let mant = h & 0x3ff;
    let v = if exp == 0 {
        (mant as f32) * 2f32.powi(-24)
    } else if exp == 31 {
        if mant == 0 { f32::INFINITY } else { f32::NAN }
    } else {
        f32::from_bits(((exp + 112) << 23) | (mant << 13))
    };
    if sign == 1 { -v } else { v }
}

/// Reads one vertex attribute component.
fn read_comp(d: &[u8], at: usize, format: i64) -> f32 {
    let b = |n: usize| d.get(at..at + n).unwrap_or(&[0; 4][..n]);
    match format {
        0 => f32::from_le_bytes(b(4).try_into().unwrap()),
        1 => half(b(2).try_into().unwrap()),
        2 => b(1)[0] as f32 / 255.0,
        3 => (b(1)[0] as i8 as f32 / 127.0).max(-1.0),
        4 => u16::from_le_bytes(b(2).try_into().unwrap()) as f32 / 65535.0,
        5 => (i16::from_le_bytes(b(2).try_into().unwrap()) as f32 / 32767.0).max(-1.0),
        6 => b(1)[0] as f32,
        7 => b(1)[0] as i8 as f32,
        8 => u16::from_le_bytes(b(2).try_into().unwrap()) as f32,
        9 => i16::from_le_bytes(b(2).try_into().unwrap()) as f32,
        10 => u32::from_le_bytes(b(4).try_into().unwrap()) as f32,
        _ => i32::from_le_bytes(b(4).try_into().unwrap()) as f32,
    }
}

fn format_size(format: i64) -> usize {
    match format {
        0 | 10 | 11 => 4,
        1 | 4 | 5 | 8 | 9 => 2,
        _ => 1,
    }
}

/// Decodes an uncompressed Unity Mesh asset (serializedVersion 10 vertex data).
pub fn decode_mesh(path: &str) -> Option<Vec<Mesh>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        // Not exported: the Mesh of that name read in place ("<name>_<n>" = the (n + 2)-th mesh of the name).
        let stem = path.rsplit('/').next()?.trim_end_matches(".asset");
        let owned = crate::owned::Owned::global()?;
        let (node, pid) = owned.resolve_mesh(stem)?;
        let b = &owned.0.bundle;
        let n = &b.nodes[node];
        let sf = crate::unity_fs::SerializedFile::open(b, n).ok()?;
        let o = sf.objects.iter().find(|o| o.path_id == pid)?;
        let m = crate::unity_fs::Mesh::read(&sf.object_data(b, n, o).ok()?, sf.big_endian, b, &owned.0.data_dir).map_err(|e| warn!("{path}: {e}")).ok()?;
        return Some(meshes_from_unity(&m));
    };
    let m = yaml_inner(&text)?;
    let vd = m.get("m_VertexData")?;
    let count = int(vd.get("m_VertexCount")) as usize;
    if count == 0 {
        return Some(vec![]);
    }
    let data = hex_bytes(vd.get("_typelessdata").and_then(Value::as_str).unwrap_or(""));
    let channels: Vec<(usize, usize, i64, usize)> = vd
        .get("m_Channels")
        .and_then(Value::as_sequence)?
        .iter()
        .map(|c| (int(c.get("stream")) as usize, int(c.get("offset")) as usize, int(c.get("format")), (int(c.get("dimension")) & 0xF) as usize))
        .collect();
    // Stream layout: stride = sum of channel sizes per stream; streams are consecutive, 16-byte aligned.
    let nstreams = channels.iter().filter(|c| c.3 > 0).map(|c| c.0 + 1).max().unwrap_or(1);
    let mut strides = vec![0usize; nstreams];
    for c in channels.iter().filter(|c| c.3 > 0) {
        strides[c.0] = strides[c.0].max(c.1 + format_size(c.2) * c.3);
    }
    let mut starts = vec![0usize; nstreams];
    let mut off = 0;
    for s in 0..nstreams {
        starts[s] = off;
        off += strides[s] * count;
        off = (off + 15) & !15;
    }
    let read = |ch: usize| -> Option<Vec<Vec<f32>>> {
        let c = channels.get(ch)?;
        if c.3 == 0 {
            return None;
        }
        Some((0..count).map(|v| (0..c.3).map(|k| read_comp(&data, starts[c.0] + v * strides[c.0] + c.1 + k * format_size(c.2), c.2)).collect()).collect())
    };
    let pos: Vec<[f32; 3]> = read(0)?.iter().map(|p| [p[0], p[1], -p[2]]).collect();
    let nrm: Option<Vec<[f32; 3]>> = read(1).map(|v| v.iter().map(|n| Vec3::new(n[0], n[1], -n[2]).normalize_or(Vec3::Y).to_array()).collect());
    let tan: Option<Vec<[f32; 4]>> = read(2).map(|v| v.iter().map(|t| [t[0], t[1], -t[2], -t.get(3).copied().unwrap_or(1.0)]).collect());
    let col: Option<Vec<[f32; 4]>> = read(3).map(|v| v.iter().map(|c| [c[0], c[1], c[2], c.get(3).copied().unwrap_or(1.0)]).collect());
    let uv0: Option<Vec<[f32; 2]>> = read(4).map(|v| v.iter().map(|t| [t[0], 1.0 - t[1]]).collect());
    let uv1: Option<Vec<[f32; 2]>> = read(5).map(|v| v.iter().map(|t| [t[0], 1.0 - t[1]]).collect());
    let ib = hex_bytes(m.get("m_IndexBuffer").and_then(Value::as_str).unwrap_or(""));
    let wide = int(m.get("m_IndexFormat")) == 1;
    let index = |i: usize| -> u32 {
        if wide {
            u32::from_le_bytes(ib.get(i * 4..i * 4 + 4).map(|b| b.try_into().unwrap()).unwrap_or([0; 4]))
        } else {
            u16::from_le_bytes(ib.get(i * 2..i * 2 + 2).map(|b| b.try_into().unwrap()).unwrap_or([0; 2])) as u32
        }
    };
    let mut out = Vec::new();
    for sm in m.get("m_SubMeshes").and_then(Value::as_sequence).cloned().unwrap_or_default() {
        let first = int(sm.get("firstByte")) as usize / if wide { 4 } else { 2 };
        let n = int(sm.get("indexCount")) as usize;
        let base = int(sm.get("baseVertex")) as u32;
        if int(sm.get("topology")) != 0 {
            warn!("ugui: {path}: non-triangle submesh skipped");
            continue;
        }
        let mut idx = Vec::with_capacity(n);
        for t in 0..n / 3 {
            let a = index(first + t * 3) + base;
            let b = index(first + t * 3 + 1) + base;
            let c = index(first + t * 3 + 2) + base;
            // Mirroring z flips the handedness: reverse the winding.
            idx.extend_from_slice(&[a, c, b]);
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD).with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos.clone());
        if let Some(n) = &nrm {
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, n.clone());
        }
        if let Some(t) = &uv0 {
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, t.clone());
        }
        if let Some(t) = &uv1 {
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, t.clone());
        }
        if let Some(t) = &tan {
            mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, t.clone());
        }
        if let Some(c) = &col {
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, c.clone());
        }
        mesh.insert_indices(Indices::U32(idx));
        if nrm.is_none() {
            mesh.compute_normals();
        }
        out.push(mesh);
    }
    Some(out)
}

/// Unity Light / Camera / Transform helpers.
pub fn unity_quat(q: [f32; 4]) -> Quat {
    // Left-handed (Unity) -> right-handed with z mirrored.
    Quat::from_xyzw(-q[0], -q[1], q[2], q[3])
}

pub fn file_id(v: Option<&Value>) -> i64 {
    file_ref(v)
}

/// Same output as [`decode_mesh`] from a Mesh read in place: one Bevy mesh per triangle submesh, Unity space mirrored
/// on Z (positions, normals, tangents with w flipped), UV v flipped, winding reversed.
pub fn meshes_from_unity(m: &crate::unity_fs::Mesh) -> Vec<Mesh> {
    if m.vertex_count == 0 {
        return vec![];
    }
    let get = |ch: usize| -> Option<Vec<Vec<f32>>> {
        let (d, v) = m.attr(ch)?;
        Some(v.chunks(d as usize).map(|c| c.to_vec()).collect())
    };
    let Some(pos) = get(0).map(|v| v.iter().map(|p| [p[0], p[1], -p[2]]).collect::<Vec<[f32; 3]>>()) else { return vec![] };
    let nrm: Option<Vec<[f32; 3]>> = get(1).map(|v| v.iter().map(|n| Vec3::new(n[0], n[1], -n[2]).normalize_or(Vec3::Y).to_array()).collect());
    let tan: Option<Vec<[f32; 4]>> = get(2).map(|v| v.iter().map(|t| [t[0], t[1], -t[2], -t.get(3).copied().unwrap_or(1.0)]).collect());
    let col: Option<Vec<[f32; 4]>> = get(3).map(|v| v.iter().map(|c| [c[0], c[1], c[2], c.get(3).copied().unwrap_or(1.0)]).collect());
    let uv0: Option<Vec<[f32; 2]>> = get(4).map(|v| v.iter().map(|t| [t[0], 1.0 - t[1]]).collect());
    let uv1: Option<Vec<[f32; 2]>> = get(5).map(|v| v.iter().map(|t| [t[0], 1.0 - t[1]]).collect());
    let idx_size = if m.index_format == 1 { 4 } else { 2 };
    let mut out = Vec::new();
    for sm in &m.submeshes {
        if sm.topology != 0 {
            continue;
        }
        let first = sm.first_byte as usize / idx_size;
        let tri = &m.indices[first.min(m.indices.len())..(first + sm.index_count as usize).min(m.indices.len())];
        let mut idx = Vec::with_capacity(tri.len());
        for t in tri.chunks_exact(3) {
            idx.extend_from_slice(&[t[0] + sm.base_vertex, t[2] + sm.base_vertex, t[1] + sm.base_vertex]);
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD).with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos.clone());
        if let Some(n) = &nrm {
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, n.clone());
        }
        if let Some(t) = &uv0 {
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, t.clone());
        }
        if let Some(t) = &uv1 {
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, t.clone());
        }
        if let Some(t) = &tan {
            mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, t.clone());
        }
        if let Some(c) = &col {
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, c.clone());
        }
        mesh.insert_indices(Indices::U32(idx));
        if nrm.is_none() {
            mesh.compute_normals();
        }
        out.push(mesh);
    }
    out
}

#[cfg(test)]
mod decode_mesh_tests {
    /// With the old exports present: the in-place meshes equal the YAML-decoded ones (RESTRAFE_MESH_CHECK=1).
    #[test]
    fn owned_mesh_matches_yaml() {
        if !crate::owned::ref_checks("RESTRAFE_MESH_CHECK") {
            return;
        }
        let dir = &crate::owned::reference_file("assets/maps/mesh");
        let mut checked = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten().take(40) {
            let p = e.path();
            let stem = p.file_stem().unwrap().to_str().unwrap().to_string();
            let a = super::decode_mesh(&p.to_string_lossy().replace('\\', "/")).expect("yaml");
            let owned = crate::owned::Owned::open().unwrap();
            let Some((node, pid)) = owned.resolve_mesh(&stem) else { panic!("{stem} not resolved") };
            let b = &owned.0.bundle;
            let n = &b.nodes[node];
            let sf = crate::unity_fs::SerializedFile::open(b, n).unwrap();
            let o = sf.objects.iter().find(|o| o.path_id == pid).unwrap();
            let m = crate::unity_fs::Mesh::read(&sf.object_data(b, n, o).unwrap(), sf.big_endian, b, &owned.0.data_dir).unwrap();
            let bm = super::meshes_from_unity(&m);
            assert_eq!(a.len(), bm.len(), "{stem} submeshes");
            for (x, y) in a.iter().zip(&bm) {
                assert_eq!(x.count_vertices(), y.count_vertices(), "{stem} vertices");
                assert_eq!(x.indices().map(|i| i.len()), y.indices().map(|i| i.len()), "{stem} indices");
                let (Some(bevy::mesh::VertexAttributeValues::Float32x3(pa)), Some(bevy::mesh::VertexAttributeValues::Float32x3(pb))) = (x.attribute(super::Mesh::ATTRIBUTE_POSITION), y.attribute(super::Mesh::ATTRIBUTE_POSITION)) else { panic!() };
                let d = pa.iter().zip(pb).map(|(p, q)| (0..3).map(|k| (p[k] - q[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
                assert!(d < 1e-4, "{stem} positions differ by {d}");
            }
            checked += 1;
        }
        println!("{checked} meshes identical");
    }
}

#[cfg(test)]
mod material_doc_tests {
    use super::*;

    const RIP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../ripped/ExportedProject/Assets");

    /// Material documents built in place equal the old exported .mat files: shader (by exported file name), keywords,
    /// floats, colours, and each texture slot's texture (by name), scale and offset.
    #[test]
    fn material_docs_match_export() {
        let Some(o) = crate::owned::Owned::global() else { return };
        // guid -> exported path, for shaders and textures
        let mut by_guid: HashMap<String, String> = HashMap::new();
        let mut dirs = vec![RIP.to_string()];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p.to_string_lossy().to_string());
                    continue;
                }
                let n = e.file_name().to_string_lossy().to_string();
                if let Some(stem) = n.strip_suffix(".meta").filter(|s| !s.ends_with(".mat") && !s.ends_with(".cs")) {
                    if let Some(g) = std::fs::read_to_string(&p).ok().and_then(|t| t.split("guid: ").nth(1).and_then(|g| g.split_whitespace().next()).map(str::to_string)) {
                        by_guid.insert(g, stem.to_string());
                    }
                }
            }
        }
        let mut ua = UnityAssets::default();
        let (mut n, mut bad, mut missing) = (0, 0, 0);
        let file_name = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
        for e in std::fs::read_dir(format!("{RIP}/Material")).into_iter().flatten().flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            let Some(stem) = file.strip_suffix(".mat") else { continue };
            let Some(old) = yaml_inner(&std::fs::read_to_string(e.path()).unwrap()) else { continue };
            let Some((nd, pid)) = o.find_named(21, stem) else {
                missing += 1;
                continue;
            };
            let new = ua.doc(&crate::owned_docs::token(nd, pid)).unwrap();
            let mut diffs: Vec<String> = Vec::new();
            // shader
            let sh = |m: &Mapping, ua: &mut UnityAssets| -> String {
                let s = m.get("m_Shader");
                match guid_ref(s).as_deref() {
                    Some(BUILTIN_EXTRA) => format!("builtin:{}", file_ref(s)),
                    Some(g) if g.starts_with("u:") => ua.path(g).map(|p| file_name(&p)).unwrap_or_default(),
                    Some(g) => by_guid.get(g).map(|p| file_name(p)).unwrap_or_else(|| format!("guid {g}")),
                    None => String::new(),
                }
            };
            let (so, sn) = (sh(&old, &mut ua), sh(&new, &mut ua));
            if so != sn {
                diffs.push(format!("shader {so} vs {sn}"));
            }
            let kw = |m: &Mapping| -> Vec<String> {
                let mut v: Vec<String> = m.get("m_ValidKeywords").and_then(Value::as_sequence).into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect();
                v.sort();
                v
            };
            if kw(&old) != kw(&new) {
                diffs.push(format!("keywords {:?} vs {:?}", kw(&old), kw(&new)));
            }
            let props = |m: &Mapping, k: &str| m.get("m_SavedProperties").and_then(|p| p.get(k)).and_then(Value::as_mapping).cloned().unwrap_or_default();
            for (k, v) in props(&old, "m_Floats") {
                let w = props(&new, "m_Floats").get(&k).and_then(Value::as_f64);
                if w.is_none_or(|w| (w - v.as_f64().unwrap_or(0.0)).abs() > 1e-4) {
                    diffs.push(format!("float {:?}: {:?} vs {:?}", k.as_str(), v.as_f64(), w));
                }
            }
            for (k, v) in props(&old, "m_Colors") {
                let (a, b) = (rgba(Some(&v)), props(&new, "m_Colors").get(&k).map(|c| rgba(Some(c))));
                if b.is_none_or(|b| a.iter().zip(b).any(|(x, y)| (x - y).abs() > 1e-4)) {
                    diffs.push(format!("colour {:?}: {a:?} vs {b:?}", k.as_str()));
                }
            }
            for (k, v) in props(&old, "m_TexEnvs") {
                let w = props(&new, "m_TexEnvs").get(&k).cloned();
                let tex_old = guid_ref(v.get("m_Texture")).map(|g| by_guid.get(&g).map(|p| file_name(p)).unwrap_or(if g == BUILTIN_EXTRA { format!("builtin:{}", file_ref(v.get("m_Texture"))) } else { format!("guid {g}") }));
                let tex_new = w.as_ref().and_then(|w| guid_ref(w.get("m_Texture"))).map(|g| if g == BUILTIN_EXTRA { format!("builtin:{}", file_ref(w.as_ref().and_then(|w| w.get("m_Texture")))) } else { ua.path(&g).map(|p| file_name(&p)).unwrap_or(g) });
                // (a texture the old export did not contain cannot be compared)
                if tex_old != tex_new && !tex_old.as_ref().is_some_and(|t| t.starts_with("guid ")) {
                    diffs.push(format!("texture {:?}: {tex_old:?} vs {tex_new:?}", k.as_str()));
                }
            }
            n += 1;
            if !diffs.is_empty() {
                bad += 1;
                if bad <= 15 || std::env::var_os("RESTRAFE_MATDOC_ALL").is_some() {
                    println!("MATDOC {stem}: {}", diffs.join("; "));
                }
            }
        }
        println!("material documents: {n} compared, {bad} differ, {missing} not found");
        assert_eq!(bad, 0);
    }
}

#[cfg(test)]
mod normal_tests {
    /// Normal maps read in place equal the old exports' reconstructed PNGs (within BC decode rounding).
    #[test]
    fn normal_maps_match_export() {
        let Some(o) = crate::owned::Owned::global() else { return };
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../ripped/ExportedProject/Assets/Texture2D");
        let mut n = 0;
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            let Some(stem) = file.strip_suffix(".png") else { continue };
            let meta = std::fs::read_to_string(format!("{dir}/{file}.meta")).unwrap_or_default();
            if !meta.contains("textureType: 1") {
                continue;
            }
            let Some((nd, pid)) = o.find_named(28, stem) else { continue };
            let Some((w, h, ours)) = super::normal_rgba(nd, pid) else { continue };
            let Some((pw, ph, theirs)) = image_png(&std::fs::read(e.path()).unwrap()) else { continue };
            assert_eq!((w, h), (pw, ph), "{stem} size");
            let mut worst = 0i32;
            for (a, b) in ours.chunks_exact(4).zip(theirs.chunks_exact(4)) {
                for c in 0..3 {
                    worst = worst.max((a[c] as i32 - b[c] as i32).abs());
                }
            }
            println!("{stem}: worst channel difference {worst}");
            assert!(worst <= 4, "{stem}: {worst}");
            n += 1;
        }
        println!("{n} normal maps match");
    }

    fn image_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        let img = bevy::image::Image::from_buffer(bytes, bevy::image::ImageType::Extension("png"), bevy::image::CompressedImageFormats::NONE, false, bevy::image::ImageSampler::Default, bevy::asset::RenderAssetUsages::RENDER_WORLD).ok()?;
        let img = img.convert(bevy::render::render_resource::TextureFormat::Rgba8Unorm).unwrap_or(img);
        Some((img.width(), img.height(), img.data?))
    }
}
