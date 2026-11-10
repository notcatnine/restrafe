//! PPv2 ScreenSpaceReflections (Camera Profile: enabled, preset Medium, maximumMarchDistance 100, distanceFade 0.5,
//! vignette 0.5), run on the PC deferred path (GraphicsSettings tier 3 renders Deferred). Port of Unity
//! PostProcessing v2 `ScreenSpaceReflections.cs` / `.hlsl` (see `assets/shaders/ppv2_ssr.wgsl`) on Bevy's deferred
//! G-buffer. Order as `ScreenSpaceReflections.Render`: Test -> Resolve -> Reproject (history) -> blur pyramid ->
//! Composite, between the opaque pass (after SAO) and the transparents (PPv2 opaque-only effect).

use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::core_pipeline::FullscreenShader;
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::binding_types::{self, sampler, texture_2d, texture_depth_2d, uniform_buffer};
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::render_asset::RenderAssets;
use bevy::render::sync_world::RenderEntity;
use bevy::render::texture::{FallbackImage, GpuImage};
use bevy::render::{Extract, ExtractSchedule, RenderApp, RenderStartup};
use std::collections::HashMap;
use std::sync::Mutex;

/// On a camera with `DeferredPrepass` + `MotionVectorPrepass`: render PPv2 SSR.
#[derive(Component, Clone, Copy, ExtractComponent)]
pub struct Ppv2Ssr {
    pub max_iterations: f32,
    pub thickness: f32,
    /// Square POT target: ClosestPowerOfTwo(min(w, h)) >> 1 for `Downsampled` presets.
    pub downsampled: bool,
    pub max_march_distance: f32,
    pub distance_fade: f32,
    pub vignette: f32,
}

impl Ppv2Ssr {
    /// Camera Profile: preset Medium (32 iterations, thickness 16, Downsampled), overrides 100 / 0.5 / 0.5.
    pub const PROFILE: Self = Self { max_iterations: 32.0, thickness: 16.0, downsampled: true, max_march_distance: 100.0, distance_fade: 0.5, vignette: 0.5 };
}

#[derive(ShaderType, Clone, Copy, Default)]
struct SsrUniform {
    view_from_world: Mat4,
    world_from_view: Mat4,
    view_from_clip: Mat4,
    clip_from_view: Mat4,
    params: Vec4,
    params2: Vec4,
    sizes: Vec4,
    misc: Vec4,
}

const FMT: TextureFormat = TextureFormat::Rgba16Float;

#[derive(Resource)]
pub struct SsrPipelines {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    test: CachedRenderPipelineId,
    resolve: CachedRenderPipelineId,
    reproject: CachedRenderPipelineId,
    downsample: CachedRenderPipelineId,
    composite: CachedRenderPipelineId,
}

/// Render world: the camera's EnvironmentMapLight specular cubemap (Unity _CameraReflectionsTexture source).
#[derive(Component)]
pub struct SsrEnv(Handle<Image>);

fn extract_env(mut commands: Commands, q: Extract<Query<(RenderEntity, Option<&bevy::light::EnvironmentMapLight>), With<Ppv2Ssr>>>) {
    for (e, env) in &q {
        let mut ec = commands.entity(e);
        match env {
            Some(env) => {
                ec.insert(SsrEnv(env.specular_map.clone()));
            }
            None => {
                ec.remove::<SsrEnv>();
            }
        }
    }
}

/// PostProcessResources.blueNoise256[0] (LDR_LLL1_0_0: linear, point, repeat).
#[derive(Resource, Clone, bevy::render::extract_resource::ExtractResource)]
pub struct SsrNoise(Handle<Image>);

fn load_noise(mut commands: Commands, assets: Res<AssetServer>) {
    let h = assets.load_builder().with_settings(|s: &mut bevy::image::ImageLoaderSettings| s.is_srgb = false).load("ppv2/LDR_LLL1_0_0.png");
    commands.insert_resource(SsrNoise(h));
}

/// Per-view persistent targets (history must survive frames).
#[derive(Resource, Default)]
pub struct SsrTargets(Mutex<HashMap<Entity, (u32, Texture, Texture, Texture, Texture)>>);

pub struct Ppv2SsrPlugin;

impl Plugin for Ppv2SsrPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((ExtractComponentPlugin::<Ppv2Ssr>::default(), bevy::render::extract_resource::ExtractResourcePlugin::<SsrNoise>::default()))
            .add_systems(Startup, load_noise);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        if std::env::var_os("RESTRAFE_DEBUG_PIPES").is_some() {
            render_app.add_systems(bevy::render::Render, debug_pipes.in_set(bevy::render::RenderSystems::Render));
        }
        render_app.init_resource::<SsrTargets>().add_systems(ExtractSchedule, extract_env).add_systems(RenderStartup, init).add_systems(
            Core3d,
            ssr.after(main_opaque_pass_3d).after(crate::ppv2::sao).before(main_transparent_pass_3d).in_set(Core3dSystems::MainPass),
        );
    }
}

fn init(mut commands: Commands, device: Res<RenderDevice>, cache: Res<PipelineCache>, assets: Res<AssetServer>, fullscreen: Res<FullscreenShader>) {
    let layout = BindGroupLayoutDescriptor::new(
        "ppv2_ssr_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                uniform_buffer::<SsrUniform>(false),
                texture_depth_2d(),
                texture_2d(TextureSampleType::Uint),
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                texture_2d(TextureSampleType::Float { filterable: true }),
                binding_types::texture_cube(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
            ),
        ),
    );
    let sampler = device.create_sampler(&SamplerDescriptor {
        label: Some("ppv2_ssr_trilinear"),
        min_filter: FilterMode::Linear,
        mag_filter: FilterMode::Linear,
        mipmap_filter: MipmapFilterMode::Linear,
        address_mode_u: AddressMode::ClampToEdge,
        address_mode_v: AddressMode::ClampToEdge,
        ..default()
    });
    let shader: Handle<Shader> = assets.load("shaders/ppv2_ssr.wgsl");
    let q = |label: &'static str, entry: &'static str, blend: Option<BlendState>| {
        cache.queue_render_pipeline(RenderPipelineDescriptor {
            label: Some(label.into()),
            layout: vec![layout.clone()],
            vertex: fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: shader.clone(),
                entry_point: Some(entry.into()),
                targets: vec![Some(ColorTargetState { format: FMT, blend, write_mask: ColorWrites::ALL })],
                ..default()
            }),
            ..default()
        })
    };
    commands.insert_resource(SsrPipelines {
        test: q("ppv2_ssr_test", "test", None),
        resolve: q("ppv2_ssr_resolve", "resolve", None),
        reproject: q("ppv2_ssr_reproject", "reproject", None),
        downsample: q("ppv2_ssr_downsample", "downsample", None),
        composite: q("ppv2_ssr_composite", "composite", None),
        layout,
        sampler,
    });
}

fn tex(device: &RenderDevice, label: &'static str, size: u32, mips: u32) -> Texture {
    device.create_texture(&TextureDescriptor {
        label: Some(label),
        size: Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        mip_level_count: mips,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: FMT,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC | TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn pass(ctx: &mut RenderContext, label: &'static str, target: &TextureView, pipeline: &RenderPipeline, bg: &BindGroup, clear: bool) {
    let mut p = ctx.command_encoder().begin_render_pass(&RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: Operations { load: if clear { LoadOp::Clear(Default::default()) } else { LoadOp::Load }, store: StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    p.set_pipeline(pipeline);
    p.set_bind_group(0, bg, &[]);
    p.draw(0..3, 0..1);
}

#[allow(clippy::too_many_arguments)]
pub fn ssr(
    view: ViewQuery<(Entity, &ViewTarget, &ViewPrepassTextures, &ExtractedView, &Ppv2Ssr, Option<&SsrEnv>)>,
    images: Res<RenderAssets<GpuImage>>,
    noise: Option<Res<SsrNoise>>,
    fallback: Res<FallbackImage>,
    pipelines: Option<Res<SsrPipelines>>,
    cache: Res<PipelineCache>,
    targets: Res<SsrTargets>,
    queue: Res<RenderQueue>,
    mut ctx: RenderContext,
) {
    let (entity, target, prepass, ev, s, env) = view.into_inner();
    let env_gpu = env.and_then(|e| images.get(&e.0));
    let cube = env_gpu.map(|g| &g.texture_view).unwrap_or(&fallback.cube.texture_view);
    let env_mips = env_gpu.map(|g| g.texture_descriptor.mip_level_count as f32).unwrap_or(1.0);
    let has_env = if env_gpu.is_some() { 1.0 } else { 0.0 };
    let noise_view = noise.as_ref().and_then(|n| images.get(&n.0)).map(|g| &g.texture_view).unwrap_or(&fallback.d2.texture_view);
    let Some(p) = pipelines else { return };
    // Off by default until the user-reported flicker (gone with SSR off) is fixed against the original; RESTRAFE_SSR=1 enables.
    if std::env::var_os("RESTRAFE_SSR").is_none() || std::env::var_os("RESTRAFE_NO_SSR").is_some() || target.main_texture_format() != FMT {
        return;
    }
    let (Some(depth), Some(gbuf), Some(mv)) = (prepass.depth_view(), prepass.deferred_view(), prepass.motion_vectors_view()) else { return };
    let (Some(test_p), Some(resolve_p), Some(reproj_p), Some(down_p), Some(comp_p)) = (
        cache.get_render_pipeline(p.test),
        cache.get_render_pipeline(p.resolve),
        cache.get_render_pipeline(p.reproject),
        cache.get_render_pipeline(p.downsample),
        cache.get_render_pipeline(p.composite),
    ) else {
        return;
    };
    let device = ctx.render_device().clone();
    let main_size = target.main_texture().size();
    let (w, h) = (main_size.width, main_size.height);
    // Mathf.ClosestPowerOfTwo(min(w, h)), >> 1 when Downsampled.
    let m = w.min(h).max(1);
    let up = m.next_power_of_two();
    let mut size = if up - m <= m - up / 2 { up } else { up / 2 };
    if s.downsampled {
        size >>= 1;
    }
    let lod_count = (((size as f32).log2() - 3.0).floor() as u32).clamp(1, 12);
    let mut map = targets.0.lock().unwrap();
    let fresh = map.get(&entity).is_none_or(|t| t.0 != size);
    if fresh {
        map.insert(
            entity,
            (size, tex(&device, "ppv2_ssr_test", size, 1), tex(&device, "ppv2_ssr_resolve_tmp", size, 1), tex(&device, "ppv2_ssr_resolve", size, lod_count + 1), tex(&device, "ppv2_ssr_history", size, 1)),
        );
    }
    let (_, test_t, tmp_t, res_t, hist_t) = map.get(&entity).unwrap();
    let world_from_view = ev.world_from_view.to_matrix();
    let clip_from_view = ev.clip_from_view;
    let u = SsrUniform {
        view_from_world: world_from_view.inverse(),
        world_from_view,
        view_from_clip: clip_from_view.inverse(),
        clip_from_view,
        params: Vec4::new(s.vignette, s.distance_fade, s.max_march_distance, lod_count as f32),
        params2: Vec4::new(w as f32 / h as f32, size as f32 / 256.0, s.thickness, s.max_iterations),
        sizes: Vec4::new(size as f32, size as f32, w as f32, h as f32),
        // reverse-Z infinite perspective: clip_from_view[3][2] = near
        // x near, y env cubemap mip count, z has env
        misc: Vec4::new(clip_from_view.w_axis.z, env_mips, has_env, 0.0),
    };
    let mut ub = UniformBuffer::from(u);
    ub.write_buffer(&device, &queue);
    let Some(ub) = ub.binding() else { return };
    let layout = cache.get_bind_group_layout(&p.layout);
    let view_of = |t: &Texture, mip: u32| t.create_view(&TextureViewDescriptor { base_mip_level: mip, mip_level_count: Some(1), ..default() });
    let bg = |a: &TextureView, b: &TextureView| {
        device.create_bind_group("ppv2_ssr_bg", &layout, &BindGroupEntries::sequential((ub.clone(), depth, gbuf, a, b, &p.sampler, mv, cube, noise_view)))
    };
    let scene = target.main_texture_view();
    let test_v = view_of(test_t, 0);
    let tmp_v = view_of(tmp_t, 0);
    let res0 = view_of(res_t, 0);
    let hist_v = view_of(hist_t, 0);
    // Test (from the opaque scene's depth / G-buffer).
    pass(&mut ctx, "ppv2_ssr_test", &test_v, test_p, &bg(scene, scene), true);
    // Resolve: scene colour at the hit uv * confidence.
    pass(&mut ctx, "ppv2_ssr_resolve", &tmp_v, resolve_p, &bg(&test_v, scene), true);
    if fresh {
        // m_ResetHistory: history = source.
        ctx.command_encoder().copy_texture_to_texture(tmp_t.as_image_copy(), hist_t.as_image_copy(), Extent3d { width: size, height: size, depth_or_array_layers: 1 });
    }
    // Reproject into the pyramid's mip 0, then history = mip 0.
    pass(&mut ctx, "ppv2_ssr_reproject", &res0, reproj_p, &bg(&tmp_v, &hist_v), true);
    ctx.command_encoder().copy_texture_to_texture(res_t.as_image_copy(), hist_t.as_image_copy(), Extent3d { width: size, height: size, depth_or_array_layers: 1 });
    // Blur pyramid.
    for i in 1..=lod_count {
        let src = view_of(res_t, i - 1);
        let dst = view_of(res_t, i);
        pass(&mut ctx, "ppv2_ssr_downsample", &dst, down_p, &bg(&src, &src), true);
    }
    // Composite: reads the scene colour (post-process ping-pong) and writes scene + SSR term, never darker than half
    // the lit colour (an additive blend let a bad probe estimate drive every lit pixel to black: user-reported flashes).
    let full = res_t.create_view(&TextureViewDescriptor::default());
    let post = target.post_process_write();
    let g = bg(&full, post.source);
    let mut rp = ctx.command_encoder().begin_render_pass(&RenderPassDescriptor {
        label: Some("ppv2_ssr_composite"),
        color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations { load: LoadOp::Clear(Default::default()), store: StoreOp::Store } })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    rp.set_pipeline(comp_p);
    rp.set_bind_group(0, &g, &[]);
    rp.draw(0..3, 0..1);
}

/// RESTRAFE_DEBUG_PIPES=1: frames with pipelines still compiling (their meshes / passes are skipped that frame).
fn debug_pipes(cache: Res<PipelineCache>, mut frame: Local<u32>, mut seen: Local<usize>) {
    *frame += 1;
    let total = cache.pipelines().count();
    if total > *seen {
        let new: Vec<String> = cache.pipelines().skip(*seen).map(|p| match &p.descriptor {
            bevy::material::descriptor::PipelineDescriptor::RenderPipelineDescriptor(d) => format!("{:?}", d.label),
            bevy::material::descriptor::PipelineDescriptor::ComputePipelineDescriptor(d) => format!("{:?}", d.label),
        }).collect();
        println!("NEWPIPES frame={} total={total} {:?}", *frame, new);
        if *frame > 100 {
            for p in cache.pipelines().skip(*seen) {
                if let bevy::material::descriptor::PipelineDescriptor::RenderPipelineDescriptor(d) = &p.descriptor {
                    let vdefs: Vec<String> = d.vertex.shader_defs.iter().map(|x| format!("{x:?}")).collect();
                    let attrs: Vec<String> = d.vertex.buffers.iter().flat_map(|b| b.attributes.iter().map(|a| format!("{}:{:?}", a.shader_location, a.format))).collect();
                    println!("  NEWPIPE {:?} vdefs={} attrs={:?}", d.label, vdefs.join(","), attrs);
                }
            }
        }
        *seen = total;
    }
    let n = cache.waiting_pipelines().count();
    if n > 0 {
        let mut labels: Vec<String> = Vec::new();
        for id in cache.waiting_pipelines() {
            if let Some(p) = cache.pipelines().nth(id) {
                let (l, defs) = match &p.descriptor {
                    bevy::material::descriptor::PipelineDescriptor::RenderPipelineDescriptor(d) => (d.label.clone(), d.fragment.as_ref().map(|f| f.shader_defs.len()).unwrap_or(0)),
                    bevy::material::descriptor::PipelineDescriptor::ComputePipelineDescriptor(d) => (d.label.clone(), 0),
                };
                labels.push(format!("{:?}/{defs}", l.map(|l| l.to_string())));
                if let bevy::material::descriptor::PipelineDescriptor::RenderPipelineDescriptor(d) = &p.descriptor {
                    if d.label.as_deref() == Some("deferred_lighting_pipeline") {
                        println!("DLDEFS frame={} {:?}", *frame, d.fragment.as_ref().map(|f| &f.shader_defs));
                    }
                }
            }
        }
        println!("PIPES frame={} waiting={n} {:?}", *frame, labels);
    }
}
