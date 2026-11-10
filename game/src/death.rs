//! Local player death, kill cam and round respawn (single-player port of the owner branches of `PlayerHealth`,
//! `KillCam` and the `GameManager` / `PlayerManager` round flow; FishNet RPCs become local calls).
//!
//! C# flow followed:
//!  1. A damage source kills the owner: `ChangeKilledState(true)` + `RemoveHealth(dmg)` (-> `HitFeedbackObservers` ->
//!     `CameraEffect.TakeHit`, see health.rs `player_health`) and usually `Explode(false, true, name, ejectDir, force,
//!     position)`; a fall below y -300 is `FirstPersonController.DespawnObject` (health -8, isKilled, fellVoid).
//!     The port's kill representation is `health = -8`, the Explode args are `PlayerHealth.last_hit`.
//!  2. `PlayerHealth.Update` (owner, health <= 0 && isKilled): `KillCam()` (scene Main Camera to position + (0, CC height,
//!     0), `isDead`, camera + AudioListener enabled), `GameManager.PlayerDied` (round end), kill-feed text (suicide /
//!     void, logged), `shouldDropWeapon` (LeftHandDrop + RightHandDrop), `DespawnObject` -> `DespawnObjectObservers`
//!     (RunLocally): player GameObject deactivated + `Explode(false, false, "", -forward, 30, pos + up*2 + right)`, a
//!     no-op when the killer already called Explode (`spawnedRagdoll`).
//!  3. `ExplodeForAll` owner branch: death clip (`PauseManager.deathAudioClip` = the 3 selfbodyhit clips, 2D, pitch
//!     0.95-1.05), ragdoll `aboubiRagdoll` at the player pose with the suit material, `Hat()` pop, AddExplosionForce,
//!     `killCam.ragdoll` / `triggerLookAtBody`, `firstPosition = pos + killCamRagdollOffset(0,2,0) +
//!     flat(ejectDir) * directionOffset(6) + Vector3.right`, bloodFX on the Hips. Here the ragdoll is a [`Corpse`]
//!     (a dressed PlayerIK body run through ragdoll.rs exactly like the dummy's death). It is pre-built and hidden
//!     ([`health::SpareCorpse`], following the player) so it ragdolls on the death frame like Instantiate does; the
//!     popped hat starts from the living body's hat pose (`PlayerSetup.hat`), not the corpse's head.
//!     A death without `isKilled` (PhysicsProp impact) runs `SafeDeathFix`: DespawnObject (ragdoll, death clip,
//!     colliders off) 0.075 s later, no weapon drop on that branch; KillCam() takes the camera at once.
//!  4. `KillCam.Update`: once the ragdoll exists, `timer = 3`; while timer >= 0 the camera sits at firstPosition looking
//!     at the Hips every frame; a left click with 0 < timer < 2.7 skips to 0; F toggles a free camera (WASD/QE/Space/Ctrl,
//!     Shift x2 speed, mouse look). After the timer it spectates the alive "PlayerGraphics" objects = the training
//!     dummy here: DOMove(enemy + (0, 2.3, 0) + enemy.forward * 2.3 (one player), 1 s while timer > -1, else 0) and
//!     DOLookAt(enemy + (0, 1.7, 0), 0.4 / 0) created every frame (stacked DOTween tweens, newest wins), texts
//!     SwitchCamText / PlayerHpText / PlayerNameText (screen_ui.rs), H hides them, mouse buttons cycle players.
//!     With no alive player (`enemy` null) KillCam forces freeCam off and re-triggers the 3 s ragdoll look every
//!     frame: the camera stays on the corpse, the click skip and the free camera do nothing.
//!  5. Round end: `GameManager.WaitForDraw` waits up to 1 s for a draw, then `WaitForSeconds(2)` and
//!     `ProgressToNextTake` -> `ObserversRoundSpawn` (OnBeforeSpawn: grenades / rockets / mines despawn) -> a fresh
//!     player prefab at the spawn point. Test map: respawn [`RESPAWN_DELAY`] = 1 + 2 s after the death (the original
//!     exploration "testMap" respawns on the next frame, which would hide the kill cam entirely). The corpse stays,
//!     as `RagdollDress` only destroys ragdolls at round start with more than 2 players ([`MAX_CORPSES`] caps them).

use crate::cosmetics::DressState;
use crate::health::{self, Corpse, DummyGraphics, HitPart, PlayerHealth};
use crate::hud::{HudCamera, HudRespawn};
use crate::humanoid::HumanoidAnimator;
use crate::input::PlayerInput;
use crate::player::fpc::{self, Fpc, LandBobPivot, LeanCamera, PlayerCamera, PlayerCameraHolder};
use crate::ragdoll::{DespawnCorpse, HasRagdoll};
use crate::tween::Tweens;
use crate::unity::{to_unity_euler, unity_euler};
use crate::weapons::markers::WeaponCam;
use crate::weapons::pickup::Hands;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

/// Player spawn point of the test map (main.rs spawn).
pub const PLAYER_SPAWN: Vec3 = Vec3::new(0.0, 0.2, 0.0);
/// GameManager.WaitForDraw (1 s) + WaitForSeconds(2) before ProgressToNextTake.
pub const RESPAWN_DELAY: f32 = 3.0;
/// KillCam: `timer = 3f` when the ragdoll appears.
const KILLCAM_TIME: f32 = 3.0;
/// PlayerIK PlayerHealth.killCamRagdollOffset / directionOffset.
const KILLCAM_RAGDOLL_OFFSET: Vec3 = Vec3::new(0.0, 2.0, 0.0);
const DIRECTION_OFFSET: f32 = 6.0;
/// Corpses kept on the map (the original never removes them in 1-2 player games); older ones are despawned.
const MAX_CORPSES: usize = 4;

/// Marker: the local player got its hidden hit-collider body (health.rs `LocalBody`).
#[derive(Component)]
pub struct HasLocalBody;

#[derive(Resource, Default)]
pub struct LocalDeath {
    pub dead: bool,
    /// Seconds since the death.
    pub since: f32,
    pub corpse: Option<Entity>,
    pub cam_text: KillCamText,
    /// `SafeDeathFix` coroutine pending (death without isKilled).
    safe_death_fix: bool,
    serial: u32,
    cam: KillCam,
    input: KillCamInput,
}

/// `KillCam` component state (scene "Main Camera"; the port reuses the player's world camera, detached from the rig).
#[derive(Default)]
struct KillCam {
    /// Camera taken over (rig cameras off, world camera unparented).
    started: bool,
    /// `ragdoll` after `FindRecursive("Hips")`: the Hips ragdoll body.
    ragdoll: Option<Entity>,
    trigger_look_at_body: bool,
    first_position: Vec3,
    timer: f32,
    free_cam: bool,
    has_pressed_freecam: bool,
    hide_hud: bool,
    /// `enemy`: the spectated "PlayerGraphics" object (the training dummy's Graphics), None = null.
    enemy: Option<Entity>,
    spectate_player_id: usize,
    pos: Vec3,
    rot: Quat,
    /// DOMove / DOLookAt tweens on the Main Camera (one of each is created every frame while spectating; DOTween runs
    /// them in creation order, so the newest one writes last).
    moves: Vec<CamTween>,
    looks: Vec<CamTween>,
}

/// One DOTween tween on the camera transform (default ease OutQuad, DOTweenSettings defaultEaseType 6).
/// Move: `start` / `end` positions. LookAt (SpecialStartupMode.SetLookAt): `towards` is the look target, `start` /
/// `end` the Euler angles captured at startup (QuaternionPlugin RotateMode.Fast: shortest per-axis).
struct CamTween {
    start: Option<Vec3>,
    end: Vec3,
    towards: Vec3,
    duration: f32,
    elapsed: f32,
}

impl CamTween {
    fn new(target: Vec3, duration: f32) -> Self {
        CamTween { start: None, end: target, towards: target, duration, elapsed: 0.0 }
    }
    /// TweenManager.Update: elapsed += dt (also on the first update), startup on the first update, then ease.
    fn step(&mut self, dt: f32) -> f32 {
        self.elapsed += dt;
        if self.duration > 0.0 { crate::tween::Ease::OutQuad.eval((self.elapsed / self.duration).min(1.0)) } else { 1.0 }
    }
    fn done(&self) -> bool {
        self.elapsed >= self.duration
    }
}

/// KillCam texts (`---USER INTERFACE---/START ROUND TEXT` SwitchCamText / PlayerHpText / PlayerNameText), written
/// by screen_ui.rs. KillCam only writes them in some branches; otherwise they keep their value.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub enum KillCamText {
    #[default]
    Empty,
    FreeCam,
    /// Spectating: `Health : ceil(h / 4 * 100)` of the enemy + its player name.
    Spectate { health: f32 },
}

/// The KillCam's own input actions (Move, Run, Interact, MouseX/Y); the player's are disabled while dead.
#[derive(Default, Clone, Copy)]
struct KillCamInput {
    move_raw: Vec2,
    /// PauseManager.gamepad: look values x14.
    gamepad: bool,
    mouse: Vec2,
    run: bool,
    interact: bool,
    enabled: bool,
}

/// Gives the local player its SelfBody hit colliders.
fn attach_local_body(mut commands: Commands, assets: Res<AssetServer>, players: Query<Entity, (With<Fpc>, Without<HasLocalBody>)>) {
    for p in &players {
        health::spawn_local_body(&mut commands, &assets, p);
        commands.entity(p).insert(HasLocalBody);
    }
}

/// `SafeDeathFix`: `WaitForSeconds(0.075f)` before DespawnObject when the death did not set `isKilled`.
const SAFE_DEATH_FIX_DELAY: f32 = 0.075;

/// Keeps one hidden, fully set-up corpse body ready ([`health::SpareCorpse`]), so the ragdoll appears on the death
/// frame like `Instantiate(aboubiRagdoll)` (the PlayerIK scene, rig and dress need a few frames).
fn ensure_spare_corpse(mut commands: Commands, assets: Res<AssetServer>, spares: Query<(), With<health::SpareCorpse>>, players: Query<&Transform, With<Fpc>>) {
    if spares.is_empty() {
        let tf = players.single().copied().unwrap_or_default();
        health::spawn_spare_corpse(&mut commands, &assets, Transform::from_translation(tf.translation).with_rotation(tf.rotation));
    }
}

/// The spare corpse sits at the player's pose (after the character-controller write-back), so its rig's
/// GlobalTransforms already are the death pose when it is used (ragdoll.rs reads them on that same frame).
fn follow_spare_corpse(players: Query<&Transform, (With<Fpc>, Without<health::SpareCorpse>)>, mut spares: Query<&mut Transform, With<health::SpareCorpse>>) {
    let Ok(p) = players.single() else { return };
    for mut t in &mut spares {
        if t.translation != p.translation || t.rotation != p.rotation {
            *t = Transform::from_translation(p.translation).with_rotation(p.rotation);
        }
    }
}

type SpareQuery<'w, 's> = Query<'w, 's, (Entity, Has<HumanoidAnimator>, Option<&'static DressState>, Option<&'static DummyGraphics>), With<health::SpareCorpse>>;

/// PlayerHealth.Update (owner): death detection, KillCam(), DespawnObject (now, or after SafeDeathFix's 0.075 s).
#[allow(clippy::too_many_arguments)]
fn local_death(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<AssetServer>,
    settings: Res<crate::settings::Settings>,
    mut st: ResMut<LocalDeath>,
    mut players: Query<(Entity, &mut PlayerHealth, &Fpc, &Transform, Option<&mut Hands>), Without<Corpse>>,
    parts: Query<(Entity, &HitPart)>,
    corpses: Query<(Entity, &Corpse), Without<DespawnCorpse>>,
    spares: SpareQuery,
    local_dress: Query<&DressState, With<health::LocalBody>>,
    gts: Query<&GlobalTransform>,
    frame: Res<bevy::diagnostic::FrameCount>,
) {
    let Ok((pe, mut ph, fpc, tf, hands)) = players.single_mut() else { return };
    let despawn = |st: &mut LocalDeath, ph: &mut PlayerHealth, commands: &mut Commands| {
        despawn_object(commands, &assets, &settings, st, pe, ph, tf, &parts, &corpses, &spares, &local_dress, &gts)
    };
    if st.dead {
        st.since += time.delta_secs();
        // SafeDeathFix: the coroutine resumes on the first frame 0.075 s after it started.
        if st.safe_death_fix && st.since >= SAFE_DEATH_FIX_DELAY {
            st.safe_death_fix = false;
            println!("SAFEDEATHFIX despawn t={:.3} after {:.3} s", time.elapsed_secs(), st.since);
            despawn(&mut *st, &mut *ph, &mut commands);
        }
        return;
    }
    // FirstPersonController.Update: y < -300 -> fellVoid + DespawnObject (health -8, isKilled, ExplodeOnDeath).
    if fpc.safe_death_falling && ph.health > 0.0 {
        ph.health = -8.0;
        ph.fell_void = true;
        ph.killed = true;
    }
    if ph.health > 0.0 {
        return;
    }
    st.dead = true;
    st.since = 0.0;
    let pos = tf.translation;
    let feed = if ph.fell_void { "fell into the void" } else if ph.suicide { "commited suicide" } else { "died" };
    info!("DEATH local player {feed} at {pos:?} health={} killed={} last_hit={:?}", ph.health, ph.killed, ph.last_hit);
    println!("DEATH t={:.2} frame={} {feed} health={} killed={} pos=({:.2},{:.2},{:.2})", time.elapsed_secs(), frame.0, ph.health, ph.killed as u8, pos.x, pos.y, pos.z);
    // KillCam(): mainCamObject.position = position + (0, CharacterController.height, 0).
    st.cam = KillCam { pos: pos + Vec3::Y * fpc.cc_height, ..default() };
    if ph.killed {
        // health <= 0 && isKilled: shouldDropWeapon -> LeftHandDrop(); RightHandDrop() (pickup.rs, this frame), DespawnObject.
        if let Some(mut h) = hands {
            h.drop_all = true;
        }
        despawn(&mut *st, &mut *ph, &mut commands);
    } else {
        // health <= 0 without isKilled (PhysicsProp impact: RemoveHealth only): StartCoroutine(SafeDeathFix()); the
        // player object stays active until it ends (no shouldDropWeapon on this branch).
        st.safe_death_fix = true;
        println!("SAFEDEATHFIX start t={:.3}", time.elapsed_secs());
    }
}

/// `DespawnObject` -> `DespawnObjectObservers` (player object off + `Explode(false, false, "", -forward, 30, pos + up*2 +
/// right)`, a no-op after the killer's own Explode) -> `ExplodeForAll` owner branch.
#[allow(clippy::too_many_arguments)]
fn despawn_object(
    commands: &mut Commands,
    assets: &AssetServer,
    settings: &crate::settings::Settings,
    st: &mut LocalDeath,
    pe: Entity,
    ph: &mut PlayerHealth,
    tf: &Transform,
    parts: &Query<(Entity, &HitPart)>,
    corpses: &Query<(Entity, &Corpse), Without<DespawnCorpse>>,
    spares: &SpareQuery,
    local_dress: &Query<&DressState, With<health::LocalBody>>,
    gts: &Query<&GlobalTransform>,
) {
    ph.dead = true;
    let pos = tf.translation;
    // ExplodeForAll args: the killer's Explode, else DespawnObjectObservers' Explode(-forward, 30, pos + up*2 + right).
    let hit = ph.last_hit.take().unwrap_or((-tf.forward().as_vec3(), 30.0, pos + Vec3::Y * 2.0 + Vec3::X));
    info!("DESPAWN local player eject_dir={:?} force={} position={:?}", hit.0, hit.1, hit.2);
    // The player GameObject is deactivated: its colliders (SelfBody hit colliders) are off.
    for (e, p) in parts {
        if p.owner == pe {
            commands.entity(e).insert(ColliderDisabled);
        }
    }
    // ExplodeForAll (IsOwner): IncreaseDeathsAmount (stat, not ported) + PlaySoundWithPitch(deathAudioClip[..], 0.95-1.05).
    health::play_self_body_hit(commands, assets, settings);
    // Instantiate(aboubiRagdoll, position, rotation) + dress + Hat + AddExplosionForce + bloodFX (ragdoll.rs, this frame).
    st.serial += 1;
    let mut old: Vec<(u32, Entity)> = corpses.iter().map(|(e, c)| (c.serial, e)).collect();
    old.sort_by_key(|o| o.0);
    while old.len() + 1 > MAX_CORPSES {
        let (_, e) = old.remove(0);
        commands.entity(e).insert(DespawnCorpse);
    }
    // Hat(PlayerSetup.hat, ..): the living player's hat, at its current pose.
    let hat_from = local_dress.iter().next().and_then(|d| d.hat).and_then(|h| gts.get(h).ok()).copied();
    // The pre-built spare (ensure_spare_corpse makes the next one); ready = rigged + dressed, as Instantiate gives.
    let (corpse, ready, graphics) = match spares.iter().next() {
        Some((e, rigged, dress, g)) => (e, rigged && dress.is_some_and(|d| d.applied().is_some()) && g.is_some(), g.map(|g| g.0)),
        None => (health::spawn_spare_corpse(commands, assets, *tf), false, None),
    };
    let ph_corpse = PlayerHealth { health: -8.0, dead: ready, last_hit: if ready { Some(hit) } else { None }, ..default() };
    commands.entity(corpse).remove::<health::SpareCorpse>().insert((
        health::Dummy,
        Corpse { pending: if ready { None } else { Some(hit) }, ready_frames: 0, serial: st.serial, hat_from },
        ph_corpse,
        Transform::from_translation(pos).with_rotation(tf.rotation),
        Visibility::Inherited,
    ));
    if let Some(g) = graphics.filter(|_| ready) {
        commands.entity(g).insert(Visibility::Inherited);
    }
    println!("CORPSE ready={} hat_from={:?}", ready as u8, hat_from.map(|g| g.translation()));
    st.corpse = Some(corpse);
    // ExplodeForAll: killCamScript.firstPosition = position + killCamRagdollOffset + flat(ejectDir) * directionOffset + right.
    let flat = Vec3::new(hit.0.x, 0.0, hit.0.z).normalize_or_zero();
    st.cam.first_position = pos + KILLCAM_RAGDOLL_OFFSET + flat * DIRECTION_OFFSET + Vec3::X;
}

/// Fallback when the spare corpse was not ready at the death: starts its ragdoll once rigged and dressed.
fn corpse_update(
    mut corpses: Query<(&mut Corpse, &mut PlayerHealth, Has<HumanoidAnimator>, Has<DressState>, Option<&DummyGraphics>)>,
    mut vis: Query<&mut Visibility>,
) {
    for (mut c, mut ph, rigged, dressed, graphics) in &mut corpses {
        if c.pending.is_none() || !rigged || !dressed || graphics.is_none() {
            continue;
        }
        // One frame after rig + dress so the hat has a GlobalTransform (the ragdoll itself starts from the prefab's
        // rest T-pose, ragdoll.rs).
        c.ready_frames += 1;
        if c.ready_frames < 2 {
            continue;
        }
        ph.dead = true;
        ph.last_hit = c.pending.take();
        if let Some(g) = graphics {
            if let Ok(mut v) = vis.get_mut(g.0) {
                *v = Visibility::Inherited;
            }
        }
        info!("corpse ragdoll start");
    }
}

/// The player's input actions are disabled with the player object; KillCam reads its own.
fn gate_input(mut input: ResMut<PlayerInput>, mut st: ResMut<LocalDeath>) {
    if !st.dead {
        return;
    }
    st.input = KillCamInput { move_raw: input.move_raw, mouse: input.mouse, run: input.run.held, interact: input.interact.pressed, enabled: input.enabled, gamepad: input.gamepad };
    // PauseManager.gamepad is not a player action: it persists.
    *input = PlayerInput { enabled: input.enabled, gamepad: input.gamepad, ..default() };
}

/// KillCam.Update for the owner (see the module doc). Runs after the camera rig / tweens, before propagation.
#[allow(clippy::too_many_arguments)]
fn kill_cam(
    mut commands: Commands,
    time: Res<Time>,
    settings: Res<crate::settings::Settings>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    menu: Res<crate::settings_menu::MenuOpen>,
    mut st: ResMut<LocalDeath>,
    rags: Query<&HasRagdoll>,
    gts: Query<&GlobalTransform>,
    dummies: Query<(&PlayerHealth, &DummyGraphics), (With<health::Dummy>, Without<Corpse>)>,
    mut cams: Query<(Entity, &mut Transform, &mut Projection, Option<&mut Tweens>), With<PlayerCamera>>,
    mut rig_cams: Query<&mut Camera, Or<(With<WeaponCam>, With<HudCamera>)>>,
) {
    if !st.dead {
        return;
    }
    let Ok((cam_e, mut tf, mut proj, tw)) = cams.single_mut() else { return };
    let dt = time.delta_secs();
    let st = &mut *st;
    let kc = &mut st.cam;
    if !kc.started {
        kc.started = true;
        // The player's cameras go off with the player object; the scene Main Camera (KillCam) renders from now on.
        kc.rot = gts.get(cam_e).map(|g| g.compute_transform().rotation).unwrap_or(tf.rotation);
        commands.entity(cam_e).remove::<ChildOf>();
        for mut c in &mut rig_cams {
            c.is_active = false;
        }
    }
    if let Some(mut tw) = tw {
        tw.kill(); // LocalScreenshake & co. act on the (inactive) player camera, not on the Main Camera
    }
    let ki = st.input;
    let paused = menu.0;
    // KillCam.Update
    let pad = if ki.gamepad { 14.0 } else { 1.0 };
    let sens = |v: f32, s: f32, inv: bool| v * s * 0.1 * if inv { -1.0 } else { 1.0 } * pad;
    let (sx, sy) = if settings.toggle_two_axis { (settings.horizontal_sensitivity, settings.vertical_sensitivity) } else { (settings.mouse_sensitivity, settings.mouse_sensitivity) };
    let num = if ki.enabled { sens(ki.mouse.x, sx, settings.invert_mouse_x) } else { 0.0 };
    let num2 = if ki.enabled { sens(ki.mouse.y, sy, settings.invert_mouse_y) } else { 0.0 };
    kc.timer -= dt;
    if kc.ragdoll.is_none() {
        // ExplodeForAll: killCamScript.ragdoll = ragdoll root; triggerLookAtBody = true.
        if let Some(h) = st.corpse.and_then(|c| rags.get(c).ok()).and_then(|r| r.hips) {
            kc.ragdoll = Some(h);
            kc.trigger_look_at_body = true;
        }
    }
    // FindGameObjectsWithTag("PlayerGraphics"): the active players' Graphics. The local player object is inactive while
    // dead, so in single player that is the training dummy while it is alive (a dead player's object is inactive).
    let players: Vec<Entity> = dummies.iter().filter(|(h, _)| !h.dead).map(|(_, g)| g.0).collect();
    let enemy_health = |e: Entity| dummies.iter().find(|(_, g)| g.0 == e).map(|(h, _)| h.health).unwrap_or(0.0);
    // PlayerHealth.KillCam(): enemy = killer (own root for a suicide, inactive -> re-found below; null for the void).
    match kc.enemy {
        None => {
            kc.free_cam = false;
            st.cam_text = KillCamText::Empty;
            kc.trigger_look_at_body = true;
        }
        Some(e) if !players.contains(&e) => kc.enemy = players.first().copied(),
        _ => {}
    }
    if kc.free_cam && !paused {
        kc.timer = -1.0;
        let (x, y) = (ki.move_raw.x, ki.move_raw.y);
        // Input.GetKey(Q / LeftControl) = -1, (E / Space) = +1 (read raw like the original).
        let mut num3 = 0.0;
        if keys.pressed(KeyCode::KeyQ) || keys.pressed(KeyCode::ControlLeft) {
            num3 = -1.0;
        }
        if keys.pressed(KeyCode::KeyE) || keys.pressed(KeyCode::Space) {
            num3 = 1.0;
        }
        let speed = if ki.run { 30.0 } else { 15.0 };
        kc.rot = unity_euler(0.0, num, 0.0) * kc.rot; // RotateAround(position, Vector3.up, num)
        kc.rot *= unity_euler(-num2, 0.0, 0.0); // RotateAround(position, transform.right, -num2)
        let r = Transform::from_rotation(kc.rot);
        kc.pos += r.right().as_vec3() * x * speed * dt;
        kc.pos += r.up().as_vec3() * num3 * speed * dt;
        kc.pos += r.forward().as_vec3() * y * speed * dt;
    }
    // Ends this Update early (C# NullReferenceException on a null enemy / an empty player list).
    let mut aborted = false;
    if let Some(hips) = kc.ragdoll {
        if ki.interact && !kc.free_cam {
            kc.free_cam = true;
            kc.moves.clear(); // transform.DOKill()
            kc.looks.clear();
            kc.has_pressed_freecam = true;
        }
        if kc.trigger_look_at_body && !kc.free_cam {
            if let Some(&p) = players.first() {
                kc.enemy = Some(p);
            }
            kc.timer = KILLCAM_TIME;
            // ragdoll = FindRecursive("Hips", ragdoll): already the Hips (it finds itself on later triggers).
            kc.trigger_look_at_body = false;
        }
        if kc.timer < 0.0 {
            if keys.just_pressed(KeyCode::KeyH) {
                kc.hide_hud = !kc.hide_hud;
            }
            // isDead (set by PlayerHealth.Update on death).
            if kc.hide_hud {
                st.cam_text = KillCamText::Empty;
            } else if kc.free_cam {
                st.cam_text = KillCamText::FreeCam;
            } else if let Some(e) = kc.enemy {
                st.cam_text = KillCamText::Spectate { health: enemy_health(e) };
            } else {
                aborted = true;
            }
            let count = players.len();
            if !aborted {
                let next = |id: usize| if count == 0 || id + 1 >= count { 0 } else { id + 1 };
                let prev = |id: usize| if id == 0 { count.saturating_sub(1) } else { id - 1 };
                if ki.interact && kc.free_cam && !kc.has_pressed_freecam {
                    kc.free_cam = false;
                    kc.spectate_player_id = next(kc.spectate_player_id);
                    kc.enemy = players.get(kc.spectate_player_id).copied();
                }
                if mouse.just_pressed(MouseButton::Left) {
                    kc.free_cam = false;
                    kc.spectate_player_id = next(kc.spectate_player_id);
                    kc.enemy = players.get(kc.spectate_player_id).copied();
                }
                if mouse.just_pressed(MouseButton::Right) {
                    kc.free_cam = false;
                    kc.spectate_player_id = prev(kc.spectate_player_id);
                    kc.enemy = players.get(kc.spectate_player_id).copied();
                }
                if !kc.free_cam {
                    if let Some(g) = kc.enemy.and_then(|e| gts.get(e).ok()) {
                        // DOMove(enemy.position + (0, 2.3, 0) - enemy.forward * (alive > 1 ? 1 : -1) * 2.3, timer > -1 ? 1 : 0)
                        // DOLookAt(enemy.position + (0, 1.7, 0), timer > -1 ? 0.4 : 0)
                        let side = if count > 1 { 1.0 } else { -1.0 };
                        let quick = kc.timer > -1.0;
                        // Graphics sits in the glb frame (rotated 180 deg about Y under the body root, health.rs), so
                        // Unity's Graphics.forward is the node's Bevy back().
                        let fwd = g.back().as_vec3();
                        kc.moves.push(CamTween::new(g.translation() + Vec3::Y * 2.3 - fwd * side * 2.3, if quick { 1.0 } else { 0.0 }));
                        kc.looks.push(CamTween::new(g.translation() + Vec3::Y * 1.7, if quick { 0.4 } else { 0.0 }));
                    } else {
                        aborted = true; // players[id] out of range / null enemy
                    }
                }
            }
        } else if !kc.free_cam {
            // transform.DOLookAt(ragdoll.position, 0); position = firstPosition; SetParent(null)
            if let Ok(g) = gts.get(hips) {
                kc.looks.push(CamTween::new(g.translation(), 0.0));
            }
            kc.pos = kc.first_position;
        }
    }
    if !aborted {
        if mouse.just_pressed(MouseButton::Left) && kc.timer > 0.0 && kc.timer < 2.7 {
            kc.timer = 0.0;
        }
        kc.has_pressed_freecam = false;
    }
    // DOTween update (after the MonoBehaviour Updates): moves, then the look-ats, which capture the position at startup.
    for m in kc.moves.iter_mut() {
        let k = m.step(dt);
        let s = *m.start.get_or_insert(kc.pos);
        kc.pos = s.lerp(m.end, k);
    }
    kc.moves.retain(|m| !m.done());
    for l in kc.looks.iter_mut() {
        let k = l.step(dt);
        if l.start.is_none() {
            l.start = Some(to_unity_euler(kc.rot));
            let d = l.towards - kc.pos;
            let look = if d.length_squared() > 1e-12 { Transform::IDENTITY.looking_to(d, Vec3::Y).rotation } else { Quat::IDENTITY };
            l.end = to_unity_euler(look);
        }
        let e = crate::tween::lerp_euler(l.start.unwrap(), l.end, k);
        kc.rot = unity_euler(e.x, e.y, e.z);
    }
    kc.looks.retain(|l| !l.done());
    if std::env::var_os("RESTRAFE_KILLCAM_LOG").is_some() {
        println!(
            "KILLCAM timer={:.2} free={} enemy={} text={:?} pos=({:.2},{:.2},{:.2}) fwd={:?} tweens={}/{}",
            kc.timer, kc.free_cam as u8, kc.enemy.is_some() as u8, st.cam_text, kc.pos.x, kc.pos.y, kc.pos.z,
            (kc.rot * Vec3::NEG_Z).to_array().map(|v| (v * 100.0).round() / 100.0), kc.moves.len(), kc.looks.len()
        );
    }
    *tf = Transform { translation: kc.pos, rotation: kc.rot, scale: Vec3::ONE };
    // KillCam.Awake: fieldOfView = Settings.normalFovValue.
    if let Projection::Perspective(p) = &mut *proj {
        p.fov = settings.normal_fov_value.to_radians();
    }
}

/// Round respawn: `PauseManager.InvokeBeforeSpawn` (grenades, rockets, mines, bubbles despawn) + `PlayerManager.
/// CmdNewRoundSpawn` (old player despawned, a fresh PlayerIK at the spawn point). The port resets the existing player.
#[allow(clippy::too_many_arguments)]
fn respawn(
    mut commands: Commands,
    mut st: ResMut<LocalDeath>,
    mut players: Query<(Entity, &mut PlayerHealth, &mut Fpc, &mut Transform, &mut KinematicCharacterController, Option<&mut Hands>), Without<Corpse>>,
    parts: Query<(Entity, &HitPart)>,
    mut rig: ParamSet<(
        Query<&mut Transform, (With<LeanCamera>, Without<Fpc>)>,
        Query<(&mut Transform, &mut Tweens), (With<LandBobPivot>, Without<Fpc>)>,
        Query<(Entity, &mut Transform), (With<PlayerCameraHolder>, Without<Fpc>)>,
        Query<(Entity, &mut Transform, Option<&mut Tweens>), (With<PlayerCamera>, Without<Fpc>)>,
    )>,
    mut rig_cams: Query<&mut Camera, Or<(With<WeaponCam>, With<HudCamera>)>>,
    round_objects: Query<Entity, Or<(With<crate::weapons::launcher::Thrown>, With<crate::weapons::launcher::Shot>)>>,
    mines: Query<(Entity, &crate::weapons::mines::Mine)>,
    mut hud: MessageWriter<HudRespawn>,
    map_spawn: Option<Res<crate::scene_map::MapSpawn>>,
    map_points: Option<Res<crate::scene_map::MapSpawnPoints>>,
) {
    if !st.dead || st.since < RESPAWN_DELAY {
        return;
    }
    let Ok((pe, mut ph, mut fpc, mut tf, mut kcc, hands)) = players.single_mut() else { return };
    // OnBeforeSpawn -> PhysicsGrenade / Obus / Bubble / PredictedProjectile / ProximityMine / Claymore.StartNewRound: Despawn.
    for e in &round_objects {
        commands.entity(e).try_despawn();
    }
    for (e, m) in &mines {
        if let Some(l) = m.line_entity() {
            commands.entity(l).try_despawn();
        }
        commands.entity(e).try_despawn();
    }
    // Fresh player prefab: PlayerHealth (health 4), FirstPersonController, CharacterController at the spawn point.
    *ph = PlayerHealth::default();
    *fpc = Fpc::default();
    *kcc = fpc::kcc(&fpc);
    // The loaded map's SpawnPoint (exploration), else the test area spawn.
    // TryRespawn -> CmdRespawn -> ReturnSpawnPoint: Shuffle(CurrentSpawnPoints) then the first one.
    let shuffled = map_points.filter(|m| !m.points.is_empty()).map(|m| m.points[crate::unity::random_range_i(0, m.points.len() as i32) as usize]);
    let spawn_tf = shuffled.or(map_spawn.and_then(|m| m.0)).filter(|_| crate::scene_map::spawn_in_map()).unwrap_or(Transform::from_translation(PLAYER_SPAWN).with_rotation(unity_euler(0.0, 0.0, 0.0)));
    *tf = Transform::from_translation(spawn_tf.translation).with_rotation(spawn_tf.rotation);
    if let Some(mut h) = hands {
        h.drop_all = false;
        h.current_interactable = None;
        h.interaction_sphere = false;
    }
    for (e, p) in &parts {
        if p.owner == pe {
            commands.entity(e).remove::<ColliderDisabled>();
        }
    }
    // Camera rig back to the prefab pose (spawn_player values); the world camera back under CameraHolder.
    for mut t in &mut rig.p0() {
        *t = Transform::from_xyz(0.0, -0.6, 0.0);
    }
    for (mut t, mut tw) in &mut rig.p1() {
        *t = Transform::from_xyz(0.0, 0.1, 0.0);
        *tw = Tweens::default();
    }
    let mut holder = None;
    for (e, mut t) in &mut rig.p2() {
        *t = Transform::from_xyz(0.0, fpc::CAMERA_HOLDER_Y, 0.0);
        holder = Some(e);
    }
    for (e, mut t, tw) in &mut rig.p3() {
        *t = Transform::default();
        if let Some(mut tw) = tw {
            *tw = Tweens::default();
        }
        if let Some(h) = holder {
            commands.entity(e).insert(ChildOf(h));
        }
    }
    for mut c in &mut rig_cams {
        c.is_active = true;
    }
    hud.write(HudRespawn);
    println!("RESPAWN after {:.2} s at {:?}", st.since, spawn_tf.translation);
    info!("local player respawned");
    let serial = st.serial;
    *st = LocalDeath { serial, ..default() };
}

pub struct DeathPlugin;

impl Plugin for DeathPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LocalDeath>()
            .add_systems(Update, (attach_local_body, ensure_spare_corpse))
            .add_systems(
                Update,
                (respawn, local_death, gate_input)
                    .chain()
                    .after(crate::input::gather_input)
                    .after(health::player_health)
                    .before(fpc::fpc_update)
                    .before(crate::ragdoll::RagdollSpawn),
            )
            .add_systems(PostUpdate, follow_spare_corpse.after(PhysicsSet::Writeback).before(TransformSystems::Propagate))
            .add_systems(Update, corpse_update.before(health::dummy_update))
            .add_systems(
                PostUpdate,
                kill_cam
                    .after(crate::tween::update_tweens)
                    .after(fpc::camera_post_update)
                    .before(TransformSystems::Propagate)
                    .before(PhysicsSet::SyncBackend),
            );
    }
}
