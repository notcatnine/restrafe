//! Ejected shell casings: `Weapon.OnShoot` / `Weapon.OnReload` instantiate `VFX_CaseEject_00_Kanye`
//! (a one-burst ParticleSystem drawing `SM_BulletCasing_00`) at the CaseEjectPoint with
//! `transform.forward = -ejectCasePoint.right`; `EjectedCaseScript` plays a clip on the first particle collision.
//!
//! Prefab values: lifetime 1 s, simulationSpeed 1.3, startSpeed 3.82, startSize 2, startRotation (0, 90 deg, 0),
//! gravityModifier 1 (-9.81) + ForceModule y -10 (world), Cone shape (angle 23.71, rotation x -31.95), collision
//! module (world, dampen 0.1, bounce 0.5, radiusScale 0.15, collisionMessages). Unity particles are not rigid
//! bodies (no spin, point-sphere collisions), so they are integrated here and swept with a sphere cast.

use super::data::UField;
use super::Item;
use crate::physics;
use crate::unity::{random_range_f, random_range_i};
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

const SIM_SPEED: f32 = 1.3;
const LIFETIME: f32 = 1.0;
const START_SPEED: f32 = 3.82;
const START_SIZE: f32 = 2.0;
const GRAVITY: f32 = -9.81 - 10.0;
const CONE_ANGLE: f32 = 23.71;
const CONE_TILT: f32 = 31.95;
const COLLIDE_RADIUS: f32 = 0.15; // radiusScale 0.15 * (startSize 2 / 2)
const DAMPEN: f32 = 0.1;
const BOUNCE: f32 = 0.5;

/// Pending casing spawn requested by `Weapon.OnShoot` / `OnReload` (consumed in PostUpdate).
#[derive(Clone, Copy, Debug)]
pub struct EjectReq {
    /// `EjectedCaseScript.shouldPlaySound`
    pub play_sound: bool,
    /// `ejectCaseIndex == 1` -> shotgunCase clip
    pub shotgun: bool,
}

impl Item {
    /// `Weapon.OnShoot` casing part (EjectVFX) + `caseSoundTimer = 0.3`.
    pub fn on_shoot_eject(&mut self) {
        if self.def.w.b("EjectVFX") {
            self.queue_eject();
        }
        self.case_sound_timer = 0.3;
    }
    /// `Weapon.OnReload`: reloadEjectVfx + `caseSoundTimer = 0.3`.
    pub fn on_reload_eject(&mut self) {
        if self.def.w.asset("reloadEjectVfx").is_some() {
            self.queue_eject();
        }
        self.case_sound_timer = 0.3;
    }
    fn queue_eject(&mut self) {
        let play_sound = if self.case_sound_timer > 0.0 { random_range_i(0, 100) > 60 } else { true };
        let shotgun = self.def.w.i("ejectCaseIndex") == 1;
        self.eject_req = Some(EjectReq { play_sound, shotgun });
    }
}

#[derive(Component)]
pub struct Casing {
    vel: Vec3,
    age: f32,
    play_sound: bool,
    shotgun: bool,
    triggered: bool,
    pitch: f32,
    /// Where the VFX_CaseEject GameObject (and its AudioSource) was instantiated; the particle flies off, the source stays.
    origin: Vec3,
}

/// VFX_CaseEject_00_Kanye AudioSource: spatialBlend 1, custom rolloff over MaxDistance 20 (keys from the prefab).
fn case_rolloff() -> std::sync::Arc<crate::audio::RolloffCurve> {
    static C: std::sync::OnceLock<std::sync::Arc<crate::audio::RolloffCurve>> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        std::sync::Arc::new(crate::audio::RolloffCurve {
            max: 20.0,
            log_min: None,
            keys: vec![[0.05, 1.0, 0.0, 0.0], [0.6292554, 0.82211775, -1.0526315, -1.0526315], [1.0, 0.0, -1.0526316, -1.0526316]],
        })
    })
    .clone()
}

#[derive(Resource)]
struct CasingAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

fn setup(mut commands: Commands, assets: Res<AssetServer>, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(CasingAssets {
        mesh: assets.load(GltfAssetLabel::Primitive { mesh: 0, primitive: 0 }.from_asset("models/vfx/SM_BulletCasing_00.glb")),
        material: materials.add(StandardMaterial {
            base_color_texture: Some(assets.load("models/vfx/T_SM_Kanye_00_BC.png")),
            metallic: 0.5,
            perceptual_roughness: 0.5,
            ..default()
        }),
    });
}

/// Unity (left-handed) <-> Bevy world: Z is mirrored.
fn mirror(v: Vec3) -> Vec3 {
    Vec3::new(v.x, v.y, -v.z)
}

/// Unity `Quaternion.LookRotation(forward)` (up = world up) as a matrix, computed in Unity space.
fn look_rotation_u(f: Vec3) -> Mat3 {
    let z = f.normalize_or_zero();
    let mut x = Vec3::Y.cross(z);
    if x.length_squared() < 1e-8 {
        x = Vec3::X;
    }
    let x = x.normalize();
    let y = z.cross(x);
    Mat3::from_cols(x, y, z)
}

fn spawn_casings(mut commands: Commands, assets: Option<Res<CasingAssets>>, mut items: Query<&mut Item>) {
    let Some(assets) = assets else { return };
    for mut item in &mut items {
        let Some(req) = item.eject_req else { continue };
        let (Some(pos), Some(dir_b)) = (item.eject_pos, item.eject_dir) else {
            // markers not cached yet (first frame after pickup): keep the request
            continue;
        };
        item.eject_req = None;
        // Emitter frame (Unity space): forward = -ejectCasePoint.right.
        let f_u = mirror(dir_b);
        let e = look_rotation_u(f_u);
        // Cone shape: rotation x -31.95 (tilts the axis towards the emitter's up), then a random direction within 23.71 deg.
        let t = CONE_TILT.to_radians();
        let axis = (e.z_axis * t.cos() + e.y_axis * t.sin()).normalize();
        let rr = random_range_f(0.0, 1.0).sqrt();
        let az = random_range_f(0.0, std::f32::consts::TAU);
        let spread = (CONE_ANGLE * rr).to_radians();
        let perp = axis.any_orthonormal_vector();
        let perp2 = axis.cross(perp);
        let dir_u = (axis * spread.cos() + (perp * az.cos() + perp2 * az.sin()) * spread.sin()).normalize();
        // Orientation: emitter rotation * startRotation(0, 90 deg, 0), converted to Bevy (M R M, M = diag(1, 1, -1)).
        let m = Mat3::from_diagonal(Vec3::new(1.0, 1.0, -1.0));
        let r_b = m * (e * Mat3::from_rotation_y(std::f32::consts::FRAC_PI_2)) * m;
        let rot = Quat::from_mat3(&r_b);
        commands
            .spawn((
                Name::new("EjectedCase"),
                Casing {
                    vel: mirror(dir_u) * START_SPEED,
                    age: 0.0,
                    play_sound: req.play_sound,
                    shotgun: req.shotgun,
                    triggered: false,
                    pitch: random_range_f(0.9, 1.1),
                    origin: pos,
                },
                Transform::from_translation(pos).with_rotation(rot),
                Visibility::default(),
            ))
            .with_children(|p| {
                // glb meshes are X-mirrored (AssetRipper); a Y180 turn lines them up with Z-mirrored world space.
                p.spawn((
                    Mesh3d(assets.mesh.clone()),
                    MeshMaterial3d(assets.material.clone()),
                    Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)).with_scale(Vec3::splat(START_SIZE)),
                ));
            });
    }
}

fn update_casings(
    mut commands: Commands,
    time: Res<Time>,
    mut sounds: MessageWriter<crate::weapons::WeaponSound>,
    rapier: ReadRapierContext,
    mut casings: Query<(Entity, &mut Casing, &mut Transform)>,
) {
    let Ok(ctx) = rapier.single() else { return };
    // simulationSpeed 1.3 scales the particle clock.
    let dt = time.delta_secs() * SIM_SPEED;
    for (e, mut c, mut tf) in &mut casings {
        c.age += dt;
        if c.age >= LIFETIME {
            commands.entity(e).despawn();
            continue;
        }
        c.vel.y += GRAVITY * dt;
        let step = c.vel * dt;
        let dist = step.length();
        if dist > 1e-6 {
            if let Some(h) = physics::sphere_cast(&ctx, tf.translation, COLLIDE_RADIUS, step, dist, !(1 << physics::layer::SELF_BODY)) {
                tf.translation += step.normalize() * h.distance.max(0.0);
                let n = h.normal.normalize_or_zero();
                if n != Vec3::ZERO {
                    c.vel = (c.vel - 2.0 * c.vel.dot(n) * n) * (1.0 - DAMPEN) * BOUNCE;
                }
                // OnParticleCollision: first collision plays a clip (when shouldPlaySound).
                if !c.triggered && c.play_sound {
                    let n_rand = random_range_i(-1, 2);
                    let clip = if c.shotgun {
                        "AudioClip/BulletCasing_01.ogg"
                    } else if n_rand > 0 {
                        "AudioClip/BulletCasing_02.ogg" // normalCase[0]
                    } else {
                        "AudioClip/BulletCasing_00.ogg" // normalCase[1]
                    };
                    // source.PlayOneShot on the casing VFX's own 3D AudioSource (pitch 0.9..1.1 from Awake).
                    sounds.write(crate::weapons::WeaponSound {
                        clip: clip.into(),
                        volume: 1.0,
                        at: Some(c.origin),
                        pitch: c.pitch,
                        linear_max: 0.0,
                        source: None,
                        rolloff: Some(case_rolloff()),
                    });
                }
                c.triggered = true;
                continue;
            }
        }
        tf.translation += step;
    }
}

pub struct CasingPlugin;

impl Plugin for CasingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup)
            .add_systems(PostUpdate, spawn_casings.after(TransformSystems::Propagate))
            .add_systems(Update, update_casings);
    }
}
