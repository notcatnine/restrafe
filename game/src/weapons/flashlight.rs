//! Port of `FlashLight.cs` (toggles a Spot Light). Networking removed: `Power` -> `PowerObservers` run directly.

use super::data::UField;
use super::{Hand, Item, WeaponSound};
use crate::input::PlayerInput;
use crate::player::fpc::Fpc;
use bevy::prelude::*;

pub fn is_flashlight(class: &str) -> bool {
    class == "FlashLight"
}

/// Prefab "Spot Light": local pos (0,-0.29,0.24) (Unity), spotAngle 77.4, range 30, intensity 3.7, white, soft shadows
/// (near plane 0.2), culling mask everything, cookie FlashlightCookie. In the built-in pipeline the cookie replaces the
/// spot falloff (innerSpotAngle is SRP-only) and spans +-tan(spotAngle / 2), as Bevy's SpotLightTexture does; inner =
/// outer leaves the cookie as the only falloff. Unity samples the cookie's alpha (linear); its R equals A up to DXT noise.
const LIGHT_INTENSITY: f32 = 3.7;
const COOKIE: &str = "textures/FlashlightCookie.png";

#[derive(Component, Default)]
pub struct FlashLightState {
    pub fire_timer: f32,
    /// `light.activeSelf` (prefab Spot Light starts active).
    pub light_on: bool,
    light: Option<Entity>,
}

#[allow(clippy::too_many_arguments)]
pub fn flashlight_update(
    mut commands: Commands,
    assets: Res<AssetServer>,
    time: Res<Time>,
    input: Res<PlayerInput>,
    players: Query<&Fpc>,
    mut lights: Query<&mut Visibility>,
    mut items: Query<(Entity, &mut Item, &mut FlashLightState)>,
    mut sounds: MessageWriter<WeaponSound>,
) {
    let dt = time.delta_secs();
    let Ok(fpc) = players.single() else { return };
    let fire1 = input.fire.value();
    let fire2 = input.fire2.value();
    for (entity, mut item, mut fs) in &mut items {
        let item = &mut *item;
        let def = item.def.clone();
        let w = &def.w;
        // Spawn the prefab's Spot Light lazily (active at start).
        if fs.light.is_none() {
            fs.light_on = true;
            let l = commands
                .spawn((
                    Name::new("Spot Light"),
                    SpotLight {
                        color: Color::WHITE,
                        intensity: crate::unity_light::unity_point_lumens(LIGHT_INTENSITY),
                        range: 30.0,
                        radius: crate::unity_light::UNITY_LIGHT_RADIUS,
                        outer_angle: (77.4_f32 / 2.0).to_radians(),
                        inner_angle: (77.4_f32 / 2.0).to_radians(),
                        shadow_maps_enabled: true,
                        shadow_map_near_z: 0.2,
                        ..default()
                    },
                    bevy::light::SpotLightTexture { image: assets.load_builder().with_settings(|s: &mut bevy::image::ImageLoaderSettings| s.is_srgb = false).load(COOKIE) },
                    super::markers::world_and_viewmodel(),
                    // Root-local: Spot Light (0,-0.29,0.24) under AimStrafePivot (0,0.283,0.367) under ElbowPivotPoint
                    // (0,0,-0.337) = Unity (0,-0.007,0.27), z mirrored; Bevy spot lights shine along local -Z.
                    Transform::from_xyz(0.0, -0.007, -0.27),
                    Visibility::Inherited,
                ))
                .id();
            commands.entity(entity).add_child(l);
            fs.light = Some(l);
        }
        // WeaponUpdate() runs in pickup.rs.
        if fs.fire_timer > 0.0 {
            fs.fire_timer -= dt;
        }
        // layer == 7 (world item) -> return
        if item.hand == Hand::None || !input.enabled {
            continue;
        }
        let reload_weapon = w.b("reloadWeapon");
        let one_press = w.b("onePressShoot");
        let in_r = item.in_right();
        let in_l = item.in_left();
        let (f_r, f_l) = if item.invert_fire { (fire2, fire1) } else { (fire1, fire2) };
        let ammo_ok = |item: &Item| reload_weapon || item.current_ammo > 0;
        if !one_press {
            if f_r > 0.1 && in_r && ammo_ok(item) {
                fire(item, &mut fs, fpc, &mut lights, &mut sounds);
            }
            if f_l > 0.1 && in_l && ammo_ok(item) {
                fire(item, &mut fs, fpc, &mut lights, &mut sounds);
            }
            // camAnimScript.rotateBack = ... (akAnim false in prefab -> always true, already the default)
            if !reload_weapon && item.current_ammo <= 0 {
                if f_r < 0.1 && in_r {
                    item.is_clicked = true;
                }
                if f_l < 0.1 && in_l {
                    item.is_clicked = true;
                }
            }
        } else {
            if f_r < 0.1 && in_r {
                item.is_clicked = true;
            }
            if f_l < 0.1 && in_l {
                item.is_clicked = true;
            }
            // camAnimScript.rotateBack = true
        }
        if item.is_clicked {
            if f_r > 0.1 && in_r {
                fire(item, &mut fs, fpc, &mut lights, &mut sounds);
                item.is_clicked = false;
            }
            if f_l > 0.1 && in_l {
                fire(item, &mut fs, fpc, &mut lights, &mut sounds);
                item.is_clicked = false;
            }
        }
        if item.shot && w.f("timeBetweenFire") < 0.2 && one_press {
            fire(item, &mut fs, fpc, &mut lights, &mut sounds);
        }
    }
}

/// `Fire()` + `Power()` + `PowerObservers()`.
fn fire(item: &mut Item, fs: &mut FlashLightState, fpc: &Fpc, lights: &mut Query<&mut Visibility>, sounds: &mut MessageWriter<WeaponSound>) {
    let def = item.def.clone();
    let w = &def.w;
    // PauseManager.pause handled by input.enabled; IsOwner always true.
    if !fpc.can_move {
        return;
    }
    if fs.fire_timer > 0.0 {
        item.shot = true;
        return;
    }
    item.shot = false;
    fs.fire_timer = w.f("timeBetweenFire");
    let reload_weapon = w.b("reloadWeapon");
    if !reload_weapon && item.current_ammo <= 0 {
        if let Some(c) = w.asset("nobulletClip") {
            sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
        }
        item.no_ammo_clicks += 1;
    }
    if (reload_weapon || item.current_ammo > 0) && (!reload_weapon || !item.is_reloading) {
        // audio.PlayOneShot(fireClip); light.SetActive(!light.activeSelf)
        if let Some(c) = w.asset("fireClip") {
            sounds.write(WeaponSound { clip: c, volume: 1.0, at: None, pitch: 1.0, linear_max: 0.0, source: None, rolloff: None });
        }
        fs.light_on = !fs.light_on;
        if let Some(l) = fs.light {
            if let Ok(mut v) = lights.get_mut(l) {
                *v = if fs.light_on { Visibility::Inherited } else { Visibility::Hidden };
            }
        }
    }
}
