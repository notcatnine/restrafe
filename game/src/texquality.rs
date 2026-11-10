//! QualitySettings texture quality (ProjectSettings/QualitySettings.asset): Low has `textureQuality: 1`
//! (= QualitySettings.masterTextureLimit 1, Unity 2021.3), Medium / High / Ultra 0. With limit 1 Unity drops the top
//! mip of every mipmapped Texture2D (half width / height); textures without mipmaps, render textures and
//! `ignoreMasterTextureLimit` textures are unaffected (no ripped texture sets that flag). Applied live on a quality
//! change like QualitySettings.SetQualityLevel.
//!
//! Port: an affected image gets a box-filtered half-size level (if it has no mip chain: the glTF / cosmetics / VFX
//! textures are loaded without one) and its sampler clamps the minimum LOD to 1, so it samples exactly the half-size
//! level and below, i.e. what Unity uploads. Leaving Low strips the added level and restores the sampler, so
//! Medium+ is byte-identical to before. Which images: every glTF texture (all 133 checked map / weapon / player / prop
//! glTF images are importer `enableMipMap: 1` textures), cosmetics PNGs (55 / 55 mipmapped), and images registered
//! with [`mark`] by loaders that know the importer setting (UGUI textures with mips, VFX textures with mips).
//! The other per-level settings: `lodBias` (0.4 / 0.7 / 1 / 5) only affects LODGroups and there is none in the
//! ripped scenes / prefabs, so it changes nothing; `anisotropicTextures` (0 / 1 / 1 / 2) is not ported (the port's
//! world textures have no mip chain, so anisotropic filtering would have nothing to act on).

use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use std::collections::HashMap;
use std::sync::Mutex;

static MARKED: Mutex<Vec<AssetId<Image>>> = Mutex::new(Vec::new());

/// Registers an image (created with `images.add`) as a mipmapped Unity texture that masterTextureLimit applies to.
pub fn mark(id: impl Into<AssetId<Image>>) {
    if let Ok(mut m) = MARKED.lock() {
        m.push(id.into());
    }
}

pub struct TexQualityPlugin;

impl Plugin for TexQualityPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TexQuality>().add_systems(PostUpdate, apply_texture_limit);
    }
}

struct Saved {
    sampler: ImageSampler,
    /// Byte length of the data before the added level (`None`: the image already had mips, data untouched).
    trunc: Option<usize>,
}

#[derive(Resource, Default)]
struct TexQuality {
    marked: Vec<AssetId<Image>>,
    reduced: HashMap<AssetId<Image>, Saved>,
    was_low: bool,
}

fn eligible(id: AssetId<Image>, server: &AssetServer, marked: &[AssetId<Image>]) -> bool {
    if marked.contains(&id) {
        return true;
    }
    let Some(p) = server.get_path(id) else { return false };
    let file = p.path().to_string_lossy().replace('\\', "/").to_lowercase();
    if file.ends_with(".glb") {
        return p.label().is_some_and(|l| l.starts_with("Texture"));
    }
    if file.starts_with("cosmetics/") && file.ends_with(".png") {
        return true;
    }
    // VFX textures loaded by path (decals): the importer's mip setting (68 / 72 have mips).
    if let Some(name) = file.strip_prefix("vfx/tex/") {
        let file = p.path().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(name.to_string());
        return crate::owned::Owned::global().and_then(|o| o.texture_header(&file)).map(|t| t.mip_count > 1).unwrap_or(true);
    }
    false
}

/// Bytes per channel for formats the box filter can average (8 / 16-bit unorm), and channel count.
fn layout(f: TextureFormat) -> Option<(usize, usize)> {
    use TextureFormat::*;
    Some(match f {
        R8Unorm => (1, 1),
        Rg8Unorm => (1, 2),
        Rgba8Unorm | Rgba8UnormSrgb | Bgra8Unorm | Bgra8UnormSrgb => (1, 4),
        R16Unorm => (2, 1),
        Rg16Unorm => (2, 2),
        Rgba16Unorm => (2, 4),
        _ => return None,
    })
}

/// 2x2 box-filtered next level (odd edges clamp), channel values averaged as stored.
fn half(src: &[u8], w: usize, h: usize, bpc: usize, ch: usize) -> Vec<u8> {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let bpp = bpc * ch;
    let mut out = vec![0u8; nw * nh * bpp];
    let rd = |x: usize, y: usize, c: usize| -> u32 {
        let o = (y * w + x) * bpp + c * bpc;
        if bpc == 1 { src[o] as u32 } else { u16::from_le_bytes([src[o], src[o + 1]]) as u32 }
    };
    for y in 0..nh {
        let (y0, y1) = ((y * 2).min(h - 1), (y * 2 + 1).min(h - 1));
        for x in 0..nw {
            let (x0, x1) = ((x * 2).min(w - 1), (x * 2 + 1).min(w - 1));
            for c in 0..ch {
                let v = (rd(x0, y0, c) + rd(x1, y0, c) + rd(x0, y1, c) + rd(x1, y1, c) + 2) / 4;
                let o = (y * nw + x) * bpp + c * bpc;
                if bpc == 1 {
                    out[o] = v as u8;
                } else {
                    out[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
                }
            }
        }
    }
    out
}

/// Debug: RESTRAFE_QUALITY_AT=<s>:<level>[,<s>:<level>...] sets the quality level at app time s (in memory only, like
/// picking it in the Graphics dropdown without saving), to test live switching.
fn debug_quality_switch(time: &Time, settings: &mut ResMut<crate::settings::Settings>) {
    let Ok(v) = std::env::var("RESTRAFE_QUALITY_AT") else { return };
    let (t0, t1) = (time.elapsed_secs() - time.delta_secs(), time.elapsed_secs());
    for (s, q) in v.split(',').filter_map(|e| e.split_once(':')).filter_map(|(s, q)| Some((s.parse::<f32>().ok()?, q.parse::<i32>().ok()?))) {
        if s > t0 && s <= t1 {
            settings.quality_setting = q;
            info!("TEXQUALITY debug: quality_setting = {q} at {t1:.2}");
        }
    }
}

fn apply_texture_limit(
    time: Res<Time>,
    mut settings: ResMut<crate::settings::Settings>,
    server: Res<AssetServer>,
    mut events: MessageReader<AssetEvent<Image>>,
    mut images: ResMut<Assets<Image>>,
    mut st: ResMut<TexQuality>,
) {
    debug_quality_switch(&time, &mut settings);
    if let Ok(mut m) = MARKED.lock() {
        let new: Vec<_> = m.drain(..).collect();
        st.marked.extend(new);
    }
    let low = settings.quality_setting == 0;
    let mut todo: Vec<AssetId<Image>> = Vec::new();
    if low && !st.was_low {
        todo.extend(images.ids());
    } else if low {
        for e in events.read() {
            if let AssetEvent::Added { id } | AssetEvent::LoadedWithDependencies { id } = e {
                todo.push(*id);
            }
        }
        // Images registered after their Added event was read.
        todo.extend(st.marked.iter().copied().filter(|id| !st.reduced.contains_key(id)));
    } else {
        events.clear();
    }
    if !low && st.was_low {
        let saved: Vec<_> = st.reduced.drain().collect();
        for (id, s) in saved {
            if let Some(mut img) = images.get_mut(id) {
                if let Some(n) = s.trunc {
                    if let Some(d) = img.data.as_mut() {
                        d.truncate(n);
                    }
                    img.texture_descriptor.mip_level_count = 1;
                }
                img.sampler = s.sampler;
            }
        }
        info!("TEXQUALITY masterTextureLimit 0 (restored)");
    }
    st.was_low = low;
    if todo.is_empty() {
        return;
    }
    todo.sort_unstable();
    todo.dedup();
    // Collect the images to reduce, downsample them in parallel, then write back.
    let mut jobs: Vec<(AssetId<Image>, usize, usize, usize, usize)> = Vec::new();
    let mut sampler_only: Vec<AssetId<Image>> = Vec::new();
    for id in todo {
        if st.reduced.contains_key(&id) || !eligible(id, &server, &st.marked) {
            continue;
        }
        let Some(img) = images.get(id) else { continue };
        let d = &img.texture_descriptor;
        if d.dimension != bevy::render::render_resource::TextureDimension::D2 || d.size.depth_or_array_layers != 1 || img.data.is_none() {
            continue;
        }
        if d.mip_level_count > 1 {
            sampler_only.push(id);
        } else if let Some((bpc, ch)) = layout(d.format) {
            if d.size.width > 1 || d.size.height > 1 {
                jobs.push((id, d.size.width as usize, d.size.height as usize, bpc, ch));
            }
        }
    }
    let results: Vec<(AssetId<Image>, Vec<u8>)> = {
        let imgs = &*images;
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(jobs.len().max(1));
        std::thread::scope(|s| {
            let hs: Vec<_> = (0..n)
                .map(|t| {
                    let jobs = &jobs;
                    s.spawn(move || {
                        jobs.iter()
                            .skip(t)
                            .step_by(n)
                            .filter_map(|&(id, w, h, bpc, ch)| imgs.get(id).and_then(|i| i.data.as_ref()).map(|d| (id, half(d, w, h, bpc, ch))))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        })
    };
    let lod1 = |s: &ImageSampler| {
        let mut d = match s {
            ImageSampler::Descriptor(d) => d.clone(),
            ImageSampler::Default => ImageSamplerDescriptor::linear(),
        };
        d.lod_min_clamp = 1.0;
        d.lod_max_clamp = d.lod_max_clamp.max(1.0);
        ImageSampler::Descriptor(d)
    };
    let (n_add, n_clamp) = (results.len(), sampler_only.len());
    for (id, lvl) in results {
        let Some(mut img) = images.get_mut(id) else { continue };
        let saved = Saved { sampler: img.sampler.clone(), trunc: img.data.as_ref().map(|d| d.len()) };
        img.sampler = lod1(&saved.sampler);
        if let Some(d) = img.data.as_mut() {
            d.extend_from_slice(&lvl);
        }
        img.texture_descriptor.mip_level_count = 2;
        st.reduced.insert(id, saved);
    }
    for id in sampler_only {
        let Some(mut img) = images.get_mut(id) else { continue };
        let saved = Saved { sampler: img.sampler.clone(), trunc: None };
        img.sampler = lod1(&saved.sampler);
        st.reduced.insert(id, saved);
    }
    if n_add + n_clamp > 0 {
        info!("TEXQUALITY masterTextureLimit 1: {n_add} images got a half-size level, {n_clamp} mipmapped clamped (total {})", st.reduced.len());
    }
}
