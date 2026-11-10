//! Weapons: data (prefab JSON), holding/switching (PlayerPickup), item behaviour, and the weapon
//! classes. Networking is removed: [ServerRpc]/[ObserversRpc] bodies run locally.

pub mod data;
pub mod beam;
pub mod casing;
pub mod charge;
pub mod flashlight;
pub mod gun;
pub mod large_raycast;
pub mod launcher;
pub mod markers;
pub mod anim;
pub mod melee;
pub mod mines;
pub mod minigun;
pub mod pickup;
pub mod propeller;
pub mod shotgun;
pub mod spin;
pub mod taser;
pub mod attached_fx;

use crate::player::fpc::{Fpc, PlayerCamera};
use crate::tween::Tweens;
use bevy::prelude::*;
use data::{UField, WeaponDb, WeaponDef};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Hand {
    #[default]
    None,
    Right,
    Left,
}

/// A weapon instance (ItemBehaviour + Weapon base state). Class-specific state lives in
/// separate components (e.g. [`gun::GunState`]).
#[derive(Component)]
pub struct Item {
    pub def: Arc<WeaponDef>,
    /// Pending Animator triggers ("Shoot" from WeaponAnimation if supplementAnimator, "Reload" from Reload()),
    /// consumed by `anim::update`.
    pub anim_triggers: Vec<&'static str>,
    /// The weapon's `audio` AudioSource pitch (changePitchOnShoot re-rolls it 0.97..1.03 on each shot attempt).
    pub audio_pitch: f32,
    /// Linear gain of the `audio` AudioSource's output mixer (Master group snapshot volume): Crisis.mixer -5.1 dB,
    /// Mac10.mixer -1.5 dB, Weapons.mixer 0 dB.
    pub audio_gain: f32,
    pub current_ammo: i32,
    pub hand: Hand,
    /// Was ever held (Weapon.heldOnce).
    pub held_once: bool,
    pub is_clicked: bool,
    pub shot: bool,
    pub no_ammo_clicks: i32,
    pub charged_bullets: f32,
    pub is_reloading: bool,
    pub cant_take_safe: bool,
    pub invert_fire: bool,
    pub case_sound_timer: f32,
    /// Has a dynamic rigidbody (dropped/thrown).
    pub physics: bool,
    pub dispenser_start: bool,
    /// Pending Invoke("DespawnObject", 0.65f).
    pub despawn_in: Option<f32>,
    /// Ground bob tween (DOLocalMove up parent.up/2, 1.4 s InOutSine yoyo).
    pub bob_origin: Option<Vec3>,
    pub bob_t: f32,
    /// World positions of the weapon's ShootPoint / FlashPoint / CaseEjectPoint child nodes (see `markers.rs`).
    pub shoot_pos: Option<Vec3>,
    pub flash_pos: Option<Vec3>,
    /// World rotation of the ShootPoint node (muzzle flash orientation).
    pub shoot_rot: Option<Quat>,
    pub eject_pos: Option<Vec3>,
    /// Pending forearm (ElbowPivotPoint) tween requested by `Weapon.WeaponAnimation`.
    pub elbow_req: Option<markers::ElbowReq>,
    /// Pending `networkAudioSource.Play()` (Some(true)) / `.Stop()` (Some(false)) on the prefab's first AudioSource (see audio.rs).
    pub src_audio: Option<bool>,
    /// World direction `-ejectCasePoint.right` (cached with `eject_pos`) and a pending Weapon.OnShoot / OnReload casing request.
    pub eject_dir: Option<Vec3>,
    pub eject_req: Option<casing::EjectReq>,
}

impl Item {
    pub fn new(def: Arc<WeaponDef>) -> Item {
        let ammo = def.w.i("currentAmmo");
        let audio_gain = def
            .prefab
            .objects
            .first()
            .and_then(|o| o.comp("AudioSource"))
            .and_then(|a| a.asset("OutputAudioMixerGroup"))
            .map(|m| crate::scene_map::mixer_gain(&m))
            .unwrap_or(1.0);
        Item {
            def,
            anim_triggers: Vec::new(),
            audio_pitch: 1.0,
            audio_gain,
            current_ammo: ammo,
            hand: Hand::None,
            held_once: false,
            is_clicked: false,
            shot: false,
            no_ammo_clicks: 0,
            charged_bullets: 0.0,
            is_reloading: false,
            cant_take_safe: false,
            invert_fire: false,
            case_sound_timer: 0.0,
            physics: false,
            dispenser_start: false,
            despawn_in: None,
            bob_origin: None,
            bob_t: 0.0,
            shoot_pos: None,
            flash_pos: None,
            shoot_rot: None,
            eject_pos: None,
            elbow_req: None,
            src_audio: None,
            eject_dir: None,
            eject_req: None,
        }
    }
    pub fn in_right(&self) -> bool {
        self.hand == Hand::Right
    }
    pub fn in_left(&self) -> bool {
        self.hand == Hand::Left
    }
}

/// End of `Weapon.Update` (layer 8 = held by the owner): `invertFire = hasObjectInHand && hasObjectInLeftHand &&
/// Settings.inverseFireBinding`. Dropped weapons keep their last value (only held weapons read it).
fn invert_fire_update(settings: Res<crate::settings::Settings>, hands: Query<&pickup::Hands>, mut items: Query<&mut Item>) {
    for h in &hands {
        let v = h.right.is_some() && h.left.is_some() && settings.inverse_fire_binding;
        for e in [h.right, h.left].into_iter().flatten() {
            if let Ok(mut it) = items.get_mut(e) {
                if it.invert_fire != v {
                    it.invert_fire = v;
                }
            }
        }
    }
}

/// Viewmodel parent (Camera/BobPosition/FPArms). Held weapons are its children.
#[derive(Component)]
pub struct FpArms;

/// Weapon.WeaponAnimation on `fpArms` (two-handed weapons), applied to the FPArms tweens by `fpbob`.
#[derive(Clone, Copy, Debug)]
pub enum FpArmsAnim {
    /// `fpArms.DOLocalRotate(fpArms.localEulerAngles - punch, duration)` (instantPush).
    RotateBy { punch: Vec3, duration: f32 },
    /// `fpArms.DOPunchRotation(-punch, duration, vibrato, elasticity)`.
    Punch { punch: Vec3, duration: f32, vibrato: i32, elasticity: f32 },
}
#[derive(Component)]
pub struct BobPosition;

/// Sound played by a weapon (AudioSource.PlayOneShot on the weapon). `clip` is an asset path
/// relative to the ripped Assets folder, e.g. "AudioClip/R_Glock.ogg".
#[derive(Message, Clone, Debug)]
pub struct WeaponSound {
    pub clip: String,
    pub volume: f32,
    /// None = 2D (owner), Some = 3D at position.
    pub at: Option<Vec3>,
    /// AudioSource.pitch (1 = normal; Bevy playback speed).
    pub pitch: f32,
    /// 3D rolloff: 0 = Logarithmic (min distance 1); > 0 = Linear from 1 m to this max distance (rolloffMode 1);
    /// < 0 = Logarithmic with min distance -linear_max.
    pub linear_max: f32,
    /// The weapon entity whose own `audio` AudioSource plays this clip (PlayOneShot), so that
    /// `AudioSource.Stop()` on that source ([`WeaponSoundStop`]) also cuts it. None = untracked.
    pub source: Option<Entity>,
    /// AudioSource custom rolloff curve (rolloffMode 2); overrides `linear_max` when set.
    pub rolloff: Option<std::sync::Arc<crate::audio::RolloffCurve>>,
}

/// `AudioSource.Stop()` on a weapon's `audio` source: stops every one-shot still playing on it.
#[derive(Message, Clone, Debug)]
pub struct WeaponSoundStop {
    pub source: Entity,
}

/// UI-facing messages from weapons (PauseManager.ChangeAmmoText, hit markers...).
#[derive(Message, Clone, Debug)]
pub enum WeaponUi {
    Ammo { right: bool, text: String, charged: String },
    /// `source`: the weapon item / projectile / mine instance whose own `marker` this replaces (Gun.cs:270-292).
    HitMarker { head: bool, source: Option<Entity> },
}

pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, melee::melee_trails.after(melee::melee_update));
        app.insert_resource(WeaponDb::load("assets/data/weapons"))
            .insert_resource(data::HandAnchors::load("assets/data/player/PlayerIK.json"))
            .add_message::<WeaponSound>()
            .add_message::<WeaponSoundStop>()
            .add_message::<WeaponUi>()
            .add_plugins(casing::CasingPlugin)
            .init_resource::<mines::PendingMineShots>()
            .add_systems(Update, (anim::attach, anim::update.after(mines::placed_mines_update).after(markers::aim_strafe_update), anim::node_animator_update))
            .add_systems(Update, pickup::reattach_colliders.before(pickup::player_pickup_update))
            .add_systems(Update, pickup::item_impact_sound)
            .add_systems(Update, invert_fire_update.after(pickup::item_behaviour_update).before(gun::gun_update))
            .add_systems(
                Update,
                (
                    pickup::player_pickup_update,
                    pickup::item_behaviour_update,
                    gun::gun_update,
                    shotgun::shotgun_update,
                    large_raycast::large_raycast_update,
                    melee::melee_update,
                    launcher::launcher_update,
                    launcher::shot_update,
                    launcher::thrown_update,
                    launcher::projectile_visuals,
                    launcher::trail_follow,
                    launcher::launcher_fx_update,
                    minigun::minigun_update,
                    charge::charge_update,
                    beam::beam_update,
                    taser::taser_update,
                    propeller::propeller_update,
                    flashlight::flashlight_update,
                    mines::mines_update,
                    mines::placed_mines_update,
                )
                    .chain()
                    .after(crate::player::fpc::fpc_update),
            )
            .init_resource::<launcher::LauncherAssets>()
            .add_plugins(markers::MarkersPlugin)
            .add_plugins(attached_fx::AttachedFxPlugin)
            .add_systems(PostUpdate, spin::spin_parts.before(TransformSystems::Propagate))
            .add_systems(Update, (launcher::pin_release, launcher::pin_fall));
    }
}

/// Spawns a weapon lying in the world (as from ItemSpawner), not held.
/// The weapon root's BoxCollider (center/size in the prefab's local space).
pub fn item_collider(def: &WeaponDef) -> bevy_rapier3d::prelude::Collider {
    let (half, center) = def
        .prefab
        .objects
        .first()
        .and_then(|o| o.comp("BoxCollider"))
        .map(|b| (b.v3("m_Size") * 0.5, data::u_pos(b.get("m_Center").unwrap_or(&serde_json::Value::Null))))
        .unwrap_or((Vec3::new(0.05, 0.1, 0.25), Vec3::ZERO));
    let half = half.abs().max(Vec3::splat(0.01));
    bevy_rapier3d::prelude::Collider::compound(vec![(center, Quat::IDENTITY, bevy_rapier3d::prelude::Collider::cuboid(half.x, half.y, half.z))])
}

pub fn spawn_world_weapon(commands: &mut Commands, def: Arc<WeaponDef>, pos: Vec3, assets: &AssetServer) -> Entity {
    let mut e = commands.spawn((
        Name::new(def.name.clone()),
        Transform::from_translation(pos).with_scale(Vec3::splat(2.0)),
        Visibility::default(),
        Tweens::default(),
        crate::physics::UnityLayer(crate::physics::layer::INTERACTABLE),
        item_collider(&def),
    ));
    // Model: the prefab exported as glTF by AssetRipper. AssetRipper mirrors X while this port
    // mirrors Z, so the scene is turned 180 degrees about Y to line up with prefab-space data.
    let scene = assets.load(GltfAssetLabel::Scene(0).from_asset(format!("models/weapons/{}.glb", def.name)));
    e.with_children(|p| {
        p.spawn((WeaponModel, WorldAssetRoot(scene), crate::unity_pbr::PrefabRender(format!("assets/data/weapons/{}.json", def.name)), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI))));
    });
    let mut item = Item::new(def.clone());
    item.bob_origin = Some(pos);
    e.insert(item);
    if gun::is_gun_class(&def.class) {
        e.insert(gun::GunState::default());
    }
    if shotgun::is_shotgun(&def.class) {
        e.insert(shotgun::ShotgunState::default());
    }
    if large_raycast::is_large_raycast(&def.class) {
        e.insert(large_raycast::LargeRaycastState::default());
    }
    if melee::is_melee(&def.class) {
        e.insert(melee::MeleeState::default());
    }
    if launcher::is_launcher(&def.class) {
        e.insert(launcher::LauncherState::new(&def));
    }
    if minigun::is_minigun(&def.class) {
        e.insert(minigun::MinigunState::default());
    }
    if charge::is_charge_gun(&def.class) {
        e.insert(charge::ChargeState::default());
    }
    if beam::is_beam_gun(&def.class) {
        e.insert(beam::BeamState::default());
    }
    if taser::is_taser(&def.class) {
        e.insert(taser::TaserState::default());
    }
    if propeller::is_propeller(&def.class) {
        e.insert(propeller::PropellerState::default());
    }
    if flashlight::is_flashlight(&def.class) {
        e.insert(flashlight::FlashLightState::default());
    }
    if mines::is_mines(&def.class) {
        e.insert(mines::MinesState::default());
    }
    e.id()
}

/// Weapon.CameraAnimation / CameraRevolverAnimation on the player camera.
pub fn camera_recoil(def: &WeaponDef, fpc: &mut Fpc, cam_tf: &Transform, cam_tw: &mut Tweens, alt_mult: f32) {
    let w = &def.w;
    fpc.cam_base_speed = w.f("cameraLerpSpeed");
    let ease = crate::tween::Ease::from_i32(w.i("shakeEase"));
    let duration = w.f("duration");
    if w.b("revolverShake") {
        let recoil = w.v3("recoil") * alt_mult;
        if !w.b("akAnim") {
            // DOPunchRotation(...).SetEase(shakeEase): the ease is overridden by DOTween (SetPunch -> OutQuad).
            cam_tw.punch_rotation(-recoil, duration, w.i("vibrato"), w.f("elasticity"));
        } else {
            let e = crate::unity::to_unity_euler(cam_tf.rotation) - recoil;
            cam_tw.local_rotate(e, duration, ease);
        }
    } else {
        cam_tw.shake_rotation(duration, w.v3("strength"), w.i("vibrato"), w.f("randomness"), w.b("fadeOut"), w.i("randomnessMode") == 1);
    }
}

pub type CamQuery<'w, 's> = Query<'w, 's, (&'static Transform, &'static GlobalTransform, &'static mut Tweens), (With<PlayerCamera>, Without<Item>)>;

/// The glTF scene child of a weapon entity.
#[derive(Component)]
pub struct WeaponModel;
