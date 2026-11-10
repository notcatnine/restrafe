//! `Suppression.cs` (SuppressionTrigger) + the four `SupressionBox` colliders of the PlayerIK camera.
//!
//! Original: a shooter's `Gun.Fire` casts `Physics.Raycast(pos, dir, out hit, inf, supLayer)` (when the shot did not hit a
//! player) and, if the hit collider is on layer 17 (the SupressionBox boxes around the victim's camera), calls
//! `Suppression.SuppressionTrigger()` on the owner: random `supClip` one-shot + `CameraEffect.TrigSup()`.
//!
//! Single-player port: there are no other shooters, so this is DORMANT unless something sends a [`NearMiss`] message
//! (the weapons' `// TODO suppression ray` spots could send `NearMiss { origin, dir }` for a bullet that is not the
//! player's own and did not hit a player). The boxes are analytic OBBs in camera space (no Rapier colliders needed).
//! Debug: `RESTRAFE_SUPPRESS_TEST=1` fires a bullet past the player's head 2 s after start.
//! The occlusion of the supLayer ray by level geometry is not modelled (only the 4 boxes are tested).

use crate::player::fpc::PlayerCamera;
use crate::postfx::TrigSuppression;
use crate::weapons::WeaponSound;
use bevy::prelude::*;

/// A bullet ray (world space, infinite length) that did not hit a player: test it against the suppression boxes.
#[derive(Message, Clone, Copy, Debug)]
pub struct NearMiss {
    pub origin: Vec3,
    pub dir: Vec3,
}

/// PlayerIK/.../Camera/Suppression/SupressionBox{,(1),(2),(3)}: (local position, Unity Y rotation deg), scale (2.18, 1.71, 0.63),
/// BoxCollider size 1, centre 0, non-trigger, layer 17.
const BOXES: [(Vec3, f32); 4] = [
    (Vec3::new(-0.538, -0.168, 0.0), 0.0),
    (Vec3::new(0.554, -0.168, 0.0), 0.0),
    (Vec3::new(-0.078, -0.168, 0.574), -90.0),
    (Vec3::new(-0.078, -0.168, -0.588), -90.0),
];
const BOX_SCALE: Vec3 = Vec3::new(2.18, 1.71, 0.63);
const SUP_CLIPS: usize = 5;

/// Ray (box-local) vs box centred at 0 with half extents `h`; Unity ignores boxes the ray starts inside.
fn ray_hits_box(o: Vec3, d: Vec3, h: Vec3) -> bool {
    ray_box_t(o, d, h).is_some()
}

/// Entry distance of [`ray_hits_box`] (None = no hit).
fn ray_box_t(o: Vec3, d: Vec3, h: Vec3) -> Option<f32> {
    let (mut tmin, mut tmax) = (0.0f32, f32::INFINITY);
    for i in 0..3 {
        if d[i].abs() < 1e-8 {
            if o[i].abs() > h[i] {
                return None;
            }
        } else {
            let (a, b) = ((-h[i] - o[i]) / d[i], (h[i] - o[i]) / d[i]);
            let (lo, hi) = if a < b { (a, b) } else { (b, a) };
            tmin = tmin.max(lo);
            tmax = tmax.min(hi);
            if tmin > tmax {
                return None;
            }
        }
    }
    // started inside (tmin stays 0 only if the origin is inside all slabs) -> no hit
    let inside = (0..3).all(|i| o[i].abs() < h[i]);
    (!inside && tmin > 0.0).then_some(tmin)
}

/// Which box (if any) does this ray pass through? `cam` = camera world transform.
#[allow(dead_code)] // dormant until something sends a NearMiss (see the module docs)
pub fn hits_suppression_boxes(cam: &GlobalTransform, origin: Vec3, dir: Vec3) -> bool {
    let inv = cam.affine().inverse();
    let (o, d) = (inv.transform_point3(origin), inv.transform_vector3(dir));
    BOXES.iter().any(|(p, ry)| {
        let centre = Vec3::new(p.x, p.y, -p.z); // Unity -> Bevy (Z mirrored)
        let rot = crate::unity::unity_euler(0.0, *ry, 0.0);
        let inv_r = rot.inverse();
        ray_hits_box(inv_r * (o - centre), inv_r * d, BOX_SCALE * 0.5)
    })
}

/// World centre of the first SupressionBox along the ray (the collider the shooter's raycast reports).
pub fn first_suppression_box(cam: &GlobalTransform, origin: Vec3, dir: Vec3) -> Option<Vec3> {
    let inv = cam.affine().inverse();
    let (o, d) = (inv.transform_point3(origin), inv.transform_vector3(dir));
    BOXES
        .iter()
        .filter_map(|(p, ry)| {
            let centre = Vec3::new(p.x, p.y, -p.z); // Unity -> Bevy (Z mirrored)
            let inv_r = crate::unity::unity_euler(0.0, *ry, 0.0).inverse();
            ray_box_t(inv_r * (o - centre), inv_r * d, BOX_SCALE * 0.5).map(|t| (t, centre))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, c)| cam.transform_point(c))
}

/// `Suppression.SuppressionTrigger`: random supClip one-shot + `CameraEffect.TrigSup()`. The clip plays on the hit
/// box's own AudioSource (spatialBlend 1, Logarithmic min 2 / max 500), so it is panned to that side of the head.
fn suppression_trigger(sounds: &mut MessageWriter<WeaponSound>, fx: &mut MessageWriter<TrigSuppression>, at: Vec3) {
    let i = crate::unity::random_range_i(0, SUP_CLIPS as i32);
    sounds.write(WeaponSound { clip: format!("AudioClip/suppression{:02}.ogg", i), volume: 1.0, at: Some(at), pitch: 1.0, linear_max: -2.0, source: None, rolloff: None });
    info!("suppression: clip {i}");
    fx.write(TrigSuppression);
}

fn near_miss(
    mut misses: MessageReader<NearMiss>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut sounds: MessageWriter<WeaponSound>,
    mut fx: MessageWriter<TrigSuppression>,
) {
    let Ok(cam) = cam.single() else { return };
    for m in misses.read() {
        if let Some(at) = first_suppression_box(cam, m.origin, m.dir) {
            suppression_trigger(&mut sounds, &mut fx, at);
        }
    }
}

/// Debug source: one bullet flying past the player's head from 10 m in front.
fn debug_source(time: Res<Time>, cam: Query<&GlobalTransform, With<PlayerCamera>>, mut out: MessageWriter<NearMiss>, mut done: Local<bool>) {
    if *done || time.elapsed_secs() < 2.0 || std::env::var_os("RESTRAFE_SUPPRESS_TEST").is_none() {
        return;
    }
    let Ok(cam) = cam.single() else { return };
    *done = true;
    let origin = cam.translation() + cam.forward() * 10.0 + cam.right() * 0.3;
    out.write(NearMiss { origin, dir: *cam.back() });
}

pub struct SuppressionPlugin;

impl Plugin for SuppressionPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NearMiss>().add_systems(Update, (debug_source, near_miss).chain());
    }
}
