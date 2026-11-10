//! Port of `DualLauncher.cs` + the projectiles it spawns:
//!  - `PredictedProjectile` (manually moved bullets/rockets/Gamma/Prophet/Mortini/...)  -> [`Shot`]
//!  - `PhysicsGrenade` (hand/stun/gland grenade), `Obus`, `Bubble` which follow the Heathen PhysKit
//!    `TrickShot` predicted ballistic path (`BallisticPathFollow`) and then become a dynamic Rigidbody -> [`Thrown`]
//!  - `ShrapnelBallistic` (HK_Caws, explodes 0.1 s after being spawned) -> [`Thrown`] with kind Shrapnel
//!
//! Facts established from the decompiled source (the lead's assumptions differ):
//!  * No projectile/grenade/rocket pushes players on explosion (no PlayerHealth.AddForce anywhere in these
//!    scripts). "Rocket jumping" is only the weapon's `playerKnockback` applied to the shooter when firing.
//!  * Explosion damage is flat (no falloff): `RemoveHealth(damage)` (Predicted) / `RemoveHealth(10)` (grenade, obus)
//!    for every PlayerHealth with a Body-layer collider inside `OverlapSphere(explosionRadius, bodyLayer)`.
//!  * Heathen `BallisticsData.Predict` in this build always returns `impact = null`, so TrickShot's
//!    `bounces` never matter: the object follows ONE predicted parabola (until the first hit / distance limit),
//!    then becomes a normal dynamic Rigidbody with the last step's velocity.

use super::data::{Fields, Prefab, UField, WeaponDef};
use super::gun::{weapon_animation, GunFx, HitCtx};
use super::{CamQuery, Item, WeaponSound, WeaponUi};
use crate::input::PlayerInput;
use crate::physics::{self, UnityLayer, UnityTag};
use crate::player::fpc::Fpc;
use crate::tween::Tweens;
use crate::unity::*;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

/// Behaviour class check for the integrator (`def.class`).
pub fn is_launcher(class: &str) -> bool {
    class == "DualLauncher"
}

const GRAVITY: Vec3 = Vec3::new(0.0, -9.81, 0.0); // DynamicsManager m_Gravity
const FIXED_DT: f32 = 1.0 / 30.0; // TimeManager Fixed Timestep 0.0333
/// Layer collision matrix row 20 (Grenade) from DynamicsManager.asset: layers 0,1,2,4,5,6,7,10,11,14,18,19,22,24,28.
const GRENADE_FILTER: u32 = 0x114c_4cf7;
const GRENADE_LAYER: u32 = 20;

// =============================================================================================
// Resources
// =============================================================================================

/// Projectile prefab JSON (`assets/data/projectiles/*.json`) + placeholder meshes/materials.
#[derive(Resource)]
pub struct LauncherAssets {
    pub prefabs: HashMap<String, Arc<Prefab>>,
    sphere: Handle<Mesh>,
    bullet: Handle<StandardMaterial>,
}

impl FromWorld for LauncherAssets {
    fn from_world(world: &mut World) -> Self {
        let sphere = world.resource_mut::<Assets<Mesh>>().add(Sphere::new(1.0));
        let mut mats = world.resource_mut::<Assets<StandardMaterial>>();
        let bullet = mats.add(StandardMaterial { base_color: Color::srgb(1.0, 0.6, 0.2), emissive: LinearRgba::rgb(6.0, 3.0, 0.5), unlit: true, ..default() });
        let mut prefabs = HashMap::new();
        if let Ok(rd) = std::fs::read_dir("assets/data/projectiles") {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                match Prefab::load(p.to_str().unwrap()) {
                    Ok(pf) => {
                        prefabs.insert(p.file_stem().unwrap().to_string_lossy().to_string(), Arc::new(pf));
                    }
                    Err(err) => error!("projectile {p:?}: {err}"),
                }
            }
        }
        LauncherAssets { prefabs, sphere, bullet }
    }
}

impl LauncherAssets {
    /// "GameObject/HandGrenade 2.prefab" -> prefab "HandGrenade 2".
    fn get(&self, asset: &str) -> Option<&Arc<Prefab>> {
        let stem = asset.rsplit('/').next()?.strip_suffix(".prefab")?;
        if let Some(p) = self.prefabs.get(stem) {
            return Some(p);
        }
        // not exported: loaded on first use (built in place by Prefab::load) and kept for the session
        static LAZY: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Option<&'static Arc<Prefab>>>>> = std::sync::OnceLock::new();
        let mut lazy = LAZY.get_or_init(Default::default).lock().unwrap();
        *lazy.entry(stem.to_string()).or_insert_with(|| Prefab::load(&format!("assets/data/projectiles/{stem}.json")).ok().map(|p| &*Box::leak(Box::new(Arc::new(p)))))
    }
}

// =============================================================================================
// Components
// =============================================================================================

#[derive(Component, Default)]
pub struct LauncherState {
    fire_timer: f32,
    /// `Start()`: grenadeTimer = timeBeforeExplosion (initialised lazily on first update).
    inited: bool,
    grenade_timer: f32,
    /// `grenadeOpen` SyncVar (pin pulled).
    pub grenade_open: bool,
    kanye_shoot: bool,
}

impl LauncherState {
    pub fn new(def: &WeaponDef) -> Self {
        LauncherState { grenade_timer: def.w.f("timeBeforeExplosion"), inited: true, ..default() }
    }
}

/// `PredictedProjectile` instance.
#[derive(Component)]
pub struct Shot {
    p: Fields,
    /// The prefab AudioSource loops and plays on awake (ProphetProj): `audio.Play()` restarts that source on the
    /// projectile instead of a one-shot at the hit point.
    looping: bool,
    dir: Vec3,
    force: f32,
    passed_time: f32,
    actual_passed: f32,
    gravity_acc: f32,
    last_pos: Vec3,
    fwd: Vec3,
    distance: f32,
    damage: f32,
    headshot: bool,
    touched: bool,
    touched2: bool,
    done: bool,
    /// Prefab AudioSource clip + linear rolloff max (`audio.Play()` on hit; PredictedProjectile.hitClip is unused).
    audio_clip: Option<String>,
    audio_max: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThrownKind {
    Grenade,
    Obus,
    Bubble,
    Shrapnel,
}

#[derive(Clone, Copy)]
pub struct Step {
    pos: Vec3,
    vel: Vec3,
    t: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `BallisticPathFollow` kinematic playback.
    Path,
    /// Rigidbody simulated by Rapier.
    Dynamic,
    /// Exploded / popped (waiting for `Destroy(gameObject, 3)`).
    Done,
}

/// `PhysicsGrenade` / `Obus` / `Bubble` / `ShrapnelBallistic` instance.
#[derive(Component)]
pub struct Thrown {
    kind: ThrownKind,
    f: Fields,
    /// The prefab AudioSource clip: PhysicsGrenade / Obus explode with `audio.Play()` (their `explosionClip` field is unused).
    audio_clip: Option<String>,
    /// Prefab AudioSource rolloff: Linear max distance (0 = Logarithmic).
    audio_max: f32,
    steps: Vec<Step>,
    time: f32,
    phase: Phase,
    follow_mask: u32,
    end_on_collision: bool,
    radius: f32,
    explosion_timer: f32,
    exploded: bool,
    hit: bool,
    hits: i32,
    touched: bool,
    touched2: bool,
    headshot: bool,
    last_pos: Vec3,
    /// PhysicsGrenade `rotateAxis` (Unity local, NOT normalized: Rotate(eulers) spins |axis| * speed deg/s).
    spin_axis: Vec3,
    spin_speed: f32,
    /// Accumulated `graph.Rotate(...)` (graph local rotation; Graph sits at the root origin with identity rotation).
    spin: Quat,
    /// Non-Linear prefab AudioSource rolloff (Shrapnel: Logarithmic min 1 / max 50), else None.
    audio_rolloff: Option<Arc<crate::audio::RolloffCurve>>,
}

#[derive(Component)]
pub struct DespawnIn(pub f32);

/// Request on a held grenade: detach its `douilleGrenade` node (by name) and let it fall.
#[derive(Component)]
pub struct PinRelease(pub String);

/// The detached pin: `AddComponent<Rigidbody>` (mass 1, no collider -> no collisions, gravity -9.81),
/// `AddForce(right * 0.4 + up * 2, Impulse)`, `AddTorque(forward * 4 + right * 4, Impulse)` (unit inertia assumed).
#[derive(Component)]
pub struct PinFall {
    vel: Vec3,
    ang: Vec3,
    age: f32,
}

pub fn pin_release(
    mut commands: Commands,
    q: Query<(Entity, &PinRelease, &GlobalTransform)>,
    kids: Query<&Children>,
    names: Query<&Name>,
    gtfs: Query<&GlobalTransform>,
) {
    for (e, pr, g) in &q {
        commands.entity(e).remove::<PinRelease>();
        let Some(node) = kids.iter_descendants(e).find(|d| names.get(*d).is_ok_and(|n| n.as_str() == pr.0)) else {
            warn!("grenade pin node {} not found", pr.0);
            continue;
        };
        let world = gtfs.get(node).map(|g| g.compute_transform()).unwrap_or_default();
        // Unity transform.right / forward of the weapon root (port space: right = +X, Unity forward = -Z here).
        let (right, up, fwd) = (g.right().as_vec3(), Vec3::Y, g.forward().as_vec3());
        commands.entity(node).remove::<ChildOf>().insert((world, PinFall { vel: right * 0.4 + up * 2.0, ang: fwd * 4.0 + right * 4.0, age: 0.0 }));
    }
}

pub fn pin_fall(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut Transform, &mut PinFall)>) {
    let dt = time.delta_secs();
    for (e, mut tf, mut p) in &mut q {
        p.age += dt;
        p.vel.y -= 9.81 * dt;
        tf.translation += p.vel * dt;
        let a = p.ang * dt;
        if a.length_squared() > 0.0 {
            tf.rotation = Quat::from_scaled_axis(a) * tf.rotation;
        }
        // No collider: it falls forever in Unity; dropped once far out of view.
        if p.age > 10.0 {
            commands.entity(e).try_despawn();
        }
    }
}

// =============================================================================================
// Shared helpers
// =============================================================================================

#[derive(SystemParam)]
pub struct LauncherOut<'w, 's> {
    sounds: MessageWriter<'w, WeaponSound>,
    fx: MessageWriter<'w, GunFx>,
    hit: HitCtx<'w, 's>,
    layers: Query<'w, 's, &'static UnityLayer>,
    tags: Query<'w, 's, &'static UnityTag>,
    xf: Query<'w, 's, &'static GlobalTransform>,
}

impl LauncherOut<'_, '_> {
    /// One-shot on a projectile's own 3D AudioSource (rolloff from its prefab, see `audio_src`).
    fn snd_at(&mut self, clip: Option<String>, at: Vec3, linear_max: f32, volume: f32) {
        if let Some(clip) = clip {
            self.sounds.write(WeaponSound { clip, volume, at: Some(at), pitch: 1.0, linear_max, source: None, rolloff: None });
        }
    }
    /// Same on a non-Linear source (Shrapnel: Logarithmic 1..50, constant past maxDistance).
    fn snd_at_r(&mut self, clip: Option<String>, at: Vec3, rolloff: Option<Arc<crate::audio::RolloffCurve>>) {
        if let Some(clip) = clip {
            self.sounds.write(WeaponSound { clip, volume: 1.0, at: Some(at), pitch: 1.0, linear_max: 0.0, source: None, rolloff });
        }
    }
    /// One-shot on the held DualLauncher's `audio` (2D for the owner, `audio.pitch` from changePitchOnShoot).
    fn snd_weapon(&mut self, clip: Option<String>, pitch: f32) {
        if let Some(clip) = clip {
            self.sounds.write(WeaponSound { clip, volume: 1.0, at: None, pitch, linear_max: 0.0, source: None, rolloff: None });
        }
    }
    fn is_glass(&self, e: Entity) -> bool {
        self.tags.get(e).map(|t| t.0 == "ShatterableGlass").unwrap_or(false)
    }
    /// `collider.GetComponentInParent<PlayerHealth>()` (Body hit colliders carry a HitPart).
    fn owner_of(&self, e: Entity) -> Option<Entity> {
        let part = self.hit.parts.get(e).ok()?;
        self.hit.healths.contains(part.owner).then_some(part.owner)
    }
    /// Distinct PlayerHealth owners of a collider list (insertion order).
    fn owners(&self, ents: &[Entity]) -> Vec<Entity> {
        let mut out: Vec<Entity> = Vec::new();
        for e in ents {
            if let Some(o) = self.owner_of(*e) {
                if !out.contains(&o) {
                    out.push(o);
                }
            }
        }
        out
    }
    /// `Instantiate(bloodSplatter, ph.transform.position, Euler(0, Random.Range(0, 360), 0))` (int Random.Range).
    fn blood_splatter(&mut self, p: &Fields, ph: Entity) {
        if let (Some(prefab), Ok(g)) = (p.asset("bloodSplatter"), self.xf.get(ph)) {
            let yaw = crate::unity::unity_euler(0.0, crate::unity::random_range_i(0, 360) as f32, 0.0);
            self.fx.write(GunFx::Spawn { prefab: Some(prefab), at: g.translation(), normal: yaw * Vec3::NEG_Z });
        }
    }
    /// Grenade / shrapnel `makeBlood`: bloodSplatter at the first PlayerHealth collider's position.
    fn first_blood(&mut self, p: &Fields, ents: &[Entity]) {
        let Some(prefab) = p.asset("bloodSplatter") else { return };
        if let Some(at) = ents.iter().find(|e| self.owner_of(**e).is_some()).and_then(|e| self.xf.get(*e).ok()).map(|g| g.translation()) {
            // Quaternion.Euler(0, Random.Range(0, 360), 0) (PhysicsGrenade.cs:452, ShrapnelBallistic.cs:226).
            let yaw = crate::unity::unity_euler(0.0, crate::unity::random_range_i(0, 360) as f32, 0.0);
            self.fx.write(GunFx::Spawn { prefab: Some(prefab), at, normal: yaw * Vec3::NEG_Z });
        }
    }
    fn is_killed(&self, owner: Entity) -> bool {
        self.hit.healths.get(owner).map(|h| h.dead || h.health <= 0.0).unwrap_or(true)
    }
    /// `ph.Explode(false, true, name, ph.position - projectile.position, ragdollEjectForce, projectile.position)`:
    /// ragdoll impulse for a victim that just died.
    fn ragdoll_hit(&mut self, ph: Entity, from: Vec3, force: f32) {
        let vp = self.xf.get(ph).map(|g| g.translation()).unwrap_or(from);
        if let Ok(mut h) = self.hit.healths.get_mut(ph) {
            if h.health <= 0.0 {
                h.last_hit = Some(((vp - from).normalize_or_zero(), force, from));
            }
        }
    }
    /// ChangeKilledState + RemoveHealth(10) (PhysicsGrenade.cs:478, Obus.cs:404/412): health - 10 (4 -> -6).
    fn kill(&mut self, owner: Entity) {
        if let Ok(mut h) = self.hit.healths.get_mut(owner) {
            h.killed = true;
            h.health -= 10.0;
        }
    }
    /// `ph.suicide = true` (+ Settings.IncreaseSuicidesAmount, a stat counter not ported).
    fn suicide(&mut self, owner: Entity) {
        if let Ok(mut h) = self.hit.healths.get_mut(owner) {
            h.suicide = true;
        }
    }
    /// `PlayerHealth.TaserEnemy(ph, stunTime)`.
    fn stun(&mut self, owner: Entity, seconds: f32) {
        if let Ok(mut h) = self.hit.healths.get_mut(owner) {
            h.stun(seconds);
        }
    }
    /// `if (health - dmg <= 0) ChangeKilledState(true)`, then always `RemoveHealth(dmg)` (health really becomes
    /// health - dmg, PredictedProjectile.cs:396-407 / Bubble.cs:138-146 / ShrapnelBallistic.cs:239-261); returns true
    /// when it killed. `shock`: the script calls KillShockWave on that kill (not for the shooter's own death, except Bubble).
    fn damage(&mut self, owner: Entity, dmg: f32, fpc: &mut Fpc, shock: bool) -> bool {
        let Ok(mut h) = self.hit.healths.get_mut(owner) else { return false };
        let lethal = h.health - dmg <= 0.0;
        h.killed |= lethal;
        h.health -= dmg;
        if lethal && shock {
            fpc.kill_shock_wave();
        }
        lethal
    }
    /// PredictedProjectile.cs:678 / ShrapnelBallistic.cs:315 HitMarker: `audio.PlayOneShot(hitSfx)` on the projectile's
    /// own 3D AudioSource (at the projectile, its prefab rolloff), not on the weapon.
    fn hit_marker(&mut self, source: Entity, head: bool, hit_sfx: Option<String>, at: Vec3, linear_max: f32) {
        self.snd_at(hit_sfx, at, linear_max, 1.0);
        // Crosshair.headshotHitClip: null in the scene (fileID 0) -> PlayOneShot plays nothing.
        self.hit.ui.write(WeaponUi::HitMarker { head, source: Some(source) });
    }
    /// ShrapnelBallistic HitMarker on its Logarithmic source.
    fn hit_marker_r(&mut self, source: Entity, head: bool, hit_sfx: Option<String>, at: Vec3, rolloff: Option<Arc<crate::audio::RolloffCurve>>) {
        self.snd_at_r(hit_sfx, at, rolloff);
        self.hit.ui.write(WeaponUi::HitMarker { head, source: Some(source) });
    }
}

/// `_rootObject` of every projectile: the local player (the only shooter in single player) and its
/// CharacterController capsule (layer Self(6) for the owner; no Rapier collider exists for it).
#[derive(Clone, Copy)]
struct Shooter {
    e: Entity,
    cap: (Vec3, Vec3, f32),
}

impl Shooter {
    fn new(e: Entity, fpc: &Fpc, tf: &Transform) -> Self {
        Shooter { e, cap: super::mines::player_capsule(fpc, tf) }
    }
    /// The owner's CC capsule overlaps a sphere (OverlapSphere with a mask containing Self).
    fn overlaps_sphere(&self, center: Vec3, radius: f32) -> bool {
        super::mines::closest_on_seg(self.cap.0, self.cap.1, center).distance(center) <= radius + self.cap.2
    }
}

/// `PlayerHealth.LocalScreenshake` on the local player's camera (for all projectile fields sharing
/// duration/minStrength/maxStrength/vibrato/randomness/shakeEase/maxDistance).
fn screenshake(cam_tw: &mut Tweens, player_pos: Vec3, at: Vec3, f: &Fields) {
    let duration = f.f("duration");
    if duration <= 0.0 {
        return;
    }
    let max_d = f.f("maxDistance");
    let t = if max_d > 0.0 { (player_pos.distance(at) / max_d).clamp(0.0, 1.0) } else { 1.0 };
    let strength = lerp(f.f("maxStrength"), f.f("minStrength"), t);
    cam_tw.kill(); // transform.DOKill()
    // transform.DOShakeRotation(duration, float strength, vibrato, randomness).SetEase(shakeEase) (ease ignored by DOTween)
    cam_tw.shake_rotation_scalar(duration, strength, f.i("vibrato"), f.f("randomness"));
}

/// Component of the first object holding one of `types`.
fn find_comp(pf: &Prefab, ty: &str) -> Option<Fields> {
    pf.first_with(ty).map(|(_, f)| f.clone())
}

/// The prefab's first AudioSource (`GetComponent<AudioSource>()`): clip and rolloff for `WeaponSound.linear_max`
/// (rolloffMode 1 Linear -> MaxDistance, 0 Logarithmic -> 0). Volume/pitch are 1 on every projectile prefab.
fn audio_src(pf: &Prefab) -> (Option<String>, f32) {
    match find_comp(pf, "AudioSource") {
        Some(a) => (a.asset("m_audioClip"), if a.i("rolloffMode") == 0 { 0.0 } else { a.f("MaxDistance") }),
        None => (None, 100.0),
    }
}

/// Non-Linear rolloff of the prefab's AudioSource (Logarithmic / Custom), built once per projectile.
fn audio_rolloff(pf: &Prefab) -> Option<Arc<crate::audio::RolloffCurve>> {
    let a = find_comp(pf, "AudioSource")?;
    crate::audio::RolloffCurve::from_source(&a, a.f("MaxDistance")).map(Arc::new)
}

fn trick_shot_fields(def: &WeaponDef) -> Option<Fields> {
    // Heathen TrickShot MonoBehaviour (extractor labels it by GUID): has `template` + `speed`.
    def.prefab
        .objects
        .iter()
        .flat_map(|o| o.components.iter())
        .find(|(_, f)| f.contains_key("template") && f.contains_key("speed"))
        .map(|(_, f)| f.clone())
}

/// `Ballistics.SphereCast(start, null, velocity, gravity, radius, resolution, maxLength, mask, ...)` flattened
/// into BallisticPath steps (position, velocity, time); the last step is the impact point if it hit.
fn predict_path(ctx: &RapierContext, start: Vec3, velocity: Vec3, radius: f32, resolution: f32, max_len: f32, mask: u32) -> Vec<Step> {
    if velocity == Vec3::ZERO {
        return Vec::new();
    }
    let mut steps = vec![Step { pos: start, vel: velocity, t: 0.0 }];
    let (mut pos, mut v, mut t) = (start, velocity, 0.0f32);
    let mut hit = None;
    for _ in 0..5000 {
        hit = physics::sphere_cast(ctx, pos, radius, v.normalize_or_zero(), resolution, mask);
        if hit.is_some() || start.distance(pos) >= max_len {
            break;
        }
        let dt = resolution / v.length();
        t += dt;
        v += dt * GRAVITY;
        pos += dt * v;
        steps.push(Step { pos, vel: v, t });
    }
    if let Some(h) = hit {
        let point = pos + v.normalize_or_zero() * h.distance;
        steps.push(Step { pos: point, vel: v, t: t + (point - pos).length() / v.length() });
    }
    steps
}

/// `BallisticPath.Lerp(time)`.
fn path_lerp(steps: &[Step], time: f32) -> Step {
    let last = *steps.last().unwrap();
    if steps.len() == 1 || time <= 0.0 {
        return steps[0];
    }
    if time >= last.t {
        return last;
    }
    for i in 1..steps.len() {
        if steps[i].t > time {
            let (a, b) = (steps[i - 1], steps[i]);
            let f = (time - a.t) / (b.t - a.t);
            return Step { pos: a.pos.lerp(b.pos, f), vel: a.vel.lerp(b.vel, f), t: time };
        }
    }
    last
}

// =============================================================================================
// DualLauncher.Update / Fire
// =============================================================================================

#[allow(clippy::too_many_arguments)]
pub fn launcher_update(
    mut commands: Commands,
    time: Res<Time>,
    input: Res<PlayerInput>,
    anchors: Res<super::data::HandAnchors>,
    rapier: ReadRapierContext,
    assets: Res<LauncherAssets>,
    mut players: Query<(&mut Fpc, &Transform, Option<&super::pickup::Hands>), Without<Item>>,
    mut cams: CamQuery,
    mut launchers: Query<(Entity, &mut Item, &mut LauncherState, &mut Transform, Option<&mut Tweens>), Without<crate::player::fpc::PlayerCamera>>,
    mut o: LauncherOut,
) {
    let dt = time.delta_secs();
    let Ok(ctx) = rapier.single() else { return };
    let Ok((mut fpc, root_tf, hands)) = players.single_mut() else { return };
    let env_focused = hands.is_some_and(|h| h.current_env.is_some());
    let Ok((cam_tf, cam_g, mut cam_tw)) = cams.single_mut() else { return };
    let (cam_tf, cam_g, root_tf) = (*cam_tf, *cam_g, *root_tf);
    let fire1 = input.fire.value();
    let fire2 = input.fire2.value();
    for (gun_e, mut item, mut st, mut tf, mut tweens) in &mut launchers {
        let item = &mut *item;
        let def = item.def.clone();
        let w = &def.w;
        if !st.inited {
            st.grenade_timer = w.f("timeBeforeExplosion");
            st.inited = true;
        }
        let grenade = w.b("grenade");
        let tbe = w.f("timeBeforeExplosion");
        // WeaponUpdate() is handled by pickup.rs (item_behaviour_update).
        if st.fire_timer > 0.0 {
            st.fire_timer -= dt;
        }
        let mut fire_requests = 0;
        if st.grenade_open && st.grenade_timer == tbe && grenade {
            // Instantiate(grenadeSmoke, transform.position + (-0.007, -0.121, -0.031), identity, shootPoint),
            // localScale 0.1, forward = transform.up; douilleGrenade detached with a Rigidbody (see `pin_fall`).
            o.fx.write(GunFx::Attached { prefab: w.asset("grenadeSmoke"), item: gun_e, offset: crate::vfx::def::u2p(Vec3::new(-0.007, -0.121, -0.031)), scale: 0.1 });
            if let Some(node) = Some(w.s("douilleGrenade")).as_deref().and_then(|r| r.strip_prefix("ref:")).and_then(|r| r.split("::").next()).map(|p| p.rsplit('/').next().unwrap_or(p).to_string()) {
                commands.entity(gun_e).try_insert(PinRelease(node));
            }
            o.snd_weapon(w.asset("douilleClip"), item.audio_pitch); // GrenadeSFX(0), DualLauncher.cs:854
            o.snd_weapon(w.asset("beforeDetonationClip"), item.audio_pitch); // GrenadeSFX(1), DualLauncher.cs:858
        }
        if st.grenade_open {
            st.grenade_timer -= dt;
        }
        if st.grenade_timer <= 0.06 && grenade && st.grenade_open {
            fire_requests += 1;
        }
        // `if (gameObject.layer == 7) return;` (dropped weapon)
        let mut proceed = item.hand != super::Hand::None;
        if proceed && st.grenade_open && grenade && input.interact.pressed && !env_focused {
            // KeyCode.F and no environment interactable (door / barrel) under the crosshair.
            fire_requests += 1;
        }
        let (mut r_fire, mut l_fire) = (false, false);
        if proceed {
            let in_r = item.in_right();
            let in_l = item.in_left();
            let (f_r, f_l) = if item.invert_fire { (fire2, fire1) } else { (fire1, fire2) };
            let one_press = w.b("onePressShoot");
            if grenade && st.grenade_timer == tbe {
                if !one_press {
                    if f_r > 0.1 && in_r {
                        st.grenade_open = true; // SetBool(true)
                    }
                    if f_l > 0.1 && in_l {
                        st.grenade_open = true;
                    }
                } else {
                    if f_r < 0.1 && in_r {
                        item.is_clicked = true;
                    }
                    if f_l < 0.1 && in_l {
                        item.is_clicked = true;
                    }
                }
                if item.is_clicked && (one_press || (!w.b("reloadWeapon") && item.current_ammo <= 0)) {
                    if f_r > 0.1 && in_r {
                        st.grenade_open = true;
                        item.is_clicked = false;
                    }
                    if f_l > 0.1 && in_l {
                        st.grenade_open = true;
                        item.is_clicked = false;
                    }
                }
                proceed = false; // `return;`
            }
            if proceed {
                if !one_press {
                    r_fire |= f_r > 0.1 && in_r;
                    l_fire |= f_l > 0.1 && in_l;
                } else {
                    if f_r < 0.1 && in_r {
                        item.is_clicked = true;
                    }
                    if f_l < 0.1 && in_l {
                        item.is_clicked = true;
                    }
                }
            }
        }
        // Fire() calls in C# order: (grenade auto/F) ... then hand fires. isClicked block comes last.
        let mut n_fire = fire_requests + r_fire as i32 + l_fire as i32;
        if proceed && item.hand != super::Hand::None {
            let in_r = item.in_right();
            let in_l = item.in_left();
            let (f_r, f_l) = if item.invert_fire { (fire2, fire1) } else { (fire1, fire2) };
            if item.is_clicked && (w.b("onePressShoot") || (!w.b("reloadWeapon") && item.current_ammo <= 0)) {
                if f_r > 0.1 && in_r {
                    n_fire += 1;
                    item.is_clicked = false;
                }
                if f_l > 0.1 && in_l {
                    n_fire += 1;
                    item.is_clicked = false;
                }
            }
        }
        for _ in 0..n_fire {
            fire(
                gun_e, &def, item, &mut st, &mut fpc, &root_tf, &cam_tf, &cam_g, &mut cam_tw, &mut tf, tweens.as_deref_mut(), &anchors, &ctx, &assets, &mut commands, &mut o,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fire(
    gun_e: Entity,
    def: &Arc<WeaponDef>,
    item: &mut Item,
    st: &mut LauncherState,
    fpc: &mut Fpc,
    root_tf: &Transform,
    cam_tf: &Transform,
    cam_g: &GlobalTransform,
    cam_tw: &mut Tweens,
    tf: &mut Transform,
    tweens: Option<&mut Tweens>,
    anchors: &super::data::HandAnchors,
    ctx: &RapierContext,
    assets: &LauncherAssets,
    commands: &mut Commands,
    o: &mut LauncherOut,
) {
    let w = &def.w;
    // PauseManager.pause / currentEnvironmentInteractable ignored.
    if !fpc.can_move {
        return;
    }
    if w.b("changePitchOnShoot") {
        item.audio_pitch = crate::unity::random_range_f(0.97, 1.03);
    }
    if st.fire_timer > 0.0 {
        return;
    }
    st.fire_timer = w.f("timeBetweenFire");
    if item.current_ammo <= 0 {
        o.snd_weapon(w.asset("nobulletClip"), item.audio_pitch); // DualLauncher.cs:265 (audio.pitch set at :256)
        item.no_ammo_clicks += 1; // PlayerPickup drops the weapon when > 1
    }
    if item.current_ammo <= 0 {
        o.snd_weapon(w.asset("nobulletClip"), item.audio_pitch); // second PlayOneShot in the C#
        return;
    }
    // Kanye: kanyeBullets is never filled in this class, so only the toggle matters.
    if w.b("kanye") {
        st.kanye_shoot = true;
    }
    let cam_pos = cam_g.translation();
    let cam_fwd = cam_g.forward().as_vec3();
    let cam_right = cam_g.right().as_vec3();
    let cam_up = cam_g.up().as_vec3();
    let mut position = cam_pos;
    let shrapnel = w.b("shrapnel");
    if shrapnel {
        let from = item.shoot_pos.unwrap_or(cam_pos - cam_up * 0.15 + cam_right * 0.2); // ShootPoint
        if let Some(h) = physics::raycast(ctx, cam_pos, cam_fwd, f32::INFINITY, w.mask("defaultLayer")) {
            position = h.point;
            o.fx.write(GunFx::Tracer { from, to: h.point, prefab: w.asset("bulletTrailLocal") });
        } else {
            position += cam_fwd * 15.0;
            o.fx.write(GunFx::Tracer { from, to: position, prefab: w.asset("bulletTrailLocal") });
        }
    }
    // trickShotData: prediction from the camera (TrickShot.selfTransform) along its forward.
    let trick = trick_shot_fields(def);
    let prediction = trick.as_ref().map(|t| {
        predict_path(ctx, cam_pos, cam_fwd * t.f("speed"), t.f("radius"), t.f("resolution"), t.f("distance"), t.mask("collisionLayers"))
    });
    if w.b("needsAmmo") {
        item.current_ammo -= 1;
    }
    // ServerFire -> ShootObserversEffect
    o.snd_weapon(w.asset("fireClip"), item.audio_pitch); // DualLauncher.cs:731
    if !w.b("grenade") {
        o.fx.write(GunFx::Muzzle { at: item.flash_pos.unwrap_or(cam_pos + cam_fwd * 0.6), prefab: w.prefab_ref("muzzleFlash") });
    }
    // ObserversFire -> SpawnProjectile (passedTime = 0 in single player)
    spawn_projectile(gun_e, def, st, position, cam_fwd, trick.as_ref(), prediction, root_tf, assets, commands, o);
    let kb = w.f("playerKnockback");
    if kb != 0.0 {
        fpc.add_force(-cam_fwd, kb);
    }
    // RocketLauncher: RocketJumpCheck only feeds the rocketJumps statistic (no gameplay effect).
    // verify: DualLauncher.Fire always calls Weapon.CameraAnimation() (DOShakeRotation), never CameraRevolverAnimation,
    // regardless of revolverShake (HandGrenade/Mortini have revolverShake=1), so camera_recoil() (which branches) is not used.
    fpc.cam_base_speed = w.f("cameraLerpSpeed");
    cam_tw.shake_rotation(w.f("duration"), w.v3("strength"), w.i("vibrato"), w.f("randomness"), w.b("fadeOut"), w.i("randomnessMode") == 1);
    let _ = cam_tf;
    weapon_animation(def, item, fpc, tf, tweens, anchors);
}

#[allow(clippy::too_many_arguments)]
fn spawn_projectile(
    _gun_e: Entity,
    def: &Arc<WeaponDef>,
    st: &LauncherState,
    position: Vec3,
    direction: Vec3,
    trick: Option<&Fields>,
    prediction: Option<Vec<Step>>,
    root_tf: &Transform,
    assets: &LauncherAssets,
    commands: &mut Commands,
    o: &mut LauncherOut,
) {
    let w = &def.w;
    let rot = Transform::IDENTITY.looking_to(direction, Vec3::Y).rotation;
    if w.b("grenade") || w.b("obus") || w.b("bubble") {
        // Instantiate(trickShot.template) + Initialize + BallisticPathFollowSyncer.Shoot(data)
        let (Some(trick), Some(steps)) = (trick, prediction) else { return };
        if steps.is_empty() {
            return;
        }
        let Some(pf) = trick.asset("template").and_then(|a| assets.get(&a).cloned()) else {
            warn!("launcher: missing projectile prefab {:?}", trick.asset("template"));
            return;
        };
        let (kind, ty) = if w.b("grenade") {
            (ThrownKind::Grenade, "PhysicsGrenade")
        } else if w.b("obus") {
            (ThrownKind::Obus, "Obus")
        } else {
            (ThrownKind::Bubble, "Bubble")
        };
        let Some(f) = find_comp(&pf, ty) else { return };
        let (audio_clip, audio_max) = audio_src(&pf);
        if kind == ThrownKind::Grenade {
            o.snd_at(f.asset("swooshClip"), position, audio_max, 1.0); // PhysicsGrenade.cs:339 Initialize: audio.PlayOneShot(swooshClip)
        }
        let follow = pf
            .objects
            .iter()
            .flat_map(|o| o.components.iter())
            .find(|(_, c)| c.contains_key("collisionMask") && c.contains_key("resumeDynamicOnEnd"))
            .map(|(_, c)| c.clone())
            .unwrap_or_default();
        // Initialize(): PhysicsGrenade explosionTimer = grenadeOpenSince (remaining fuse), Obus = timeBeforeExplosion,
        // Bubble = timeBeforeDestroy.
        let explosion_timer = match kind {
            ThrownKind::Grenade => st.grenade_timer,
            ThrownKind::Obus => f.f("timeBeforeExplosion"),
            _ => f.f("timeBeforeDestroy"),
        };
        let stem = trick.asset("template").map(|a| model_stem(&a)).unwrap_or_default();
        let axis = f.v3("rotateAxis");
        commands.spawn((
            Transform { translation: position, rotation: rot, scale: Vec3::ONE },
            Visibility::default(),
            ProjFx(stem),
            Thrown {
                kind,
                spin_axis: axis,
                spin_speed: f.f("rotateSpeed"),
                spin: Quat::IDENTITY,
                audio_rolloff: audio_rolloff(&pf),
                f,
                audio_clip,
                audio_max,
                steps,
                time: 0.0,
                phase: Phase::Path,
                follow_mask: follow.mask("collisionMask"),
                end_on_collision: follow.b("endOnCollision"),
                radius: trick.f("radius"),
                explosion_timer,
                exploded: false,
                hit: false,
                hits: 0,
                touched: false,
                touched2: false,
                headshot: false,
                last_pos: Vec3::ZERO,
            },
        ));
        return;
    }
    if w.b("shrapnel") {
        let Some(pf) = w.asset("shrapnelProj").and_then(|a| assets.get(&a).cloned()) else { return };
        let Some(f) = find_comp(&pf, "ShrapnelBallistic") else { return };
        let (audio_clip, audio_max) = audio_src(&pf);
        commands.spawn((
            Mesh3d(assets.sphere.clone()),
            MeshMaterial3d(assets.bullet.clone()),
            Transform { translation: position, rotation: rot, scale: Vec3::splat(0.1) },
            Visibility::default(),
            Thrown {
                kind: ThrownKind::Shrapnel,
                spin_axis: Vec3::ZERO,
                spin_speed: 0.0,
                spin: Quat::IDENTITY,
                audio_rolloff: audio_rolloff(&pf),
                f,
                audio_clip,
                audio_max,
                steps: Vec::new(),
                time: 0.0,
                phase: Phase::Dynamic,
                follow_mask: 0,
                end_on_collision: false,
                radius: 0.0,
                explosion_timer: 0.1, // Action(): WaitForSeconds(0.1) then Explode()
                exploded: false,
                hit: false,
                hits: 0,
                touched: false,
                touched2: false,
                headshot: false,
                last_pos: Vec3::ZERO,
            },
        ));
        return;
    }
    if w.b("rebond") {
        // TODO RebondBalle (no DualLauncher prefab uses it).
        return;
    }
    // Default: PredictedProjectile
    spawn_predicted_shot(w.asset("_projectile"), w.f("launchForce"), position, direction, root_tf, assets, commands);
}

/// `SpawnProjectile` default branch (`Instantiate(_projectile, position, LookRotation(direction))` + `Initialize`);
/// `pub` so BeamGun (beam.rs) can spawn its `_projectile` (BeamLoadBullet) the same way.
#[allow(clippy::too_many_arguments)]
pub fn spawn_predicted_shot(
    projectile: Option<String>,
    launch_force: f32,
    position: Vec3,
    direction: Vec3,
    root_tf: &Transform,
    assets: &LauncherAssets,
    commands: &mut Commands,
) {
    let Some(pf) = projectile.as_ref().and_then(|a| assets.get(a).cloned()) else {
        warn!("launcher: missing projectile prefab {:?}", projectile);
        return;
    };
    let Some(p) = find_comp(&pf, "PredictedProjectile") else { return };
    // Initialize(): rotation LookRotation(-direction); position += rootObject.forward * launchOffset
    let pos = position + root_tf.forward().as_vec3() * p.f("launchOffset");
    let _r = p.f("radius");
    // AudioSource with loop + playOnAwake (ProphetProj: Prophet_Loop_00) plays while the projectile flies.
    let looping = find_comp(&pf, "AudioSource").filter(|a| a.b("Loop") && a.b("m_PlayOnAwake")).and_then(|a| a.asset("m_audioClip"));
    let looping_ref = looping.clone();
    let (audio_clip, audio_max) = audio_src(&pf);
    commands.spawn((
        Transform { translation: pos, rotation: Transform::IDENTITY.looking_to(-direction, Vec3::Y).rotation, scale: Vec3::ONE },
        crate::audio::LoopEmitter::effect(looping_ref.unwrap_or_default(), audio_max),
        Visibility::default(),
        ProjFx(projectile.as_deref().map(model_stem).unwrap_or_default()),
        Shot {
            dir: direction,
            force: launch_force,
            passed_time: 0.0,
            actual_passed: 0.0,
            gravity_acc: p.f("gravityStart"),
            last_pos: Vec3::ZERO, // private Vector3 lastPosition (default zero) — first-frame forward quirk preserved
            fwd: -direction,
            distance: 0.0,
            damage: p.f("damage"),
            headshot: false,
            touched: false,
            touched2: false,
            done: false,
            audio_clip,
            audio_max,
            looping: looping.is_some(),
            p,
        },
    ));
}

// =============================================================================================
// PredictedProjectile.Update (Move + HandleCollision)
// =============================================================================================

#[allow(clippy::too_many_arguments)]
pub fn shot_update(
    mut commands: Commands,
    time: Res<Time>,
    rapier: ReadRapierContext,
    assets: Res<LauncherAssets>,
    mut players: Query<(Entity, &mut Fpc, &Transform), (Without<Shot>, Without<Item>)>,
    mut cams: CamQuery,
    mut shots: Query<(Entity, &mut Shot, &mut Transform), (Without<crate::player::fpc::PlayerCamera>, Without<Fpc>, Without<Item>)>,
    mut o: LauncherOut,
) {
    let Ok(ctx) = rapier.single() else { return };
    let Ok((player_e, mut fpc, player_tf)) = players.single_mut() else { return };
    let Ok((_, _, mut cam_tw)) = cams.single_mut() else { return };
    let player_pos = player_tf.translation;
    let shooter = Shooter::new(player_e, &fpc, player_tf);
    let dt = time.delta_secs();
    for (e, mut s, mut tf) in &mut shots {
        if s.done {
            continue;
        }
        let s = &mut *s;
        // ---- Update(): currentPosition = position; Move(); HandleCollision(); lastPosition = currentPosition
        let current = tf.translation;
        // Move()
        let velocity = s.last_pos - current;
        if !s.p.b("useGravity") {
            s.fwd = s.dir;
        } else if velocity != Vec3::ZERO {
            s.fwd = (-velocity).normalize();
        }
        let mut num = 0.0;
        if s.passed_time > 0.0 {
            // catch-up for network latency; passedTime is 0 in single player
            let mut n2 = s.passed_time * 0.08;
            s.passed_time -= n2;
            if s.passed_time <= dt / 2.0 {
                n2 += s.passed_time;
                s.passed_time = 0.0;
            }
            num = n2;
        }
        let step_t = dt + num;
        s.actual_passed += step_t;
        let use_physics = s.p.b("usePhysics");
        if s.p.b("useGravity") {
            s.gravity_acc += s.p.f("gravity") * step_t;
        }
        if use_physics {
            s.force -= s.p.f("friction") * step_t;
        }
        let speed = if use_physics { s.force } else { s.p.f("MOVE_RATE") };
        // Debug: RESTRAFE_PROJ_SLOW=<factor> scales projectile travel (visual inspection).
        let speed = speed * std::env::var("RESTRAFE_PROJ_SLOW").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
        tf.translation += s.dir * (speed * step_t);
        s.distance += speed * step_t;
        if s.distance > 150.0 && s.p.b("destroySelf") {
            s.done = true;
            commands.entity(e).insert((DespawnIn(3.0), Visibility::Hidden));
            continue;
        }
        // The C# prophet projectile never despawns (shouldDestroy = 0, destroySelf = 0); cap it so it cannot leak.
        if s.distance > 2000.0 {
            commands.entity(e).despawn();
            continue;
        }
        if s.p.b("useGravity") {
            tf.translation -= Vec3::Y * (s.gravity_acc * step_t);
        }
        tf.rotation = Transform::IDENTITY.looking_to(s.fwd, Vec3::Y).rotation;
        if std::env::var_os("RESTRAFE_DEBUG_SHOT").is_some() {
            println!("SHOT {e} pos={:.2?} speed={speed:.1} dist={:.1} gravity={}", tf.translation, s.distance, s.p.b("useGravity"));
        }
        // HandleCollision()
        shot_collision(e, s, &tf, &ctx, &assets, &mut commands, &mut o, &mut fpc, player_pos, &mut cam_tw, shooter);
        s.last_pos = current;
    }
}

#[allow(clippy::too_many_arguments)]
fn shot_collision(
    e: Entity,
    s: &mut Shot,
    tf: &Transform,
    ctx: &RapierContext,
    assets: &LauncherAssets,
    commands: &mut Commands,
    o: &mut LauncherOut,
    fpc: &mut Fpc,
    player_pos: Vec3,
    cam_tw: &mut Tweens,
    shooter: Shooter,
) {
    // prophetHit is never set by any code path.
    if !s.p.b("skipSafeStartTimer") && s.actual_passed < 0.1 {
        return;
    }
    let pos = tf.translation;
    let fwd = s.fwd;
    let player_mask = s.p.mask("playerLayer");
    let radius = s.p.f("radius");
    let backup = physics::raycast(ctx, pos - fwd, fwd, s.p.f("backupRayLength"), player_mask);
    let array = physics::overlap_sphere(ctx, pos, radius, player_mask);
    if let Some(h) = physics::raycast(ctx, pos, Vec3::new(fwd.x, 0.0, fwd.z), 2.0, s.p.mask("headLayer")) {
        if let Ok(part) = o.hit.parts.get(h.entity) {
            if part.name == "Head_Col" || part.name == "Neck_1_Col" {
                s.headshot = true; // sticky, never reset in the C#
            }
        }
    }
    let should_destroy = s.p.b("shouldDestroy");
    // The owner's CharacterController (layer Self) is in `array` / hit by the backup ray when playerLayer contains Self.
    let self_in_mask = player_mask & (1 << physics::layer::SELF) != 0;
    let owner_in_array = self_in_mask && shooter.overlaps_sphere(pos, radius);
    if !array.is_empty() || owner_in_array {
        // flag (collider child of the gun: held weapons have no active collider) / flag2 (child of the shooter: its CC
        // capsule; its hit colliders are on SelfBody, never in playerLayer) -> nothing happens this frame.
        if owner_in_array {
            return;
        }
        let is_glass = array.iter().any(|a| o.is_glass(*a));
        let normal = if o.owner_of(array[0]).is_none() && should_destroy && !is_glass {
            physics::sphere_cast(ctx, pos - fwd, radius * 1.4, fwd, 1.0, player_mask).map(|h| h.normal)
        } else {
            None
        };
        shot_resolve(e, s, pos, array[0], is_glass, normal, Vec3::ZERO, ctx, assets, commands, o, fpc, player_pos, cam_tw, shooter);
    } else {
        let Some(h) = backup else { return };
        if s.touched || s.touched2 || !should_destroy {
            return;
        }
        // flag4: the ray reached the shooter's CC capsule before `h` (Unity reports the nearest hit).
        let start = pos - fwd;
        if self_in_mask && super::mines::seg_seg_dist(start, start + fwd * h.distance, shooter.cap.0, shooter.cap.1) <= shooter.cap.2 {
            return;
        }
        let is_glass = o.is_glass(h.entity);
        shot_resolve(e, s, pos, h.entity, is_glass, Some(h.normal), fwd * 1.5, ctx, assets, commands, o, fpc, player_pos, cam_tw, shooter);
    }
}

/// Shared body of both HandleCollision branches (they only differ in how the first collider is found).
#[allow(clippy::too_many_arguments)]
fn shot_resolve(
    e: Entity,
    s: &mut Shot,
    pos: Vec3,
    first: Entity,
    is_glass: bool,
    vfx_normal: Option<Vec3>,
    vfx_offset: Vec3,
    ctx: &RapierContext,
    _assets: &LauncherAssets,
    commands: &mut Commands,
    o: &mut LauncherOut,
    fpc: &mut Fpc,
    player_pos: Vec3,
    cam_tw: &mut Tweens,
    shooter: Shooter,
) {
    // TODO ShatterableGlass.Shatter3D for glass colliders (isGlass projectiles keep flying).
    let should_destroy = s.p.b("shouldDestroy");
    let hit_sfx = s.p.asset("hitSfx");
    // PredictedProjectile.cs:431/511/591/667 `audio.Play()` = prefab AudioSource clip (its hitClip field is unused).
    let hit_clip = s.audio_clip.clone();
    let audio_max = s.audio_max;
    let mut played = false;
    let direct = o.owner_of(first);
    // `componentInParent != null && (!shouldDestroy || componentInParent != _rootObject.PlayerHealth)`
    if let Some(ph) = direct.filter(|ph| !should_destroy || *ph != shooter.e) {
        if !o.is_killed(ph) {
            if s.headshot {
                s.damage *= 2.0;
            }
            // TODO Settings head/bodyshot counters
            let dmg = s.damage;
            o.damage(ph, dmg, fpc, true);
            o.ragdoll_hit(ph, pos, s.p.f("ragdollEjectForce"));
            o.hit_marker(e, s.headshot, hit_sfx.clone(), pos, audio_max);
            // Instantiate(bloodVfx) always, plus headBloodVfx on headshots (both at identity rotation).
            o.fx.write(GunFx::Spawn { prefab: s.p.asset("bloodVfx"), at: pos, normal: Vec3::NEG_Z });
            if s.headshot {
                o.fx.write(GunFx::Spawn { prefab: s.p.asset("headBloodVfx"), at: pos, normal: Vec3::NEG_Z });
            }
        }
        o.blood_splatter(&s.p, ph);
        if should_destroy {
            s.done = true;
            commands.entity(e).insert((DespawnIn(3.0), Visibility::Hidden));
            screenshake(cam_tw, player_pos, pos, &s.p);
        }
        if s.looping {
            commands.entity(e).insert(crate::audio::RestartLoop); // audio.Play() on the looping source
        } else {
            o.snd_at(hit_clip.clone(), pos, audio_max, 1.0); // audio.Play()
        }
        played = true;
    }
    if should_destroy && !is_glass && direct.is_none() {
        if let Some(n) = vfx_normal {
            // Instantiate(hitVfx, position, orientateVfx ? LookRotation(normal) : identity) (PredictedProjectile.cs:435/595).
            o.fx.write(GunFx::Spawn { prefab: s.p.asset("hitVfx"), at: pos, normal: if s.p.b("orientateVfx") { n } else { Vec3::NEG_Z } });
            // SurfacesImpact: SpawnVFX(0, position, normal, tag, hit transform) = the surface's <x>HitImpact, parented; nothing
            // on NoSound / Mine / Grenade / Hat or a Player-layer parent (PredictedProjectile.cs:732).
            if s.p.b("SurfacesImpact") && o.layers.get(first).map(|l| l.0) != Ok(crate::physics::layer::PLAYER) {
                let tag = o.tags.get(first).map(|t| t.0.clone()).unwrap_or_default();
                if let Some((impact, _)) = crate::fx::surface_fields(&tag) {
                    o.fx.write(GunFx::SpawnOn { prefab: s.p.asset(impact), at: pos, normal: n, parent: Some(first) });
                }
            }
            // SurfacesVFX: SpawnVFX(1, ...) = the surface's hole / HitFx (cs:442 at position; backup-ray branch cs:601 at
            // position + forward * 1.5). BeamLoadBullet / Gamma / Kanye / NuggetBullet.
            if s.p.b("SurfacesVFX") && o.layers.get(first).map(|l| l.0) != Ok(crate::physics::layer::PLAYER) {
                let tag = o.tags.get(first).map(|t| t.0.clone()).unwrap_or_default();
                if let Some((_, hole)) = crate::fx::surface_fields(&tag) {
                    o.fx.write(GunFx::SpawnOn { prefab: s.p.asset(hole), at: pos + vfx_offset, normal: n, parent: Some(first) });
                }
            }
            if s.p.b("spawnObject") {
                // OnHit: Instantiate(objToSpawn (DF_*_Zone), position, identity).transform.up = normal (fx.rs DamageZone).
                o.fx.write(GunFx::Spawn { prefab: s.p.asset("objToSpawn"), at: pos, normal: n });
            }
        }
    }
    // Explosion: OverlapSphere(explosionRadius, bodyLayer)
    let er = s.p.f("explosionRadius");
    if er > 0.0 && !is_glass {
        let bodies = physics::overlap_sphere(ctx, pos, er, s.p.mask("bodyLayer"));
        if !bodies.is_empty() {
            let owners = o.owners(&bodies);
            for ph in owners {
                if o.is_killed(ph) {
                    continue;
                }
                o.blood_splatter(&s.p, ph);
                let dmg = s.damage;
                if ph == shooter.e {
                    // Same client as the weapon: ChangeKilledState + suicide (no Explode / KillShockWave here: the ragdoll
                    // comes from PlayerHealth.Update -> DespawnObject's default Explode), then RemoveHealth(damage).
                    if o.damage(ph, dmg, fpc, false) {
                        o.suicide(ph);
                    }
                } else {
                    o.damage(ph, dmg, fpc, true); // kill => ChangeKilledState + Explode ragdoll + KillShockWave
                    o.ragdoll_hit(ph, pos, s.p.f("ragdollEjectForce"));
                }
                o.hit_marker(e, s.headshot, hit_sfx.clone(), pos, audio_max);
                s.touched = true;
                s.touched2 = true;
            }
            // A direct player hit spawns no hitVfx (PredictedProjectile.cs:433 requires componentInParent == null).
        }
    }
    if should_destroy && !is_glass {
        o.fx.write(GunFx::Spawn { prefab: s.p.asset("explosionDecal"), at: pos, normal: Vec3::NEG_Z }); // Instantiate(explosionDecal, position, identity)
        s.done = true;
        commands.entity(e).insert((DespawnIn(3.0), Visibility::Hidden));
        if !played {
            o.snd_at(hit_clip, pos, audio_max, 1.0);
        }
        screenshake(cam_tw, player_pos, pos, &s.p);
    }
}

// =============================================================================================
// PhysicsGrenade / Obus / Bubble / ShrapnelBallistic
// =============================================================================================

#[allow(clippy::too_many_arguments)]
pub fn thrown_update(
    mut commands: Commands,
    time: Res<Time>,
    rapier: ReadRapierContext,
    assets: Res<LauncherAssets>,
    mut events: MessageReader<CollisionEvent>,
    mut players: Query<(Entity, &mut Fpc, &Transform), (Without<Thrown>, Without<Item>)>,
    mut cams: CamQuery,
    mut thrown: Query<(Entity, &mut Thrown, &mut Transform), (Without<crate::player::fpc::PlayerCamera>, Without<Fpc>, Without<Item>, Without<ProjModel>)>,
    mut models: Query<(&ChildOf, &mut Transform), (With<ProjModel>, Without<Thrown>, Without<crate::player::fpc::PlayerCamera>, Without<Fpc>, Without<Item>)>,
    mut o: LauncherOut,
) {
    // PhysicsGrenade.Update graph.Rotate (all phases until the explosion disables the script): the model wrapper
    // (180 deg Y glb root) carries the accumulated graph rotation.
    for (parent, mut mtf) in &mut models {
        if let Ok((_, th, _)) = thrown.get(parent.parent()) {
            mtf.rotation = th.spin * Quat::from_rotation_y(std::f32::consts::PI);
        }
    }
    let Ok(ctx) = rapier.single() else { return };
    let Ok((player_e, mut fpc, player_tf)) = players.single_mut() else { return };
    let Ok((_, _, mut cam_tw)) = cams.single_mut() else { return };
    let player_pos = player_tf.translation;
    let shooter = Shooter::new(player_e, &fpc, player_tf);
    let dt = time.delta_secs();

    // OnCollisionEnter (dynamic phase)
    let mut contacts: Vec<(Entity, Entity)> = Vec::new();
    for ev in events.read() {
        if let CollisionEvent::Started(a, b, _) = ev {
            contacts.push((*a, *b));
            contacts.push((*b, *a));
        }
    }
    for (me, other) in contacts {
        let Ok((_, mut th, tf)) = thrown.get_mut(me) else { continue };
        if th.phase != Phase::Dynamic {
            continue;
        }
        match th.kind {
            ThrownKind::Grenade => {
                // OnCollisionEnter: hits <= maxHits(5) && timer >= 0 -> hitClip @0.56
                if th.hits <= 5 && !(th.explosion_timer < 0.0) {
                    if let Some(c) = th.f.asset("hitClip") {
                        o.sounds.write(WeaponSound { clip: c, volume: 0.56, at: Some(tf.translation), pitch: 1.0, linear_max: th.audio_max, source: None, rolloff: None });
                    }
                    th.hits += 1;
                }
            }
            ThrownKind::Obus => th.hit = true,
            ThrownKind::Bubble => {
                let layer = o.layers.get(other).map(|l| l.0).unwrap_or(0);
                if (layer == physics::layer::BODY || layer == physics::layer::SELF_BODY) && !th.exploded {
                    if let Some(ph) = o.owner_of(other) {
                        let dmg = th.f.f("damage");
                        o.damage(ph, dmg, &mut fpc, true); // Bubble: KillShockWave on every kill, its owner included
                        o.ragdoll_hit(ph, tf.translation, th.f.f("ragdollEjectForce"));
                        o.fx.write(GunFx::Spawn { prefab: th.f.asset("hitVfx"), at: tf.translation, normal: Vec3::NEG_Z }); // Bubble.cs:147 Instantiate(hitVfx, position, identity)
                    }
                    // HandleExplosion(position)
                    pop_bubble(me, &mut th, tf.translation, &mut commands, &mut o, &mut cam_tw, player_pos);
                }
            }
            ThrownKind::Shrapnel => {}
        }
    }

    for (e, mut th, mut tf) in &mut thrown {
        let th = &mut *th;
        if th.phase == Phase::Done {
            continue;
        }
        let cur = tf.translation;
        // ---- Update()
        match th.kind {
            ThrownKind::Grenade => {
                th.explosion_timer -= dt;
                // graph.Rotate(rotateAxis * rotateSpeed * dt): Transform.Rotate(eulers), Space.Self ->
                // localRotation *= Quaternion.Euler(eulers) (axis not normalized), every frame incl. the Rigidbody phase.
                let eul = th.spin_axis * (th.spin_speed * dt);
                th.spin = (th.spin * crate::unity::unity_euler(eul.x, eul.y, eul.z)).normalize();
                if th.explosion_timer < 0.0 && th.explosion_timer > -2.0 && !th.exploded {
                    th.exploded = true;
                    explode_blast(e, th, cur, &ctx, &assets, &mut commands, &mut o, &mut fpc, player_pos, &mut cam_tw, shooter);
                    continue;
                }
            }
            ThrownKind::Obus => {
                // `velocity = lastPosition - currentPosition` with currentPosition never assigned (Obus.cs:102/171/185):
                // always zero, so the LookRotation(-velocity) branch never runs; the shell keeps its spawn rotation
                // (BallisticPathFollow only sets position).
                if th.hit {
                    th.explosion_timer -= dt;
                }
                if th.explosion_timer < 0.0 && th.explosion_timer > -2.0 && !th.exploded {
                    th.exploded = true;
                    explode_blast(e, th, cur, &ctx, &assets, &mut commands, &mut o, &mut fpc, player_pos, &mut cam_tw, shooter);
                    continue;
                }
            }
            ThrownKind::Bubble => {
                th.explosion_timer -= dt;
                if th.explosion_timer < 0.0 && !th.exploded {
                    pop_bubble(e, th, cur, &mut commands, &mut o, &mut cam_tw, player_pos);
                    continue;
                }
            }
            ThrownKind::Shrapnel => {
                th.explosion_timer -= dt;
                if th.explosion_timer <= 0.0 && !th.exploded {
                    th.exploded = true;
                    explode_shrapnel(e, th, cur, &ctx, &assets, &mut commands, &mut o, &mut fpc, player_pos, &mut cam_tw, shooter);
                    continue;
                }
            }
        }
        th.last_pos = cur;
        // ---- BallisticPathFollow.LateUpdate()
        if th.phase == Phase::Path {
            th.time += dt;
            let total = th.steps.last().map(|s| s.t).unwrap_or(0.0);
            if th.steps.is_empty() {
                th.phase = Phase::Dynamic;
                continue;
            }
            if th.time >= total {
                // end of path: no planned impact -> EndOfPath(lastStep.position, lastStep.velocity)
                let last = *th.steps.last().unwrap();
                end_of_path(e, th, last.pos, last.vel, &assets, &mut commands, &mut tf);
            } else {
                let step = path_lerp(&th.steps, th.time);
                let mut ended = false;
                if th.end_on_collision {
                    // ConsiderCollision(position, step.velocity, null): any hit ends the path
                    let hit = physics::sphere_cast(&ctx, tf.translation, th.radius, step.vel.normalize_or_zero(), step.vel.length() * FIXED_DT * 2.0, th.follow_mask);
                    if hit.is_some() {
                        let p = tf.translation;
                        end_of_path(e, th, p, step.vel, &assets, &mut commands, &mut tf);
                        ended = true;
                    }
                }
                if !ended {
                    tf.translation = step.pos;
                }
            }
        }
    }
}

/// `BallisticPathFollow.EndOfPath`: resumeDynamicOnEnd -> Rigidbody non-kinematic with the final velocity.
fn end_of_path(e: Entity, th: &mut Thrown, pos: Vec3, vel: Vec3, _assets: &LauncherAssets, commands: &mut Commands, tf: &mut Transform) {
    tf.translation = pos;
    th.phase = Phase::Dynamic;
    let radius = match th.kind {
        ThrownKind::Bubble => 0.3,
        _ => 0.4,
    }; // SphereCollider m_Radius of the prefab
    commands.entity(e).insert((
        RigidBody::Dynamic,
        Collider::ball(radius),
        // Layer 20 (Grenade) + collision matrix row 20 (a UnityLayer component would overwrite these groups).
        CollisionGroups::new(Group::from_bits_retain(1 << GRENADE_LAYER), Group::from_bits_retain(GRENADE_FILTER)),
        ColliderMassProperties::Mass(1.0),
        Velocity::linear(vel),
        // PhysicMaterial "Default": bounciness 0.5, friction 0.5 (average combine) against Unity's default 0.6 on the map
        // = 0.55. Rapier's default collider friction is 0.5, so 0.6 averages to the same 0.55.
        Restitution { coefficient: 0.5, combine_rule: CoefficientCombineRule::Average },
        Friction { coefficient: 0.6, combine_rule: CoefficientCombineRule::Average },
        Damping { linear_damping: 0.0, angular_damping: 0.05 },
        Ccd::enabled(),
        GravityScale(1.0),
        ActiveEvents::COLLISION_EVENTS,
    ));
}

fn hide_and_expire(e: Entity, commands: &mut Commands) {
    // Destroy(gameObject, 3f); graph.SetActive(false)
    commands.entity(e).insert((DespawnIn(3.0), Visibility::Hidden));
}

/// PhysicsGrenade.HandleExplosion / Obus.HandleExplosion: kill everything with a Body collider in the blast.
#[allow(clippy::too_many_arguments)]
fn explode_blast(
    e: Entity,
    th: &mut Thrown,
    pos: Vec3,
    ctx: &RapierContext,
    _assets: &LauncherAssets,
    commands: &mut Commands,
    o: &mut LauncherOut,
    fpc: &mut Fpc,
    player_pos: Vec3,
    cam_tw: &mut Tweens,
    shooter: Shooter,
) {
    th.phase = Phase::Done;
    let f = &th.f;
    let radius = f.f("explosionRadius");
    let body_mask = f.mask("bodyLayer");
    let frag = f.b("fragGrenade"); // PhysicsGrenade only
    let stun = f.b("stunGrenade");
    let mut ents: Vec<Entity> = Vec::new();
    if !frag {
        ents = physics::overlap_sphere(ctx, pos, radius, body_mask);
    } else {
        // System.Random(seed from the position rounded to 5 m): the same blast spot gives the original's rays.
        for d in crate::unity::frag_ray_dirs(pos, f.f("numberOfRays") as i32) {
            let hit = physics::raycast(ctx, pos, d, radius, body_mask);
            // RenderObject(normalized, hit ? distance : explosionRadius) for every ray.
            let len = hit.as_ref().map(|h| pos.distance(h.point)).unwrap_or(radius);
            o.fx.write(GunFx::Tracer { from: pos, to: pos + d * len, prefab: Some(format!("{}grenade", crate::fx::RAY_LINE)) });
            if let Some(h) = hit {
                ents.push(h.entity);
            }
        }
    }
    if std::env::var_os("RESTRAFE_DEBUG_HITS").is_some() {
        println!("BLAST at {pos:?} r={radius} frag={frag} mask={body_mask} hits={}", ents.len());
    }
    // ShatterableGlass.Shatter3D TODO for glass colliders in `ents`.
    // First collider with a PlayerHealth: Instantiate(bloodSplatter, collider position) once (makeBlood), not for stun.
    if !stun {
        o.first_blood(f, &ents);
    }
    for ph in o.owners(&ents) {
        if o.is_killed(ph) {
            continue;
        }
        if !stun {
            // ChangeKilledState + RemoveHealth(10) + Explode(dismemberment) + SetKiller; the thrower itself (`_rootObject`)
            // is killed too ("suicide", IncreaseSuicidesAmount), without KillShockWave / kill log.
            o.kill(ph);
            o.ragdoll_hit(ph, pos, f.f("ragdollEjectForce"));
            if ph == shooter.e {
                // PhysicsGrenade sets ph.suicide (cs:483); Obus only bumps the stat counter (Obus.cs:401-407).
                if th.kind == ThrownKind::Grenade {
                    o.suicide(ph);
                }
            } else {
                fpc.kill_shock_wave();
            }
        } else {
            o.stun(ph, f.f("stunTime")); // TaserEnemy(ph, stunTime)
        }
    }
    screenshake(cam_tw, player_pos, pos, f);
    o.snd_at(th.audio_clip.clone().or_else(|| f.asset("explosionClip")), pos, th.audio_max, 1.0); // audio.Play() (prefab AudioSource clip)
    // PhysicsGrenade.HandleExplosion / Obus: explosionVfx at identity (not for fragGrenade; its "ball" child scaled to
    // vfxScale), then always explosionDecal at identity (PhysicsGrenade.cs:540-548, Obus.cs:435-436).
    if !frag {
        let ball = if f.get("vfxScale").is_some() { f.v3("vfxScale") } else { Vec3::ONE };
        o.fx.write(GunFx::SpawnBall { prefab: f.asset("explosionVfx"), at: pos, ball });
    }
    o.fx.write(GunFx::Spawn { prefab: f.asset("explosionDecal"), at: pos, normal: Vec3::NEG_Z });
    hide_and_expire(e, commands);
}

/// ShrapnelBallistic.Explode (HK_Caws): `numberOfRays` random rays; only the first non-shooter PlayerHealth is hurt.
#[allow(clippy::too_many_arguments)]
fn explode_shrapnel(
    e: Entity,
    th: &mut Thrown,
    pos: Vec3,
    ctx: &RapierContext,
    _assets: &LauncherAssets,
    commands: &mut Commands,
    o: &mut LauncherOut,
    fpc: &mut Fpc,
    player_pos: Vec3,
    cam_tw: &mut Tweens,
    shooter: Shooter,
) {
    th.phase = Phase::Done;
    let f = th.f.clone();
    o.snd_at_r(f.asset("explosionClip"), pos, th.audio_rolloff.clone()); // ShrapnelBallistic.cs:181 (Logarithmic source)
    let radius = f.f("explosionRadius");
    let body_mask = f.mask("bodyLayer");
    let mut ents: Vec<Entity> = Vec::new();
    if f.b("fragGrenade") {
        for d in crate::unity::frag_ray_dirs(pos, f.f("numberOfRays") as i32) {
            let hit = physics::raycast(ctx, pos, d, radius, body_mask);
            // RenderObject(normalized, hit ? distance : explosionRadius) for every ray.
            let len = hit.as_ref().map(|h| pos.distance(h.point)).unwrap_or(radius);
            o.fx.write(GunFx::Tracer { from: pos, to: pos + d * len, prefab: Some(format!("{}shrapnel:{}:{}", crate::fx::RAY_LINE, f.f("lineFadeSpeed"), f.f("lineDecreaseSpeed"))) });
            if let Some(h) = hit {
                ents.push(h.entity);
                // headshot = hit collider name is Head_Col/Neck_1_Col (overwritten by every ray; last hit wins)
                th.headshot = o.hit.parts.get(h.entity).map(|p| p.name == "Head_Col" || p.name == "Neck_1_Col").unwrap_or(false);
            }
        }
    } else {
        ents = physics::overlap_sphere(ctx, pos, radius, body_mask);
    }
    o.first_blood(&f, &ents); // Instantiate(bloodSplatter, collider position) once (makeBlood)
    // One entry per collider (not per PlayerHealth): `touched` / `touched2` make only the first shooter / non-shooter
    // entry count. No isKilled check in this script.
    for ent in ents {
        let Some(ph) = o.owner_of(ent) else { continue };
        let dmg = if th.headshot { f.f("damage") * 2.0 } else { f.f("damage") };
        if ph == shooter.e {
            if th.touched {
                continue;
            }
            // ChangeKilledState + suicide + Explode when lethal (no KillShockWave), HitMarker, RemoveHealth.
            o.hit_marker_r(e, th.headshot, f.asset("hitSfx"), pos, th.audio_rolloff.clone());
            if o.damage(ph, dmg, fpc, false) {
                o.suicide(ph);
                o.ragdoll_hit(ph, pos, f.f("ragdollEjectForce"));
            }
            th.touched = true;
            continue;
        }
        if th.touched2 {
            continue;
        }
        o.hit_marker_r(e, th.headshot, f.asset("hitSfx"), pos, th.audio_rolloff.clone());
        o.damage(ph, dmg, fpc, true);
        o.ragdoll_hit(ph, pos, f.f("ragdollEjectForce"));
        th.touched2 = true;
    }
    screenshake(cam_tw, player_pos, pos, &f);
    o.fx.write(GunFx::Spawn { prefab: f.asset("explosionDecal"), at: pos, normal: Vec3::NEG_Z }); // VFX_Explosion_HKCaws, identity
    o.snd_at_r(th.audio_clip.clone(), pos, th.audio_rolloff.clone()); // ShrapnelBallistic.cs:280 audio.Play() (prefab clip BeamLoadImpact)
    hide_and_expire(e, commands);
}

/// Bubble.HandleExplosion -> Explode (pop).
fn pop_bubble(e: Entity, th: &mut Thrown, pos: Vec3, commands: &mut Commands, o: &mut LauncherOut, cam_tw: &mut Tweens, player_pos: Vec3) {
    th.exploded = true;
    th.phase = Phase::Done;
    o.fx.write(GunFx::Spawn { prefab: th.f.asset("hitVfx"), at: pos, normal: Vec3::NEG_Z }); // Bubble.cs:377 Instantiate(hitVfx, position, identity)
    o.snd_at(th.audio_clip.clone(), pos, th.audio_max, 1.0); // Bubble.cs:378 audio.Play() (prefab AudioSource clip)
    screenshake(cam_tw, player_pos, pos, &th.f); // duration 0 in the prefab -> no-op
    commands.entity(e).remove::<(Collider, RigidBody)>(); // GetComponent<Collider>().enabled = false
    hide_and_expire(e, commands);
}

/// Lifetimes (`Destroy(obj, t)`).
pub fn launcher_fx_update(mut commands: Commands, time: Res<Time>, mut despawn: Query<(Entity, &mut DespawnIn)>) {
    let dt = time.delta_secs();
    for (e, mut d) in &mut despawn {
        d.0 -= dt;
        if d.0 <= 0.0 {
            commands.entity(e).try_despawn();
        }
    }
}

// =============================================================================================
// Projectile visuals: real prefab model (models/projectiles/<prefab>.glb) + prefab ParticleSystem children
// =============================================================================================

/// Projectile prefab stem ("GameObject/Rocket.prefab" -> "Rocket").
#[derive(Component)]
pub struct ProjFx(pub String);

/// The projectile model wrapper (glb root, child of the projectile); carries `Thrown::spin` (PhysicsGrenade graph).
#[derive(Component)]
pub struct ProjModel;

fn model_stem(asset: &str) -> String {
    asset.rsplit('/').next().unwrap_or(asset).trim_end_matches(".prefab").to_string()
}

/// Adds the glb model (X-mirror -> 180 deg Y) and the prefab's ParticleSystems (rocket smoke trail etc.) to a new projectile.
pub fn projectile_visuals(mut vfx: crate::vfx::Vfx, assets: Res<AssetServer>, q: Query<(Entity, &Transform, &ProjFx), Added<ProjFx>>) {
    for (e, tf, fx) in &q {
        if fx.0.is_empty() {
            continue;
        }
        let path = format!("models/projectiles/{}.glb", fx.0);
        if std::path::Path::new("assets").join(&path).exists() || crate::owned::Owned::global().is_some_and(|o| o.resolve_prefab(&fx.0, "sharedassets").is_some()) {
            let scene = assets.load(GltfAssetLabel::Scene(0).from_asset(path));
            let json = format!("assets/data/projectiles/{}.json", fx.0);
            // Instantiate keeps the prefab root's localScale (Rocket / Kanye 0.134, Gamma 0.293, ProphetProj 0.095), which
            // the glb root node lacks; the Shot root stays at scale 1 (radius / colliders in world units).
            static SCALES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Vec3>>> = std::sync::OnceLock::new();
            let root_scale = *SCALES.get_or_init(Default::default).lock().unwrap().entry(json.clone()).or_insert_with(|| {
                crate::weapons::data::Prefab::load(&json)
                    .ok()
                    .and_then(|p| p.objects.first().and_then(|o| o.comp("Transform")).map(|t| t.v3("m_LocalScale")))
                    .filter(|s| s.x > 0.0)
                    .unwrap_or(Vec3::ONE)
            });
            let m = vfx
                .commands
                .spawn((bevy::world_serialization::WorldAssetRoot(scene), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)).with_scale(root_scale), Visibility::default(), ProjModel))
                .id();
            if crate::owned::data_exists(&json) {
                vfx.commands.entity(m).insert((crate::unity_pbr::PrefabRender(json), crate::unity_pbr::HiddenUntilReady, Visibility::Hidden));
            }
            vfx.commands.entity(m).try_insert(ChildOf(e));
        }
        if crate::owned::data_exists(&format!("assets/data/vfx/{}.json", fx.0)) {
            if let Some(root) = vfx.spawn(&fx.0, tf.translation, tf.rotation, crate::vfx::SpawnOpts::default()) {
                vfx.commands.entity(root).try_insert(TrailOf(e));
            }
        }
    }
}

/// Marks a prefab-VFX root (rocket smoke trail ...) that follows projectile `.0`; removed with it.
#[derive(Component)]
pub struct TrailOf(pub Entity);

pub fn trail_follow(
    mut commands: Commands,
    proj: Query<(&Transform, &Visibility), Without<TrailOf>>,
    mut trails: Query<(Entity, &TrailOf, &mut Transform, &crate::vfx::sim::FxRoot)>,
) {
    for (e, t, mut tf, root) in &mut trails {
        match proj.get(t.0) {
            Ok((p, v)) if *v != Visibility::Hidden => {
                tf.translation = p.translation;
                tf.rotation = p.rotation;
            }
            _ => {
                for x in &root.extra {
                    commands.entity(*x).try_despawn();
                }
                commands.entity(e).try_despawn();
            }
        }
    }
}
