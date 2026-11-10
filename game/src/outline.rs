//! Weapon hover outline: `ItemBehaviour.OnFocus` sets `_OutlineWidth = 0.007` on every material of the hovered
//! weapon (S_WeaponOutline_00 family, Amplify outline: inverted hull, `_Color_Outline` = (1, 0.82, 0.43) on all
//! but one material), `OnLoseFocus` sets it back to 0. Ported as an extra inverted-hull mesh per weapon mesh while
//! the weapon is `Hands::current_interactable` (and not held).

use crate::weapons::pickup::Hands;
use crate::weapons::{Hand, Item};
use bevy::camera::visibility::RenderLayers;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Face, RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::ShaderRef;

const OUTLINE_WIDTH: f32 = 0.007;
const OUTLINE_COLOR: LinearRgba = LinearRgba::new(1.0, 0.819_607_9, 0.427_451, 1.0);

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct OutlineMat {
    #[uniform(0)]
    pub color: LinearRgba,
    #[uniform(0)]
    pub width: f32,
}

impl Material for OutlineMat {
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn vertex_shader() -> ShaderRef {
        "shaders/weapon_outline.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "shaders/weapon_outline.wgsl".into()
    }
    fn specialize(_: &MaterialPipeline, descriptor: &mut RenderPipelineDescriptor, layout: &MeshVertexBufferLayoutRef, _: MaterialPipelineKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        let vertex_layout = layout.0.get_layout(&[Mesh::ATTRIBUTE_POSITION.at_shader_location(0), Mesh::ATTRIBUTE_NORMAL.at_shader_location(1)])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        descriptor.primitive.cull_mode = Some(Face::Front);
        Ok(())
    }
}

/// Marks an outline hull spawned for a hovered weapon.
#[derive(Component)]
struct OutlineHull;

#[derive(Resource)]
struct OutlineHandle(Handle<OutlineMat>);

fn setup(mut commands: Commands, mut mats: ResMut<Assets<OutlineMat>>) {
    commands.insert_resource(OutlineHandle(mats.add(OutlineMat { color: OUTLINE_COLOR, width: OUTLINE_WIDTH })));
}

#[allow(clippy::too_many_arguments)]
fn update(
    mut commands: Commands,
    mat: Res<OutlineHandle>,
    hands: Query<&Hands>,
    items: Query<&Item>,
    children: Query<&Children>,
    meshes: Query<(&Mesh3d, Option<&RenderLayers>), Without<OutlineHull>>,
    hulls: Query<(Entity, &ChildOf), With<OutlineHull>>,
    mut current: Local<Option<Entity>>,
) {
    let focus = hands.iter().find_map(|h| h.current_interactable).filter(|e| items.get(*e).is_ok_and(|i| i.hand == Hand::None));
    if focus == *current && (focus.is_none() || !hulls.is_empty()) {
        return;
    }
    // OnLoseFocus: _OutlineWidth = 0.
    for (h, _) in &hulls {
        commands.entity(h).despawn();
    }
    *current = focus;
    let Some(root) = focus else { return };
    // OnFocus: _OutlineWidth = 0.007 on every MeshRenderer material under the weapon.
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if let Ok((m, layers)) = meshes.get(e) {
            let mut hull = commands.spawn((OutlineHull, Mesh3d(m.0.clone()), MeshMaterial3d(mat.0.clone()), Transform::IDENTITY, ChildOf(e)));
            if let Some(l) = layers {
                hull.insert(l.clone());
            }
        }
        if let Ok(k) = children.get(e) {
            stack.extend(k.iter());
        }
    }
}

pub struct OutlinePlugin;

impl Plugin for OutlinePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<OutlineMat>::default()).add_systems(Startup, setup).add_systems(Update, update);
    }
}
