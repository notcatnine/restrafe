//! Unity's transparent draw order for the 3D transparent phase.
//!
//! Unity (built-in pipeline) draws the transparent queues in render-queue order first, then, within a queue, back to
//! front by the distance from the camera position to the renderer's bounds centre (Camera.transparencySortMode
//! Default = Perspective for perspective cameras: "distance from camera position to the object center"). Bevy sorts
//! `Transparent3d` by the view-space depth of the mesh AABB centre, which differs for objects off the view axis.
//! This re-sorts every perspective view's phase after Bevy's `sort_phase_system`, before batching:
//! key = (render queue, -|centre - camera| (sign of the view depth kept) + depth bias). Entities without a
//! [`RenderQueue`] are queue 3000 (Transparent). Particles put their AABB centre on the camera axis at their Unity key
//! (vfx/sim.rs), where the Euclidean distance with the depth sign equals the view depth, so their order is unchanged.

use bevy::core_pipeline::core_3d::{Transparent3d, TransparentSortingInfo3d};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_phase::{sort_phase_system, ViewSortedRenderPhases};
use bevy::render::sync_world::MainEntity;
use bevy::render::view::ExtractedView;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSystems};

/// Unity render queue of a transparent renderer (default 3000), e.g. 3001 for the Easy Decal SSD shaders
/// ("Transparent+1", from the shaders' tags in data.unity3d).
#[derive(Component, Clone, Copy)]
pub struct RenderQueue(pub i32);

#[derive(Resource, Default)]
struct Queues(HashMap<MainEntity, i32>);

pub struct TransparentSortPlugin;

impl Plugin for TransparentSortPlugin {
    fn build(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_resource::<Queues>()
            .add_systems(ExtractSchedule, extract_queues)
            .add_systems(Render, unity_sort.after(sort_phase_system::<Transparent3d>).in_set(RenderSystems::PhaseSort));
    }
}

fn extract_queues(mut queues: ResMut<Queues>, q: Extract<Query<(Entity, &RenderQueue)>>) {
    queues.0.clear();
    for (e, rq) in &q {
        queues.0.insert(MainEntity::from(e), rq.0);
    }
}

fn unity_sort(views: Query<&ExtractedView>, mut phases: ResMut<ViewSortedRenderPhases<Transparent3d>>, queues: Res<Queues>) {
    if std::env::var_os("RESTRAFE_BEVY_SORT").is_some() {
        return; // debug: Bevy's view-depth order
    }
    for view in &views {
        // Orthographic views: Unity sorts along the view direction (= Bevy's key).
        if view.clip_from_view.w_axis.w == 1.0 {
            continue;
        }
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else { continue };
        let cam = view.world_from_view.translation();
        let rangefinder = view.rangefinder3d();
        for item in phase.items.values_mut() {
            if let TransparentSortingInfo3d::Sorted { mesh_center, depth_bias } = item.sorting_info {
                let z = rangefinder.distance(&mesh_center);
                let d = (mesh_center - cam).length();
                item.distance = if z > 0.0 { d } else { -d } + depth_bias;
            }
        }
        let queue = |e: &MainEntity| queues.0.get(e).copied().unwrap_or(3000);
        phase.items.sort_by(|ka, a, kb, b| queue(&ka.1).cmp(&queue(&kb.1)).then(a.distance.total_cmp(&b.distance)));
    }
}
