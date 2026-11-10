//! Unity physics layer emulation on top of Rapier collision groups, plus `Physics.*` query helpers.
//!
//! Unity layer N == Rapier group bit N, so serialized `LayerMask.m_Bits` values from prefabs
//! can be used directly. Project settings: queriesHitTriggers = false, queriesHitBackfaces = true.

use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

#[allow(dead_code)]
pub mod layer {
    pub const DEFAULT: u32 = 0;
    pub const TRANSPARENT_FX: u32 = 1;
    pub const IGNORE_RAYCAST: u32 = 2;
    pub const PLAYER: u32 = 3;
    pub const WATER: u32 = 4;
    pub const UI: u32 = 5;
    pub const SELF: u32 = 6;
    pub const INTERACTABLE: u32 = 7;
    pub const HELD_WEAPON: u32 = 8;
    pub const OTHER_HELD_WEAPON: u32 = 9;
    pub const LADDER: u32 = 10;
    pub const BODY: u32 = 11;
    pub const MEMBERS: u32 = 12;
    pub const POST_PROCESS: u32 = 13;
    pub const SHOOT_THROUGH: u32 = 14;
    pub const NEW_GUN: u32 = 15;
    pub const SELF_BODY: u32 = 16;
    pub const HEAD_SUP: u32 = 17;
    pub const DONT_COLLIDE_WITH_PLAYER: u32 = 18;
    pub const INTERACT_ENVIRONMENT: u32 = 19;
    pub const GRENADE: u32 = 20;
    pub const MELEE: u32 = 21;
    pub const WEAPON_CAM: u32 = 22;
    pub const RAGDOLL: u32 = 23;
    pub const GLASS: u32 = 24;
    pub const HUD: u32 = 25;
    pub const PAUSE_MENU_HUD: u32 = 26;
    pub const INVISIBLE_WALL: u32 = 27;
}

/// `Physics.DefaultRaycastLayers` (everything except Ignore Raycast), minus SelfBody: the only SelfBody colliders are the
/// local player's own hit colliders, and every default-layer query in the port starts at that player (feet rays, camera
/// rays) where Unity's queries never report the collider they start in. Prefab masks containing bit 16 (explosions) still
/// hit them.
pub const DEFAULT_RAYCAST_LAYERS: u32 = !(1 << layer::IGNORE_RAYCAST) & !(1 << layer::SELF_BODY);

/// DynamicsManager layer collision matrix row 16 (SelfBody), little-endian `7fff49f7`: layers 0-6, 8-16, 19, 22, 24-26, 28-31.
/// (Not Interactable(7), Grenade(20), Ragdoll(23).)
pub const SELF_BODY_COLLIDES: u32 = 0xf749_ff7f;

/// FirstPersonController.landLayer (PlayerIK prefab).
pub const LAND_LAYER: u32 = 17317889;
/// FirstPersonController.ladderLayer (PlayerIK prefab).
pub const LADDER_LAYER: u32 = 1024;

/// Unity layer of a collider; drives its Rapier `CollisionGroups` membership.
#[derive(Component, Clone, Copy, Debug)]
pub struct UnityLayer(pub u32);

/// Unity tag on a collider's GameObject (e.g. "Footsteps/Concrete/Default", "Ladder/Metal").
#[derive(Component, Clone, Debug, Default)]
pub struct UnityTag(pub String);

/// Layer collision matrix row of Interactable (7), DynamicsManager `m_LayerCollisionMatrix`: loose weapons hit the
/// world, glass and each other, but not Player / Self / Body / SelfBody / HeadSup / DontCollideWithPlayer / Grenade /
/// Melee / Ragdoll / InvisibleWall.
pub const INTERACTABLE_COLLIDES: u32 = 0xf748_f7b7;

/// Layer collision matrix row 23 (Ragdoll), little-endian `01c8a8f7`: Default, Body(11), ShootThrough(14), NewGun(15),
/// InteractEnvironment(19), Melee(21), Ragdoll, Glass, HUD(25), PauseMenuHud(26), 28-31. Not Player / Self /
/// Interactable / SelfBody / DontCollideWithPlayer / Grenade. (Was read big-endian before 2026-10-29.)
pub const RAGDOLL_COLLIDES: u32 = 0xf7a8_c801;

/// Layer collision matrix row 18 (DontCollideWithPlayer; the worn / popped hat, PlayerSetup.ChangeDress layer 18),
/// little-endian `018058f7`: Default, NewGun(15), InteractEnvironment(19), Grenade(20), WeaponCam(22), Glass(24),
/// 25, 26, 28-31. Not Player / Body / Interactable / Ragdoll.
pub const DONT_COLLIDE_WITH_PLAYER_COLLIDES: u32 = 0xf758_8001;

/// Layer collision matrix row 21 (Melee; MeleeChildCollision's kinematic `Collision` child), little-endian `0148c8f7`:
/// Default, Body(11), ShootThrough(14), InteractEnvironment(19), WeaponCam(22), Ragdoll(23), Glass(24), 25, 26, 28-31.
pub const MELEE_COLLIDES: u32 = 0xf7c8_4801;

/// Contact pairs of a kinematic shape at a pose (DynamicsManager m_ContactPairsMode 3 = EnableAll, so kinematic-static
/// pairs report OnCollisionEnter too): every non-trigger collider in `mask` it overlaps, with a contact point on that
/// collider and its surface normal there, pointing toward the shape (Collision.contacts[0]).
pub fn shape_contacts(ctx: &RapierContext, shape: &Collider, pos: Vec3, rot: Quat, mask: u32) -> Vec<RayHit> {
    let mut ents = Vec::new();
    ctx.intersect_shape(pos, rot, &*shape.raw, filter(mask), |e| {
        ents.push(e);
        true
    });
    ents.into_iter()
        .map(|e| {
            let only = |x: Entity| x == e;
            let opts = ShapeCastOptions { max_time_of_impact: 0.01, target_distance: 0.0, stop_at_penetration: true, compute_impact_geometry_on_penetration: true };
            match ctx.cast_shape(pos, rot, Vec3::Y, &*shape.raw, opts, filter(mask).predicate(&only)).and_then(|(_, h)| h.details) {
                Some(d) => {
                    let mut n = d.normal1.normalize_or_zero();
                    if n.dot(pos - d.witness1) < 0.0 {
                        n = -n;
                    }
                    RayHit { entity: e, point: d.witness1, normal: n, distance: 0.0 }
                }
                None => RayHit { entity: e, point: pos, normal: Vec3::Y, distance: 0.0 },
            }
        })
        .collect()
}

/// Collision groups for a collider on Unity layer `l`. Rows of the layer collision matrix are applied for SelfBody and
/// Interactable, Ragdoll and DontCollideWithPlayer; every other layer collides with every layer.
pub fn groups_for_layer(l: u32) -> CollisionGroups {
    if l == layer::SELF_BODY {
        return CollisionGroups::new(Group::from_bits_retain(1 << l), Group::from_bits_retain(SELF_BODY_COLLIDES));
    }
    if l == layer::INTERACTABLE {
        return CollisionGroups::new(Group::from_bits_retain(1 << l), Group::from_bits_retain(INTERACTABLE_COLLIDES));
    }
    if l == layer::RAGDOLL {
        return CollisionGroups::new(Group::from_bits_retain(1 << l), Group::from_bits_retain(RAGDOLL_COLLIDES));
    }
    if l == layer::DONT_COLLIDE_WITH_PLAYER {
        return CollisionGroups::new(Group::from_bits_retain(1 << l), Group::from_bits_retain(DONT_COLLIDE_WITH_PLAYER_COLLIDES));
    }
    CollisionGroups::new(Group::from_bits_retain(1 << l), Group::ALL)
}

pub fn sync_layers(mut commands: Commands, q: Query<(Entity, &UnityLayer), Changed<UnityLayer>>) {
    for (e, l) in &q {
        commands.entity(e).insert(groups_for_layer(l.0));
    }
}

pub struct RayHit {
    pub entity: Entity,
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

fn filter(mask: u32) -> QueryFilter<'static> {
    QueryFilter::new()
        .exclude_sensors()
        .groups(CollisionGroups::new(
            Group::ALL,
            Group::from_bits_retain(mask),
        ))
}

/// `Physics.Raycast(origin, dir, out hit, maxDistance, layerMask)`.
pub fn raycast(
    ctx: &RapierContext,
    origin: Vec3,
    dir: Vec3,
    max: f32,
    mask: u32,
) -> Option<RayHit> {
    let dir = dir.normalize_or_zero();
    if dir == Vec3::ZERO {
        return None;
    }
    // solid=false: a ray starting inside a collider reports its exit, like queriesHitBackfaces.
    ctx.cast_ray_and_get_normal(origin, dir, max, false, filter(mask))
        .map(|(entity, hit)| RayHit {
            entity,
            point: hit.point,
            normal: hit.normal,
            distance: hit.time_of_impact,
        })
}

/// `Physics.Raycast(origin, dir, maxDistance, layerMask)` boolean form.
pub fn ray(ctx: &RapierContext, origin: Vec3, dir: Vec3, max: f32, mask: u32) -> bool {
    raycast(ctx, origin, dir, max, mask).is_some()
}

/// `Physics.CheckSphere`.
pub fn check_sphere(ctx: &RapierContext, center: Vec3, radius: f32, mask: u32) -> bool {
    let mut hit = false;
    ctx.intersect_shape(
        center,
        Quat::IDENTITY,
        &*Collider::ball(radius).raw,
        filter(mask),
        |_| {
            hit = true;
            false
        },
    );
    hit
}

/// `Physics.OverlapSphere`.
pub fn overlap_sphere(ctx: &RapierContext, center: Vec3, radius: f32, mask: u32) -> Vec<Entity> {
    let mut out = Vec::new();
    ctx.intersect_shape(
        center,
        Quat::IDENTITY,
        &*Collider::ball(radius).raw,
        filter(mask),
        |e| {
            out.push(e);
            true
        },
    );
    out
}

/// `Physics.CapsuleCast(p, p, radius, dir, ...)` / `SphereCast` exactly as Unity documents it: colliders the sphere already
/// overlaps at the origin are never reported (PhysX sweep without initial-overlap hits). [`sphere_cast`] keeps reporting an
/// overlapped collider the sweep moves deeper into, which its other callers rely on.
pub fn sphere_sweep_unity(ctx: &RapierContext, origin: Vec3, radius: f32, dir: Vec3, max: f32, mask: u32) -> Option<RayHit> {
    let dir = dir.normalize_or_zero();
    if dir == Vec3::ZERO {
        return None;
    }
    let start = overlap_sphere(ctx, origin, radius, mask);
    let opts = ShapeCastOptions { max_time_of_impact: max, target_distance: 0.0, stop_at_penetration: false, compute_impact_geometry_on_penetration: true };
    let skip = |e: Entity| !start.contains(&e);
    ctx.cast_shape(origin, Quat::IDENTITY, dir, &*Collider::ball(radius).raw, opts, filter(mask).predicate(&skip)).map(|(entity, hit)| {
        let d = hit.details.unwrap_or(ShapeCastHitDetails { witness1: Vec3::ZERO, witness2: Vec3::ZERO, normal1: Vec3::ZERO, normal2: Vec3::ZERO });
        let mut normal = -d.normal1;
        if normal.dot(dir) > 0.0 {
            normal = -normal;
        }
        RayHit { entity, point: d.witness1, normal, distance: hit.time_of_impact }
    })
}

/// `Physics.SphereCast(origin, radius, dir, out hit, maxDistance, layerMask)`.
pub fn sphere_cast(ctx: &RapierContext, origin: Vec3, radius: f32, dir: Vec3, max: f32, mask: u32) -> Option<RayHit> {
    let dir = dir.normalize_or_zero();
    if dir == Vec3::ZERO {
        return None;
    }
    let opts = ShapeCastOptions {
        max_time_of_impact: max,
        target_distance: 0.0,
        // Unity's SphereCast ignores colliders already overlapping the sphere at the origin.
        stop_at_penetration: false,
        compute_impact_geometry_on_penetration: true,
    };
    ctx.cast_shape(origin, Quat::IDENTITY, dir, &*Collider::ball(radius).raw, opts, filter(mask)).map(|(entity, hit)| {
        let d = hit.details.unwrap_or(ShapeCastHitDetails { witness1: Vec3::ZERO, witness2: Vec3::ZERO, normal1: Vec3::ZERO, normal2: Vec3::ZERO });
        // Observed: -normal1 points along the cast (both for sweep hits and starting penetrations), i.e. it is the hit
        // collider's inward normal. A surface hit by a sphere moving along `dir` always faces against it (RaycastHit.normal).
        let mut normal = -d.normal1;
        if normal.dot(dir) > 0.0 {
            normal = -normal;
        }
        RayHit { entity, point: d.witness1, normal, distance: hit.time_of_impact }
    })
}

/// `Physics.RaycastAll` sorted by distance (the callers sort, as the C# does).
pub fn raycast_all(ctx: &RapierContext, origin: Vec3, dir: Vec3, max: f32, mask: u32) -> Vec<RayHit> {
    let dir = dir.normalize_or_zero();
    let mut out = Vec::new();
    if dir == Vec3::ZERO {
        return out;
    }
    ctx.intersect_ray(origin, dir, max, false, filter(mask), |entity, hit| {
        out.push(RayHit { entity, point: hit.point, normal: hit.normal, distance: hit.time_of_impact });
        true
    });
    out.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    out
}

/// `Physics.OverlapBox(center, halfExtents, orientation, layerMask)`.
pub fn overlap_box(ctx: &RapierContext, center: Vec3, half: Vec3, rot: Quat, mask: u32) -> Vec<Entity> {
    let mut out = Vec::new();
    let shape = Collider::cuboid(half.x.abs(), half.y.abs(), half.z.abs());
    ctx.intersect_shape(center, rot, &*shape.raw, filter(mask), |e| {
        out.push(e);
        true
    });
    out
}

/// Unity's `Rigidbody.maxAngularVelocity` (ProjectSettings DynamicsManager `m_DefaultMaxAngularSpeed`, never overridden
/// by the game's scripts): every dynamic body's angular speed is capped at 7 rad/s. Rapier has no such limit (dropped
/// weapons would spin several times faster from their torque impulses).
pub const MAX_ANGULAR_SPEED: f32 = 7.0;

pub fn clamp_angular_speed(mut q: Query<(&RigidBody, &mut Velocity)>) {
    for (rb, mut v) in &mut q {
        if *rb == RigidBody::Dynamic && v.angular.length_squared() > MAX_ANGULAR_SPEED * MAX_ANGULAR_SPEED {
            v.angular = v.angular.normalize() * MAX_ANGULAR_SPEED;
        }
    }
}
