//! Weapon prefab child markers (ShootPointObject / FlashPointObject / EjectCasePointObject /
//! ElbowPivotPoint / AimStrafePivot) found by name inside the weapon's glb instance, the
//! `ElbowPivotPoint` forearm animation (Weapon.WeaponAnimation + WeaponUpdate), the
//! `ItemBehaviour.aimStrafePivot` lean, and the viewmodel ("HeldWeapon" layer 8) render layer with
//! its `WeaponCam` (a child of the player camera, fov 75, near 0.01, far 20, depth-only clear).
//!
//! glb nodes live in AssetRipper's X-mirrored space: a Unity local rotation (x,y,z,w) is stored as
//! (x,-y,-z,w). [`flip`] converts between the port's `unity_euler` quaternions and glb-local ones.

use super::{Hand, Item, WeaponModel};
use crate::player::fpc::{Fpc, PlayerCamera};
use crate::tween::{lerp_euler, punch_keyframes, Ease, KeyPath};
use crate::unity::*;
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;

use super::data::UField;

/// Render layer of held weapons + first-person arms (Unity layer HeldWeapon = 8).
pub const VIEWMODEL_LAYER: usize = 2;

pub fn viewmodel_layers() -> RenderLayers {
    RenderLayers::layer(VIEWMODEL_LAYER)
}
/// For lights that must light both the world and the viewmodel.
pub fn world_and_viewmodel() -> RenderLayers {
    RenderLayers::from_layers(&[0, VIEWMODEL_LAYER])
}

/// `Camera/WeaponCam`.
#[derive(Component)]
pub struct WeaponCam;

/// Forearm tween request produced by `weapon_animation` (consumed by [`elbow_update`]).
#[derive(Clone, Debug)]
pub enum ElbowReq {
    /// `elbowPivot.DOLocalRotate(to, duration)` (Unity Euler degrees).
    Rotate { to: Vec3, duration: f32 },
    /// `elbowPivot.DOPunchRotation(punch, duration, vibrato, elasticity)`.
    Punch { punch: Vec3, duration: f32, vibrato: i32, elasticity: f32 },
}

enum ElbowKind {
    Rotate(Vec3),
    Punch(KeyPath),
}

struct ElbowActive {
    start: Option<Vec3>,
    elapsed: f32,
    duration: f32,
    kind: ElbowKind,
}

/// Marker nodes of a weapon's glb instance (filled once the scene has spawned).
#[derive(Component, Default)]
pub struct WeaponMarkers {
    pub shoot_point: Option<Entity>,
    pub flash_point: Option<Entity>,
    pub eject_point: Option<Entity>,
    pub elbow_pivot: Option<Entity>,
    pub aim_strafe_pivot: Option<Entity>,
    elbow_tween: Option<ElbowActive>,
}

#[derive(Component, Default)]
struct MarkersPending {
    frames_with_scene: u32,
}

/// glb-local <-> port (`unity_euler`) quaternion; an involution.
pub(crate) fn flip(q: Quat) -> Quat {
    Quat::from_xyzw(-q.x, q.y, -q.z, q.w)
}

fn node_name(def: &super::data::WeaponDef, comp: &str) -> Option<String> {
    def.prefab.objects.iter().find(|o| o.comp(comp).is_some()).map(|o| o.path.rsplit('/').next().unwrap_or("").to_string())
}

/// Finds the marker nodes by name below each new weapon's [`WeaponModel`].
fn resolve_markers(
    mut commands: Commands,
    items: Query<(Entity, &Item, Option<&MarkersPending>), Without<WeaponMarkers>>,
    kids: Query<&Children>,
    models: Query<(), With<WeaponModel>>,
    names: Query<&Name>,
) {
    for (e, item, pending) in &items {
        let Some(model) = kids.get(e).ok().and_then(|c| c.iter().find(|c| models.contains(*c))) else { continue };
        let Ok(model_kids) = kids.get(model) else { continue };
        if model_kids.is_empty() {
            continue;
        }
        let want = [
            node_name(&item.def, "ShootPointObject"),
            node_name(&item.def, "FlashPointObject"),
            node_name(&item.def, "EjectCasePointObject"),
            node_name(&item.def, "ElbowPivotPoint"),
            node_name(&item.def, "AimStrafePivot"),
        ];
        let mut found: [Option<Entity>; 5] = [None; 5];
        for d in kids.iter_descendants(model) {
            let Ok(n) = names.get(d) else { continue };
            for (i, w) in want.iter().enumerate() {
                if found[i].is_none() && w.as_deref() == Some(n.as_str()) {
                    found[i] = Some(d);
                }
            }
        }
        let complete = want.iter().zip(&found).all(|(w, f)| w.is_none() || f.is_some());
        let frames = pending.map(|p| p.frames_with_scene).unwrap_or(0);
        if !complete && frames < 30 {
            commands.entity(e).insert(MarkersPending { frames_with_scene: frames + 1 });
            continue;
        }
        if !complete {
            warn!("weapon {}: marker nodes missing in glb: {:?} -> {:?}", item.def.name, want, found);
        }
        commands.entity(e).remove::<MarkersPending>().insert(WeaponMarkers {
            shoot_point: found[0],
            flash_point: found[1],
            eject_point: found[2],
            elbow_pivot: found[3],
            aim_strafe_pivot: found[4],
            elbow_tween: None,
        });
    }
}

/// Caches the markers' world positions on the [`Item`] (after transform propagation).
fn update_marker_positions(mut items: Query<(&mut Item, &WeaponMarkers)>, globals: Query<&GlobalTransform>) {
    for (mut item, m) in &mut items {
        let pos = |e: Option<Entity>| e.and_then(|e| globals.get(e).ok()).map(|g| g.translation());
        item.shoot_pos = pos(m.shoot_point);
        item.flash_pos = pos(m.flash_point);
        // `shootPoint.rotation`: the glb node's -Z is Unity's -forward (glb X mirror + scene Y180), while VFX
        // spawns take port rotations whose -Z is Unity forward (like the camera's), hence the Y180.
        item.shoot_rot = m.shoot_point.and_then(|e| globals.get(e).ok()).map(|g| g.rotation() * Quat::from_rotation_y(std::f32::consts::PI));
        item.eject_pos = pos(m.eject_point);
        // `-ejectCasePoint.right` in Unity world == the glb node's +X axis here (glb X mirror + scene Y180).
        item.eject_dir = m.eject_point.and_then(|e| globals.get(e).ok()).map(|g| g.right().as_vec3());
    }
}

/// Weapon.WeaponUpdate elbow slerp + the DOLocalRotate / DOPunchRotation tweens requested by
/// Weapon.WeaponAnimation. Runs after the weapon chain (DOTween updates after scripts).
fn elbow_update(time: Res<Time>, mut items: Query<(&mut Item, &mut WeaponMarkers)>, mut tfs: Query<&mut Transform, Without<Item>>) {
    let dt = time.delta_secs();
    for (mut item, mut m) in &mut items {
        let Some(elbow) = m.elbow_pivot else {
            item.elbow_req = None;
            continue;
        };
        let Ok(mut tf) = tfs.get_mut(elbow) else { continue };
        // Update: elbowPivot.localRotation = Slerp(.., identity, foreArmAnimationSpeed * dt)
        let speed = item.def.w.f("foreArmAnimationSpeed");
        tf.rotation = slerp(tf.rotation, Quat::IDENTITY, speed * dt);
        if let Some(req) = item.elbow_req.take() {
            m.elbow_tween = Some(match req {
                ElbowReq::Rotate { to, duration } => ElbowActive { start: None, elapsed: 0.0, duration, kind: ElbowKind::Rotate(to) },
                ElbowReq::Punch { punch, duration, vibrato, elasticity } => {
                    let (tos, durs) = punch_keyframes(punch, duration, vibrato, elasticity);
                    ElbowActive { start: None, elapsed: 0.0, duration, kind: ElbowKind::Punch(KeyPath { tos, durs, ease: Ease::OutQuad }) }
                }
            });
        }
        let Some(a) = m.elbow_tween.as_mut() else { continue };
        let start = *a.start.get_or_insert_with(|| to_unity_euler(flip(tf.rotation)));
        a.elapsed += dt;
        let e = match &a.kind {
            ElbowKind::Rotate(to) => {
                let k = if a.duration > 0.0 { (a.elapsed / a.duration).min(1.0) } else { 1.0 };
                lerp_euler(start, *to, Ease::OutQuad.eval(k))
            }
            ElbowKind::Punch(path) => path.eval(start, a.elapsed),
        };
        tf.rotation = flip(unity_euler(e.x, e.y, e.z));
        if a.elapsed >= a.duration {
            m.elbow_tween = None;
        }
    }
}

/// ItemBehaviour.Update aimStrafePivot lean (right hand only). Faithful to the C#, which feeds the
/// local rotation's quaternion components in as Euler degrees.
pub(super) fn aim_strafe_update(time: Res<Time>, players: Query<&Fpc>, items: Query<(&Item, &WeaponMarkers)>, mut tfs: Query<&mut Transform, Without<Item>>) {
    let Ok(fpc) = players.single() else { return };
    let dt = time.delta_secs();
    for (item, m) in &items {
        let (Some(pivot), true) = (m.aim_strafe_pivot, item.hand == Hand::Right) else { continue };
        let Ok(mut tf) = tfs.get_mut(pivot) else { continue };
        let ib = &item.def.item;
        let max_pivot = -ib.f("maxPivot"); // Start: maxPivot = 0 - maxPivot
        let speed = ib.f("aimStrafeLeanSpeed");
        let q = tf.rotation; // Unity local rotation components = (x, -y, -z)
        let (ux, uy, uz) = (q.x, -q.y, -q.z);
        let aiming = fpc.is_aiming && ib.b("aimWeapon") && !item.is_reloading;
        let ix = fpc.current_input_raw.x;
        let z = if aiming && ix != 0.0 { uz + ix * max_pivot } else { 0.0 };
        let target = flip(unity_euler(ux, uy, z));
        tf.rotation = slerp(tf.rotation, target, speed * dt);
    }
}

/// Spawns `WeaponCam` under every player camera.
fn spawn_weapon_cam(mut commands: Commands, cams: Query<Entity, Added<PlayerCamera>>) {
    for cam in &cams {
        commands.spawn((
            Name::new("WeaponCam"),
            WeaponCam,
            Camera3d::default(),
            // Between the world camera (0) and the HUD camera; depth is cleared (Camera3d default) so the
            // viewmodel always draws over the world, colour is kept.
            Camera { order: 1, clear_color: ClearColorConfig::None, ..default() },
            Projection::Perspective(PerspectiveProjection { fov: 75f32.to_radians(), near: 0.01, far: 20.0, ..default() }),
            Transform::default(),
            viewmodel_layers(),
            ChildOf(cam),
        ));
    }
}

/// PlayerSetup (spawn) camera set-up by `Settings.qualitySetting` (cameras = Camera, WeaponCam, UICam):
/// * Medium (1): the world camera's culling mask becomes `lowMask` (highMask + HeldWeapon), WeaponCam is disabled: the
///   viewmodel is drawn by the world camera (its fov, Camera Profile post-processing, no WeaponCam bloom).
/// * Low (0): the same, plus UICam disabled (HideHUD(true): the minimal UI, see screen_ui.rs).
/// * High / Ultra: highMask, WeaponCam + UICam on.
/// The death kill cam turns the rig cameras off on its own (death.rs); they stay as set here otherwise.
/// Only OnStartClient reads the level: Settings.SetQualityLevelDropdown changes QualitySettings and HideHUD, not the
/// cameras, so a level picked in the menu reaches them at the next spawn (latched here at startup and each respawn).
fn apply_quality(
    settings: Res<crate::settings::Settings>,
    death: Res<crate::death::LocalDeath>,
    mut commands: Commands,
    world: Query<(Entity, Option<&RenderLayers>), With<PlayerCamera>>,
    mut weapon: Query<&mut Camera, (With<WeaponCam>, Without<crate::hud::HudCamera>)>,
    mut hud: Query<&mut Camera, (With<crate::hud::HudCamera>, Without<WeaponCam>)>,
    mut spawn_q: Local<Option<(i32, bool)>>,
) {
    let q = match *spawn_q {
        Some((q, was_dead)) if !(was_dead && !death.dead) => q,
        _ => settings.quality_setting,
    };
    *spawn_q = Some((q, death.dead));
    let want = if q < 2 { world_and_viewmodel() } else { RenderLayers::layer(0) };
    for (e, layers) in &world {
        if layers.cloned().unwrap_or_default() != want {
            commands.entity(e).insert(want.clone());
        }
    }
    if death.dead {
        return;
    }
    for mut c in &mut weapon {
        if c.is_active != (q >= 2) {
            c.is_active = q >= 2;
        }
    }
    for mut c in &mut hud {
        if c.is_active != (q >= 1) {
            c.is_active = q >= 1;
        }
    }
}

/// Held weapons are on the viewmodel layer, dropped ones back on the world layer; glb scene
/// descendants (spawned asynchronously) inherit the layers of their parent.
fn viewmodel_layers_update(
    mut commands: Commands,
    items: Query<(Entity, &Item, Has<RenderLayers>)>,
    kids: Query<&Children>,
    unlabeled: Query<(Entity, &ChildOf), (Without<RenderLayers>, Without<WeaponCam>)>,
    labeled: Query<&RenderLayers>,
    lights: Query<Entity, (Or<(With<SpotLight>, With<PointLight>)>, Without<RenderLayers>)>,
) {
    for (e, item, has) in &items {
        if item.hand != Hand::None && !has {
            commands.entity(e).insert(viewmodel_layers());
        } else if item.hand == Hand::None && has {
            commands.entity(e).remove::<RenderLayers>();
            for d in kids.iter_descendants(e) {
                commands.entity(d).remove::<RenderLayers>();
            }
        }
    }
    for l in &lights {
        // Lights (e.g. flashlight spot) must affect both the world and the viewmodel.
        commands.entity(l).insert(world_and_viewmodel());
    }
    for (e, p) in &unlabeled {
        if lights.contains(e) {
            continue;
        }
        if let Ok(layers) = labeled.get(p.parent()) {
            commands.entity(e).insert(layers.clone());
        }
    }
}

pub struct MarkersPlugin;

impl Plugin for MarkersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                spawn_weapon_cam,
                apply_quality,
                resolve_markers,
                viewmodel_layers_update,
                (aim_strafe_update, elbow_update).after(super::pickup::item_behaviour_update),
            ),
        )
        .add_systems(PostUpdate, update_marker_positions.after(TransformSystems::Propagate));
    }
}
