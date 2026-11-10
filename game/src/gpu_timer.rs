//! Debug: RESTRAFE_GPU_TIMER=1 writes GPU timestamps between the Core3d phases of every camera and prints
//! "GPUTIME total=<ms> cam<i>:<phase>=<ms> ..." once a second and for every frame whose GPU total exceeds 8 ms
//! (RESTRAFE_GPU_TIMER_MS=<ms> changes the threshold). Bevy 0.19's RenderDiagnosticsPlugin only times a few passes.

use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::core_pipeline::upscaling::upscaling;
use bevy::prelude::*;
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const MAX: u32 = 128;
const RING: usize = 4;

struct Slot {
    read: wgpu::Buffer,
    labels: Vec<String>,
    mapped: Arc<AtomicBool>,
    busy: bool,
}

#[derive(Resource)]
struct GpuTimer {
    qs: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    slots: Vec<Slot>,
    cur: usize,
    labels: Vec<String>,
    cams: Vec<Entity>,
    period_ns: f32,
    last_print: std::time::Instant,
    frame: u64,
}

pub struct GpuTimerPlugin;

impl Plugin for GpuTimerPlugin {
    fn build(&self, app: &mut App) {
        // Debug: RESTRAFE_CORE3D_AMBIGUITY=1 logs unordered system pairs of the Core3d (per-camera render) schedule.
        if std::env::var_os("RESTRAFE_CORE3D_AMBIGUITY").is_some() {
            if let Some(r) = app.get_sub_app_mut(RenderApp) {
                r.edit_schedule(Core3d, |s| {
                    let mut b = s.get_build_settings().clone();
                    b.ambiguity_detection = bevy::ecs::schedule::LogLevel::Warn;
                    s.set_build_settings(b);
                });
            }
        }
        if std::env::var_os("RESTRAFE_GPU_TIMER").is_none() {
            return;
        }
        let Some(r) = app.get_sub_app_mut(RenderApp) else { return };
        r.add_systems(RenderStartup, init)
            .add_systems(Render, (resolve.after(RenderSystems::Render).before(RenderSystems::Cleanup), readback.in_set(RenderSystems::Cleanup)))
            .add_systems(
                Core3d,
                (
                    mark("begin").before(Core3dSystems::Prepass),
                    mark("prepass+shadow").after(Core3dSystems::Prepass).before(Core3dSystems::MainPass),
                    mark("deferred_light").in_set(Core3dSystems::MainPass).after(bevy::pbr::deferred::deferred_lighting).before(main_opaque_pass_3d),
                    mark("opaque").in_set(Core3dSystems::MainPass).after(main_opaque_pass_3d).before(crate::ppv2::sao),
                    mark("sao+ssr").in_set(Core3dSystems::MainPass).after(crate::ppv2_ssr::ssr).before(main_transparent_pass_3d),
                    mark("transparent").after(main_transparent_pass_3d).before(Core3dSystems::EarlyPostProcess),
                    mark("post").after(Core3dSystems::PostProcess).before(upscaling),
                    mark("upscale").after(upscaling),
                ),
            );
    }
}

fn init(mut commands: Commands, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    let d = device.wgpu_device();
    if !d.features().contains(wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS) {
        println!("GPUTIME unsupported: device lacks TIMESTAMP_QUERY(_INSIDE_ENCODERS)");
        return;
    }
    let qs = d.create_query_set(&wgpu::QuerySetDescriptor { label: Some("gpu_timer"), ty: wgpu::QueryType::Timestamp, count: MAX });
    let size = (MAX as u64) * 8;
    let resolve = d.create_buffer(&wgpu::BufferDescriptor { label: Some("gpu_timer_resolve"), size, usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    let slots = (0..RING)
        .map(|_| Slot {
            read: d.create_buffer(&wgpu::BufferDescriptor { label: Some("gpu_timer_read"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
            labels: Vec::new(),
            mapped: Arc::new(AtomicBool::new(false)),
            busy: false,
        })
        .collect();
    let period_ns = queue.get_timestamp_period();
    commands.insert_resource(GpuTimer { qs, resolve, slots, cur: 0, labels: Vec::new(), cams: Vec::new(), period_ns, last_print: std::time::Instant::now(), frame: 0 });
}

fn mark(label: &'static str) -> impl FnMut(ViewQuery<(Entity, Option<&bevy::render::camera::ExtractedCamera>)>, Option<ResMut<GpuTimer>>, RenderContext) {
    move |view: ViewQuery<(Entity, Option<&bevy::render::camera::ExtractedCamera>)>, t: Option<ResMut<GpuTimer>>, mut ctx: RenderContext| {
        let Some(mut t) = t else { return };
        let n = t.labels.len() as u32;
        if n >= MAX || t.slots[t.cur].busy {
            return;
        }
        let (cam, ec) = view.into_inner();
        let order = ec.map(|c| c.order).unwrap_or(-999);
        let ci = match t.cams.iter().position(|c| *c == cam) {
            Some(i) => i,
            None => {
                t.cams.push(cam);
                t.cams.len() - 1
            }
        };
        ctx.command_encoder().write_timestamp(&t.qs, n);
        t.labels.push(format!("cam{ci}(o{order}):{label}"));
    }
}

fn resolve(t: Option<ResMut<GpuTimer>>, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    let Some(mut t) = t else { return };
    let n = t.labels.len() as u32;
    let cur = t.cur;
    if n < 2 || t.slots[cur].busy {
        t.labels.clear();
        return;
    }
    let mut enc = device.wgpu_device().create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("gpu_timer_resolve") });
    enc.resolve_query_set(&t.qs, 0..n, &t.resolve, 0);
    enc.copy_buffer_to_buffer(&t.resolve, 0, &t.slots[cur].read, 0, n as u64 * 8);
    queue.submit([enc.finish()]);
    let labels = std::mem::take(&mut t.labels);
    let slot = &mut t.slots[cur];
    slot.labels = labels;
    slot.busy = true;
    slot.mapped.store(false, Ordering::SeqCst);
    let flag = slot.mapped.clone();
    slot.read.slice(..n as u64 * 8).map_async(wgpu::MapMode::Read, move |r| {
        if r.is_ok() {
            flag.store(true, Ordering::SeqCst);
        }
    });
    t.cur = (cur + 1) % RING;
    t.cams.clear();
    t.frame += 1;
}

fn readback(t: Option<ResMut<GpuTimer>>, device: Res<RenderDevice>) {
    let Some(mut t) = t else { return };
    let _ = device.wgpu_device().poll(wgpu::PollType::Poll);
    let thr: f32 = std::env::var("RESTRAFE_GPU_TIMER_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(8.0);
    let period = t.period_ns;
    let mut lines = Vec::new();
    for slot in t.slots.iter_mut() {
        if !slot.busy || !slot.mapped.load(Ordering::SeqCst) {
            continue;
        }
        let n = slot.labels.len();
        let ts: Vec<u64> = {
            let data = slot.read.slice(..n as u64 * 8).get_mapped_range();
            data.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect()
        };
        slot.read.unmap();
        slot.busy = false;
        let ms = |a: u64, b: u64| b.saturating_sub(a) as f32 * period / 1e6;
        let total = ms(ts[0], ts[n - 1]);
        let mut parts = String::new();
        for i in 1..n {
            let d = ms(ts[i - 1], ts[i]);
            if d >= 0.05 {
                parts += &format!(" {}={d:.2}", slot.labels[i]);
            }
        }
        lines.push((total, parts));
    }
    for (total, parts) in lines {
        if total > thr || t.last_print.elapsed().as_secs_f32() > 1.0 {
            t.last_print = std::time::Instant::now();
            println!("GPUTIME frame={} total={total:.2}ms{parts}", t.frame);
        }
    }
}
