//! Debug: RESTRAFE_FRAME_PROBE=1 prints a phase breakdown of every slow frame (> 20 ms) without the overhead of the
//! chrome trace: "MPROBE" for the main world (First..Update..PostUpdate..Last), "RPROBE" for the render world
//! (between the RenderSystems sets).

use bevy::prelude::*;
use bevy::render::{Render, RenderApp, RenderSystems};
use std::time::Instant;

#[derive(Resource, Default)]
struct Marks(Vec<(&'static str, Option<Instant>)>);

pub struct FrameProbePlugin;

impl Plugin for FrameProbePlugin {
    fn build(&self, app: &mut App) {
        if std::env::var_os("RESTRAFE_FRAME_PROBE").is_none() {
            return;
        }
        app.init_resource::<Marks>()
            .add_systems(First, mark("start"))
            .add_systems(PreUpdate, mark("pre_update"))
            .add_systems(Update, mark("update"))
            .add_systems(PostUpdate, mark("post_update"))
            .add_systems(Last, report("MPROBE"));
        let Some(r) = app.get_sub_app_mut(RenderApp) else { return };
        use RenderSystems as S;
        r.init_resource::<Marks>().add_systems(
            Render,
            (
                mark("start").before(S::ExtractCommands),
                mark("extract_cmds").after(S::ExtractCommands).before(S::PrepareAssets),
                mark("prepare_assets").after(S::PrepareAssets).before(S::PrepareMeshes),
                mark("prepare_meshes").after(S::PrepareMeshes).before(S::CreateViews),
                mark("views").after(S::CreateViews).before(S::Specialize),
                mark("specialize").after(S::Specialize).before(S::PrepareViews),
                mark("prepare_views").after(S::PrepareViews).before(S::Queue),
                mark("queue").after(S::QueueSweep).before(S::PhaseSort),
                mark("sort").after(S::PhaseSort).before(S::Prepare),
                mark("prepare").after(S::PrepareResourcesFlush).before(S::PrepareBindGroups),
                mark("bind_groups").after(S::PrepareBindGroups).before(S::Render),
                mark("render").after(S::Render).before(S::Cleanup),
                report("RPROBE").after(S::Cleanup).before(S::PostCleanup),
            ),
        );
    }
}

fn mark(label: &'static str) -> impl FnMut(ResMut<Marks>) {
    move |mut m: ResMut<Marks>| {
        if label == "start" {
            m.0.clear();
        }
        m.0.push((label, Some(Instant::now())));
    }
}

fn report(tag: &'static str) -> impl FnMut(ResMut<Marks>) {
    move |mut m: ResMut<Marks>| {
        let now = Instant::now();
        m.0.push(("end", Some(now)));
        let Some(Some(t0)) = m.0.first().map(|x| x.1) else { return };
        let total = (now - t0).as_secs_f32() * 1000.0;
        if total > 20.0 {
            let mut s = String::new();
            for w in m.0.windows(2) {
                if let (Some(a), Some(b)) = (w[0].1, w[1].1) {
                    let d = (b - a).as_secs_f32() * 1000.0;
                    if d > 0.5 {
                        s += &format!(" {}={d:.1}", w[0].0);
                    }
                }
            }
            println!("{tag} total={total:.1}ms{s}");
        }
    }
}
