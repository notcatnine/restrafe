//! Map props: `PhysicsProp` (BarrelProp: grab / carry / throw, CharacterController motion, impact damage) with its
//! `PropDamage` dent states, and `Pot` (ceramic pots that break when shot). FishNet RPCs are local calls.
//!
//! * `InteractEnvironment.Awake` puts the prop on layer 19 (InteractEnvironment); PlayerPickup finds it with the
//!   environmentInteractionLayer ray (`Hands::current_env`, pickup.rs) and calls `OnInteract` on F or mouse 0
//!   (`Hands::env_interact`).
//! * `PhysicsProp.Update` (owner): grabbed -> BoxCollider is a trigger, CharacterController off, the prop sits
//!   `distanceFromPlayer` in front of the camera (or against the first Default-layer surface), mouse 0 held / F throws it
//!   (`AddForce(cam.forward, launchForce)`); released -> `CharacterController.Move((moveDirection + customForceFinal) * dt)`
//!   with its own gravity and an `impact` vector decaying at `deceleration` / `airdeceleration`.
//! * `OnControllerColliderHit` on a PlayerHealth with `|impact| > 2.5` (once per throw): bloodImpact, RemoveHealth(damage),
//!   BumpPlayer (AddForce(impact, 120) + shouldDropWeapon).
//! * `Weapon.TriggerEnvironment` (tag NoSound): `PropDamage.Damage()` swaps to the next dent state with a hit clip;
//!   `Pot.Die()` destroys the pot and spawns its vfx with `Quaternion.Euler(-90, 0, 0)`.

use crate::health::{HitPart, PlayerHealth};
use crate::input::PlayerInput;
use crate::physics::{self, UnityLayer, UnityTag, layer};
use crate::player::fpc::{Fpc, PlayerCamera};
use crate::vfx::def::SoundDef;
use crate::weapons::data::{Prefab, UField, u_pos};
use crate::weapons::gun::GunFx;
use crate::weapons::pickup::Hands;
use crate::weapons::WeaponSound;
use bevy::prelude::*;
use bevy::world_serialization::WorldAssetRoot;
use bevy_rapier3d::prelude::*;

/// Environment objects hit by weapon rays this frame (`Weapon.TriggerEnvironment`), see `HitCtx::trigger_environment`.
#[derive(Resource, Default)]
pub struct PendingEnvHits(pub Vec<Entity>);

/// Layer collision matrix row of InteractEnvironment (19): what a released prop's CharacterController collides with.
const INTERACT_ENVIRONMENT_COLLIDES: u32 = 0xf7ff_7fff;

/// `PhysicsProp` state + serialized fields.
#[derive(Component)]
pub struct PhysicsProp {
    pub popup_text: String,
    pub grabbed: bool,
    pub player: Option<Entity>,
    default_mask: u32,
    launch_force: f32,
    distance_from_player: f32,
    damage: f32,
    blood_impact: Option<String>,
    gravity: f32,
    mass: f32,
    air_deceleration: f32,
    deceleration: f32,
    /// BoxCollider size.z (`hitInfo.point - cam.forward * (col.size.z / 2)`).
    box_depth: f32,
    sound: Option<SoundDef>,
    move_direction: Vec3,
    impact: Vec3,
    custom_force_final: Vec3,
    can_hit: bool,
    throw_timer: f32,
    should_play_sfx: bool,
    should_play_sfx2: bool,
    grounded: bool,
    /// The CharacterController was off (grabbed) until this frame: its output still holds the pre-grab Move.
    stale_output: bool,
    /// dt of the Move whose result is still pending: the C# runs `impact.y = 0` (grounded) and the impact
    /// deceleration right after `Move`, with that Move's isGrounded; here the Move runs in the physics step, so these
    /// steps wait for its output.
    post_move_dt: Option<f32>,
}

/// `PropDamage`: GameObject states (by node name) shown one after another on each hit.
#[derive(Component)]
pub struct PropDamage {
    states: Vec<String>,
    index: usize,
    sound: Option<SoundDef>,
}

/// `Pot` on the pot root.
#[derive(Component)]
pub struct Pot {
    vfx: Option<String>,
}

/// An InteractEnvironment collider on another entity than its script (map doors) -> the scripted entity.
#[derive(Component, Clone, Copy)]
pub struct EnvTarget(pub Entity);

/// A prop collider created on a child node (pots' MeshCollider) -> its prop root.
#[derive(Component)]
pub struct PropPart(pub Entity);

/// Pot waiting for its glb mesh to build the convex MeshCollider.
#[derive(Component)]
struct PotColliderPending;

fn ref_name(r: &str) -> String {
    // "ref:BarrelProp/PF_Barrel_00/SM_Barrel_00.000::GameObject" -> "SM_Barrel_00.000"
    r.trim_start_matches("ref:").split("::").next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string()
}

fn play(sounds: &mut MessageWriter<WeaponSound>, s: &Option<SoundDef>, at: Vec3) {
    let Some(s) = s else { return };
    // hitClips[RoundToInt(Random.Range(0, n))] (int overload: 0..n-1)
    let k = (crate::unity::random_range_f(0.0, s.clips.len() as f32) as usize).min(s.clips.len() - 1);
    sounds.write(WeaponSound { clip: s.clips[k].clone(), volume: s.volume, at: s.spatial.then_some(at), pitch: s.pitch, linear_max: s.linear_max, source: None, rolloff: s.rolloff.clone() });
}

/// Spawns `GameObject/BarrelProp.prefab` at `pos` (prop root = CharacterController centre).
pub fn spawn_barrel(commands: &mut Commands, assets: &AssetServer, pos: Vec3) -> Option<Entity> {
    spawn_barrel_at(commands, assets, Transform::from_translation(pos))
}

/// [`spawn_barrel`] with a full root pose (a BarrelProp placed in a loaded map).
pub fn spawn_barrel_at(commands: &mut Commands, assets: &AssetServer, root_tf: Transform) -> Option<Entity> {
    const JSON: &str = "assets/data/props/BarrelProp.json";
    let prefab = Prefab::load(JSON).map_err(|e| warn!("BarrelProp: {e}")).ok()?;
    let root = prefab.objects.first()?;
    let pp = root.comp("PhysicsProp")?;
    let audio = root.comp("AudioSource").map(|a| serde_json::Value::Object(a.clone()));
    let clips = |f: &crate::weapons::data::Fields| SoundDef::parse(&serde_json::json!({ "hitClips": f.get("hitClips").cloned().unwrap_or_default() }), audio.as_ref());
    let bx = root.comp("BoxCollider");
    let size = bx.map(|b| b.v3("m_Size")).unwrap_or(Vec3::ONE);
    let center = bx.and_then(|b| b.get("m_Center")).map(u_pos).unwrap_or(Vec3::ZERO);
    let cc = root.comp("CharacterController");
    let radius = cc.map(|c| c.f("m_Radius")).unwrap_or(0.5);
    let height = cc.map(|c| c.f("m_Height")).unwrap_or(1.0).max(2.0 * radius);
    let cc_center = cc.and_then(|c| c.get("m_Center")).map(u_pos).unwrap_or(Vec3::ZERO);
    let skin = cc.map(|c| c.f("m_SkinWidth")).unwrap_or(0.05);
    let slope = cc.map(|c| c.f("m_SlopeLimit")).unwrap_or(45.0);
    let half_seg = (height * 0.5 - radius).max(0.0);
    let shape = if half_seg > 1e-4 { Collider::capsule_y(half_seg, radius) } else { Collider::ball(radius) };
    let prop = PhysicsProp {
        popup_text: pp.s("popupText"),
        grabbed: false,
        player: None,
        default_mask: pp.mask("defaultMask"),
        launch_force: pp.f("launchForce"),
        distance_from_player: pp.f("distanceFromPlayer"),
        damage: pp.f("damage"),
        blood_impact: pp.asset("bloodImpact"),
        gravity: pp.f("gravity"),
        mass: pp.f("mass").max(1e-4),
        air_deceleration: pp.f("airdeceleration"),
        deceleration: pp.f("deceleration"),
        box_depth: size.z.abs(),
        sound: clips(pp),
        move_direction: Vec3::ZERO,
        impact: Vec3::ZERO,
        custom_force_final: Vec3::ZERO,
        can_hit: false,
        throw_timer: 0.0,
        should_play_sfx: false,
        should_play_sfx2: false,
        grounded: false,
        stale_output: false,
        post_move_dt: None,
    };
    let damage = root.comp("PropDamage").map(|d| PropDamage {
        states: d.get("states").and_then(|s| s.as_array()).map(|a| a.iter().filter_map(|r| r.as_str()).map(ref_name).collect()).unwrap_or_default(),
        index: 0,
        sound: clips(d),
    });
    let scene = assets.load(GltfAssetLabel::Scene(0).from_asset("models/props/BarrelProp.glb"));
    let half = (size * 0.5).abs().max(Vec3::splat(0.01));
    let mut e = commands.spawn((
        Name::new("BarrelProp"),
        root_tf,
        Visibility::default(),
        UnityLayer(layer::INTERACT_ENVIRONMENT),
        UnityTag("NoSound".into()),
        Collider::compound(vec![(center, Quat::IDENTITY, Collider::cuboid(half.x, half.y, half.z))]),
        KinematicCharacterController {
            custom_shape: Some((shape, cc_center, Quat::IDENTITY)),
            offset: CharacterLength::Absolute(skin),
            max_slope_climb_angle: slope.to_radians(),
            min_slope_slide_angle: 90f32.to_radians(),
            snap_to_ground: None,
            autostep: None,
            filter_groups: Some(CollisionGroups::new(Group::from_bits_retain(1 << layer::INTERACT_ENVIRONMENT), Group::from_bits_retain(INTERACT_ENVIRONMENT_COLLIDES))),
            filter_flags: QueryFilterFlags::EXCLUDE_SENSORS,
            // Unity CharacterControllers push no rigidbodies (rapier 0.35 also panics transferring impulses to ragdolls).
            apply_impulse_to_dynamic_bodies: false,
            ..default()
        },
        prop,
    ));
    if let Some(d) = damage {
        e.insert(d);
    }
    e.with_children(|p| {
        // AssetRipper's glb is X-mirrored; the port mirrors Z (see weapons::spawn_world_weapon).
        p.spawn((WorldAssetRoot(scene), crate::unity_pbr::PrefabRender(JSON.into()), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI))));
    });
    Some(e.id())
}

/// Spawns `GameObject/<name>.prefab` (a ceramic pot) with its base at `pos`, rotated by `yaw` (Unity degrees).
pub fn spawn_pot(commands: &mut Commands, assets: &AssetServer, name: &str, pos: Vec3, yaw: f32) -> Option<Entity> {
    // The prefab root's own rotation (Euler -90 on X: the mesh is Z-up), which AssetRipper's glb root node does not carry.
    let json = format!("assets/data/props/{name}.json");
    let prefab = Prefab::load(&json).map_err(|e| warn!("{name}: {e}")).ok()?;
    let root_rot = prefab.objects.first()?.comp("Transform").and_then(|t| t.get("m_LocalRotation")).map(crate::weapons::data::u_quat).unwrap_or(Quat::IDENTITY);
    spawn_pot_at(commands, assets, name, Transform::from_translation(pos).with_rotation(crate::unity::unity_euler(0.0, yaw, 0.0) * root_rot))
}

/// [`spawn_pot`] with the pot root's full pose (port space, prefab root rotation included).
pub fn spawn_pot_at(commands: &mut Commands, assets: &AssetServer, name: &str, root_tf: Transform) -> Option<Entity> {
    let json = format!("assets/data/props/{name}.json");
    let prefab = Prefab::load(&json).map_err(|e| warn!("{name}: {e}")).ok()?;
    let root = prefab.objects.first()?;
    let vfx = root.comp("Pot").and_then(|p| p.asset("vfx"));
    let scene = assets.load(GltfAssetLabel::Scene(0).from_asset(format!("models/props/{name}.glb")));
    let e = commands
        .spawn((Name::new(name.to_string()), root_tf, Visibility::default(), Pot { vfx }, PotColliderPending))
        .with_children(|p| {
            p.spawn((WorldAssetRoot(scene), crate::unity_pbr::PrefabRender(json.clone()), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI))));
        })
        .id();
    Some(e)
}

/// MeshCollider (convex) on the pot's mesh node, layer 18 (DontCollideWithPlayer), tag NoSound.
fn pot_colliders(
    mut commands: Commands,
    pots: Query<Entity, (With<Pot>, With<PotColliderPending>)>,
    kids: Query<&Children>,
    meshes_q: Query<&Mesh3d>,
    meshes: Res<Assets<Mesh>>,
) {
    for pot in &pots {
        let mut done = false;
        for d in kids.iter_descendants(pot) {
            let Ok(m) = meshes_q.get(d) else { continue };
            let Some(mesh) = meshes.get(&m.0) else { continue };
            let Some(col) = Collider::from_bevy_mesh(mesh, &ComputedColliderShape::ConvexHull) else { continue };
            debug!("pot collider for {pot:?}");
            commands.entity(d).with_children(|c| {
                c.spawn((Transform::default(), col, UnityLayer(layer::DONT_COLLIDE_WITH_PLAYER), UnityTag("NoSound".into()), PropPart(pot)));
            });
            done = true;
        }
        if done {
            commands.entity(pot).remove::<PotColliderPending>();
        }
    }
}

/// `Weapon.TriggerEnvironment` hits: PropDamage.Damage / Pot.Die.
fn env_hits(
    mut commands: Commands,
    mut hits: ResMut<PendingEnvHits>,
    parts: Query<&PropPart>,
    pots: Query<(&Pot, &GlobalTransform)>,
    mut damages: Query<(&mut PropDamage, &GlobalTransform)>,
    kids: Query<&Children>,
    names: Query<&Name>,
    mut vis: Query<&mut Visibility>,
    mut sounds: MessageWriter<WeaponSound>,
    mut fx: MessageWriter<GunFx>,
    pigeons: Query<(&crate::scene_props::Pigeon, &GlobalTransform)>,
) {
    let mut broken: Vec<Entity> = Vec::new();
    for e in std::mem::take(&mut hits.0) {
        let root = parts.get(e).map(|p| p.0).unwrap_or(e);
        if let Ok((pot, g)) = pots.get(root) {
            // Pot.Die(): Destroy(gameObject); Instantiate(vfx, position, Euler(-90, 0, 0)) (forward = up).
            if !broken.contains(&root) {
                broken.push(root);
                fx.write(GunFx::Spawn { prefab: pot.vfx.clone(), at: g.translation(), normal: Vec3::Y });
                commands.entity(root).try_despawn();
            }
            continue;
        }
        // Pigeon.Die(): Instantiate(dieVfx, position, identity); Destroy(gameObject).
        if let Ok((pg, g)) = pigeons.get(root) {
            if !broken.contains(&root) {
                broken.push(root);
                fx.write(GunFx::Spawn { prefab: pg.die_vfx.clone(), at: g.translation(), normal: Vec3::NEG_Z });
                commands.entity(root).try_despawn();
            }
            continue;
        }
        if let Ok((mut d, g)) = damages.get_mut(root) {
            // PropDamage.Damage(): index++; if (index < states.Length) { hit clip; states[index-1] off; states[index] on }
            d.index += 1;
            if d.index < d.states.len() {
                play(&mut sounds, &d.sound, g.translation());
                let (prev, next) = (d.states[d.index - 1].clone(), d.states[d.index].clone());
                for n in kids.iter_descendants(root) {
                    let Ok(name) = names.get(n) else { continue };
                    if let Ok(mut v) = vis.get_mut(n) {
                        if name.as_str() == prev {
                            *v = Visibility::Hidden;
                        } else if name.as_str() == next {
                            *v = Visibility::Inherited;
                        }
                    }
                }
            }
        }
    }
}

/// `PhysicsProp.OnInteract(player)` (owner side; CmdInteract toggles grabbed / player locally).
fn on_interact(e: Entity, p: &mut PhysicsProp, me: Entity, hands: &mut Hands, cam_fwd: Vec3) {
    if p.grabbed && p.player != Some(me) {
        return;
    }
    match hands.held_env {
        None => hands.held_env = Some(e),
        Some(h) if h != e => return,
        _ => {}
    }
    p.should_play_sfx2 = true;
    if p.grabbed && p.player == Some(me) {
        // AddForce(cam.forward, launchForce): impact += dir.normalized * force / mass
        p.impact += cam_fwd.normalize_or_zero() * p.launch_force / p.mass;
    }
    p.throw_timer = 0.3;
    // CmdInteract: SetGrabbed(!grabbed); SetPlayer(grabbed ? player : null)
    p.grabbed = !p.grabbed;
    p.player = if p.grabbed { Some(me) } else { None };
}

#[allow(clippy::too_many_arguments)]
fn physics_prop_update(
    mut commands: Commands,
    time: Res<Time>,
    input: Res<PlayerInput>,
    rapier: ReadRapierContext,
    mut players: Query<(Entity, &mut Hands, &mut Fpc)>,
    cams: Query<&GlobalTransform, With<PlayerCamera>>,
    mut props: Query<(Entity, &mut PhysicsProp, &mut Transform, &mut KinematicCharacterController, Option<&KinematicCharacterControllerOutput>, Has<Sensor>)>,
    parts: Query<&HitPart>,
    mut healths: Query<&mut PlayerHealth>,
    mut sounds: MessageWriter<WeaponSound>,
    mut fx: MessageWriter<GunFx>,
    env_targets: Query<&EnvTarget>,
    (maps, mut ready_frames): (Query<Has<crate::scene_map::MapSetupDone>, With<crate::scene_map::MapGlb>>, Local<u32>),
) {
    // The map's BarrelProps spawn with its colliders: a long first frame would drop them below the not yet queryable
    // floor (they then fall forever). They wait until the map colliders are in the physics world.
    if !crate::scene_map::map_physics_ready(&maps, &mut ready_frames) {
        return;
    }
    let dt = time.delta_secs();
    let Ok(ctx) = rapier.single() else { return };
    let Ok((me, mut hands, mut fpc)) = players.single_mut() else { return };
    let Ok(cam) = cams.single() else { return };
    let (cam_pos, cam_fwd) = (cam.translation(), cam.forward().as_vec3());
    let interact = hands.env_interact.map(|e| env_targets.get(e).map(|t| t.0).unwrap_or(e));
    for (e, mut p, mut tf, mut kcc, out, sensor) in &mut props {
        let p = &mut *p;
        // PlayerPickup.HandleInteraction / HandleInteractEnvironment (input callbacks run before Update, so a grab sets
        // throwTimer = 0.3 before this frame's canThrow).
        if interact == Some(e) && input.enabled {
            on_interact(e, p, me, &mut hands, cam_fwd);
        }
        p.throw_timer -= dt;
        let can_throw = p.throw_timer < 0.0;
        // col.isTrigger = grabbed; physics.enabled = !grabbed
        if p.grabbed != sensor {
            if p.grabbed {
                commands.entity(e).insert(Sensor);
            } else {
                commands.entity(e).remove::<Sensor>();
            }
        }
        // `if (grabbed)` is evaluated once: a grab released below (owner killed) still runs this frame's grabbed branch.
        let grabbed = p.grabbed;
        if p.grabbed {
            if let Some(owner) = p.player {
                if healths.get(owner).is_ok_and(|h| h.dead || h.health <= 0.0) {
                    p.grabbed = false;
                    p.player = None;
                }
            } else {
                p.grabbed = false;
            }
        }
        if grabbed {
            match physics::raycast(&ctx, cam_pos, cam_fwd, p.distance_from_player, p.default_mask) {
                Some(h) => {
                    if p.should_play_sfx {
                        p.should_play_sfx = false;
                        play(&mut sounds, &p.sound, tf.translation);
                    }
                    tf.translation = h.point - cam_fwd * (p.box_depth / 2.0);
                }
                None => {
                    tf.translation = cam_pos + cam_fwd * p.distance_from_player;
                    p.should_play_sfx = true;
                }
            }
            p.move_direction = Vec3::ZERO;
            p.custom_force_final = Vec3::ZERO;
            p.can_hit = true;
            p.stale_output = true;
            p.post_move_dt = None;
            kcc.translation = None;
            if can_throw && input.enabled && (input.fire_kbm.held || input.interact.pressed) && p.player == Some(me) {
                on_interact(e, p, me, &mut hands, cam_fwd);
                p.throw_timer = 0.3;
            }
            continue;
        }
        // CharacterController.isGrounded + OnControllerColliderHit from the last Move.
        if let Some(o) = out.filter(|_| !std::mem::take(&mut p.stale_output)) {
            p.grounded = o.grounded;
            // Tail of the previous Update, after its Move: impact.y = 0 when grounded, then the impact lerp, both with
            // that Move's isGrounded.
            if let Some(pdt) = p.post_move_dt.take() {
                post_move(p, pdt);
            }
            // OnControllerColliderHit also sets customForceFinal = 0, but inside Move, before the C# reassigns
            // customForceFinal = impact: it never reaches the next Move, so it is not reproduced here.
            for c in &o.collisions {
                let owner = parts.get(c.entity).ok().map(|h| h.owner).filter(|o| healths.contains(*o));
                if let (Some(owner), true) = (owner, p.can_hit && p.impact.length() > 2.5) {
                    p.can_hit = false;
                    // Instantiate(bloodImpact, collision.point, LookRotation(collision.normal)): rapier's witness1 /
                    // normal1 are the world-space point and outward normal on the hit collider.
                    let dir = p.impact.normalize_or_zero();
                    let (point, normal) = c.hit.details.map(|d| (d.witness1, d.normal1)).unwrap_or((c.character_translation + dir * 0.5, -dir));
                    fx.write(GunFx::Spawn { prefab: p.blood_impact.clone(), at: point, normal });
                    if let Ok(mut ph) = healths.get_mut(owner) {
                        if ph.health - p.damage <= 0.0 {
                            fpc.kill_shock_wave(); // propKills++, KillShockWave
                        }
                        ph.health -= p.damage; // RemoveHealth(damage)
                        // BumpPlayerServer(impact, 120, ph) -> bounceDirection/Force + shouldDropWeapon.
                        ph.add_force(p.impact, 120.0);
                    }
                    if owner == me {
                        hands.drop_all = true;
                    }
                }
                if p.should_play_sfx2 {
                    p.should_play_sfx2 = false;
                    play(&mut sounds, &p.sound, tf.translation);
                }
            }
        }
        if !p.grounded {
            p.move_direction.y -= p.gravity * dt;
        }
        if std::env::var_os("RESTRAFE_PROP_TRACE").is_some() {
            println!("PROPTRACE dt={dt:.5} pos=({:.4},{:.4},{:.4}) grounded={} impact=({:.3},{:.3},{:.3}) md_y={:.3}", tf.translation.x, tf.translation.y, tf.translation.z, p.grounded, p.impact.x, p.impact.y, p.impact.z, p.move_direction.y);
        }
        if std::env::var_os("RESTRAFE_DEBUG_PROPS").is_some() && !p.grounded {
            println!("PROPDBG {e:?} t={:.2} dt={dt:.3} y={:.2} grounded={} out={} vy={:.2}", time.elapsed_secs(), tf.translation.y, p.grounded, out.is_some(), p.move_direction.y);
        }
        // A grounded prop with no force left would only be pressed into the ground again (CharacterController.Move with
        // moveDirection.y): skip the (costly) character-controller query until something moves it.
        let resting = p.grounded && p.impact.length_squared() < 1e-4 && p.custom_force_final == Vec3::ZERO && p.should_play_sfx2 == false;
        kcc.translation = if resting { None } else { Some((p.move_direction + p.custom_force_final) * dt) };
        if resting {
            p.stale_output = true; // no new Move: keep grounded, don't replay the old collisions
        }
        p.custom_force_final = if p.impact.length() > 0.2 { p.impact } else { Vec3::ZERO };
        if resting {
            post_move(p, dt);
        } else {
            p.post_move_dt = Some(dt);
        }
        if hands.held_env == Some(e) {
            hands.held_env = None;
        }
    }
}

/// `if (physics.isGrounded) impact.y = 0; impact = Lerp(impact, 0, (grounded ? deceleration : airdeceleration) * dt)`.
fn post_move(p: &mut PhysicsProp, dt: f32) {
    if p.grounded {
        p.impact.y = 0.0;
    }
    let k = (if p.grounded { p.deceleration } else { p.air_deceleration }) * dt;
    p.impact = p.impact.lerp(Vec3::ZERO, k.clamp(0.0, 1.0));
}

pub struct PropsPlugin;

impl Plugin for PropsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PendingEnvHits>()
            .add_systems(Update, (pot_colliders, physics_prop_update.after(crate::weapons::pickup::player_pickup_update), env_hits));
    }
}
