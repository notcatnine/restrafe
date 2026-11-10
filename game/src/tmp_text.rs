//! TextMeshPro port: SDF font assets, "TextMeshPro/Distance Field" materials, TMP text layout and a Bevy material
//! running the TMP SDF shader (`assets/shaders/tmp_sdf.wgsl`).
//!
//! * `TmpFont`: `MonoBehaviour/<name>.asset` (TMP_FontAsset: face info, glyph + character tables, atlas texture with the
//!   SDF in alpha). Read at runtime from the AssetRipper export.
//! * `TmpMatProps`: `Material/<name>.mat` (properties + keywords). `_ScaleRatioA/B/C` and the mesh padding are
//!   recomputed like `ShaderUtilities.UpdateShaderRatios` / `GetPadding` do at runtime.
//! * `layout`: the single-line subset of `TextMeshPro.GenerateTextMesh` the game's texts need (glyph quads with
//!   material padding, horizontal Left/Center/Right, vertical Top/Middle/Bottom, margins, character spacing).
//! * `TmpText` component: rebuilds its mesh when changed; rendered with `TmpSdfMaterial`.
//!
//! Also hosts the grab popup (`PauseManager.grabPopup`, a TextMeshProUGUI on the ScreenSpaceOverlay canvas
//! "---USER INTERFACE---" of MainMenu.unity): `ItemBehaviour.OnFocus` shows
//! `weaponName.ToLower() + " [" + key + "]"` while `PlayerPickup.HandleInteractionCheck` has a current interactable.
//! It is drawn as a TMP mesh by an orthographic overlay camera whose world units are canvas units.

use crate::weapons::pickup::Hands;
use crate::weapons::Item;
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::ScalingMode;
use bevy::image::ImageSampler;
#[cfg(test)]
use bevy::image::{CompressedImageFormats, ImageType};
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../ripped/ExportedProject/Assets");

#[derive(Clone, Copy, Debug)]
struct Glyph {
    advance: f32,
    bearing: Vec2,
    size: Vec2,
    /// Atlas rect in Unity texture space (origin bottom-left), without padding: min = (x, y), max = (x + w, y + h).
    rect: Rect,
    scale: f32,
}

impl Default for Glyph {
    fn default() -> Self {
        Glyph { advance: 0.0, bearing: Vec2::ZERO, size: Vec2::ZERO, rect: Rect::default(), scale: 1.0 }
    }
}

/// TMP_FontAsset (face info, glyphs, atlas).
pub struct TmpFont {
    pub point_size: f32,
    pub face_scale: f32,
    pub ascent: f32,
    pub descent: f32,
    /// m_FaceInfo.m_LineHeight (line advance in font units).
    pub line_height: f32,
    /// m_FaceInfo.m_StrikethroughOffset / m_UnderlineThickness (font units), for `<s>`.
    pub strikethrough_offset: f32,
    pub underline_thickness: f32,
    /// m_FaceInfo.m_UnderlineOffset (font units).
    pub underline_offset: f32,
    /// TMP_FontAsset normalStyle / normalSpacingOffset / boldStyle / boldSpacing / italicStyle (font style metrics).
    pub normal_style: f32,
    pub normal_spacing_offset: f32,
    pub bold_style: f32,
    pub bold_spacing: f32,
    pub italic_style: f32,
    pub atlas_size: Vec2,
    glyphs: HashMap<char, Glyph>,
    /// Raw atlas (SDF in alpha, linear sampling, no mipmaps like TMP atlases).
    pub atlas: Handle<Image>,
}

#[cfg(test)]
fn num(line: &str, key: &str) -> Option<f32> {
    line.trim().strip_prefix(key).and_then(|v| v.trim().parse().ok())
}

impl TmpFont {
    /// The TMP_FontAsset of that name and its first atlas texture, read from the installation.
    pub fn load(name: &str, images: &mut Assets<Image>) -> Option<TmpFont> {
        let (n, p) = crate::owned::Owned::global()?.find_named(114, name)?;
        Self::load_at(n, p, images)
    }

    /// The TMP_FontAsset at (node, path id).
    pub fn load_at(n: usize, p: i64, images: &mut Assets<Image>) -> Option<TmpFont> {
        let o = crate::owned::Owned::global()?;
        let (_, f) = crate::prefab_json::script_object(o, o.assemblies()?, n, p).map_err(|e| warn!("TMP font {n}:{p}: {e}")).ok()?;
        let atlas = f.get("m_AtlasTextures")?.as_array()?.first()?.as_str()?.strip_prefix("asset:")?.to_string();
        let stem = atlas.rsplit('/').next()?.trim_end_matches(".png");
        let (w, h, rgba, _) = o.rgba_texture(stem).map_err(|e| warn!("TMP font {n}:{p}: {e}")).ok()?;
        let mut img = Image::new(Extent3d { width: w, height: h, depth_or_array_layers: 1 }, TextureDimension::D2, rgba, TextureFormat::Rgba8Unorm, RenderAssetUsages::RENDER_WORLD);
        img.sampler = ImageSampler::linear();
        Self::from_fields(&f, images.add(img))
    }

    /// TMP_FontAsset fields (prefab_json layout) -> font metrics and glyphs.
    fn from_fields(f: &serde_json::Map<String, serde_json::Value>, atlas: Handle<Image>) -> Option<TmpFont> {
        let num = |v: &serde_json::Value, k: &str, d: f32| v.get(k).and_then(|x| x.as_f64()).map(|x| x as f32).unwrap_or(d);
        let top = |k: &str, d: f32| f.get(k).and_then(|x| x.as_f64()).map(|x| x as f32).unwrap_or(d);
        let fi = f.get("m_FaceInfo")?;
        let mut by_index: HashMap<u32, Glyph> = HashMap::new();
        for g in f.get("m_GlyphTable")?.as_array()? {
            let (m, r) = (&g["m_Metrics"], &g["m_GlyphRect"]);
            let min = Vec2::new(num(r, "m_X", 0.0), num(r, "m_Y", 0.0));
            by_index.insert(
                g["m_Index"].as_u64()? as u32,
                Glyph {
                    advance: num(m, "m_HorizontalAdvance", 0.0),
                    bearing: Vec2::new(num(m, "m_HorizontalBearingX", 0.0), num(m, "m_HorizontalBearingY", 0.0)),
                    size: Vec2::new(num(m, "m_Width", 0.0), num(m, "m_Height", 0.0)),
                    rect: Rect { min, max: min + Vec2::new(num(r, "m_Width", 0.0), num(r, "m_Height", 0.0)) },
                    scale: num(g, "m_Scale", 1.0),
                },
            );
        }
        let glyphs = f
            .get("m_CharacterTable")?
            .as_array()?
            .iter()
            .filter_map(|c| Some((char::from_u32(c["m_Unicode"].as_u64()? as u32)?, *by_index.get(&(c["m_GlyphIndex"].as_u64()? as u32))?)))
            .collect();
        let (ascent, descent) = (num(fi, "m_AscentLine", 0.0), num(fi, "m_DescentLine", 0.0));
        let line_height = num(fi, "m_LineHeight", 0.0);
        Some(TmpFont {
            point_size: num(fi, "m_PointSize", 90.0),
            face_scale: num(fi, "m_Scale", 1.0),
            ascent,
            descent,
            line_height: if line_height > 0.0 { line_height } else { ascent - descent },
            strikethrough_offset: num(fi, "m_StrikethroughOffset", 0.0),
            underline_thickness: num(fi, "m_UnderlineThickness", 0.0),
            underline_offset: num(fi, "m_UnderlineOffset", 0.0),
            normal_style: top("normalStyle", 0.0),
            normal_spacing_offset: top("normalSpacingOffset", 0.0),
            bold_style: top("boldStyle", 0.75),
            bold_spacing: top("boldSpacing", 7.0),
            italic_style: top("italicStyle", 35.0),
            atlas_size: Vec2::new(top("m_AtlasWidth", 1024.0), top("m_AtlasHeight", 1024.0)),
            glyphs,
            atlas,
        })
    }

    /// Loads a TMP_FontAsset from an old exported `.asset` (the reference of the comparison test).
    #[cfg(test)]
    pub fn load_path(path: &str, images: &mut Assets<Image>) -> Option<TmpFont> {
        let txt = std::fs::read_to_string(path).ok()?;
        let (mut point_size, mut face_scale, mut ascent, mut descent, mut aw, mut ah) = (90.0, 1.0, 0.0, 0.0, 1024.0, 1024.0);
        let mut line_height = 0.0;
        let (mut strikethrough_offset, mut underline_thickness, mut underline_offset) = (0.0, 0.0, 0.0);
        // TMP_FontAsset defaults.
        let (mut normal_style, mut normal_spacing_offset, mut bold_style, mut bold_spacing, mut italic_style) = (0.0, 0.0, 0.75, 7.0, 35.0);
        let mut atlas_guid = None;
        let mut by_index: HashMap<u32, Glyph> = HashMap::new();
        let mut chars: Vec<(u32, u32)> = Vec::new();
        let (mut section, mut cur_idx, mut g, mut in_rect, mut uni) = ("", None::<u32>, Glyph::default(), false, 0u32);
        let mut lines = txt.lines().peekable();
        while let Some(l) = lines.next() {
            let t = l.trim();
            if !l.starts_with("   ") {
                if let Some(v) = num(t, "normalStyle:") {
                    normal_style = v;
                } else if let Some(v) = num(t, "normalSpacingOffset:") {
                    normal_spacing_offset = v;
                } else if let Some(v) = num(t, "boldStyle:") {
                    bold_style = v;
                } else if let Some(v) = num(t, "boldSpacing:") {
                    bold_spacing = v;
                } else if let Some(v) = num(t, "italicStyle:") {
                    italic_style = v;
                }
            }
            if l.starts_with("  m_") && !l.starts_with("   ") {
                section = t.split(':').next().unwrap_or("");
                if section == "m_AtlasTextures" {
                    atlas_guid = lines.peek().and_then(|n| n.split("guid: ").nth(1)).map(|g| g.split(',').next().unwrap_or("").to_string());
                }
            }
            match section {
                "m_FaceInfo" => {
                    if let Some(v) = num(t, "m_PointSize:") {
                        point_size = v;
                    } else if let Some(v) = num(t, "m_Scale:") {
                        face_scale = v;
                    } else if let Some(v) = num(t, "m_AscentLine:") {
                        ascent = v;
                    } else if let Some(v) = num(t, "m_DescentLine:") {
                        descent = v;
                    } else if let Some(v) = num(t, "m_LineHeight:") {
                        line_height = v;
                    } else if let Some(v) = num(t, "m_StrikethroughOffset:") {
                        strikethrough_offset = v;
                    } else if let Some(v) = num(t, "m_UnderlineThickness:") {
                        underline_thickness = v;
                    } else if let Some(v) = num(t, "m_UnderlineOffset:") {
                        underline_offset = v;
                    }
                }
                "m_GlyphTable" => {
                    if let Some(v) = num(t.trim_start_matches("- "), "m_Index:") {
                        if let Some(i) = cur_idx {
                            by_index.insert(i, g);
                        }
                        cur_idx = Some(v as u32);
                        g = Glyph::default();
                        in_rect = false;
                    } else if t == "m_GlyphRect:" {
                        in_rect = true;
                    } else if let Some(v) = num(t, "m_Width:") {
                        if in_rect { g.rect.max.x = v } else { g.size.x = v }
                    } else if let Some(v) = num(t, "m_Height:") {
                        if in_rect { g.rect.max.y = v } else { g.size.y = v }
                    } else if let Some(v) = num(t, "m_HorizontalBearingX:") {
                        g.bearing.x = v;
                    } else if let Some(v) = num(t, "m_HorizontalBearingY:") {
                        g.bearing.y = v;
                    } else if let Some(v) = num(t, "m_HorizontalAdvance:") {
                        g.advance = v;
                    } else if let Some(v) = num(t, "m_X:") {
                        g.rect.min.x = v;
                    } else if let Some(v) = num(t, "m_Y:") {
                        g.rect.min.y = v;
                    } else if let Some(v) = num(t, "m_Scale:") {
                        g.scale = v;
                    }
                }
                "m_CharacterTable" => {
                    if let Some(v) = num(t.trim_start_matches("- "), "m_Unicode:") {
                        uni = v as u32;
                    } else if let Some(v) = num(t, "m_GlyphIndex:") {
                        chars.push((uni, v as u32));
                    }
                }
                _ => {
                    if let Some(v) = num(t, "m_AtlasWidth:") {
                        aw = v;
                    } else if let Some(v) = num(t, "m_AtlasHeight:") {
                        ah = v;
                    }
                }
            }
        }
        if let Some(i) = cur_idx {
            by_index.insert(i, g);
        }
        let glyphs = chars
            .into_iter()
            .filter_map(|(u, i)| {
                let mut g = *by_index.get(&i)?;
                // m_GlyphRect stores x, y, width, height: turn into min/max.
                g.rect.max += g.rect.min;
                Some((char::from_u32(u)?, g))
            })
            .collect();
        let atlas_guid = atlas_guid?;
        // Atlas texture: Texture2D/, or next to the font asset (e.g. Resources/fonts & materials/).
        let tex = find_texture(&atlas_guid).or_else(|| {
            let dir = std::path::Path::new(path).parent()?;
            std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find_map(|p| {
                let s = p.to_string_lossy().to_string();
                let hit = s.ends_with(".meta") && std::fs::read_to_string(&p).ok()?.contains(&format!("guid: {atlas_guid}"));
                hit.then(|| s.trim_end_matches(".meta").to_string())
            })
        })?;
        let bytes = std::fs::read(tex).ok()?;
        let img = Image::from_buffer(&bytes, ImageType::Extension("png"), CompressedImageFormats::NONE, false, ImageSampler::linear(), RenderAssetUsages::RENDER_WORLD).ok()?;
        Some(TmpFont { point_size, face_scale, ascent, descent, line_height: if line_height > 0.0 { line_height } else { ascent - descent }, strikethrough_offset, underline_thickness, underline_offset, normal_style, normal_spacing_offset, bold_style, bold_spacing, italic_style, atlas_size: Vec2::new(aw, ah), glyphs, atlas: images.add(img) })
    }
}

#[cfg(test)]
fn find_texture(guid: &str) -> Option<String> {
    std::fs::read_dir(format!("{ROOT}/Texture2D")).ok()?.flatten().map(|e| e.path()).find_map(|p| {
        let s = p.to_string_lossy().to_string();
        let meta = s.ends_with(".meta") && std::fs::read_to_string(&p).ok()?.contains(&format!("guid: {guid}"));
        meta.then(|| s.trim_end_matches(".meta").to_string())
    })
}

/// Unity `Mathf.GammaToLinearSpace` (exact sRGB curve, also applied to HDR values > 1).
pub fn gamma_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else if v < 1.0 {
        ((v + 0.055) / 1.055).powf(2.4)
    } else {
        v.powf(2.2)
    }
}

/// Gamma-space Unity colour (as serialized / passed to Material.SetColor) -> linear shader value.
pub fn linear_color(c: [f32; 4]) -> Vec4 {
    Vec4::new(gamma_to_linear(c[0]), gamma_to_linear(c[1]), gamma_to_linear(c[2]), c[3])
}

/// Value the "TextMeshPro/Distance Field" shader receives for one of its colour properties (_FaceColor,
/// _OutlineColor, _UnderlayColor, _GlowColor, _SpecularColor - all declared `[HDR] Color`, m_Flags 16 in the build's
/// shader): the serialized/SetColor value as-is, NOT sRGB->linear converted.
///
/// Measured on a capture of the original game's HUD (current build): the text pixels equal the TMP formula
/// evaluated with the raw values to within ~3/255 on the FPS, ping, ammo and health texts (mean of the G>190 pixels:
/// ref (188,237,174) vs raw (186,239,171) vs linearised (135,238,119) for the FPS counter; ammo ref (196,242,180),
/// raw (194,235,172), linearised (138,242,119)); with linearised colours R/B are 50-60 levels too low (the
/// "saturated green" look). REFERENCE (older build) shows the same. `RESTRAFE_TMP_LINEAR_COLORS=1` restores the
/// linearised values for comparisons.
pub fn tmp_color(c: [f32; 4]) -> Vec4 {
    if std::env::var_os("RESTRAFE_TMP_LINEAR_COLORS").is_some() {
        return linear_color(c);
    }
    Vec4::from_array(c)
}

/// A "TextMeshPro/Distance Field" material's saved properties.
#[derive(Clone, Debug, Default)]
pub struct TmpMatProps {
    floats: HashMap<String, f32>,
    colors: HashMap<String, [f32; 4]>,
    keywords: Vec<String>,
}

impl TmpMatProps {
    /// The material of that name, read from the installation.
    pub fn load(name: &str) -> Option<TmpMatProps> {
        let (n, p) = crate::owned::Owned::global()?.find_named(21, name)?;
        Self::load_at(n, p)
    }

    /// The material at (node, path id).
    pub fn load_at(n: usize, p: i64) -> Option<TmpMatProps> {
        let o = crate::owned::Owned::global()?;
        let j = crate::prefab_json::material_json(o, o.assemblies()?, n, p).ok()?;
        let mut m = TmpMatProps::default();
        for (k, v) in j["floats"].as_object()? {
            m.floats.insert(k.clone(), v.as_f64()? as f32);
        }
        for (k, v) in j["colors"].as_object()? {
            let c = v.as_array()?;
            let g = |i: usize| c.get(i).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
            m.colors.insert(k.clone(), [g(0), g(1), g(2), g(3)]);
        }
        m.keywords = j["keywords"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
        Some(m)
    }

    /// Loads a material from an old exported `.mat` (the reference of the comparison test).
    #[cfg(test)]
    pub fn load_path(path: &str) -> Option<TmpMatProps> {
        let txt = std::fs::read_to_string(path).ok()?;
        let mut m = TmpMatProps::default();
        let mut section = "";
        for l in txt.lines() {
            let t = l.trim();
            if l.starts_with("  m_ValidKeywords") {
                section = "kw";
                continue;
            }
            if l.starts_with("    m_Floats") {
                section = "f";
                continue;
            }
            if l.starts_with("    m_Colors") {
                section = "c";
                continue;
            }
            match section {
                "kw" => match t.strip_prefix("- ") {
                    Some(k) => m.keywords.push(k.to_string()),
                    None => section = "",
                },
                "f" | "c" if !l.starts_with("      ") => section = "",
                "f" => {
                    if let Some((k, v)) = t.split_once(':') {
                        if let Ok(v) = v.trim().parse() {
                            m.floats.insert(k.to_string(), v);
                        }
                    }
                }
                "c" => {
                    if let Some((k, v)) = t.split_once(':') {
                        let g = |c: &str| v.split(&format!("{c}: ")).nth(1).and_then(|x| x.split([',', '}']).next()).and_then(|x| x.trim().parse::<f32>().ok()).unwrap_or(0.0);
                        m.colors.insert(k.to_string(), [g("r"), g("g"), g("b"), g("a")]);
                    }
                }
                _ => {}
            }
        }
        Some(m)
    }

    /// Material float with the TMP_SDF.shader property default.
    pub fn f(&self, k: &str) -> f32 {
        if let Some(v) = self.floats.get(k) {
            return *v;
        }
        match k {
            "_GradientScale" => 5.0,
            "_TextureWidth" | "_TextureHeight" => 512.0,
            "_ScaleX" | "_ScaleY" | "_ScaleRatioA" | "_ScaleRatioB" | "_ScaleRatioC" => 1.0,
            "_WeightBold" => 0.5,
            "_PerspectiveFilter" => 0.875,
            "_GlowInner" | "_GlowOuter" => 0.05,
            "_GlowPower" => 0.75,
            "_Bevel" | "_Diffuse" | "_Ambient" => 0.5,
            "_LightAngle" => 3.1416,
            "_SpecularPower" => 2.0,
            "_Reflectivity" => 10.0,
            _ => 0.0,
        }
    }

    /// Material colour in gamma space (as serialized).
    pub fn color(&self, k: &str) -> [f32; 4] {
        self.colors.get(k).copied().unwrap_or(match k {
            "_FaceColor" | "_SpecularColor" => [1.0, 1.0, 1.0, 1.0],
            "_OutlineColor" => [0.0, 0.0, 0.0, 1.0],
            "_UnderlayColor" => [0.0, 0.0, 0.0, 0.5],
            "_GlowColor" => [0.0, 1.0, 0.0, 0.5],
            _ => [0.0; 4],
        })
    }

    pub fn has(&self, kw: &str) -> bool {
        self.keywords.iter().any(|k| k == kw)
    }

    /// `ShaderUtilities.UpdateShaderRatios` (m_clamp = 1, RATIOS_OFF not set).
    pub fn ratios(&self) -> Vec3 {
        let grad = self.f("_GradientScale");
        let dilate = self.f("_FaceDilate");
        let weight = self.f("_WeightNormal").max(self.f("_WeightBold")) / 4.0;
        let a = (grad - 1.0) / (grad * (weight + dilate + self.f("_OutlineWidth") + self.f("_OutlineSoftness")).max(1.0));
        let range = (weight + dilate) * (grad - 1.0);
        let b = (grad - 1.0 - range).max(0.0) / (grad * (self.f("_GlowOffset") + self.f("_GlowOuter")).max(1.0));
        let ul = self.f("_UnderlayOffsetX").abs().max(self.f("_UnderlayOffsetY").abs()) + self.f("_UnderlayDilate") + self.f("_UnderlaySoftness");
        let c = (grad - 1.0 - range).max(0.0) / (grad * ul.max(1.0));
        Vec3::new(a, b, c)
    }

    /// `_GradientScale * _ScaleRatioA` (TMP font style padding factor).
    pub fn grad_ratio(&self) -> f32 {
        self.f("_GradientScale") * self.ratios().x
    }

    /// `ShaderUtilities.GetPadding(material, enableExtraPadding: false, isBold: false)`.
    pub fn padding(&self) -> f32 {
        let r = self.ratios();
        let dilate = self.f("_FaceDilate") * r.x;
        let softness = self.f("_OutlineSoftness") * r.x;
        let outline = self.f("_OutlineWidth") * r.x;
        let mut uniform = outline + softness + dilate;
        if self.has("GLOW_ON") {
            uniform = uniform.max(dilate + self.f("_GlowOffset") * r.y + self.f("_GlowOuter") * r.y);
        }
        let mut pad = Vec4::ZERO;
        if self.has("UNDERLAY_ON") {
            let ox = self.f("_UnderlayOffsetX") * r.z;
            let oy = self.f("_UnderlayOffsetY") * r.z;
            let ext = dilate + self.f("_UnderlayDilate") * r.z + self.f("_UnderlaySoftness") * r.z;
            pad = Vec4::new(ext - ox, ext - oy, ext + ox, ext + oy).max(Vec4::ZERO);
        }
        let pad = pad.max(Vec4::splat(uniform)).min(Vec4::ONE) * self.f("_GradientScale");
        pad.max_element() + 1.25
    }

    pub fn params(&self) -> TmpSdfParams {
        let r = self.ratios();
        let kw = |k: &str| if self.has(k) { 1.0 } else { 0.0 };
        TmpSdfParams {
            face_color: tmp_color(self.color("_FaceColor")),
            outline_color: tmp_color(self.color("_OutlineColor")),
            underlay_color: tmp_color(self.color("_UnderlayColor")),
            glow_color: tmp_color(self.color("_GlowColor")),
            specular_color: tmp_color(self.color("_SpecularColor")),
            shape: Vec4::new(self.f("_FaceDilate"), self.f("_OutlineWidth"), self.f("_OutlineSoftness"), self.f("_WeightNormal")),
            ratios: Vec4::new(r.x, r.y, r.z, self.f("_GradientScale")),
            underlay: Vec4::new(self.f("_UnderlayOffsetX"), self.f("_UnderlayOffsetY"), self.f("_UnderlayDilate"), self.f("_UnderlaySoftness")),
            glow: Vec4::new(self.f("_GlowOffset"), self.f("_GlowInner"), self.f("_GlowOuter"), self.f("_GlowPower")),
            bevel: Vec4::new(self.f("_Bevel"), self.f("_BevelOffset"), self.f("_BevelWidth"), self.f("_BevelClamp")),
            light: Vec4::new(self.f("_BevelRoundness"), self.f("_LightAngle"), self.f("_SpecularPower"), self.f("_Reflectivity")),
            misc: Vec4::new(self.f("_Diffuse"), self.f("_Ambient"), self.f("_Sharpness"), self.f("_PerspectiveFilter")),
            tex: Vec4::new(self.f("_TextureWidth"), self.f("_TextureHeight"), self.f("_ScaleX"), self.f("_ScaleY")),
            flags: Vec4::new(kw("UNDERLAY_ON"), kw("BEVEL_ON"), kw("GLOW_ON"), self.f("_WeightBold")),
            extra: Vec4::new(self.f("_ShaderFlags"), 0.0, 0.0, 0.0),
        }
    }
}

/// Uniform block of `tmp_sdf.wgsl` (see the shader for the packing).
#[derive(ShaderType, Clone, Copy, Debug, Default, Reflect)]
pub struct TmpSdfParams {
    pub face_color: Vec4,
    pub outline_color: Vec4,
    pub underlay_color: Vec4,
    pub glow_color: Vec4,
    pub specular_color: Vec4,
    pub shape: Vec4,
    pub ratios: Vec4,
    pub underlay: Vec4,
    pub glow: Vec4,
    pub bevel: Vec4,
    pub light: Vec4,
    pub misc: Vec4,
    pub tex: Vec4,
    pub flags: Vec4,
    pub extra: Vec4,
}

/// The TMP "Distance Field" shader as a Bevy material (premultiplied alpha, no depth write, no culling).
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct TmpSdfMaterial {
    #[uniform(0)]
    pub params: TmpSdfParams,
    #[texture(1)]
    #[sampler(2)]
    pub atlas: Handle<Image>,
}

impl Material for TmpSdfMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/tmp_sdf.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "shaders/tmp_sdf.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Premultiplied
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(_: &MaterialPipeline, descriptor: &mut RenderPipelineDescriptor, layout: &MeshVertexBufferLayoutRef, _: MaterialPipelineKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        let vertex_layout = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
            Mesh::ATTRIBUTE_UV_1.at_shader_location(3),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(4),
        ])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        // TMP_SDF: Cull [_CullMode] (0 = off), ZWrite Off.
        descriptor.primitive.cull_mode = None;
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_write_enabled = Some(false).into();
        }
        Ok(())
    }
}

/// TMP text box / formatting (TMP_Text serialized fields).
#[derive(Clone, Copy, Debug)]
pub struct TmpLayout {
    pub font_size: f32,
    /// RectTransform rect size and pivot.
    pub rect_size: Vec2,
    pub pivot: Vec2,
    /// m_margin: x left, y top, z right, w bottom.
    pub margin: Vec4,
    /// HorizontalAlignmentOptions (1 Left, 2 Center, 4 Right).
    pub h_align: u32,
    /// VerticalAlignmentOptions (256 Top, 512 Middle, 1024 Bottom).
    pub v_align: u32,
    /// m_isOrthographic (UGUI / orthographic text: no 0.1 world scale).
    pub orthographic: bool,
    pub char_spacing: f32,
}

/// One glyph quad in RectTransform-local Unity coordinates (corners BL, TL, TR, BR) with Unity atlas UVs.
pub struct GlyphQuad {
    pub pos: [Vec2; 4],
    pub uv: [Vec2; 4],
    pub scale: f32,
}

/// `TextMeshPro.GenerateTextMesh` for one line of text (no rich text, kerning tables are empty in the game's fonts).
pub fn layout(font: &TmpFont, padding: f32, l: &TmpLayout, text: &str) -> Vec<GlyphQuad> {
    let world = if l.orthographic { 1.0 } else { 0.1 };
    let base_scale = l.font_size / font.point_size * font.face_scale * world;
    let spacing_scale = l.font_size * 0.01 * world;
    let mut quads = Vec::new();
    let mut x_advance = 0.0;
    let mut max_advance = 0.0;
    for c in text.chars() {
        let Some(g) = font.glyphs.get(&c) else { continue };
        let s = base_scale * g.scale;
        if !c.is_whitespace() && g.size.x > 0.0 && g.size.y > 0.0 {
            // Padding is clamped so that padding + style padding (0 for normal weight) <= _GradientScale.
            let p = padding;
            let tl = Vec2::new(x_advance + (g.bearing.x - p) * s, (g.bearing.y + p) * s);
            let bl = Vec2::new(tl.x, tl.y - (g.size.y + p * 2.0) * s);
            let tr = Vec2::new(bl.x + (g.size.x + p * 2.0) * s, tl.y);
            let br = Vec2::new(tr.x, bl.y);
            let a = font.atlas_size;
            let u0 = (g.rect.min.x - p) / a.x;
            let v0 = (g.rect.min.y - p) / a.y;
            let u1 = (g.rect.max.x + p) / a.x;
            let v1 = (g.rect.max.y + p) / a.y;
            quads.push(GlyphQuad { pos: [bl, tl, tr, br], uv: [Vec2::new(u0, v0), Vec2::new(u0, v1), Vec2::new(u1, v1), Vec2::new(u1, v0)], scale: s });
        }
        x_advance += g.advance * s + l.char_spacing * spacing_scale;
        if !c.is_whitespace() {
            max_advance = x_advance - l.char_spacing * spacing_scale;
        }
    }
    // Alignment (m_RectTransformCorners are pivot-relative local corners).
    let left = -l.pivot.x * l.rect_size.x;
    let bottom = -l.pivot.y * l.rect_size.y;
    let top = bottom + l.rect_size.y;
    let m = l.margin;
    let width = l.rect_size.x - m.x - m.z;
    let ascender = font.ascent * base_scale;
    let descender = font.descent * base_scale;
    let y = match l.v_align {
        256 => top - ascender - m.y,
        1024 => bottom - descender + m.w,
        _ => (bottom + top) * 0.5 - (ascender + m.y + descender - m.w) * 0.5,
    };
    let x = left
        + m.x
        + match l.h_align {
            1 => 0.0,
            4 => width - max_advance,
            _ => width * 0.5 - max_advance * 0.5,
        };
    for q in &mut quads {
        for p in &mut q.pos {
            *p += Vec2::new(x, y);
        }
    }
    quads
}

/// TextMeshProUGUI formatting for [`layout_ugui`] (multi-line subset of `TMP_Text`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TmpUiLayout {
    pub font_size: f32,
    pub rect_size: Vec2,
    pub pivot: Vec2,
    /// m_margin: x left, y top, z right, w bottom.
    pub margin: Vec4,
    /// HorizontalAlignmentOptions (1 Left, 2 Center, 4 Right, 8 Justified, 16 Flush, 32 Geometry).
    pub h_align: u32,
    /// VerticalAlignmentOptions (256 Top, 512 Middle, 1024 Bottom, 2048 Baseline, 4096 Geometry, 8192 Capline).
    pub v_align: u32,
    pub char_spacing: f32,
    pub word_spacing: f32,
    pub line_spacing: f32,
    pub paragraph_spacing: f32,
    pub wrap: bool,
    /// m_fontStyle (FontStyles: 1 Bold, 2 Italic, 4 Underline, 8 LowerCase, 16 UpperCase).
    pub style: u32,
    /// The material's `_GradientScale * _ScaleRatioA` (bold / normal style padding).
    pub grad_ratio: f32,
}

/// Font scale of a UGUI text (orthographic: no 0.1 world factor).
pub fn ui_font_scale(font: &TmpFont, font_size: f32) -> f32 {
    font_size / font.point_size * font.face_scale
}

/// Width (pen advance up to the last visible glyph) of a single line.
pub fn ui_line_width(font: &TmpFont, l: &TmpUiLayout, line: &str) -> f32 {
    let line = &strip_strike(line).0;
    let base = ui_font_scale(font, l.font_size);
    let em = l.font_size * 0.01;
    let extra = style_spacing(font, l) * em;
    let (mut x, mut w) = (0.0, 0.0);
    for c in line.chars() {
        let Some(g) = font.glyphs.get(&c) else { continue };
        x += g.advance * base * g.scale + extra + l.char_spacing * em;
        if c == ' ' {
            x += l.word_spacing * em;
        }
        if !c.is_whitespace() {
            // TMP compares xAdvance + the glyph advance (without its style / character spacing) to the width.
            w = x - extra - l.char_spacing * em;
        }
    }
    w
}

/// `TMP_Text.GenerateTextMesh` for UGUI texts: line feeds, word wrapping at spaces (or mid-word when a word does
/// not fit), per-line horizontal alignment and block vertical alignment (TMP uses the first line's ascender and the
/// last visible line's descender). Quads are in RectTransform-local Unity coordinates.
/// The only rich-text tag the game's UI texts use: `<s>..</s>` (completed challenges). Returns the text without the
/// tags and a per-char strikethrough flag.
pub fn strip_strike(text: &str) -> (String, Vec<bool>) {
    let (mut out, mut flags, mut on) = (String::new(), Vec::new(), false);
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("<s>") {
            on = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("</s>") {
            on = false;
            rest = r;
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            flags.push(on);
            rest = &rest[c.len_utf8()..];
        }
    }
    (out, flags)
}

/// Advance added per character by the font style (`normalSpacingOffset` + `boldSpacing` when bold), in em / 100.
fn style_spacing(font: &TmpFont, l: &TmpUiLayout) -> f32 {
    font.normal_spacing_offset + if l.style & 1 != 0 { font.bold_spacing } else { 0.0 }
}

/// Applies the case styles (LowerCase 8 / UpperCase 16) to a text.
pub fn style_case(text: &str, style: u32) -> String {
    if style & 16 != 0 {
        text.to_uppercase()
    } else if style & 8 != 0 {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

pub fn layout_ugui(font: &TmpFont, padding: f32, l: &TmpUiLayout, text: &str) -> Vec<GlyphQuad> {
    let text = style_case(text, l.style);
    let (text, struck) = strip_strike(&text);
    let text = text.as_str();
    let bold = l.style & 1 != 0;
    let italic = l.style & 2 != 0;
    let underline = l.style & 4 != 0;
    // TextMeshProUGUI.GenerateTextMesh: style padding (num7) = (boldStyle | normalStyle) / 4 * _GradientScale *
    // _ScaleRatioA, the glyph padding clamped so that both fit in _GradientScale; bold glyphs carry a negative scale
    // (the shader then uses _WeightBold).
    let grad = l.grad_ratio;
    let style_pad = if bold { font.bold_style } else { font.normal_style } / 4.0 * grad;
    let padding = if grad > 0.0 && style_pad + padding > grad { grad - style_pad } else { padding };
    let extra = style_spacing(font, l);
    let shear = if italic { font.italic_style * 0.01 } else { 0.0 };
    // Flags aligned with the concatenated lines (CR and the paragraph LFs are dropped).
    let mut struck: std::collections::VecDeque<bool> = text.chars().zip(struck).filter(|(c, _)| *c != '\r' && *c != '\n').map(|(_, f)| f).collect();
    let base = ui_font_scale(font, l.font_size);
    let em = l.font_size * 0.01;
    let m = l.margin;
    let width = (l.rect_size.x - m.x - m.z).max(0.0);
    // Justified (8) / Flush (16): TMP lets a line run 5 % past the width before wrapping (`num13 * 1.05f`).
    let justify = l.h_align & (8 | 16) != 0;
    let fit = if justify { width * 1.05 } else { width } + 0.001;
    // Break into lines; `wrapped[i]`: line i ends at a word wrap (not at a line feed / the end of the text).
    let mut lines: Vec<String> = Vec::new();
    let mut wrapped: Vec<bool> = Vec::new();
    for para in text.replace("\r", "").split('\n') {
        if !l.wrap {
            lines.push(para.to_string());
            wrapped.push(false);
            continue;
        }
        let mut cur = String::new();
        // TMP word breaks: after a space, a hyphen (45), U+200B or a soft hyphen (TextMeshProUGUI.GenerateTextMesh).
        for word in para.split_inclusive(|c: char| c == ' ' || c == '-' || c == '\u{200B}' || c == '\u{AD}') {
            let cand = format!("{cur}{word}");
            if cur.is_empty() || ui_line_width(font, l, &cand) <= fit {
                cur = cand;
                // A single word wider than the box is broken per character.
                if ui_line_width(font, l, &cur) > fit {
                    let mut part = String::new();
                    for c in cur.chars() {
                        let t = format!("{part}{c}");
                        if !part.is_empty() && ui_line_width(font, l, &t) > fit {
                            lines.push(std::mem::take(&mut part));
                            wrapped.push(true);
                            part.push(c);
                        } else {
                            part = t;
                        }
                    }
                    cur = part;
                }
            } else {
                lines.push(std::mem::take(&mut cur));
                wrapped.push(true);
                cur = word.to_string();
            }
        }
        lines.push(cur);
        wrapped.push(false);
    }
    let line_adv = font.line_height * base + l.line_spacing * em;
    let ascender = font.ascent * base;
    let descender = font.descent * base;
    let left = -l.pivot.x * l.rect_size.x;
    let bottom = -l.pivot.y * l.rect_size.y;
    let top = bottom + l.rect_size.y;
    let last_visible = lines.iter().rposition(|s| s.chars().any(|c| !c.is_whitespace())).unwrap_or(0);
    let offsets: Vec<f32> = (0..lines.len()).map(|i| -(i as f32) * line_adv).collect();
    let last_desc = offsets.get(last_visible).copied().unwrap_or(0.0) + descender;
    let y0 = match l.v_align {
        256 => top - ascender - m.y,
        1024 => bottom - last_desc + m.w,
        _ => (bottom + top) * 0.5 - (ascender + m.y + last_desc - m.w) * 0.5,
    };
    let mut quads = Vec::new();
    for (i, s) in lines.iter().enumerate() {
        let lw = ui_line_width(font, l, s);
        let x0 = left
            + m.x
            + match l.h_align {
                2 => width * 0.5 - lw * 0.5,
                4 => width - lw,
                _ => 0.0,
            };
        let mut x = 0.0;
        // Strikethrough run on this line: (start x, end x) of the struck visible glyph quads.
        let mut run: Option<(f32, f32)> = None;
        let mut ul: Option<(f32, f32, f32)> = None;
        let baseline = y0 + offsets[i];
        // Justified / Flush lines (TextMeshProUGUI.GenerateTextMesh): lines that wrapped (or any line for Flush, or a line
        // wider than the box) are stretched to the width: the spare width goes 60 % to the spaces and 40 %
        // (m_wordWrappingRatios) to the other characters, each character shifting everything after it.
        let stretch = justify && (wrapped.get(i).copied().unwrap_or(false) || l.h_align & 16 != 0 || lw > width);
        let (mut jx, mut js, mut jc) = (0.0f32, 0.0f32, 0.0f32);
        if stretch {
            let spare = width - lw;
            let chars: Vec<char> = s.chars().collect();
            let visible = chars.iter().filter(|c| !c.is_whitespace()).count() as f32;
            let last_visible = chars.last().is_some_and(|c| !c.is_whitespace());
            let mut spaces = chars.iter().filter(|c| c.is_whitespace()).count() as f32 - if last_visible { 0.0 } else { 1.0 };
            let mut others = visible - 1.0;
            if chars.first().is_some_and(|c| c.is_whitespace()) {
                spaces -= 1.0;
                others += 1.0;
            }
            let ratio = if spaces > 0.0 { 0.4 } else { 1.0 };
            js = spare * (1.0 - ratio) / spaces.max(1.0);
            jc = if others > 0.0 { spare * ratio / others } else { 0.0 };
        }
        for (k, c) in s.chars().enumerate() {
            if stretch && k > 0 {
                jx += if c.is_whitespace() { js } else { jc };
            }
            let xj = x0 + jx;
            let st = struck.pop_front().unwrap_or(false);
            if !st {
                if let Some((a, b)) = run.take() {
                    strike_quads(font, padding, base, baseline, a, b, &mut quads);
                }
            }
            let Some(g) = font.glyphs.get(&c) else { continue };
            let sc = base * g.scale;
            if st && !c.is_whitespace() {
                let (a, b) = (xj + x + (g.bearing.x - padding) * sc, xj + x + (g.bearing.x + g.size.x + padding) * sc);
                run = Some(run.map_or((a, b), |(ra, _)| (ra, b)));
            }
            if !c.is_whitespace() && g.size.x > 0.0 && g.size.y > 0.0 {
                let (p, sp) = (padding, style_pad);
                let mut tl = Vec2::new(xj + x + (g.bearing.x - p - sp) * sc, y0 + offsets[i] + (g.bearing.y + p) * sc);
                let mut bl = Vec2::new(tl.x, tl.y - (g.size.y + p * 2.0) * sc);
                let mut tr = Vec2::new(bl.x + (g.size.x + p * 2.0 + sp * 2.0) * sc, tl.y);
                let mut br = Vec2::new(tr.x, bl.y);
                if shear != 0.0 {
                    // Italic: top edge shifted by angle * (bearingY + padding + style padding), bottom edge by angle *
                    // (bearingY - height - padding - style padding), both re-centred.
                    let top = shear * (g.bearing.y + p + sp) * sc;
                    let bot = shear * (g.bearing.y - g.size.y - p - sp) * sc;
                    let mid = (top - bot) * 0.5;
                    tl.x += top - mid;
                    tr.x += top - mid;
                    bl.x += bot - mid;
                    br.x += bot - mid;
                }
                if underline {
                    ul = Some(match ul {
                        Some((a, _, m)) => (a, tr.x, f32::max(m, sc)),
                        None => (bl.x, tr.x, sc),
                    });
                }
                let a = font.atlas_size;
                let u0 = (g.rect.min.x - p) / a.x;
                let v0 = (g.rect.min.y - p) / a.y;
                let u1 = (g.rect.max.x + p) / a.x;
                let v1 = (g.rect.max.y + p) / a.y;
                quads.push(GlyphQuad { pos: [bl, tl, tr, br], uv: [Vec2::new(u0, v0), Vec2::new(u0, v1), Vec2::new(u1, v1), Vec2::new(u1, v0)], scale: if bold { -sc } else { sc } });
            }
            x += g.advance * sc + extra * em + l.char_spacing * em;
            if c == ' ' {
                x += l.word_spacing * em;
            }
        }
        if let Some((a, b)) = run.take() {
            strike_quads(font, padding, base, baseline, a, b, &mut quads);
        }
        // Underline (TMP_Text.DrawUnderlineMesh): from the first visible glyph's bottom-left to the last one's
        // top-right, at baseline + underlineOffset * the largest glyph scale.
        if let Some((a, b, m)) = ul.take() {
            line_quads(font, padding, m, baseline + font.underline_offset * m, a, b, &mut quads);
        }
    }
    quads
}

/// TMP_Text.DrawUnderlineMesh for a strikethrough from x `a` to `b` at baseline + strikethroughOffset: the underline
/// glyph ('_') split into a left cap, a stretched middle and a right cap, underlineThickness tall (+ padding).
fn strike_quads(font: &TmpFont, padding: f32, scale: f32, baseline: f32, a: f32, b: f32, quads: &mut Vec<GlyphQuad>) {
    line_quads(font, padding, scale, baseline + font.strikethrough_offset * scale, a, b, quads);
}

/// The underline-glyph line of [`strike_quads`] with its top at `y`.
fn line_quads(font: &TmpFont, padding: f32, scale: f32, y: f32, a: f32, b: f32, quads: &mut Vec<GlyphQuad>) {
    let Some(g) = font.glyphs.get(&'_') else { return };
    let (y_bot, y_top) = (y - (font.underline_thickness + padding) * scale, y + padding * scale);
    let seg = (g.size.x * 0.5 * scale).min((b - a) * 0.5);
    let at = font.atlas_size;
    let (rx, ry, w, h) = (g.rect.min.x, g.rect.min.y, g.rect.max.x - g.rect.min.x, g.rect.max.y - g.rect.min.y);
    let (v0, v1) = ((ry - padding) / at.y, (ry + h + padding) / at.y);
    let (u0, um, u1) = ((rx - padding) / at.x, (rx + w * 0.5) / at.x, (rx + w + padding) / at.x);
    for (xa, xb, ua, ub) in [(a, a + seg, u0, um), (a + seg, b - seg, um, um), (b - seg, b, um, u1)] {
        quads.push(GlyphQuad {
            pos: [Vec2::new(xa, y_bot), Vec2::new(xa, y_top), Vec2::new(xb, y_top), Vec2::new(xb, y_bot)],
            uv: [Vec2::new(ua, v0), Vec2::new(ua, v1), Vec2::new(ub, v1), Vec2::new(ub, v0)],
            scale,
        });
    }
}

/// A TextMeshPro text. The entity also needs `Mesh3d` (created on first build) and `MeshMaterial3d<TmpSdfMaterial>`.
#[derive(Component, Clone)]
pub struct TmpText {
    pub text: String,
    /// Vertex colour (TMP m_fontColor / `text.color`, raw Color32 / 255).
    pub color: [f32; 4],
    pub font: Arc<TmpFont>,
    /// `GetPadding` of the text's material.
    pub padding: f32,
    pub layout: TmpLayout,
    /// The glb HUD hierarchy mirrors X relative to Unity.
    pub mirror_x: bool,
}

/// Builds the TMP mesh for `t`.
pub fn build_mesh(t: &TmpText) -> Mesh {
    quads_mesh(&layout(&t.font, t.padding, &t.layout, &t.text), t.color, t.mirror_x)
}

/// TMP mesh of laid-out glyph quads (vertex colour `color`; `mirror_x` for the X-mirrored glb hierarchies).
pub fn quads_mesh(quads: &[GlyphQuad], color: [f32; 4], mirror_x: bool) -> Mesh {
    let mx = if mirror_x { -1.0 } else { 1.0 };
    let (mut pos, mut nrm, mut uv0, mut uv1, mut col, mut idx) = (vec![], vec![], vec![], vec![], vec![], vec![]);
    for q in quads {
        let i = pos.len() as u32;
        for k in 0..4 {
            pos.push([q.pos[k].x * mx, q.pos[k].y, 0.0]);
            nrm.push([0.0, 0.0, -1.0]);
            // Unity UV v points up, image rows (and our UVs) go down.
            uv0.push([q.uv[k].x, 1.0 - q.uv[k].y]);
            uv1.push([0.0, q.scale]);
            col.push(color);
        }
        idx.extend_from_slice(&[i, i + 1, i + 2, i + 2, i + 3, i]);
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv0)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, uv1)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_indices(Indices::U32(idx))
}

/// Re-generates the mesh of changed texts (TMP regenerates on text / colour changes; lossy scale changes are handled
/// in the shader).
pub fn rebuild_meshes(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut q: Query<(Entity, &TmpText, Option<&Mesh3d>, &mut Visibility), Changed<TmpText>>) {
    for (e, t, mesh, mut vis) in &mut q {
        let empty = t.text.chars().all(char::is_whitespace);
        let want = if empty { Visibility::Hidden } else { Visibility::Inherited };
        if *vis != want {
            *vis = want;
        }
        if empty {
            continue;
        }
        let m = build_mesh(t);
        match mesh {
            Some(h) => {
                let _ = meshes.insert(h.id(), m);
            }
            None => {
                // The mesh asset is replaced in place on later changes, so its first Aabb would go stale.
                commands.entity(e).insert((Mesh3d(meshes.add(m)), bevy::camera::visibility::NoFrustumCulling));
            }
        }
    }
}

/// Loaded TMP fonts and materials, by asset name.
#[derive(Resource, Default)]
pub struct TmpAssets {
    fonts: HashMap<String, Option<Arc<TmpFont>>>,
    mats: HashMap<String, Option<Arc<TmpMatProps>>>,
}

impl TmpAssets {
    pub fn font(&mut self, name: &str, images: &mut Assets<Image>) -> Option<Arc<TmpFont>> {
        self.fonts.entry(name.to_string()).or_insert_with(|| TmpFont::load(name, images).map(Arc::new)).clone()
    }
    pub fn material(&mut self, name: &str) -> Option<Arc<TmpMatProps>> {
        self.mats.entry(name.to_string()).or_insert_with(|| TmpMatProps::load(name).map(Arc::new)).clone()
    }
}

/// Render layer of the overlay canvas camera (grab popup).
pub const OVERLAY_LAYER: usize = 3;

#[derive(Component)]
struct GrabPopup;

/// GrabPopup: canvas centre + (0, 47.35); grabPopup: rect 425.67 x 50, scale (1.57, 1, 1), TMP 18 pt, white, centred,
/// margin z -0.102, font TabSecret, material "MajorMonoDisplay-Regular Atlas Material" (MainMenu.unity).
fn setup(mut commands: Commands, mut tmp: ResMut<TmpAssets>, mut images: ResMut<Assets<Image>>, mut mats: ResMut<Assets<TmpSdfMaterial>>) {
    // "---USER INTERFACE---" canvas: ScreenSpaceOverlay, CanvasScaler 1920x1080 match height (1): one world unit of
    // this camera is one canvas unit.
    commands.spawn((
        Name::new("OverlayCanvasCam"),
        Camera3d::default(),
        Camera { order: 3, clear_color: ClearColorConfig::None, ..default() },
        Projection::Orthographic(OrthographicProjection { scaling_mode: ScalingMode::FixedVertical { viewport_height: 1080.0 }, near: -100.0, far: 100.0, ..OrthographicProjection::default_3d() }),
        bevy::core_pipeline::tonemapping::Tonemapping::None,
        Transform::default(),
        RenderLayers::layer(OVERLAY_LAYER),
    ));
    let (Some(font), Some(mat)) = (tmp.font("TabSecret", &mut images), tmp.material("MajorMonoDisplay-Regular Atlas Material")) else {
        warn!("grab popup: TabSecret TMP font / material not found");
        return;
    };
    let material = mats.add(TmpSdfMaterial { params: mat.params(), atlas: font.atlas.clone() });
    let root = commands.spawn((Name::new("GrabPopup"), Transform::from_xyz(0.0, 47.35, 0.0), Visibility::Hidden, GrabPopup)).id();
    commands.spawn((
        Name::new("grabPopup"),
        TmpText {
            text: String::new(),
            color: [1.0; 4],
            font,
            padding: mat.padding(),
            layout: TmpLayout {
                font_size: 18.0,
                rect_size: Vec2::new(425.67, 50.0),
                pivot: Vec2::splat(0.5),
                margin: Vec4::new(0.0, 0.0, -0.101_959_236, 0.0),
                h_align: 2,
                v_align: 512,
                orthographic: true,
                char_spacing: 0.0,
            },
            mirror_x: false,
        },
        MeshMaterial3d(material),
        Transform::from_scale(Vec3::new(1.57, 1.0, 1.0)),
        Visibility::Hidden,
        RenderLayers::layer(OVERLAY_LAYER),
        ChildOf(root),
    ));
}

fn key_label(b: &crate::input::Bind) -> String {
    match b {
        crate::input::Bind::Key(k) => format!("{k:?}").trim_start_matches("Key").trim_start_matches("Digit").to_string(),
        crate::input::Bind::Mouse(m) => format!("{m:?}"),
        #[allow(unreachable_patterns)]
        _ => String::new(),
    }
}

/// `ItemBehaviour.OnFocus` / `PlayerPickup.HandleInteractionCheck` grab popup.
fn update(
    settings: Res<crate::settings::Settings>,
    input: Res<crate::input::PlayerInput>,
    hands: Query<&Hands>,
    items: Query<&Item>,
    props: Query<&crate::props::PhysicsProp>,
    doors: Query<&crate::scene_props::Door>,
    env_targets: Query<&crate::props::EnvTarget>,
    mut popup: Query<&mut Visibility, With<GrabPopup>>,
    mut text: Query<&mut TmpText>,
    children: Query<&Children, With<GrabPopup>>,
) {
    let Ok(mut vis) = popup.single_mut() else { return };
    let item = hands.iter().find_map(|h| h.current_interactable).and_then(|i| items.get(i).ok()).filter(|i| i.hand == crate::weapons::Hand::None);
    // PhysicsProp.OnFocus: (grabbed ? "drop" : "grab") + " " + popupText.ToLower() + " [" + letter + "]"
    let env_e = hands.iter().find_map(|h| h.current_env).map(|e| env_targets.get(e).map(|t| t.0).unwrap_or(e));
    let env = env_e.and_then(|e| props.get(e).ok());
    // Door.OnFocus: (isOpen ? closeDoor : popupText).ToLower() + " [" + letter + "]"
    let door = env_e.and_then(|e| doors.get(e).ok());
    // PauseManager.Update: InteractPromptLetter = (gamepad ? interactPromptTextGamepad : interactPromptText).text
    // (MainMenu scene: "X" / "F").
    let key = if input.gamepad { "X".to_string() } else { settings.bindings.interact.first().map(key_label).unwrap_or_default() };
    let s = match (item, env) {
        (Some(item), _) => format!("{} [{}]", item.def.weapon_name().to_lowercase(), key.to_lowercase()),
        (None, Some(p)) => format!("{} {} [{}]", if p.grabbed { "drop" } else { "grab" }, p.popup_text.to_lowercase(), key),
        (None, None) if door.is_some() => {
            let d = door.unwrap();
            format!("{} [{}]", if d.is_open { d.close_text.to_lowercase() } else { d.popup_text.to_lowercase() }, key.to_lowercase())
        }
        (None, None) => {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            return;
        }
    };
    if *vis != Visibility::Inherited {
        *vis = Visibility::Inherited;
    }
    for c in children.iter().flat_map(|k| k.iter()) {
        if let Ok(mut t) = text.get_mut(c) {
            if t.text != s {
                t.text = s.clone();
            }
        }
    }
}

pub struct TmpTextPlugin;

impl Plugin for TmpTextPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<TmpSdfMaterial>::default())
            .init_resource::<TmpAssets>()
            .add_systems(Startup, setup)
            .add_systems(Update, update)
            .add_systems(PostUpdate, rebuild_meshes.before(bevy::camera::visibility::VisibilitySystems::CalculateBounds));
    }
}

#[cfg(test)]
mod owned_tests {
    use super::*;

    /// TMP fonts and their materials read in place equal the old asset YAML: metrics, every glyph, atlas alpha.
    #[test]
    fn tmp_fonts_match_export() {
        if crate::owned::Owned::global().is_none() {
            return;
        }
        let dir = format!("{ROOT}/MonoBehaviour");
        let mut images = Assets::<Image>::default();
        let mut n = 0;
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            let Some(stem) = file.strip_suffix(".asset") else { continue };
            let txt = std::fs::read_to_string(e.path()).unwrap();
            if !txt.contains("m_GlyphTable:") {
                continue;
            }
            let Some(a) = TmpFont::load_path(&e.path().to_string_lossy(), &mut images) else { continue };
            let b = TmpFont::load(stem, &mut images).unwrap_or_else(|| panic!("{stem}: not read in place"));
            let m = |a: f32, b: f32, what: &str| assert!((a - b).abs() <= 1e-4 * a.abs().max(1.0), "{stem} {what}: {a} vs {b}");
            m(a.point_size, b.point_size, "point size");
            m(a.face_scale, b.face_scale, "scale");
            m(a.ascent, b.ascent, "ascent");
            m(a.descent, b.descent, "descent");
            m(a.line_height, b.line_height, "line height");
            m(a.strikethrough_offset, b.strikethrough_offset, "strikethrough");
            m(a.underline_thickness, b.underline_thickness, "underline thickness");
            m(a.underline_offset, b.underline_offset, "underline offset");
            m(a.normal_style, b.normal_style, "normal style");
            m(a.normal_spacing_offset, b.normal_spacing_offset, "normal spacing");
            m(a.bold_style, b.bold_style, "bold style");
            m(a.bold_spacing, b.bold_spacing, "bold spacing");
            m(a.italic_style, b.italic_style, "italic style");
            assert_eq!(a.atlas_size, b.atlas_size, "{stem} atlas size");
            assert_eq!(a.glyphs.len(), b.glyphs.len(), "{stem} glyph count");
            for (c, ga) in &a.glyphs {
                let gb = b.glyphs.get(c).unwrap_or_else(|| panic!("{stem}: glyph {c:?} missing"));
                m(ga.advance, gb.advance, "advance");
                assert!((ga.bearing - gb.bearing).abs().max_element() < 1e-3 && (ga.size - gb.size).abs().max_element() < 1e-3, "{stem} {c:?} metrics");
                assert!(ga.rect.min == gb.rect.min && ga.rect.max == gb.rect.max, "{stem} {c:?} rect");
                m(ga.scale, gb.scale, "glyph scale");
            }
            let (ia, ib) = (images.get(&a.atlas).unwrap(), images.get(&b.atlas).unwrap());
            assert_eq!(ia.size(), ib.size(), "{stem} atlas pixels size");
            let (da, db) = (ia.data.as_ref().unwrap(), ib.data.as_ref().unwrap());
            let alpha_a: Vec<u8> = da.chunks(da.len() / (ia.width() * ia.height()) as usize).map(|p| *p.last().unwrap()).collect();
            let alpha_b: Vec<u8> = db.chunks(4).map(|p| p[3]).collect();
            let diff = alpha_a.iter().zip(&alpha_b).filter(|(x, y)| x != y).count();
            println!("{stem}: {} glyphs, atlas {}x{}, {diff} alpha bytes differ", b.glyphs.len(), ib.width(), ib.height());
            assert_eq!(diff, 0, "{stem} atlas");
            n += 1;
        }
        println!("{n} TMP fonts match");
        // materials of the HUD / map texts
        let mdir = format!("{ROOT}/Material");
        let mut k = 0;
        for e in std::fs::read_dir(&mdir).into_iter().flatten().flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            let Some(stem) = file.strip_suffix(".mat").filter(|s| s.contains("SDF") || s.contains("Atlas Material")) else { continue };
            let a = TmpMatProps::load_path(&e.path().to_string_lossy()).unwrap();
            let Some(b) = TmpMatProps::load(stem) else {
                println!("{stem}: not read in place");
                continue;
            };
            for (key, v) in &a.floats {
                assert!(b.floats.get(key).is_some_and(|w| (v - w).abs() <= 1e-4 * v.abs().max(1.0)), "{stem}.{key}");
            }
            for (key, v) in &a.colors {
                let w = b.colors.get(key).unwrap_or_else(|| panic!("{stem}.{key}"));
                assert!(v.iter().zip(w).all(|(x, y)| (x - y).abs() < 1e-4), "{stem}.{key}: {v:?} vs {w:?}");
            }
            let (mut ka, mut kb) = (a.keywords.clone(), b.keywords.clone());
            ka.sort();
            kb.sort();
            assert_eq!(ka, kb, "{stem} keywords");
            k += 1;
        }
        println!("{k} TMP materials match");
    }
}
