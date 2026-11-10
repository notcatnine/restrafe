//! Port of `PlayerHealth.cs` (health 4, damage, death) and the training dummy: a PlayerIK body
//! (real model + the prefab's exact per-bone hit colliders on the Body layer) carrying PlayerHealth,
//! so weapons hit it through the same code path as another player.

use crate::physics::{UnityLayer, layer};
use crate::weapons::data::{Prefab, UField};
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use bevy::world_serialization::WorldInstanceReady;
use std::sync::Arc;

#[derive(Component)]
pub struct PlayerHealth {
    pub health: f32,
    pub full_health: f32,
    pub dead: bool,
    /// Dummy only: seconds until respawn after death (the real game ends the round instead).
    pub respawn_in: f32,
    /// TaserEnemy: controller.canMove = false (stunVFX on) until any pending UnfreezePlayer coroutine ends.
    pub stunned: bool,
    /// Pending `UnfreezePlayer(stunTime)` coroutines (seconds left). Each sets canMove = true when it ends, so a
    /// second taser during a stun is cut short by the first one's coroutine (PlayerHealth.cs:344-354, 1217-1265).
    pub unfreeze_in: Vec<f32>,
    /// Pending PlayerHealth.AddForce / BumpPlayer impulses -> owner's CustomAddForce (consumed by the motor).
    pub pending_forces: Vec<(Vec3, f32)>,
    /// Killing hit for the ragdoll (ExplodeForAll args): (ejectForceDir, weapon ragdollEjectForce, hit point).
    pub last_hit: Option<(Vec3, f32, Vec3)>,
    /// `PlayerHealth.suicide` (set by the owner's own explosion; kill-feed text only).
    pub suicide: bool,
    /// `PlayerHealth.fellVoid` (FirstPersonController: y < -300).
    pub fell_void: bool,
    /// `PlayerHealth.isKilled` (ChangeKilledState(true) / FirstPersonController.DespawnObject). A death without it
    /// (e.g. a PhysicsProp impact: RemoveHealth only) goes through `SafeDeathFix` (death.rs).
    pub killed: bool,
}

impl PlayerHealth {
    /// `PlayerHealth.AddForce(dir, factor)` == controller.customForceScript.AddForce(dir, factor).
    pub fn add_force(&mut self, dir: Vec3, factor: f32) {
        self.pending_forces.push((dir, factor));
    }
    /// `TaserEnemy(enemy, stunTime)`: canMove = false + StartCoroutine(UnfreezePlayer(stunTime)).
    pub fn stun(&mut self, seconds: f32) {
        self.stunned = true;
        self.unfreeze_in.push(seconds);
    }
    /// Advances the UnfreezePlayer coroutines; true when one ended this frame (canMove = true).
    pub fn tick_stun(&mut self, dt: f32) -> bool {
        let mut fired = false;
        self.unfreeze_in.retain_mut(|t| {
            *t -= dt;
            if *t <= 0.0 {
                fired = true;
                false
            } else {
                true
            }
        });
        if fired {
            self.stunned = false;
        }
        fired
    }
}

impl Default for PlayerHealth {
    fn default() -> Self {
        PlayerHealth { health: 4.0, full_health: 4.0, dead: false, respawn_in: 0.0, stunned: false, unfreeze_in: Vec::new(), pending_forces: Vec::new(), last_hit: None, suicide: false, fell_void: false, killed: false }
    }
}

/// A hit collider belonging to a PlayerHealth owner (Unity: collider under the player root, layer Body).
#[derive(Component)]
pub struct HitPart {
    pub owner: Entity,
    /// GameObject name, e.g. "Head_Col", "Neck_1_Col" (headshots), "Leg_3_L_Col".
    pub name: String,
}

#[derive(Component)]
pub struct Dummy;

/// A dead player's body (`PlayerHealth.aboubiRagdoll` instance): a Dummy that never respawns and has no hit colliders,
/// motor or blocker capsule. `pending` = ExplodeForAll args when its rig/dress were not ready yet (see death.rs).
#[derive(Component)]
pub struct Corpse {
    pub pending: Option<(Vec3, f32, Vec3)>,
    /// Frames since the humanoid rig started animating (the ragdoll starts from that pose).
    pub ready_frames: u32,
    /// Death order (oldest corpses are removed first, see death.rs MAX_CORPSES).
    pub serial: u32,
    /// `PlayerHealth.Hat(PlayerSetup.hat, ..)`: the hat that pops is the living player's, so it starts from the
    /// player body's hat pose (Graphics at (0, -0.2, 0), animated), not from the corpse's own head.
    pub hat_from: Option<GlobalTransform>,
}

/// Every `aboubiRagdoll` body (spare or in use): dressed + rigged like the dummy, never animated.
#[derive(Component)]
pub struct CorpseBody;

/// The pre-built, hidden corpse waiting for the next local death (death.rs). It carries no [`Dummy`] /
/// [`PlayerHealth`] / [`Corpse`], so no gameplay system sees it until it is used.
#[derive(Component)]
pub struct SpareCorpse;

/// The local player's invisible third-person body: the PlayerIK body hit colliders on layer SelfBody (16), as
/// `PlayerSetup.SetGameLayerRecursive(graphics, 16)` does for the owner. Child of the player root.
#[derive(Component)]
pub struct LocalBody;

/// The local body's Graphics node: PlayerSetup.ChangeSkinWidth (ServerRpc + ObserversRpc, both RunLocally, so it also
/// runs for the owner) sets graphics.localPosition = (0, -skinWidth, 0) whenever FirstPersonController changes the
/// CharacterController skin width (0.07 crouched, 0.2 otherwise).
#[derive(Component)]
pub struct LocalGraphics;

/// `FirstPersonController.Update` -> `ChangeSkinWidth`: the owner's Graphics follows the skin width.
pub fn local_graphics_skin(players: Query<&crate::player::fpc::Fpc>, mut q: Query<&mut Transform, With<LocalGraphics>>) {
    let Ok(f) = players.single() else { return };
    for mut t in &mut q {
        let y = -f.cc_skin;
        if (t.translation.y - y).abs() > 1e-5 {
            t.translation.y = y;
        }
    }
}

/// A local body part the owner sees while sliding (FirstPersonController.torso / legLeft / legRight).
#[derive(Component)]
struct LocalSlideBodyPart;

/// FirstPersonController slide body parts: SetActive(showBodyParts) on torso / legs (the rest of the local body stays
/// hidden: gameObjectsToDisableForMe).
/// Debug: RESTRAFE_BONE_LOG=1 prints the local body's hips / feet / toes world y (and the player root) every 0.25 s.
fn bone_log(bodies: Query<(Entity, &GlobalTransform), With<LocalBody>>, kids: Query<&Children>, named: Query<(&Name, &GlobalTransform)>, time: Res<Time>, mut t: Local<f32>) {
    if std::env::var_os("RESTRAFE_BONE_LOG").is_none() {
        return;
    }
    *t += time.delta_secs();
    if *t < 0.25 {
        return;
    }
    *t = 0.0;
    for (b, g) in &bodies {
        let mut out = format!("BONES t={:.2} root={:.3}", time.elapsed_secs(), g.translation().y);
        for d in kids.iter_descendants(b) {
            if let Ok((n, ng)) = named.get(d) {
                if matches!(n.as_str(), "Hips" | "Leg.L" | "Leg.R" | "Foot.L" | "Foot.R" | "Shin.L" | "Shin.R" | "Toes.L" | "Toes.R") {
                    let l = g.affine().inverse().transform_point3(ng.translation());
                    if n.as_str() == "Hips" || n.as_str() == "Leg.L" {
                        let (_, rq, _) = (g.affine().inverse() * ng.affine()).to_scale_rotation_translation();
                        out += &format!(" {}_rot(bevy local)={:.3?}", n, rq);
                    }
                    out += &format!(" {}={:.3} local(unity x,y,z)=({:.3},{:.3},{:.3})", n, ng.translation().y, l.x, l.y, -l.z);
                }
            }
        }
        println!("{out}");
    }
}

fn local_slide_body(players: Query<&crate::player::fpc::Fpc>, mut parts: Query<(&mut Visibility, &Name, &InheritedVisibility), With<LocalSlideBodyPart>>, mut n: Local<u32>) {
    let Ok(f) = players.single() else { return };
    // Debug: RESTRAFE_SHOW_BODY=1 forces them on.
    let want = if f.show_body_parts || std::env::var_os("RESTRAFE_SHOW_BODY").is_some() { Visibility::Visible } else { Visibility::Inherited };
    *n += 1;
    for (mut v, name, iv) in &mut parts {
        if *n == 300 && std::env::var_os("RESTRAFE_SHOW_BODY").is_some() {
            println!("BODYPART {name} vis={:?} inherited={}", *v, iv.get());
        }
        if *v != want {
            *v = want;
        }
    }
}

/// The dummy's "Graphics" node (hidden on death like PlayerHealth.graphics).
#[derive(Component)]
pub struct DummyGraphics(pub Entity);

#[derive(Resource)]
pub struct PlayerPrefab(pub Arc<Prefab>);

/// What a PlayerIK body instance is used for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BodyKind {
    /// Training dummy: Body (11) hit colliders, motor, respawns.
    Dummy,
    /// Dead player's ragdoll body: no colliders (its Graphics stay hidden until the ragdoll starts).
    Corpse,
    /// Local player's hit colliders on SelfBody (16), owned by the player root.
    Local,
    /// Customization menu preview (`COSMETICSSS/PreviewAboubi`): no colliders, rendered on layer 23 only.
    Preview,
}

/// The customization menu's preview body (see cosmetics.rs).
#[derive(Component)]
pub struct PreviewBody;

/// `COSMETICSSS/PreviewAboubi/PF_Aboubi_04`: a dressed, idle PlayerIK body without colliders.
pub fn spawn_preview_body(commands: &mut Commands, assets: &AssetServer, tf: Transform) -> Entity {
    let root = commands.spawn((Name::new("PreviewAboubi"), PreviewBody, tf, Visibility::default())).id();
    attach_body_scene(commands, assets, root, root, BodyKind::Preview);
    root
}

pub fn spawn_dummy(commands: &mut Commands, assets: &AssetServer, pos: Vec3, yaw_unity: f32) -> Entity {
    let root = commands
        .spawn((
            Name::new("TrainingDummy"),
            Dummy,
            PlayerHealth::default(),
            dummy_motor_bundle(pos),
            Transform::from_translation(pos).with_rotation(crate::unity::unity_euler(0.0, yaw_unity, 0.0)),
            Visibility::default(),
        ))
        .id();
    commands.entity(root).with_children(|p| {
        // CharacterController capsule (layer Player): blocks movement. Queries whose mask includes Player (e.g. the frag
        // grenade's bodyLayer rays) hit it first and resolve it with GetComponentInParent<PlayerHealth>, hence the HitPart.
        p.spawn((
            Transform::from_xyz(0.0, 1.0, 0.0),
            Collider::capsule_y(0.6, 0.4),
            UnityLayer(layer::PLAYER),
            HitPart { owner: root, name: "Player".into() },
        ));
    });
    attach_body_scene(commands, assets, root, root, BodyKind::Dummy);
    root
}

/// The body behind `Instantiate(aboubiRagdoll, ..)`, built ahead of time (hidden) so a death shows it on the same
/// frame: the PlayerIK scene, rig and dress take a few frames to be ready. death.rs turns it into a [`Corpse`].
pub fn spawn_spare_corpse(commands: &mut Commands, assets: &AssetServer, tf: Transform) -> Entity {
    let root = commands.spawn((Name::new("PlayerRagdoll"), CorpseBody, SpareCorpse, tf, Visibility::Hidden)).id();
    attach_body_scene(commands, assets, root, root, BodyKind::Corpse);
    root
}

/// The local player's hidden hit-collider body under `player` (see [`LocalBody`]).
pub fn spawn_local_body(commands: &mut Commands, assets: &AssetServer, player: Entity) -> Entity {
    let body = commands.spawn((Name::new("LocalBody"), LocalBody, Transform::default(), Visibility::Hidden, ChildOf(player))).id();
    attach_body_scene(commands, assets, body, player, BodyKind::Local);
    body
}

/// PlayerIK.glb instance under `parent`; hit colliders belong to `owner` (the PlayerHealth entity).
fn attach_body_scene(commands: &mut Commands, assets: &AssetServer, parent: Entity, owner: Entity, kind: BodyKind) {
    let scene = assets.load(GltfAssetLabel::Scene(0).from_asset("models/player/PlayerIK.glb"));
    commands.entity(parent).with_children(|p| {
        // AssetRipper mirrors X, we mirror Z -> rotate the glb 180 degrees.
        p.spawn((WorldAssetRoot(scene), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI))))
            .observe(move |ev: On<WorldInstanceReady>, mut commands: Commands, children: Query<&Children>, names: Query<&Name>, prefab: Res<PlayerPrefab>, assets: Res<AssetServer>| {
                setup_dummy_instance(ev.entity, owner, kind, &mut commands, &children, &names, &prefab.0, &assets);
            });
    });
}

/// `PlayerSetup.StunMatObservers`: `stunVFX.SetActive(!canMove)` (taser / stun grenade). The other half of that RPC
/// sets "Float2" on the body / arm materials, which S_StunnedAboubi_00 does not have (no visible effect).
/// Holds the spawned particle root while active.
#[derive(Component)]
pub struct StunVfx(Option<Entity>);

pub const STUN_VFX_KEY: &str = "playerfx:VFX_TaserStunned_00";

pub fn stun_vfx(mut commands: Commands, mut vfx: crate::vfx::Vfx, mut q: Query<(Entity, &mut StunVfx, &PlayerHealth)>) {
    for (owner, mut sv, ph) in &mut q {
        let stunned = ph.stunned && !ph.dead;
        match (stunned, sv.0) {
            (true, None) => {
                info!("stun start (TaserEnemy) on {owner:?}, unfreeze in {:?}", ph.unfreeze_in);
                if !vfx.is_registered(STUN_VFX_KEY) {
                    match crate::vfx::def::PrefabDef::load_sub("assets/data/player/PlayerIK.json", Some("PlayerIK/Graphics/VFX_TaserStunned_00")) {
                        Ok(mut def) => {
                            // Inactive in the prefab; SetActive(true) is what turns it on.
                            if let Some(o) = def.objects.first_mut() {
                                o.active = true;
                            }
                            vfx.register(STUN_VFX_KEY, def)
                        }
                        Err(e) => {
                            warn!("stun vfx: {e}");
                            continue;
                        }
                    }
                }
                if let Some(root) = vfx.spawn(STUN_VFX_KEY, Vec3::ZERO, Quat::IDENTITY, crate::vfx::SpawnOpts { persistent: true, ..default() }) {
                    // Graphics at (0, -0.2, 0) + the node's local (0, 1, 0); glb frame -> port frame (Y 180).
                    vfx.commands.entity(root).insert((Transform::from_xyz(0.0, 0.8, 0.0).with_rotation(Quat::from_rotation_y(std::f32::consts::PI)), ChildOf(owner)));
                    sv.0 = Some(root);
                }
            }
            (false, Some(root)) => {
                info!("stun end (UnfreezePlayer) on {owner:?}");
                commands.entity(root).try_despawn();
                sv.0 = None;
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn setup_dummy_instance(scene_root: Entity, owner: Entity, kind: BodyKind, commands: &mut Commands, children: &Query<&Children>, names: &Query<&Name>, prefab: &Prefab, assets: &AssetServer) {
    let mut n_cols = 0;
    let mut sample = Vec::new();
    // Walk the instance, tracking the Unity path of each node.
    let mut stack: Vec<(Entity, String)> = vec![(scene_root, String::new())];
    while let Some((e, path)) = stack.pop() {
        let name = names.get(e).map(|n| n.as_str().to_string()).unwrap_or_default();
        let name = if path.is_empty() && name.starts_with("Scene") { String::new() } else { name };
        let my_path = if name.is_empty() { path.clone() } else if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
        // PlayerSetup.stunVFX (inactive in the prefab): its particles are spawned by `stun_vfx` while the owner is stunned.
        if name == "VFX_TaserStunned_00" && matches!(kind, BodyKind::Dummy | BodyKind::Local) {
            commands.entity(owner).insert(StunVfx(None));
        }
        // Only the body is visible on a remote player: hide the first-person rig, HUD canvas, etc.
        if matches!(name.as_str(), "CameraLeanPivot" | "Canvas" | "SlopeDetection" | "CamPointPosition" | "VFX_TaserStunned_00") {
            commands.entity(e).insert(Visibility::Hidden);
            continue;
        }
        // PlayerSetup.OnStartClient (every player instance): graphics.transform.localPosition = (0, -0.2, 0).
        if name == "Graphics" && matches!(kind, BodyKind::Dummy | BodyKind::Local) {
            commands.entity(e).insert(Transform::from_xyz(0.0, -0.2, 0.0));
            if kind == BodyKind::Local {
                commands.entity(e).insert(LocalGraphics);
            }
        }
        // FirstPersonController torso / legLeft / legRight: shown to the owner while sliding (ActiveTorso).
        if kind == BodyKind::Local && matches!(name.as_str(), "SM_Aboubi00_Torso" | "SK_Aboubi00_Leg_Left00" | "SK_Aboubi00_Leg_Right00") {
            commands.entity(e).insert(LocalSlideBodyPart);
        }
        if name == "Graphics" && kind != BodyKind::Local {
            commands.entity(owner).insert(DummyGraphics(e));
            if kind == BodyKind::Corpse {
                // Shown by death.rs when the ragdoll takes over (avoids a standing body for the first frames).
                commands.entity(e).insert(Visibility::Hidden);
            }
        }
        if name.ends_with("_Col") {
            // Collider meshes are disabled renderers in the prefab.
            commands.entity(e).insert(Visibility::Hidden);
            if matches!(kind, BodyKind::Corpse | BodyKind::Preview) {
                // The ragdoll prefab has its own colliders (ragdoll.rs); no hit colliders on a corpse / preview.
            } else if let Some(obj) = prefab.find(&my_path) {
                let l = if kind == BodyKind::Local { layer::SELF_BODY } else { layer::BODY };
                add_unity_collider(commands, e, obj, owner, &name, l, assets);
                n_cols += 1;
            } else if sample.len() < 3 {
                sample.push(my_path.clone());
            }
        }
        if let Ok(kids) = children.get(e) {
            for k in kids.iter() {
                stack.push((k, my_path.clone()));
            }
        }
    }
    info!("{kind:?} body instance ready: {n_cols} hit colliders attached; unmatched e.g. {sample:?}");
}

/// Unity Capsule/Box/Sphere/Mesh collider on a glb node. glb nodes are in AssetRipper's
/// X-mirrored space, so prefab-local offsets are converted with x -> -x.
fn add_unity_collider(commands: &mut Commands, e: Entity, obj: &crate::weapons::data::PrefabObject, owner: Entity, name: &str, unity_layer: u32, assets: &AssetServer) {
    let glb = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
    let part = (HitPart { owner, name: name.to_string() }, UnityLayer(unity_layer));
    if let Some(c) = obj.comp("CapsuleCollider") {
        let r = c.f("m_Radius");
        let half = (c.f("m_Height") * 0.5 - r).max(0.0);
        let center = glb(c.v3("m_Center"));
        let (a, b) = match c.i("m_Direction") {
            0 => (Vec3::new(-half, 0.0, 0.0), Vec3::new(half, 0.0, 0.0)),
            2 => (Vec3::new(0.0, 0.0, -half), Vec3::new(0.0, 0.0, half)),
            _ => (Vec3::new(0.0, -half, 0.0), Vec3::new(0.0, half, 0.0)),
        };
        commands.entity(e).insert((Collider::capsule(center + a, center + b, r), part));
    } else if let Some(c) = obj.comp("BoxCollider") {
        let h = c.v3("m_Size").abs() * 0.5;
        commands.entity(e).insert((Collider::compound(vec![(glb(c.v3("m_Center")), Quat::IDENTITY, Collider::cuboid(h.x, h.y, h.z))]), part));
    } else if let Some(c) = obj.comp("SphereCollider") {
        commands.entity(e).insert((Collider::compound(vec![(glb(c.v3("m_Center")), Quat::IDENTITY, Collider::ball(c.f("m_Radius")))]), part));
    } else if let Some(c) = obj.comp("MeshCollider") {
        if let Some(mesh) = c.asset("m_Mesh") {
            // "Mesh/SM_Aboubi_Head01_Col.asset" -> exported glb of the same mesh.
            let stem = mesh.trim_start_matches("Mesh/").trim_end_matches(".asset");
            let handle: Handle<Mesh> = assets.load(format!("models/player/{stem}.glb#Mesh0/Primitive0"));
            let shape = if c.b("m_Convex") { ComputedColliderShape::ConvexHull } else { ComputedColliderShape::TriMesh(TriMeshFlags::default()) };
            commands.entity(e).insert((Mesh3d(handle), Visibility::Hidden, AsyncCollider(shape), part));
        }
    }
}

/// PlayerHealth.Update for the dummy: death hides the body (graphics.SetActive(false)) and
/// disables its colliders; respawn after 3 s is a test-map addition.
pub fn dummy_update(
    mut commands: Commands,
    time: Res<Time>,
    mut dummies: Query<(Entity, &mut PlayerHealth, Option<&DummyGraphics>, Option<&DummyMotor>, &mut Transform), (With<Dummy>, Without<Corpse>)>,
    parts: Query<(Entity, &HitPart)>,
    mut vis: Query<&mut Visibility>,
) {
    for (e, mut h, g, motor, mut tf) in &mut dummies {
        if h.health <= 0.0 && !h.dead {
            h.dead = true;
            h.respawn_in = 3.0;
            for (pe, p) in &parts {
                if p.owner == e {
                    commands.entity(pe).insert(ColliderDisabled);
                }
            }
        }
        if h.dead {
            h.respawn_in -= time.delta_secs();
            if h.respawn_in <= 0.0 {
                h.dead = false;
                h.health = h.full_health;
                if let Some(m) = motor {
                    tf.translation = m.spawn;
                }
                if let Some(g) = g {
                    if let Ok(mut v) = vis.get_mut(g.0) {
                        *v = Visibility::Inherited;
                    }
                }
                for (pe, p) in &parts {
                    if p.owner == e {
                        commands.entity(pe).remove::<ColliderDisabled>();
                    }
                }
            }
        }
    }
}

pub struct HealthPlugin;

impl Plugin for HealthPlugin {
    fn build(&self, app: &mut App) {
        let prefab = Prefab::load("assets/data/player/PlayerIK.json").expect("PlayerIK.json");
        app.insert_resource(PlayerPrefab(Arc::new(prefab))).add_systems(Update, (dummy_update, dummy_motor, player_health, local_slide_body, bone_log, stun_vfx));
    }
}

/// Movement for the dummy: what the enemy's FirstPersonController does with no input —
/// gravity (30, grounded -> -1, clamp -40) + CustomAddForce impact (mass 3, decel 5 ground /
/// 0.5 air, applied when |impact| > 0.2, impact.y zeroed when grounded). Taser stun (canMove=false)
/// only freezes input, which a dummy has none of, so stun just counts down.
#[derive(Component)]
pub struct DummyMotor {
    pub move_y: f32,
    pub impact: Vec3,
    pub grounded: bool,
    pub spawn: Vec3,
    last_motion: Vec3,
}

pub fn dummy_motor_bundle(spawn: Vec3) -> impl Bundle {
    (
        DummyMotor { move_y: -1.0, impact: Vec3::ZERO, grounded: false, spawn, last_motion: Vec3::ZERO },
        KinematicCharacterController {
            custom_shape: Some((Collider::capsule_y(0.6, 0.4), Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY)),
            offset: CharacterLength::Absolute(0.2),
            max_slope_climb_angle: 65f32.to_radians(),
            min_slope_slide_angle: 90f32.to_radians(),
            snap_to_ground: None,
            autostep: Some(CharacterAutostep { max_height: CharacterLength::Absolute(0.3), min_width: CharacterLength::Absolute(0.01), include_dynamic_bodies: false }),
            // Ignore its own Body hit colliders and the Player-layer blocker capsule.
            filter_groups: Some(CollisionGroups::new(
                Group::from_bits_retain(1 << layer::PLAYER),
                Group::from_bits_retain(!((1 << layer::PLAYER) | (1 << layer::BODY) | (1 << layer::RAGDOLL) | (1 << layer::DONT_COLLIDE_WITH_PLAYER) | (1 << layer::IGNORE_RAYCAST))),
            )),
            filter_flags: QueryFilterFlags::EXCLUDE_SENSORS,
            // Unity CharacterControllers push no rigidbodies (rapier 0.35 also panics transferring impulses to ragdolls).
            apply_impulse_to_dynamic_bodies: false,
            ..default()
        },
        Collider::ball(0.01),
        Sensor,
        CollisionGroups::new(Group::NONE, Group::NONE),
    )
}

pub fn dummy_motor(
    time: Res<Time>,
    mut q: Query<(&mut DummyMotor, &mut PlayerHealth, &mut Transform, &mut KinematicCharacterController, Option<&KinematicCharacterControllerOutput>)>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    for (mut m, mut ph, mut tf, mut kcc, out) in &mut q {
        if let Some(o) = out {
            m.grounded = o.grounded && m.last_motion.y <= 0.0;
        }
        ph.tick_stun(dt);
        // CustomAddForce.AddForce: impact += dir.normalized * force / mass(3)
        for (dir, f) in std::mem::take(&mut ph.pending_forces) {
            m.impact += dir.normalize_or_zero() * f / 3.0;
        }
        // CustomAddForce.Update
        let custom = if m.impact.length() > 0.2 { m.impact } else { Vec3::ZERO };
        if m.grounded {
            m.impact.y = 0.0;
        }
        let decel = if m.grounded { 5.0 } else { 0.5 };
        let imp = m.impact;
        m.impact = crate::unity::lerp3(imp, Vec3::ZERO, decel * dt);
        // FirstPersonController gravity (gravity 30, jumpGravity 30, cap -40)
        if m.grounded {
            m.move_y = -1.0;
        } else if m.move_y > -40.0 {
            m.move_y -= 30.0 * dt;
        }
        if ph.dead {
            m.impact = Vec3::ZERO;
        }
        let motion = (Vec3::new(0.0, m.move_y, 0.0) + custom) * dt;
        m.last_motion = motion;
        kcc.translation = Some(motion);
        // Fell out of the map -> back to spawn (FPC: y < -300 kills; respawn for the test map).
        if tf.translation.y < -300.0 {
            tf.translation = m.spawn;
        }
    }
}

/// `CameraEffect.selfBodyHitClips` == `PauseManager.deathAudioClip` (MainMenu.unity): the three selfbodyhit clips.
pub const SELF_HIT: [&str; 3] = ["AudioClip/selfbodyhit01.ogg", "AudioClip/selfbodyhit_02.ogg", "AudioClip/selfbodyhit_03.ogg"];

/// `SoundManager.PlaySoundWithPitch(clips[RoundToInt(Random.Range(0, 3))], Random.Range(0.95, 1.05))` on the 2D effects source.
pub fn play_self_body_hit(commands: &mut Commands, assets: &AssetServer, settings: &crate::settings::Settings) {
    let clip = SELF_HIT[crate::unity::random_range_i(0, 3) as usize];
    commands.spawn((
        AudioPlayer::new(assets.load::<AudioSource>(clip)),
        PlaybackSettings {
            mode: bevy::audio::PlaybackMode::Despawn,
            volume: bevy::audio::Volume::Linear(settings.effects_volume * settings.master_volume),
            speed: crate::unity::random_range_f(0.95, 1.05),
            ..default()
        },
    ));
}

/// Local player's PlayerHealth: TaserEnemy freeze, AddForce, and `RemoveHealth -> HitFeedbackObservers ->
/// CameraEffect.TakeHit` (vignette + self body hit clip) on every health decrease, the lethal one included. Death,
/// kill cam and respawn are in death.rs.
pub fn player_health(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<AssetServer>,
    settings: Res<crate::settings::Settings>,
    mut players: Query<(&mut PlayerHealth, &mut crate::player::fpc::Fpc), Without<Dummy>>,
    mut last: Local<Option<f32>>,
    mut take_hit: MessageWriter<crate::postfx::TakeHit>,
) {
    let Ok((mut ph, mut fpc)) = players.single_mut() else { return };
    // TaserEnemy on the local player: canMove = false until UnfreezePlayer.
    if ph.stunned {
        fpc.can_move = false;
    }
    if ph.tick_stun(time.delta_secs()) {
        fpc.can_move = true;
    }
    for (dir, f) in std::mem::take(&mut ph.pending_forces) {
        fpc.custom_add_force(dir, f);
    }
    if let Some(prev) = *last {
        // A fall into the void sets health directly (FirstPersonController.DespawnObject), no RemoveHealth.
        if ph.health < prev && prev > 0.0 && !ph.fell_void {
            info!("local player hit: health {prev} -> {}", ph.health);
            take_hit.write(crate::postfx::TakeHit);
            play_self_body_hit(&mut commands, &assets, &settings);
        }
    }
    *last = Some(ph.health);
}
