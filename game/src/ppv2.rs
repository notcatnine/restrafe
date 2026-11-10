//! Unity Post Processing Stack v2 effects the original cameras apply on top of `postfx.rs`:
//!
//! * PlayerIK `Camera` (main, volume layer 3|13 -> `Camera Profile`): AmbientOcclusion (SAO, intensity 0.97);
//!   its Bloom is `active: 0`. Vignette / LensDistortion / ColorGrading live in `postfx.rs`.
//! * `WeaponCam` and `UICam` (volume layer 22 -> `WeaponCam Profile`): Bloom of their own pixels (see `bloom`; intensity 2, threshold 0.8, soft
//!   knee 0.5, clamp 65472, diffusion 7, white, not fast mode) + AmbientOcclusion (SAO, intensity 0.97), and
//!   PostProcessLayer antialiasing = FXAA (not fast mode).
//!
//! All three Unity cameras are HDR (m_HDR 1, tier HDR on, deferred on desktop tiers) in a Linear project. With
//! built-in camera stacking each camera renders into its own HDR target that starts from the 8-bit back buffer
//! composite of the previous cameras, then runs its PPv2 stack over the whole frame and resolves (clamps) back to
//! the back buffer. Bevy: every camera on the window is `Hdr` + `Msaa::Off` so they share one Rgba16Float main
//! texture; the world camera's `postfx` and the bloom uber pass of WeaponCam/UICam clamp to [0,1] (= back buffer
//! resolve). The overlay cameras (TMP popup, Bevy UI) get no post-processing, like Unity's overlay canvases.
//!
//! Order per camera: opaque -> SAO composition (BeforeImageEffectsOpaque) -> transparent -> bloom/uber -> FXAA.

use crate::hud::HudCamera;
use crate::player::fpc::PlayerCamera;
use crate::weapons::markers::WeaponCam;
use bevy::anti_alias::fxaa::{Fxaa, Sensitivity};
use bevy::camera::{Exposure, Hdr};
use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass, ViewPrepassTextures};
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::core_pipeline::tonemapping::{tonemapping, Tonemapping};
use bevy::core_pipeline::FullscreenShader;
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, texture_depth_2d, uniform_buffer};
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

const PYRAMID_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
const AO_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;
const VIEW_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
/// Camera exposure replacing the old tonemapper's brightness (Bevy default `Exposure::BLENDER` is 9.7).
pub const EV100: f32 = 10.4;

/// PPv2 `Bloom` settings (raw profile values; derived shader values computed per frame like `Bloom.Render`).
#[derive(Component, ExtractComponent, Clone, Copy)]
pub struct Ppv2Bloom {
    pub intensity: f32,
    pub threshold: f32,
    pub soft_knee: f32,
    pub clamp: f32,
    pub diffusion: f32,
    /// `color` (gamma space, as serialized).
    pub color: Vec3,
    /// Clamp the result (the stacked camera resolves to the 8-bit back buffer).
    pub clamp_output: bool,
}

impl Ppv2Bloom {
    /// `WeaponCam Profile.asset` Bloom (overrides: intensity 2, threshold 0.8; rest = PPv2 defaults).
    pub const WEAPON_CAM_PROFILE: Self =
        Self { intensity: 2.0, threshold: 0.8, soft_knee: 0.5, clamp: 65472.0, diffusion: 7.0, color: Vec3::ONE, clamp_output: true };
}

/// PPv2 `AmbientOcclusion` in ScalableAmbientObscurance mode.
#[derive(Component, ExtractComponent, Clone, Copy)]
pub struct Ppv2Sao {
    pub intensity: f32,
    pub radius: f32,
    /// AmbientOcclusionQuality: 0 Lowest .. 4 Ultra.
    pub quality: u32,
    /// `color` (gamma space).
    pub color: Vec3,
    /// Debug id for `RESTRAFE_SAO_VIEW=main|weapon|hud` (shows 1 - occlusion of that camera).
    pub debug: &'static str,
}

impl Ppv2Sao {
    /// `Camera Profile` and `WeaponCam Profile` AmbientOcclusion (overrides: mode SAO, intensity 0.97; radius 0.25,
    /// quality Medium, colour black = defaults).
    pub const PROFILE: Self = Self { intensity: 0.97, radius: 0.25, quality: 2, color: Vec3::ZERO, debug: "" };
}

pub struct Ppv2Plugin;

impl Plugin for Ppv2Plugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((ExtractComponentPlugin::<Ppv2Bloom>::default(), ExtractComponentPlugin::<Ppv2Sao>::default()))
            .add_systems(PostUpdate, setup_cameras);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_resource::<Ppv2Uniforms>()
            .add_systems(RenderStartup, init_pipelines)
            .add_systems(Render, prepare.in_set(RenderSystems::PrepareResources))
            .add_systems(
                Core3d,
                (
                    bloom_snapshot.before(main_opaque_pass_3d).in_set(Core3dSystems::MainPass),
                    sao.after(main_opaque_pass_3d).before(main_transparent_pass_3d).in_set(Core3dSystems::MainPass),
                    bloom.before(tonemapping).in_set(Core3dSystems::PostProcess),
                ),
            );
    }
}

/// Camera setup. Every camera rendering to the window must share the HDR main texture (Bevy keys the shared
/// texture on format + MSAA), and post passes must not tonemap the composite again.
fn setup_cameras(
    mut commands: Commands,
    cams: Query<(Entity, Has<PlayerCamera>, Has<WeaponCam>, Has<HudCamera>, Has<Camera3d>), Added<Camera>>,
) {
    if std::env::var("RESTRAFE_NO_PPV2").is_ok() {
        return;
    }
    let no_sao = std::env::var("RESTRAFE_NO_SAO").is_ok();
    let no_bloom = std::env::var("RESTRAFE_NO_BLOOM").is_ok();
    for (e, main, weapon, hud, is_3d) in &cams {
        let mut ec = commands.entity(e);
        // Deferred rendering path on desktop tiers: no MSAA in the original.
        ec.insert((Hdr, Msaa::Off));
        if main || weapon || hud {
            // PPv2 ColorGrading tonemapper None (Camera Profile; the WeaponCam Profile has no grading): linear output,
            // clamped by the back buffer. The port's lights were tuned under Bevy's default TonyMcMapface; the
            // exposure (+0.7 EV) keeps the overall brightness of the test map (mid-tones and shadows measured on screenshots).
            let ev100 = std::env::var("RESTRAFE_EV100").ok().and_then(|s| s.parse().ok()).unwrap_or(EV100);
            ec.insert((Tonemapping::None, Exposure { ev100 }));
        }
        if main {
            if is_3d {
                ec.insert((DepthPrepass, NormalPrepass, Ppv2Sao { debug: "main", ..Ppv2Sao::PROFILE }));
                // Deferred shading (PC tier 3) for the opaque lit surfaces: G-buffer for PPv2 SSR.
                // RESTRAFE_FORWARD=1: forward only (comparison).
                if std::env::var_os("RESTRAFE_FORWARD").is_none() {
                    ec.insert((bevy::core_pipeline::prepass::DeferredPrepass, crate::ppv2_ssr::Ppv2Ssr::PROFILE));
                    if std::env::var_os("RESTRAFE_NO_MV").is_none() {
                        ec.insert(bevy::core_pipeline::prepass::MotionVectorPrepass);
                    }
                }
            }
        } else if weapon || hud {
            if std::env::var_os("RESTRAFE_FORWARD").is_none() {
                // Bevy cannot queue deferred materials on a view with prepasses but no DeferredPrepass.
                ec.insert(bevy::core_pipeline::prepass::DeferredPrepass);
            }
            ec.insert((
                DepthPrepass,
                NormalPrepass,
                Ppv2Sao { debug: if weapon { "weapon" } else { "hud" }, ..Ppv2Sao::PROFILE },
                Ppv2Bloom::WEAPON_CAM_PROFILE,
                // PPv2 FXAA (FinalPass, quality preset): edge threshold 0.063, min 0.0312.
                Fxaa { enabled: true, edge_threshold: Sensitivity::Ultra, edge_threshold_min: Sensitivity::High },
            ));
        } else {
            ec.insert(Tonemapping::None);
        }
        if no_sao {
            ec.remove::<Ppv2Sao>();
        }
        // RESTRAFE_NO_BLOOM=1 (all), =weapon / =hud (that camera only).
        if no_bloom && (weapon || hud) {
            let which = std::env::var("RESTRAFE_NO_BLOOM").unwrap_or_default();
            if (which != "weapon" && which != "hud") || (which == "weapon") == weapon {
                ec.remove::<Ppv2Bloom>();
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Render world
// ---------------------------------------------------------------------------------------------

#[derive(ShaderType, Clone, Copy, Default)]
struct BloomUniform {
    threshold: Vec4,
    params: Vec4,
    color: Vec4,
}

#[derive(ShaderType, Clone, Copy, Default)]
struct SaoUniform {
    ao_params: Vec4,
    ao_color: Vec4,
    proj: Vec4,
    screen: Vec4,
    view_from_world: Mat4,
}

#[derive(Resource, Default)]
pub(crate) struct Ppv2Uniforms {
    bloom: DynamicUniformBuffer<BloomUniform>,
    sao: DynamicUniformBuffer<SaoUniform>,
}

#[derive(Resource)]
pub(crate) struct Ppv2Pipelines {
    bloom_layout: BindGroupLayoutDescriptor,
    sao_layout: BindGroupLayoutDescriptor,
    depth_layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    prefilter: CachedRenderPipelineId,
    downsample: CachedRenderPipelineId,
    upsample: CachedRenderPipelineId,
    uber: CachedRenderPipelineId,
    copy: CachedRenderPipelineId,
    ao_estimate: CachedRenderPipelineId,
    ao_blur_h: CachedRenderPipelineId,
    ao_blur_v: CachedRenderPipelineId,
    ao_composite: CachedRenderPipelineId,
    ao_debug: CachedRenderPipelineId,
}

fn init_pipelines(
    mut commands: Commands,
    device: Res<RenderDevice>,
    cache: Res<PipelineCache>,
    assets: Res<AssetServer>,
    fullscreen: Res<FullscreenShader>,
) {
    let two_tex = |label: &'static str, uniform: BindGroupLayoutEntryBuilder| {
        BindGroupLayoutDescriptor::new(
            label,
            &BindGroupLayoutEntries::sequential(
                ShaderStages::FRAGMENT,
                (
                    texture_2d(TextureSampleType::Float { filterable: true }),
                    texture_2d(TextureSampleType::Float { filterable: true }),
                    sampler(SamplerBindingType::Filtering),
                    uniform,
                ),
            ),
        )
    };
    let bloom_layout = two_tex("ppv2_bloom_layout", uniform_buffer::<BloomUniform>(true));
    let sao_layout = two_tex("ppv2_sao_layout", uniform_buffer::<SaoUniform>(true));
    let depth_layout = BindGroupLayoutDescriptor::new(
        "ppv2_sao_depth_layout",
        &BindGroupLayoutEntries::single(ShaderStages::FRAGMENT, texture_depth_2d()),
    );
    let sampler = device.create_sampler(&SamplerDescriptor {
        label: Some("ppv2_bilinear_clamp"),
        min_filter: FilterMode::Linear,
        mag_filter: FilterMode::Linear,
        address_mode_u: AddressMode::ClampToEdge,
        address_mode_v: AddressMode::ClampToEdge,
        ..default()
    });
    let bloom_shader: Handle<Shader> = assets.load("shaders/ppv2_bloom.wgsl");
    let sao_shader: Handle<Shader> = assets.load("shaders/ppv2_sao.wgsl");
    let queue = |label: &'static str,
                     layout: Vec<BindGroupLayoutDescriptor>,
                     shader: &Handle<Shader>,
                     entry: &'static str,
                     format: TextureFormat,
                     blend: Option<BlendState>| {
        cache.queue_render_pipeline(RenderPipelineDescriptor {
            label: Some(label.into()),
            layout,
            vertex: fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: shader.clone(),
                entry_point: Some(entry.into()),
                targets: vec![Some(ColorTargetState { format, blend, write_mask: ColorWrites::ALL })],
                ..default()
            }),
            ..default()
        })
    };
    let bl = vec![bloom_layout.clone()];
    let prefilter = queue("ppv2_bloom_prefilter", bl.clone(), &bloom_shader, "prefilter", PYRAMID_FORMAT, None);
    let downsample = queue("ppv2_bloom_downsample", bl.clone(), &bloom_shader, "downsample", PYRAMID_FORMAT, None);
    let upsample = queue("ppv2_bloom_upsample", bl.clone(), &bloom_shader, "upsample", PYRAMID_FORMAT, None);
    let copy = queue("ppv2_bloom_copy", bl.clone(), &bloom_shader, "copy", VIEW_FORMAT, None);
    let uber = queue("ppv2_bloom_uber", bl, &bloom_shader, "uber", VIEW_FORMAT, None);
    let sl = vec![sao_layout.clone()];
    let ao_estimate =
        queue("ppv2_sao_estimate", vec![sao_layout.clone(), depth_layout.clone()], &sao_shader, "estimate", AO_FORMAT, None);
    let ao_blur_h = queue("ppv2_sao_blur_h", sl.clone(), &sao_shader, "blur_h", AO_FORMAT, None);
    let ao_blur_v = queue("ppv2_sao_blur_v", sl.clone(), &sao_shader, "blur_v", AO_FORMAT, None);
    // Blend Zero OneMinusSrcColor, Zero OneMinusSrcAlpha
    let multiply = BlendState {
        color: BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::OneMinusSrc, operation: BlendOperation::Add },
        alpha: BlendComponent {
            src_factor: BlendFactor::Zero,
            dst_factor: BlendFactor::OneMinusSrcAlpha,
            operation: BlendOperation::Add,
        },
    };
    let ao_debug = queue("ppv2_sao_debug", sl.clone(), &sao_shader, "composite_debug", VIEW_FORMAT, None);
    let ao_composite = queue("ppv2_sao_composite", sl, &sao_shader, "composite", VIEW_FORMAT, Some(multiply));
    commands.insert_resource(Ppv2Pipelines {
        bloom_layout,
        sao_layout,
        depth_layout,
        sampler,
        prefilter,
        downsample,
        upsample,
        uber,
        copy,
        ao_estimate,
        ao_blur_h,
        ao_blur_v,
        ao_composite,
        ao_debug,
    });
}

#[derive(Component)]
struct BloomView {
    /// The frame before this camera's opaque pass (what the previous cameras drew).
    before: CachedTexture,
    down: Vec<CachedTexture>,
    up: Vec<CachedTexture>,
    offset: u32,
}

#[derive(Component)]
pub(crate) struct SaoView {
    mask: CachedTexture,
    blur: CachedTexture,
    result: CachedTexture,
    offset: u32,
}

fn tex(cache: &mut TextureCache, device: &RenderDevice, label: &'static str, size: UVec2, format: TextureFormat) -> CachedTexture {
    cache.get(
        device,
        TextureDescriptor {
            label: Some(label),
            size: Extent3d { width: size.x.max(1), height: size.y.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
    )
}

/// `Mathf.GammaToLinearSpace`.
fn gamma_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else if c < 1.0 {
        ((c + 0.055) / 1.055).powf(2.4)
    } else {
        c.powf(2.2)
    }
}

fn prepare(
    mut commands: Commands,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut cache: ResMut<TextureCache>,
    mut uniforms: ResMut<Ppv2Uniforms>,
    views: Query<(Entity, &ExtractedCamera, &ExtractedView, Option<&Ppv2Bloom>, Option<&Ppv2Sao>)>,
) {
    let uniforms = &mut *uniforms;
    uniforms.bloom.clear();
    uniforms.sao.clear();
    for (e, cam, view, bloom, sao) in &views {
        let Some(size) = cam.physical_viewport_size else { continue };
        if let Some(b) = bloom {
            // Bloom.Render: half-res pyramid (anamorphicRatio 0).
            let (tw, th) = ((size.x as f32 / 2.0).floor() as u32, (size.y as f32 / 2.0).floor() as u32);
            let s = tw.max(th).max(1) as f32;
            let logs = s.log2() + b.diffusion.min(10.0) - 10.0;
            let logs_i = logs.floor();
            let iterations = (logs_i as i32).clamp(1, 16) as usize;
            let sample_scale = 0.5 + logs - logs_i;
            let lthresh = gamma_to_linear(b.threshold);
            let knee = lthresh * b.soft_knee + 1e-5;
            let lclamp = gamma_to_linear(b.clamp);
            let intensity = (b.intensity / 10.0).exp2() - 1.0;
            let color = Vec3::new(gamma_to_linear(b.color.x), gamma_to_linear(b.color.y), gamma_to_linear(b.color.z));
            let offset = uniforms.bloom.push(&BloomUniform {
                threshold: Vec4::new(lthresh, lthresh - knee, knee * 2.0, 0.25 / knee),
                params: Vec4::new(lclamp, sample_scale, intensity, if b.clamp_output { 1.0 } else { 0.0 }),
                color: color.extend(1.0),
            });
            let (mut w, mut h) = (tw.max(1), th.max(1));
            let (mut down, mut up) = (Vec::new(), Vec::new());
            for _ in 0..iterations {
                down.push(tex(&mut cache, &device, "ppv2_bloom_down", UVec2::new(w, h), PYRAMID_FORMAT));
                up.push(tex(&mut cache, &device, "ppv2_bloom_up", UVec2::new(w, h), PYRAMID_FORMAT));
                w = (w / 2).max(1);
                h = (h / 2).max(1);
            }
            let before = tex(&mut cache, &device, "ppv2_bloom_before", size, VIEW_FORMAT);
            commands.entity(e).insert(BloomView { before, down, up, offset });
        }
        if let Some(a) = sao {
            // ScalableAO.Render: quarter-res AO buffer unless High/Ultra; sample counts {4, 6, 10, 8, 12}.
            let downsampling = a.quality < 3;
            let ts = if downsampling { 2 } else { 1 };
            let samples = [4.0, 6.0, 10.0, 8.0, 12.0][a.quality.min(4) as usize];
            let clip = view.clip_from_view;
            let color = Vec3::ONE - Vec3::new(gamma_to_linear(a.color.x), gamma_to_linear(a.color.y), gamma_to_linear(a.color.z));
            let offset = uniforms.sao.push(&SaoUniform {
                ao_params: Vec4::new(a.intensity, a.radius.max(1e-4), if downsampling { 0.5 } else { 1.0 }, samples),
                ao_color: color.extend(1.0),
                proj: Vec4::new(clip.x_axis.x, clip.y_axis.y, clip.w_axis.z, 0.0),
                screen: Vec4::new(size.x as f32, size.y as f32, 0.0, 0.0),
                view_from_world: view.world_from_view.to_matrix().inverse(),
            });
            commands.entity(e).insert(SaoView {
                mask: tex(&mut cache, &device, "ppv2_sao_mask", size / ts, AO_FORMAT),
                blur: tex(&mut cache, &device, "ppv2_sao_blur", size, AO_FORMAT),
                result: tex(&mut cache, &device, "ppv2_sao_result", size, AO_FORMAT),
                offset,
            });
        }
    }
    uniforms.bloom.write_buffer(&device, &queue);
    uniforms.sao.write_buffer(&device, &queue);
}

fn fullscreen_pass(
    ctx: &mut RenderContext,
    label: &'static str,
    target: &TextureView,
    pipeline: &RenderPipeline,
    groups: &[(&BindGroup, &[u32])],
) {
    let mut pass = ctx.command_encoder().begin_render_pass(&RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: Operations::default(),
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    for (i, (g, offs)) in groups.iter().enumerate() {
        pass.set_bind_group(i as u32, *g, offs);
    }
    pass.draw(0..3, 0..1);
}


fn bloom(
    view: ViewQuery<(&ViewTarget, &BloomView)>,
    pipelines: Option<Res<Ppv2Pipelines>>,
    cache: Res<PipelineCache>,
    uniforms: Res<Ppv2Uniforms>,
    mut ctx: RenderContext,
) {
    let (target, bv) = view.into_inner();
    let Some(p) = pipelines else { return };
    if target.main_texture_format() != VIEW_FORMAT {
        return;
    }
    let (Some(prefilter), Some(downsample), Some(upsample), Some(uber), Some(ub)) = (
        cache.get_render_pipeline(p.prefilter),
        cache.get_render_pipeline(p.downsample),
        cache.get_render_pipeline(p.upsample),
        cache.get_render_pipeline(p.uber),
        uniforms.bloom.binding(),
    ) else {
        return;
    };
    let layout = cache.get_bind_group_layout(&p.bloom_layout);
    let device = ctx.render_device().clone();
    let bg = |a: &TextureView, b: &TextureView| {
        device.create_bind_group("ppv2_bloom_bg", &layout, &BindGroupEntries::sequential((a, b, &p.sampler, ub.clone())))
    };
    let offs = [bv.offset];
    let n = bv.down.len();

    // Prefilter13: this camera's own pixels -> down[0]. The game's frame is not bloomed by the stacked cameras
    // (user: almost no bloom off the sand; with the whole frame bloomed the sand clips white): only what the camera
    // itself draws (viewmodel / HUD) feeds the bloom, which is then added over the whole frame.
    let src = target.main_texture_view();
    let g = bg(src, &bv.before.default_view);
    fullscreen_pass(&mut ctx, "ppv2_bloom_prefilter", &bv.down[0].default_view, prefilter, &[(&g, &offs)]);
    // Downsample13
    for i in 1..n {
        let s = &bv.down[i - 1].default_view;
        let g = bg(s, s);
        fullscreen_pass(&mut ctx, "ppv2_bloom_downsample", &bv.down[i].default_view, downsample, &[(&g, &offs)]);
    }
    // UpsampleTent: up[i] = tent(lastUp) + down[i]
    let mut last_up = &bv.down[n - 1].default_view;
    for i in (0..n.saturating_sub(1)).rev() {
        let g = bg(last_up, &bv.down[i].default_view);
        fullscreen_pass(&mut ctx, "ppv2_bloom_upsample", &bv.up[i].default_view, upsample, &[(&g, &offs)]);
        last_up = &bv.up[i].default_view;
    }
    // Uber: colour + tent(lastUp) * intensity * colour
    let post = target.post_process_write();
    let g = bg(post.source, last_up);
    fullscreen_pass(&mut ctx, "ppv2_bloom_uber", post.destination, uber, &[(&g, &offs)]);
}

/// Copies the frame as the previous cameras left it, before this bloom camera draws (see `bloom`).
fn bloom_snapshot(
    view: ViewQuery<(&ViewTarget, &BloomView)>,
    pipelines: Option<Res<Ppv2Pipelines>>,
    cache: Res<PipelineCache>,
    uniforms: Res<Ppv2Uniforms>,
    mut ctx: RenderContext,
) {
    let (target, bv) = view.into_inner();
    let Some(p) = pipelines else { return };
    if target.main_texture_format() != VIEW_FORMAT {
        return;
    }
    let (Some(copy), Some(ub)) = (cache.get_render_pipeline(p.copy), uniforms.bloom.binding()) else { return };
    let layout = cache.get_bind_group_layout(&p.bloom_layout);
    let src = target.main_texture_view();
    let g = ctx.render_device().create_bind_group("ppv2_bloom_copy_bg", &layout, &BindGroupEntries::sequential((src, src, &p.sampler, ub)));
    fullscreen_pass(&mut ctx, "ppv2_bloom_copy", &bv.before.default_view, copy, &[(&g, &[bv.offset])]);
}

pub(crate) fn sao(
    view: ViewQuery<(&ViewTarget, &ViewPrepassTextures, &SaoView, &Ppv2Sao)>,
    pipelines: Option<Res<Ppv2Pipelines>>,
    cache: Res<PipelineCache>,
    uniforms: Res<Ppv2Uniforms>,
    mut ctx: RenderContext,
) {
    let (target, prepass, sv, settings) = view.into_inner();
    let debug = std::env::var("RESTRAFE_SAO_VIEW").is_ok_and(|v| v == settings.debug);
    let Some(p) = pipelines else { return };
    if target.main_texture_format() != VIEW_FORMAT {
        return;
    }
    let (Some(depth), Some(normal)) = (prepass.depth_view(), prepass.normal_view()) else { return };
    let (Some(estimate), Some(blur_h), Some(blur_v), Some(composite), Some(ub)) = (
        cache.get_render_pipeline(p.ao_estimate),
        cache.get_render_pipeline(p.ao_blur_h),
        cache.get_render_pipeline(p.ao_blur_v),
        cache.get_render_pipeline(if debug { p.ao_debug } else { p.ao_composite }),
        uniforms.sao.binding(),
    ) else {
        return;
    };
    let layout = cache.get_bind_group_layout(&p.sao_layout);
    let device = ctx.render_device().clone();
    let bg = |a: &TextureView, b: &TextureView| {
        device.create_bind_group("ppv2_sao_bg", &layout, &BindGroupEntries::sequential((a, b, &p.sampler, ub.clone())))
    };
    let dg = device.create_bind_group("ppv2_sao_depth_bg", &cache.get_bind_group_layout(&p.depth_layout), &BindGroupEntries::single(depth));
    let offs = [sv.offset];

    let g = bg(normal, normal);
    fullscreen_pass(&mut ctx, "ppv2_sao_estimate", &sv.mask.default_view, estimate, &[(&g, &offs), (&dg, &[])]);
    let g = bg(&sv.mask.default_view, normal);
    fullscreen_pass(&mut ctx, "ppv2_sao_blur_h", &sv.blur.default_view, blur_h, &[(&g, &offs)]);
    let g = bg(&sv.blur.default_view, normal);
    fullscreen_pass(&mut ctx, "ppv2_sao_blur_v", &sv.result.default_view, blur_v, &[(&g, &offs)]);
    // Composition onto the camera target (multiply), before transparents.
    let g = bg(&sv.result.default_view, normal);
    let attachment = target.get_color_attachment();
    let mut pass = ctx.command_encoder().begin_render_pass(&RenderPassDescriptor {
        label: Some("ppv2_sao_composite"),
        color_attachments: &[Some(attachment)],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(composite);
    pass.set_bind_group(0, &g, &offs);
    pass.draw(0..3, 0..1);
}
