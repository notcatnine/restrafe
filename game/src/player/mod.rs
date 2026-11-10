pub mod fpc;

use bevy::prelude::*;
use bevy_rapier3d::prelude::PhysicsSet;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<fpc::FpcSound>()
            .add_systems(Update, (fpc::fpc_update.after(crate::input::gather_input), fpc::slide_dust_fx.after(fpc::fpc_update), fpc::high_fall_vfx.after(fpc::fpc_update), fpc::local_body_anim.after(fpc::fpc_update), crate::health::local_graphics_skin.after(fpc::fpc_update)))
            // DOTween runs after gameplay scripts; CameraShakeConstrains after that.
            .add_systems(
                PostUpdate,
                (crate::tween::update_tweens, fpc::camera_post_update)
                    .chain()
                    .before(PhysicsSet::SyncBackend)
                    .before(TransformSystems::Propagate),
            );
    }
}
