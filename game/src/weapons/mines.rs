//! Port of `WeaponHandSpawner.cs` (APMine / Claymore / ProximityMine / StunMine weapons) and the placed
//! objects it spawns: `ProximityMine.cs` (ProximityMine, StunMine, APMine objects) and `Claymore.cs`.
//!
//! Networking is stripped: SpawnObject / ChangeState / ExplodeServer / ExplodeObservers run locally.
//! Notes on the C# that shaped the port:
//!  * ProximityMine/StunMine/APMine objects all use `ProximityMine.cs`; `instantExplode` (APMine) explodes on the first
//!    trigger entry, the others arm after 1 s and explode when a "Player" LEAVES the trigger box (OnTriggerExit).
//!  * Mines use `bodyLayer` = 83711 which does NOT contain layer 11 (Body) but does contain layer 3 (Player), i.e. the
//!    CharacterController capsule of a player is what is hit (not the per-bone hit colliders).
//!  * Claymore uses `bodyLayer` = 67656 (Player + Body + SelfBody...), a ray along `forward` of length `rayLength`
//!    as trigger, and an OverlapBox for damage; everything inside is killed (RemoveHealth(10)), owner included.
//!  * There is no owner immunity anywhere: the owner (rootObject) is hurt like everybody else (suicide).
//!  * No explosion applies a force (no PlayerHealth.AddForce in these scripts).
//!  * ProximityMine.OnTriggerStay/Exit/Enter are physics callbacks (30 Hz fixed step); Stay is emulated at 1/30 s.

use super::data::{Fields, HandAnchors, Prefab, UField, WeaponDef};
use super::gun::{weapon_animation, HitCtx};
use super::pickup::Hands;
use super::{CamQuery, Hand, Item, WeaponSound, WeaponUi};
use crate::input::PlayerInput;
use crate::physics::{self, UnityLayer, UnityTag};
use crate::player::fpc::Fpc;
use crate::tween::{Ease, Tweens};
use crate::unity::*;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;


const FIXED_DT: f32 = 1.0 / 30.0;

/// Behaviour class check for the integrator (`def.class`).
pub fn is_mines(class: &str) -> bool {
    class == "WeaponHandSpawner"
}

// =============================================================================================
// Components / messages
// =============================================================================================

/// WeaponHandSpawner state on the weapon (the held item).
#[derive(Component, Default)]
pub struct MinesState {
    pub fire_timer: f32,
    pub can_place: bool,
    pub position: Vec3,
    pub rotation: Quat,
    /// `previewObject.gameObject.activeSelf`
    pub preview_active: bool,
    preview: Option<Entity>,
    preview_meshed: bool,
    inited: bool,
}

/// World-space stand-in for the weapon's `previewObject` (the real one is a child of the held weapon).
#[derive(Component)]
pub struct MinePreview {
    pub weapon: Entity,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MineKind {
    /// ProximityMine.cs (ProximityMine, StunMine, APMine objects)
    Proximity,
    /// Claymore.cs
    Claymore,
}

impl Mine {
    /// Claymore ray line (unparented; destroyed in OnDisable with the mine).
    pub fn line_entity(&self) -> Option<Entity> {
        self.line
    }
}

/// A placed ProximityMine.cs / Claymore.cs object.
#[derive(Component)]
pub struct Mine {
    pub kind: MineKind,
    /// Serialized fields of the ProximityMine / Claymore component.
    pub f: Fields,
    /// `_rootObject` (the placing player's entity).
    pub owner: Entity,
    pub can_explode: bool,
    /// `detonated` SyncVar.
    pub detonated: bool,
    activated: bool,
    can_activate: bool,
    bip_timer: f32,
    stay_acc: f32,
    inside: Vec<Entity>,
    // ActivateTrap coroutine
    stage: u8,
    stage_t: f32,
    graph: Option<Entity>,
    /// Unity local scale of the graph node (glb) when found.
    graph_base: Transform,
    graph_name: Option<String>,
    indicator_name: Option<String>,
    ray_length: f32,
    line: Option<Entity>,
    hitbox: Entity,
    /// Trigger box (ProximityMine BoxCollider, isTrigger) in world units, mine-local.
    trig_center: Vec3,
    trig_half: Vec3,
    touched: bool,
    touched2: bool,
}

/// The collider a bullet can hit on a placed mine (tag "Mine" Cube / the Claymore body, tag "Claymore").
#[derive(Component)]
pub struct MineHitbox {
    pub mine: Entity,
}

/// Weapon.TriggerEnvironment for tag "Mine" / "Claymore" -> `ChangeState()` (only if `canExplode`).
#[derive(Message, Clone, Copy, Debug)]
pub struct MineShot {
    pub mine: Entity,
}

/// Call from every bullet raycast hit loop where `TriggerEnvironment(hit.collider.gameObject, ...)` is called.
/// `hit` = the entity of the collider that was hit.
/// Shots queued by weapon systems this frame (a resource, so systems that both hit and read mines don't conflict).
#[derive(Resource, Default)]
pub struct PendingMineShots(pub Vec<MineShot>);

pub fn trigger_environment_mine(hit: Entity, hitboxes: &Query<&MineHitbox>, out: &mut PendingMineShots) {
    if let Ok(h) = hitboxes.get(hit) {
        out.0.push(MineShot { mine: h.mine });
    }
}

// =============================================================================================
// Geometry helpers
// =============================================================================================

pub(crate) fn closest_on_seg(a: Vec3, b: Vec3, p: Vec3) -> Vec3 {
    let ab = b - a;
    let l2 = ab.length_squared();
    let t = if l2 > 1e-9 { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    a + ab * t
}

/// Distance between segments p1q1 and p2q2 (Ericson, Real-Time Collision Detection).
pub(crate) fn seg_seg_dist(p1: Vec3, q1: Vec3, p2: Vec3, q2: Vec3) -> f32 {
    let (d1, d2, r) = (q1 - p1, q2 - p2, p1 - p2);
    let (a, e, f) = (d1.length_squared(), d2.length_squared(), d2.dot(r));
    let (s, t);
    if a <= 1e-9 && e <= 1e-9 {
        return r.length();
    }
    if a <= 1e-9 {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= 1e-9 {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let denom = a * e - b * b;
            let mut s0 = if denom > 1e-9 { ((b * f - c * e) / denom).clamp(0.0, 1.0) } else { 0.0 };
            let mut t0 = (b * s0 + f) / e;
            if t0 < 0.0 {
                t0 = 0.0;
                s0 = (-c / a).clamp(0.0, 1.0);
            } else if t0 > 1.0 {
                t0 = 1.0;
                s0 = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = s0;
            t = t0;
        }
    }
    ((p1 + d1 * s) - (p2 + d2 * t)).length()
}

/// The local player's CharacterController capsule (no Rapier collider exists for it): segment ends + radius.
pub(crate) fn player_capsule(fpc: &Fpc, tf: &Transform) -> (Vec3, Vec3, f32) {
    let r = fpc.p.cc_radius.max(0.01);
    let half = (fpc.cc_height * 0.5 - r).max(0.0);
    let c = tf.translation + fpc.cc_center;
    (c - Vec3::Y * half, c + Vec3::Y * half, r)
}

fn capsule_vs_obb(a: Vec3, b: Vec3, r: f32, center: Vec3, rot: Quat, half: Vec3) -> bool {
    let inv = rot.inverse();
    let (la, lb) = (inv * (a - center), inv * (b - center));
    let n = ((lb - la).length() / 0.1).ceil().max(1.0) as i32;
    (0..=n).any(|i| {
        let p = la + (lb - la) * (i as f32 / n as f32);
        (p.abs() - half).max(Vec3::ZERO).length() <= r
    })
}

fn angle_deg(n: Vec3, up: Vec3) -> f32 {
    // Vector3.Angle with a zero vector returns 0.
    if n.length_squared() < 1e-12 { 0.0 } else { n.angle_between(up).to_degrees() }
}

/// Query filter equal to physics.rs `filter(mask)` (not exported there).
fn qfilter(mask: u32) -> QueryFilter<'static> {
    QueryFilter::new().exclude_sensors().groups(CollisionGroups::new(Group::ALL, Group::from_bits_retain(mask)))
}

/// `Physics.OverlapBox(center, halfExtents, rotation, mask)`.
fn overlap_box(ctx: &RapierContext, center: Vec3, half: Vec3, rot: Quat, mask: u32) -> Vec<Entity> {
    let mut out = Vec::new();
    let shape = Collider::cuboid(half.x.max(1e-4), half.y.max(1e-4), half.z.max(1e-4));
    ctx.intersect_shape(center, rot, &*shape.raw, qfilter(mask), |e| {
        out.push(e);
        true
    });
    out
}

/// Raycast that ignores one collider (the claymore's own body, which the C# ray starts inside of).
fn ray_excluding(ctx: &RapierContext, origin: Vec3, dir: Vec3, max: f32, mask: u32, exclude: Entity) -> Option<physics::RayHit> {
    let dir = dir.normalize_or_zero();
    if dir == Vec3::ZERO {
        return None;
    }
    ctx.cast_ray_and_get_normal(origin, dir, max, false, qfilter(mask).exclude_collider(exclude)).map(|(entity, hit)| physics::RayHit {
        entity,
        point: hit.point,
        normal: hit.normal,
        distance: hit.time_of_impact,
    })
}

// =============================================================================================
// WeaponHandSpawner (the held weapon)
// =============================================================================================

#[allow(clippy::too_many_arguments)]
pub fn mines_update(
    mut commands: Commands,
    time: Res<Time>,
    input: Res<PlayerInput>,
    anchors: Res<HandAnchors>,
    assets: Res<AssetServer>,
    rapier: ReadRapierContext,
    src: PreviewSrc,
    mut players: Query<(Entity, &mut Fpc, &Transform, Option<&Hands>), Without<Item>>,
    mut cams: CamQuery,
    mut spawners: Query<(Entity, &mut Item, &mut MinesState, &mut Transform, &GlobalTransform, Option<&mut Tweens>), Without<crate::player::fpc::PlayerCamera>>,
    previews: Query<(Entity, &MinePreview)>,
    mut sounds: MessageWriter<WeaponSound>,
) {
    let dt = time.delta_secs();
    // Previews of weapons that no longer exist (weapon despawned: Destroy takes the child preview along).
    for (pe, p) in &previews {
        if !spawners.contains(p.weapon) {
            commands.entity(pe).despawn();
        }
    }
    let Ok(ctx) = rapier.single() else { return };
    let ctx = &ctx;
    let Ok((player_e, mut fpc, root_tf, hands)) = players.single_mut() else { return };
    let Ok((cam_tf, cam_g, mut cam_tw)) = cams.single_mut() else { return };
    let (cam_tf, cam_g, root_tf) = (*cam_tf, *cam_g, *root_tf);
    let fire1 = input.fire.value();
    let fire2 = input.fire2.value();
    for (we, mut item, mut st, mut tf, gt, mut tweens) in &mut spawners {
        let item = &mut *item;
        let def = item.def.clone();
        let w = &def.w;
        if !st.inited {
            st.inited = true;
            st.rotation = Quat::IDENTITY;
        }
        // WeaponUpdate() is handled by pickup.rs (item_behaviour_update).
        if st.fire_timer > 0.0 {
            st.fire_timer -= dt;
        }
        // Lazily create the preview stand-in (hidden).
        if st.preview.is_none() {
            let pe = commands.spawn((Name::new(format!("{}_Preview", def.name)), MinePreview { weapon: we }, Transform::default(), Visibility::Hidden)).id();
            st.preview = Some(pe);
        }
        // Its model: a copy of the weapon's own `Preview/PreviewParent` renderers (M_Mine_Preview_00, converted by
        // PrefabRender) once the weapon glb is loaded. Preview's lossy scale = weapon scale (2) * its local scale.
        if !st.preview_meshed {
            if let Some(pp) = crate::unity_pbr::find_named(we, "PreviewParent", &src.children, &src.names) {
                let parts: Vec<_> = src.children.get(pp).map(|c| c.iter().filter_map(|k| src.prims.get(k).ok()).map(|(m, mat, t)| (m.clone(), mat.clone(), *t)).collect()).unwrap_or_default();
                if !parts.is_empty() {
                    st.preview_meshed = true;
                    let local = def.prefab.objects.iter().find(|o| o.path.ends_with("/Preview")).map(|o| o.local_transform().scale).unwrap_or(Vec3::ONE);
                    let pp_tf = src.tfs.get(pp).copied().unwrap_or_default();
                    let pe = st.preview.unwrap();
                    commands.entity(pe).with_children(|p| {
                        // glb space (X mirrored) -> port space: 180 deg about Y, as for every weapon model.
                        p.spawn((Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)).with_scale(local * 2.0), Visibility::Inherited)).with_children(|p| {
                            p.spawn((pp_tf, Visibility::Inherited)).with_children(|p| {
                                for (m, mat, t) in parts {
                                    p.spawn((m, mat, t));
                                }
                            });
                        });
                    });
                }
            }
        }
        let pe = st.preview.unwrap();
        // `if (gameObject.layer == 7) { hide preview; return; }` (dropped weapon)
        if item.hand == Hand::None {
            if st.preview_active {
                st.preview_active = false;
                commands.entity(pe).insert(Visibility::Hidden);
            }
            continue;
        }
        // ---- HandlePlacement ------------------------------------------------------------------
        let max_d = w.f("maxInteractionDistance");
        let land = w.mask("landLayer");
        let claymore = w.b("claymore");
        let apmine = w.b("apmine");
        let cam_pos = cam_g.translation();
        let cam_fwd = cam_g.forward().as_vec3();
        let root_fwd = root_tf.forward().as_vec3();
        let hit = physics::raycast(ctx, cam_pos, cam_fwd, max_d, land);
        let interaction_distance = hit.as_ref().map(|h| h.distance).unwrap_or(max_d);
        let hit2 = physics::raycast(ctx, root_tf.translation + Vec3::Y, root_fwd, max_d, land);
        let interaction_distance_front = hit2.as_ref().map(|h| h.distance).unwrap_or(max_d);
        let hit3 = physics::raycast(ctx, cam_pos, cam_fwd, interaction_distance + 0.1, land);
        let n1 = hit.as_ref().map(|h| h.normal).unwrap_or(Vec3::ZERO); // hitInfo.normal (zero when no hit)
        let ang = angle_deg(n1, Vec3::Y);
        let place = hit3.is_some()
            && if apmine {
                ang >= 0.0
            } else if claymore {
                ang > 70.0
            } else {
                ang < 65.0
            };
        let hit4 = physics::raycast(ctx, root_tf.translation + root_fwd * interaction_distance_front + Vec3::Y, -Vec3::Y, 2.0, land);
        let place_down = hit4.as_ref().map(|h| angle_deg(h.normal, Vec3::Y) < 65.0).unwrap_or(false);
        // previewObject.transform.up = normal: FromToRotation(currentUp, normal) * currentRotation, where the preview is
        // a child of the held weapon (approximated by the weapon's world rotation).
        let base_rot = gt.compute_transform().rotation;
        let align = |n: Vec3| -> Quat {
            let n = n.normalize_or_zero();
            if n == Vec3::ZERO { base_rot } else { Quat::from_rotation_arc(base_rot * Vec3::Y, n) * base_rot }
        };
        if place {
            let h3 = hit3.as_ref().unwrap();
            st.position = h3.point;
            let n = if n1 == Vec3::ZERO { h3.normal } else { n1 };
            let preview_rot = align(n);
            st.rotation = if claymore { Transform::IDENTITY.looking_to(h3.normal, Vec3::Y).rotation } else { preview_rot };
            commands.entity(pe).insert(Transform::from_translation(st.position).with_rotation(preview_rot)); // previewObject.transform.up = normal (also for the Claymore)
            if !st.preview_active {
                st.preview_active = true;
                commands.entity(pe).insert(Visibility::Visible);
            }
            st.can_place = true;
        } else if w.b("canPlaceMaxDistance") && place_down && w.b("proximityMine") {
            let h4 = hit4.as_ref().unwrap();
            st.position = h4.point;
            st.rotation = align(h4.normal);
            commands.entity(pe).insert(Transform::from_translation(st.position).with_rotation(st.rotation));
            if !st.preview_active {
                st.preview_active = true;
                commands.entity(pe).insert(Visibility::Visible);
            }
            st.can_place = true;
        } else if st.preview_active {
            st.preview_active = false;
            commands.entity(pe).insert(Visibility::Hidden);
            st.can_place = false;
        }
        // ---- Update input -> Fire() -----------------------------------------------------------
        let in_r = item.in_right();
        let in_l = item.in_left();
        let (f_r, f_l) = if item.invert_fire { (fire2, fire1) } else { (fire1, fire2) };
        let mut fires = 0;
        if !w.b("onePressShoot") {
            if f_r > 0.1 && in_r {
                fires += 1;
            }
            if f_l > 0.1 && in_l {
                fires += 1;
            }
        } else {
            if f_r < 0.1 && in_r {
                item.is_clicked = true;
            }
            if f_l < 0.1 && in_l {
                item.is_clicked = true;
            }
        }
        if item.is_clicked && (w.b("onePressShoot") || (!w.b("reloadWeapon") && item.current_ammo <= 0)) {
            if f_r > 0.1 && in_r {
                fires += 1;
                item.is_clicked = false;
            }
            if f_l > 0.1 && in_l {
                fires += 1;
                item.is_clicked = false;
            }
        }
        for _ in 0..fires {
            // Fire(): !pause && no environment interactable && IsOwner && canMove && fireTimer <= 0
            let interactable = hands.map(|h| h.current_interactable.is_some()).unwrap_or(false);
            if !input.enabled || interactable || !fpc.can_move || st.fire_timer > 0.0 {
                continue;
            }
            st.fire_timer = w.f("timeBetweenFire");
            if item.current_ammo <= 0 {
                if let Some(c) = w.asset("nobulletClip") {
                    sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
                }
            }
            if item.current_ammo > 0 && st.can_place {
                // SpawnObject(objToSpawn, position, rotation)  (PauseManager.BetweenRounds ignored)
                spawn_mine(&mut commands, &assets, &def, st.position, st.rotation, player_e);
                // RemoveAmmo()
                item.current_ammo -= 1;
                // Weapon.WeaponUpdate inHandDespawn (drop + despawn at 0 ammo): pickup.rs.
                // CameraAnimation(): duration 0 for the mines that have no shake -> DOShake is a no-op.
                fpc.cam_base_speed = w.f("cameraLerpSpeed");
                if w.f("duration") > 0.0 {
                    super::camera_recoil(&def, &mut fpc, &cam_tf, &mut cam_tw, 1.0);
                }
                // WeaponAnimation()
                if w.f("animationDuration") > 0.0 {
                    weapon_animation(&def, item, &mut fpc, &mut tf, tweens.as_deref_mut(), &anchors);
                }
            }
        }
    }
}

/// `RpcLogic___SpawnObject`: Instantiate(objToSpawn) + sync owner/weapon.
fn spawn_mine(commands: &mut Commands, assets: &AssetServer, def: &WeaponDef, pos: Vec3, rot: Quat, owner: Entity) {
    let w = &def.w;
    let Some(obj) = w.asset("objToSpawn") else { return };
    let stem = obj.trim_start_matches("GameObject/").trim_end_matches(".prefab").to_string();
    let Ok(prefab) = Prefab::load(&format!("assets/data/projectiles/{stem}.json")) else {
        error!("mine prefab assets/data/projectiles/{stem}.json missing (extract it)");
        return;
    };
    let claymore = !(w.b("proximityMine") || w.b("apmine")) && w.b("claymore");
    let kind = if claymore { MineKind::Claymore } else { MineKind::Proximity };
    let comp_name = if claymore { "Claymore" } else { "ProximityMine" };
    let Some((root_obj, f)) = prefab.first_with(comp_name).map(|(o, f)| (o, f.clone())) else {
        error!("{stem}: no {comp_name} component");
        return;
    };
    let root_path = root_obj.path.clone();
    let root_scale = root_obj.local_transform().scale.x;
    let conv = |v: Vec3| Vec3::new(v.x, v.y, -v.z); // Unity local -> Bevy local
    let bc = root_obj.comp("BoxCollider").cloned().unwrap_or_default();
    let bc_center = conv(bc.v3("m_Center")) * root_scale;
    let bc_half = bc.v3("m_Size").abs() * 0.5 * root_scale;
    // `graph` / `indicator` GameObjects (nodes of the placed prefab glb), by name.
    let node_of = |k: &str| f.get(k).and_then(|v| v.as_str()).and_then(|r| r.strip_prefix("ref:")).and_then(|r| r.split("::").next()).and_then(|p| p.rsplit('/').next()).map(str::to_string);
    let (graph_name, indicator_name) = (node_of("graph"), node_of("indicator"));

    // Root entity: unit scale (Rapier colliders ignore it cleanly); all prefab dimensions are pre-scaled.
    let root = commands
        .spawn((Name::new(stem.clone()), Transform::from_translation(pos).with_rotation(rot), Visibility::default()))
        .id();
    let mut hitbox = root;
    // Bullet-hit collider: Claymore = root BoxCollider (tag "Claymore", layer 0); others = child "Cube" (layer 18, tag "Mine").
    // (The Claymore also has a "Cube" child tagged "Mine" whose TriggerEnvironment would throw an NRE in the C# -> omitted.)
    if claymore {
        hitbox = commands
            .spawn((
                Name::new("ClaymoreBody"),
                Transform::from_translation(bc_center),
                Visibility::default(),
                RigidBody::Fixed,
                Collider::cuboid(bc_half.x, bc_half.y, bc_half.z),
                UnityLayer(root_obj.layer),
                UnityTag("Claymore".into()),
                MineHitbox { mine: root },
            ))
            .id();
    } else if let Some(cube) = prefab.find(&format!("{root_path}/Cube")) {
        let lt = cube.local_transform();
        let size = cube.comp("BoxCollider").map(|b| b.v3("m_Size")).unwrap_or(Vec3::ONE).abs();
        let half = size * lt.scale * 0.5 * root_scale;
        hitbox = commands
            .spawn((
                Name::new("Cube"),
                Transform::from_translation(lt.translation * root_scale),
                Visibility::default(),
                RigidBody::Fixed,
                Collider::cuboid(half.x, half.y, half.z),
                UnityLayer(cube.layer),
                UnityTag("Mine".into()),
                MineHitbox { mine: root },
            ))
            .id();
    }
    commands.entity(root).add_child(hitbox);
    // Visual: the placed prefab itself (models/projectiles/<objToSpawn>.glb; its inactive Indicator, disabled Cube
    // renderer and scale-0 radius sphere are handled through PrefabRender + graph / indicator below).
    let scene = assets.load(GltfAssetLabel::Scene(0).from_asset(format!("models/projectiles/{stem}.glb")));
    commands.entity(root).with_children(|p| {
        p.spawn((WorldAssetRoot(scene), crate::unity_pbr::PrefabRender(format!("assets/data/projectiles/{stem}.json")), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)).with_scale(Vec3::splat(root_scale))));
    });
    commands.entity(root).insert(Mine {
        kind,
        f,
        owner,
        can_explode: false,
        detonated: false,
        activated: true,
        can_activate: true,
        bip_timer: 0.0,
        stay_acc: 0.0,
        inside: Vec::new(),
        stage: 0,
        stage_t: 0.0,
        graph: None,
        graph_base: Transform::IDENTITY,
        graph_name,
        indicator_name,
        ray_length: 0.0,
        line: None,
        hitbox,
        trig_center: bc_center,
        trig_half: bc_half,
        touched: false,
        touched2: false,
    });
}

// =============================================================================================
// Placed mines (ProximityMine.cs / Claymore.cs)
// =============================================================================================

/// The held weapon's glb nodes, to copy its preview model.
#[derive(SystemParam)]
pub struct PreviewSrc<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    names: Query<'w, 's, &'static Name>,
    tfs: Query<'w, 's, &'static Transform, (Without<Item>, Without<MinePreview>)>,
    prims: Query<'w, 's, (&'static Mesh3d, &'static MeshMaterial3d<crate::unity_pbr::UnityPbr>, &'static Transform), (Without<Item>, Without<MinePreview>)>,
}

/// Nodes of placed-mine glb scenes + material conversion (graph / indicator / realMat).
#[derive(SystemParam)]
pub struct MineScene<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    names: Query<'w, 's, &'static Name>,
    tfs: Query<'w, 's, &'static Transform, (Without<Mine>, Without<Item>)>,
    prims: Query<'w, 's, (), With<Mesh3d>>,
    mats: crate::unity_pbr::UnityMatParams<'w>,
}

#[derive(SystemParam)]
pub struct MineOut<'w, 's> {
    sounds: MessageWriter<'w, WeaponSound>,
    hit: HitCtx<'w, 's>,
    parents: Query<'w, 's, &'static ChildOf>,
    xf: Query<'w, 's, &'static GlobalTransform>,
}

impl MineOut<'_, '_> {
    fn snd(&mut self, clip: Option<String>, at: Option<Vec3>) {
        if let Some(clip) = clip {
            self.sounds.write(WeaponSound { clip, volume: 1.0, at, pitch: 1.0, linear_max: 100.0, source: None, rolloff: None });
        }
    }
    /// `collider.GetComponentInParent<PlayerHealth>()`
    fn ph_of(&self, mut e: Entity) -> Option<Entity> {
        loop {
            if self.hit.healths.contains(e) {
                return Some(e);
            }
            e = self.parents.get(e).ok()?.parent();
        }
    }
    fn is_killed(&self, e: Entity) -> bool {
        self.hit.healths.get(e).map(|h| h.dead || h.health <= 0.0).unwrap_or(true)
    }
    /// `if (health - dmg <= 0) ChangeKilledState(true)`, then always `RemoveHealth(dmg)` (ProximityMine.cs:275-301):
    /// health really becomes health - dmg. True when killed.
    fn remove_health(&mut self, e: Entity, dmg: f32) -> bool {
        let Ok(mut h) = self.hit.healths.get_mut(e) else { return false };
        let lethal = h.health - dmg <= 0.0;
        h.killed |= lethal;
        h.health -= dmg;
        lethal
    }
    /// `ph.Explode(false, true, name, ph.position - mine.position, ragdollEjectForce, mine.position)` for a victim that just died.
    fn ragdoll_hit(&mut self, e: Entity, from: Vec3, force: f32) {
        let vp = self.xf.get(e).map(|g| g.translation()).unwrap_or(from);
        if let Ok(mut h) = self.hit.healths.get_mut(e) {
            if h.health <= 0.0 {
                h.last_hit = Some(((vp - from).normalize_or_zero(), force, from));
            }
        }
    }
    /// `ph.suicide = true` (owner killed by its own mine).
    fn mark_suicide(&mut self, e: Entity) {
        if let Ok(mut h) = self.hit.healths.get_mut(e) {
            h.suicide = true;
        }
    }
    /// `ChangeKilledState(true)` + `RemoveHealth(10)` (Claymore.cs:253-254 / 263-264): health - 10 (max health is 4,
    /// so this always kills).
    fn force_kill(&mut self, e: Entity) {
        if let Ok(mut h) = self.hit.healths.get_mut(e) {
            h.killed = true;
            h.health -= 10.0;
        }
    }
    /// ProximityMine.HitMarker(false): audio.PlayOneShot(hitSfx) + hit marker UI. ProximityMine.cs:397: `audio` is the
    /// mine's own 3D AudioSource (Linear 1..100 on every mine prefab), so the hit sound comes from the mine.
    fn hit_marker(&mut self, source: Entity, sfx: Option<String>, at: Vec3) {
        self.snd(sfx, Some(at));
        self.hit.ui.write(WeaponUi::HitMarker { head: false, source: Some(source) });
    }
}

#[allow(clippy::too_many_arguments)]
pub fn placed_mines_update(
    mut commands: Commands,
    time: Res<Time>,
    rapier: ReadRapierContext,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut mines: Query<(Entity, &mut Mine, &Transform)>,
    mut players: Query<(Entity, &mut Fpc, &Transform), (Without<Item>, Without<Mine>)>,
    mut cams: CamQuery,
    mut o: MineOut,
    mut ms: MineScene,
) {
    let dt = time.delta_secs();
    let Ok(ctx) = rapier.single() else { return };
    let ctx = &ctx;
    let Ok((player_e, mut fpc, player_tf)) = players.single_mut() else { return };
    let player_tf = *player_tf;
    let Ok((_, _, mut cam_tw)) = cams.single_mut() else { return };
    let (pa, pb, pr) = player_capsule(&fpc, &player_tf);
    let _ = &assets;
    // Weapon.TriggerEnvironment -> ChangeState() (only when canExplode)
    for s in std::mem::take(&mut o.hit.mine_shot.0) {
        if let Ok((_, mut m, _)) = mines.get_mut(s.mine) {
            if m.can_explode {
                m.detonated = true;
            }
        }
    }
    for (e, mut m, tf) in &mut mines {
        let m = &mut *m;
        let pos = tf.translation;
        let rot = tf.rotation;
        let fwd = tf.forward().as_vec3();
        let instant = m.kind == MineKind::Proximity && m.f.b("instantExplode");
        // ---- Start(): ActivateTrap coroutine --------------------------------------------------
        match (m.kind, m.stage) {
            (MineKind::Proximity, 0) | (MineKind::Claymore, 0) => {
                m.stage_t += dt;
                let wait = if m.kind == MineKind::Claymore { 1.0 } else if instant { 0.0 } else { 1.0 };
                if m.stage_t >= wait {
                    m.can_explode = true;
                    o.snd(m.f.asset("activationSound"), Some(pos)); // audio.PlayOneShot(activationSound)
                    m.stage = 1;
                    m.stage_t = 0.0;
                    if m.kind == MineKind::Claymore {
                        let box_z = m.f.v3("boxdimensions").z;
                        let hit = ray_excluding(ctx, pos + fwd * 0.2, fwd, box_z, m.f.mask("playerLayer"), m.hitbox);
                        m.ray_length = match hit {
                            Some(h) => h.point.distance(pos),
                            None => box_z,
                        };
                        // RenderObject: LineRenderer (Sprites/Default, red, alpha 0.025, width 0.02), unparented.
                        let mesh = meshes.add(Cuboid::new(0.02, 0.02, m.ray_length));
                        let mat = mats.add(StandardMaterial { base_color: Color::srgba(1.0, 0.0, 0.0, 0.025), alpha_mode: AlphaMode::Blend, unlit: true, ..default() });
                        let line = commands
                            .spawn((Name::new("RaycastLine"), Mesh3d(mesh), MeshMaterial3d(mat), Transform::from_translation(pos + fwd * (m.ray_length * 0.5)).with_rotation(rot)))
                            .id();
                        m.line = Some(line);
                    } else if !instant {
                        // graph.transform.DOScale(1.858382, 0.2): the radius sphere node of the placed prefab glb
                        // (scale 0 in the prefab; M_ProximityMine_00 / M_StunMine_00 = S_ProximityMine_00 ring).
                        if let Some(g) = m.graph_name.as_deref().and_then(|n| crate::unity_pbr::find_named(e, n, &ms.children, &ms.names)) {
                            m.graph_base = ms.tfs.get(g).copied().unwrap_or_default();
                            m.graph = Some(g);
                        }
                    }
                }
            }
            (MineKind::Proximity, 1) => {
                m.stage_t += dt;
                if let Some(g) = m.graph {
                    let k = Ease::OutQuad.eval((m.stage_t / 0.2).clamp(0.0, 1.0));
                    commands.entity(g).try_insert(m.graph_base.with_scale(Vec3::splat(1.858382 * k)));
                }
                if m.stage_t >= 0.2 {
                    // graph.GetComponent<MeshRenderer>().materials = { realMat }; indicator.SetActive(true)
                    if let (Some(g), Some(real)) = (m.graph, m.f.asset("realMat").and_then(|a| a.strip_prefix("Material/").and_then(|a| a.strip_suffix(".mat")).map(str::to_string))) {
                        if let Some(h) = ms.mats.material(&real) {
                            for k in ms.children.get(g).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default() {
                                if ms.prims.contains(k) {
                                    commands.entity(k).try_remove::<MeshMaterial3d<StandardMaterial>>().try_insert(MeshMaterial3d(h.clone()));
                                }
                            }
                        }
                    }
                    if let Some(i) = m.indicator_name.as_deref().and_then(|n| crate::unity_pbr::find_named(e, n, &ms.children, &ms.names)) {
                        commands.entity(i).try_insert(Visibility::Inherited);
                    }
                    m.stage = 2;
                }
            }
            _ => {}
        }
        // ---- Claymore.Update: ray trigger -----------------------------------------------------
        if m.kind == MineKind::Claymore && m.can_explode {
            let mut hit = physics::ray(ctx, pos, fwd, m.ray_length, m.f.mask("bodyLayer"));
            if !hit {
                // local player's capsule (no collider in Rapier)
                hit = !o.is_killed(player_e) && seg_seg_dist(pos, pos + fwd * m.ray_length, pa, pb) <= pr;
            }
            if hit {
                m.detonated = true;
                m.can_explode = false;
            }
        }
        // ---- ProximityMine trigger callbacks (OnTriggerEnter / Stay / Exit) -------------------
        if m.kind == MineKind::Proximity {
            let center = pos + rot * m.trig_center;
            let mut cur: Vec<Entity> = Vec::new();
            if capsule_vs_obb(pa, pb, pr, center, rot, m.trig_half) && !cur.contains(&player_e) {
                cur.push(player_e);
            }
            // other players (dummy): their CharacterController capsule lives on layer Player(3)
            for ce in overlap_box(ctx, center, m.trig_half, rot, 1 << physics::layer::PLAYER) {
                if let Some(ph) = o.ph_of(ce) {
                    if ph != player_e && !cur.contains(&ph) {
                        cur.push(ph);
                    }
                }
            }
            if std::env::var_os("RESTRAFE_DEBUG_MINES").is_some() && cur != m.inside {
                println!("MINE {e:?} at {pos:?} inside {:?} -> {cur:?} (player {player_e:?} at {pa:?}) can_explode={} can_activate={}", m.inside, m.can_explode, m.can_activate);
            }
            let entered: Vec<Entity> = cur.iter().copied().filter(|x| !m.inside.contains(x)).collect();
            // A player that died / was despawned inside the trigger sends no OnTriggerExit in Unity (disabled colliders).
            let exited: Vec<Entity> = m.inside.iter().copied().filter(|x| !cur.contains(x) && !o.is_killed(*x)).collect();
            m.inside = cur;
            if !entered.is_empty() && instant && m.can_activate {
                // OnTriggerEnter
                m.can_activate = false;
                m.detonated = true; // ChangeState()
            }
            // OnTriggerStay (fixed step): bip sound (bipTimer reloads to 0.006 s -> every physics step)
            m.stay_acc += dt;
            while m.stay_acc >= FIXED_DT {
                m.stay_acc -= FIXED_DT;
                if m.can_explode && !instant {
                    for _ in 0..m.inside.len() {
                        m.bip_timer -= FIXED_DT;
                        if m.bip_timer < 0.0 {
                            m.bip_timer = 0.006;
                            let clip = m.f.asset("bipClip");
                            o.snd(clip, None); // SoundManager.PlaySound(bipClip)
                        }
                    }
                }
            }
            // OnTriggerExit
            if !exited.is_empty() && m.can_explode && !instant && m.can_activate {
                m.can_activate = false;
                m.detonated = true; // ChangeState()
            }
        }
        // ---- Update: HandleExplosion ----------------------------------------------------------
        if m.detonated && m.activated {
            let player_pos = player_tf.translation;
            match m.kind {
                MineKind::Proximity => explode_proximity(e, m, pos, ctx, &mut o, &mut fpc, player_e, (pa, pb, pr)),
                MineKind::Claymore => explode_claymore(m, pos, rot, fwd, ctx, &mut o, &mut fpc, player_e, (pa, pb, pr)),
            }
            m.detonated = false;
            m.activated = false;
            // ExplodeObservers: LocalScreenshake on every Player (only the local camera exists), Destroy, VFX, sound.
            let duration = m.f.f("duration");
            if duration > 0.0 {
                let max_d = m.f.f("maxDistance");
                let t = if max_d > 0.0 { (player_pos.distance(pos) / max_d).clamp(0.0, 1.0) } else { 1.0 };
                let strength = lerp(m.f.f("maxStrength"), m.f.f("minStrength"), t);
                cam_tw.kill(); // transform.DOKill()
                cam_tw.shake_rotation_scalar(duration, strength, m.f.i("vibrato"), m.f.f("randomness")); // PlayerHealth.LocalScreenshake
            }
            o.snd(m.f.asset("explosionClip"), None); // SoundManager.PlaySound
            let (radius, color) = match m.kind {
                MineKind::Proximity => (m.f.f("explosionRadius").max(0.5), if m.f.b("stunMine") { LinearRgba::rgb(0.3, 1.0, 6.0) } else { LinearRgba::rgb(6.0, 3.0, 0.5) }),
                MineKind::Claymore => (2.0, LinearRgba::rgb(6.0, 3.0, 0.5)),
            };
            // explosionVfx prefab (VFX_ClaymoreExplosion_00 / VFX_RocketExplosion_00 / VFX_StunMine_00), spawned by fx.rs.
            let _ = (color, radius);
            // Instantiate(explosionVfx, position, identity) (ProximityMine.cs:550, Claymore.cs:467).
            commands.spawn(crate::fx::VfxRequest { prefab: m.f.asset("explosionVfx"), pos, normal: Vec3::NEG_Z });
            if let Some(l) = m.line.take() {
                commands.entity(l).despawn(); // OnDisable: Destroy(lineObject)
            }
            commands.entity(e).despawn(); // Destroy(gameObject)
        }
    }
}

/// ProximityMine.HandleExplosion (IsOwner is always true in single player).
fn explode_proximity(e: Entity, m: &mut Mine, pos: Vec3, ctx: &RapierContext, o: &mut MineOut, fpc: &mut Fpc, player_e: Entity, cap: (Vec3, Vec3, f32)) {
    let radius = m.f.f("explosionRadius");
    let damage = m.f.f("damage");
    let stun = m.f.b("stunMine");
    let stun_time = m.f.f("stunTime");
    let hit_sfx = m.f.asset("hitSfx");
    // OverlapSphere(position, explosionRadius, bodyLayer) -> distinct PlayerHealth
    let mut list: Vec<Entity> = Vec::new();
    if closest_on_seg(cap.0, cap.1, pos).distance(pos) <= radius + cap.2 && o.hit.healths.contains(player_e) {
        list.push(player_e);
    }
    for ce in physics::overlap_sphere(ctx, pos, radius, m.f.mask("bodyLayer")) {
        if let Some(ph) = o.ph_of(ce) {
            if !list.contains(&ph) {
                list.push(ph);
            }
        }
    }
    for ph in list {
        if o.is_killed(ph) {
            continue;
        }
        if ph == m.owner {
            // owner: suicide (ChangeKilledState, suicide = true, Explode ragdoll) when it would die
            o.hit_marker(e, hit_sfx.clone(), pos);
            if o.remove_health(ph, damage) {
                o.mark_suicide(ph);
            }
            o.ragdoll_hit(ph, pos, m.f.f("ragdollEjectForce"));
            m.touched = true;
        } else {
            if o.hit.healths.get(ph).map(|h| h.health - damage <= 0.0).unwrap_or(false) {
                fpc.kill_shock_wave(); // KillShockWave (+ SendKillLog / Explode ragdoll TODO)
            }
            o.hit_marker(e, hit_sfx.clone(), pos);
            o.remove_health(ph, damage);
            o.ragdoll_hit(ph, pos, m.f.f("ragdollEjectForce"));
            m.touched2 = true;
        }
        if stun {
            if let Ok(mut h) = o.hit.healths.get_mut(ph) {
                h.stun(stun_time); // TaserEnemy(ph, stunTime)
            }
        }
    }
}

/// Claymore.HandleExplosion.
#[allow(clippy::too_many_arguments)]
fn explode_claymore(m: &mut Mine, pos: Vec3, rot: Quat, fwd: Vec3, ctx: &RapierContext, o: &mut MineOut, fpc: &mut Fpc, player_e: Entity, cap: (Vec3, Vec3, f32)) {
    let bd = m.f.v3("boxdimensions");
    let half = Vec3::new(bd.x * 0.5, bd.y * 0.5, m.ray_length * 0.5);
    let center = pos + fwd * (m.ray_length * 0.5);
    // One entry per collider (the `touched`/`touched2` flags make only the first owner/non-owner entry count).
    let mut list: Vec<Entity> = Vec::new();
    if capsule_vs_obb(cap.0, cap.1, cap.2, center, rot, half) && o.hit.healths.contains(player_e) {
        list.push(player_e);
    }
    for ce in overlap_box(ctx, center, half, rot, m.f.mask("bodyLayer")) {
        if let Some(ph) = o.ph_of(ce) {
            list.push(ph);
        }
    }
    for ph in list {
        if ph == m.owner && !m.touched {
            // ChangeKilledState, RemoveHealth(10), suicide, Explode, SetKiller (no isKilled check in Claymore.cs:249-270)
            o.force_kill(ph);
            o.mark_suicide(ph);
            o.ragdoll_hit(ph, pos, m.f.f("ragdollEjectForce"));
            m.touched = true;
        } else if ph != m.owner && !m.touched2 {
            o.force_kill(ph); // ChangeKilledState(true) + RemoveHealth(10)
            o.ragdoll_hit(ph, pos, m.f.f("ragdollEjectForce"));
            fpc.kill_shock_wave();
            m.touched2 = true;
        }
    }
}

