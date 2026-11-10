//! GrabPass for `S_DistortionRefraction_00` (grenade / rocket / explosion shockwave spheres, Gust / Gamma /
//! Tromblon muzzle shockwaves).
//!
//! The Unity shader has an unnamed `GrabPass {}` before its forward pass, i.e. the screen is copied to
//! `_GrabTexture` right before each shockwave draws, after everything earlier in the queue order (the opaque
//! scene and the other transparents of the main camera: the materials use queue 3001 and 3000). Bevy's
//! screen-space transmission copies the main texture *before* the transparent pass, so the distortion material
//! (`ParticleMat`, `PMode::Distortion`: Opaque + `reads_view_transmission_texture`) is queued into
//! `Transmissive3d` as usual, Bevy's own transmissive pass is disabled for every view (its
//! `ScreenSpaceTransmission` is stripped in the render world after the transmission texture was prepared), and
//! [`distortion_pass`] copies the main texture into the view's transmission texture and draws the phase after
//! `main_transparent_pass_3d`, before the camera's post-processing (= Unity, where the grab happens inside the
//! camera's render before PPv2).

use bevy::core_pipeline::core_3d::main_transparent_pass_3d;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::pbr::{ScreenSpaceTransmission, Transmissive3d, ViewTransmissionTexture};
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::render_phase::ViewSortedRenderPhases;
use bevy::render::render_resource::{Extent3d, RenderPassDescriptor, StoreOp};
use bevy::render::renderer::{RenderContext, ViewQuery};
use bevy::render::view::{ExtractedView, ViewDepthTexture, ViewTarget};
use bevy::render::{Render, RenderApp, RenderSystems};

pub struct DistortionPlugin;

impl Plugin for DistortionPlugin {
    fn build(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .add_systems(Render, strip_bevy_transmission.in_set(RenderSystems::PrepareBindGroups))
            .add_systems(Core3d, distortion_pass.after(main_transparent_pass_3d).in_set(Core3dSystems::MainPass));
    }
}

/// Bevy's `main_transmissive_pass_3d` needs `ScreenSpaceTransmission` on the view; removing it (re-extracted
/// every frame) after `prepare_core_3d_transmission_textures` keeps the texture but skips Bevy's pass.
fn strip_bevy_transmission(world: &mut World) {
    let views: Vec<Entity> = world.query_filtered::<Entity, With<ScreenSpaceTransmission>>().iter(world).collect();
    for e in views {
        world.entity_mut(e).remove::<ScreenSpaceTransmission>();
    }
}

fn distortion_pass(
    world: &World,
    view: ViewQuery<(&ExtractedCamera, &ExtractedView, &ViewTarget, &ViewTransmissionTexture, &ViewDepthTexture)>,
    phases: Res<ViewSortedRenderPhases<Transmissive3d>>,
    mut ctx: RenderContext,
) {
    let view_entity = view.entity();
    let (camera, extracted_view, target, grab, depth) = view.into_inner();
    let Some(phase) = phases.get(&extracted_view.retained_view_entity) else {
        return;
    };
    if phase.items.is_empty() {
        return;
    }
    let Some(size) = camera.physical_target_size else {
        return;
    };
    // GrabPass: copy the colour rendered so far.
    ctx.command_encoder().copy_texture_to_texture(
        target.main_texture().as_image_copy(),
        grab.texture.as_image_copy(),
        Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 },
    );
    let desc = RenderPassDescriptor {
        label: Some("restrafe_distortion_pass"),
        color_attachments: &[Some(target.get_color_attachment())],
        depth_stencil_attachment: Some(depth.get_attachment(StoreOp::Store)),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    };
    let mut pass = ctx.begin_tracked_render_pass(desc);
    if let Some(viewport) = camera.viewport.as_ref() {
        pass.set_camera_viewport(viewport);
    }
    if let Err(err) = phase.render(&mut pass, world, view_entity) {
        error!("distortion pass: {err:?}");
    }
}
