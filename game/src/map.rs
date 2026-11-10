//! Lighting shared with the loaded map (the realtime sun stand-in, switched off inside the map, and the ambient), the
//! training dummy in the map's spawn room, and the old testing ground: open floor, walls for wall-jumps, ramps for slope
//! slides (concrete + ice), vault boxes, a ladder, a weapon rack, props and a dummy. The testing ground only exists
//! for the regression autotests that need it ([`test_range`]); players get weapons from the "`" weapon menu.

use crate::physics::{UnityLayer, UnityTag, layer};
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;


fn block(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    mat: Handle<StandardMaterial>,
    name: &str,
    size: Vec3,
    tf: Transform,
    tag: &str,
    unity_layer: u32,
) -> Entity {
    commands
        .spawn((
            Name::new(name.to_string()),
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(mat),
            tf,
            RigidBody::Fixed,
            Collider::cuboid(size.x / 2.0, size.y / 2.0, size.z / 2.0),
            UnityLayer(unity_layer),
            UnityTag(tag.to_string()),
        ))
        .id()
}

/// Test-map lighting/material constants (also used to synthesise the HUD reflection probe, `hud::hud_environment`).
pub const FLOOR_SRGB: [f32; 3] = [0.55, 0.55, 0.58];
pub const WALL_SRGB: [f32; 3] = [0.75, 0.72, 0.68];
pub const WALL_HEIGHT: f32 = 12.0;
pub const FLOOR_HALF: f32 = 40.0;
pub const SUN_LUX: f32 = 9000.0;
/// Sun rotation (EulerRot::YXZ yaw, pitch).
pub const SUN_YAW_PITCH: (f32, f32) = (0.6, -0.9);
pub const AMBIENT: f32 = 400.0;

/// The test area's sun (switched off while the player is in the loaded map, which has no realtime sun).
#[derive(Component)]
pub struct TestSun;

/// Test area footprint (x/z half extent around the origin).
pub const TEST_AREA_HALF: f32 = FLOOR_HALF + 2.0;

/// The testing ground is built only for autotests that run outside the map (`scene_map::spawn_in_map` false).
pub fn test_range() -> bool {
    !crate::scene_map::spawn_in_map()
}

/// Training dummy in the spawn room (where the test-area teleporter used to be, facing the initial spawn).
const MAP_DUMMY: Vec3 = Vec3::new(-40.6, -18.27, -152.9);

pub fn setup_map(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let grid = mats.add(StandardMaterial {
        base_color: Color::srgb(FLOOR_SRGB[0], FLOOR_SRGB[1], FLOOR_SRGB[2]),
        perceptual_roughness: 0.9,
        ..default()
    });
    let wall = mats.add(StandardMaterial {
        base_color: Color::srgb(WALL_SRGB[0], WALL_SRGB[1], WALL_SRGB[2]),
        perceptual_roughness: 0.9,
        ..default()
    });
    let ramp = mats.add(StandardMaterial {
        base_color: Color::srgb(0.6, 0.5, 0.42),
        ..default()
    });
    let ice = mats.add(StandardMaterial {
        base_color: Color::srgb(0.7, 0.85, 1.0),
        perceptual_roughness: 0.2,
        ..default()
    });
    let ladder = mats.add(StandardMaterial {
        base_color: Color::srgb(0.3, 0.3, 0.32),
        metallic: 0.8,
        ..default()
    });
    commands.spawn((
        TestSun,
        DirectionalLight {
            illuminance: SUN_LUX,
            shadow_maps_enabled: std::env::var_os("RESTRAFE_NO_SHADOWS").is_none(),
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, SUN_YAW_PITCH.0, SUN_YAW_PITCH.1, 0.0)),
        crate::weapons::markers::world_and_viewmodel(),
    ));
    commands.insert_resource(GlobalAmbientLight {
        brightness: AMBIENT,
        // Lightmapped map geometry (scene_map.rs) gets its indirect light from the lightmaps only, as in Unity.
        affects_lightmapped_meshes: false,
        ..default()
    });
    if !test_range() {
        return;
    }
    let concrete = "Footsteps/Concrete/Default";
    let d = layer::DEFAULT;

    block(
        &mut commands,
        &mut meshes,
        grid.clone(),
        "Floor",
        Vec3::new(80.0, 1.0, 80.0),
        Transform::from_xyz(0.0, -0.5, 0.0),
        concrete,
        d,
    );
    // Perimeter walls (also wall-jump surfaces).
    for (i, (x, z, sx, sz)) in [
        (0.0, -40.0, 80.0, 1.0),
        (0.0, 40.0, 80.0, 1.0),
        (-40.0, 0.0, 1.0, 80.0),
        (40.0, 0.0, 1.0, 80.0),
    ]
    .into_iter()
    .enumerate()
    {
        block(
            &mut commands,
            &mut meshes,
            wall.clone(),
            &format!("Wall{i}"),
            Vec3::new(sx, 12.0, sz),
            Transform::from_xyz(x, 6.0, z),
            concrete,
            d,
        );
    }
    // Parallel wall-jump corridor.
    block(
        &mut commands,
        &mut meshes,
        wall.clone(),
        "WJ_A",
        Vec3::new(1.0, 10.0, 16.0),
        Transform::from_xyz(-14.0, 5.0, -18.0),
        concrete,
        d,
    );
    block(
        &mut commands,
        &mut meshes,
        wall.clone(),
        "WJ_B",
        Vec3::new(1.0, 10.0, 16.0),
        Transform::from_xyz(-20.0, 5.0, -18.0),
        concrete,
        d,
    );
    // Ramps: 20 deg and 35 deg concrete, 25 deg ice.
    for (name, ang, x, mat, tag) in [
        ("Ramp20", 20f32, 10.0, ramp.clone(), concrete),
        ("Ramp35", 35.0, 18.0, ramp.clone(), concrete),
        ("RampIce", 25.0, 26.0, ice.clone(), "Footsteps/Ice"),
    ] {
        let len = 16.0;
        let h = (ang.to_radians()).sin() * len / 2.0;
        block(
            &mut commands,
            &mut meshes,
            mat,
            name,
            Vec3::new(6.0, 0.5, len),
            Transform::from_xyz(x, h, -18.0).with_rotation(Quat::from_rotation_x(ang.to_radians())),
            tag,
            d,
        );
    }
    // Vault boxes (1.0 m, 1.3 m, 2.2 m).
    for (i, hgt) in [1.0f32, 1.3, 2.2].into_iter().enumerate() {
        block(
            &mut commands,
            &mut meshes,
            wall.clone(),
            &format!("Box{i}"),
            Vec3::new(3.0, hgt, 3.0),
            Transform::from_xyz(-6.0 + i as f32 * 5.0, hgt / 2.0, 8.0),
            concrete,
            d,
        );
    }
    // Platform + ladder.
    block(
        &mut commands,
        &mut meshes,
        wall.clone(),
        "Platform",
        Vec3::new(8.0, 0.5, 8.0),
        Transform::from_xyz(20.0, 6.0, 18.0),
        concrete,
        d,
    );
    block(
        &mut commands,
        &mut meshes,
        ladder,
        "Ladder",
        Vec3::new(1.2, 6.5, 0.2),
        Transform::from_xyz(20.0, 3.25, 13.9),
        "Ladder/Metal",
        layer::LADDER,
    );
}

/// Weapon rack: every weapon prefab floating on a grid along the north side of the map
/// (test-map convenience; the original spawns weapons through ItemSpawner).
pub fn spawn_weapon_rack(
    mut commands: Commands,
    db: Res<crate::weapons::data::WeaponDb>,
    assets: Res<AssetServer>,
) {
    let mut rack = Vec::new();
    for (i, def) in db.defs.iter().enumerate().filter(|_| test_range()) {
        if std::env::var("RESTRAFE_RACK_SKIP").map(|s| s.split(",").any(|n| n == def.name)).unwrap_or(false) {
            continue;
        }
        let (row, col) = (i / 12, i % 12);
        let spawner = Vec3::new(-30.25 + col as f32 * 5.5, 0.0, 22.0 + row as f32 * 3.0);
        spawn_item_spawner(&mut commands, &assets, spawner, &def.name);
        // ItemSpawner.Spawn: Instantiate(item, transform.position + Vector3.up * 0.5f, LookRotation(transform.right)).
        let pos = spawner + Vec3::Y * 0.5;
        // LookRotation(transform.right) of an unrotated spawner = Euler(0, 90, 0).
        let rot = crate::unity::unity_euler(0.0, 90.0, 0.0);
        let e = crate::weapons::spawn_world_weapon(&mut commands, def.clone(), pos, &assets);
        commands.entity(e).entry::<Transform>().and_modify(move |mut t| t.rotation = rot);
        rack.push((def.name.clone(), pos, Some(e)));
    }
    let n = rack.len();
    let rot = crate::unity::unity_euler(0.0, 90.0, 0.0);
    commands.insert_resource(WeaponRack { slots: rack, timers: vec![0.0; n], delays: vec![2.0; n], rots: vec![rot; n] });
}

/// `GameObject/ItemSpawner.prefab` (map weapon spawner): the SM_Spawner_00 disc (glb + PrefabRender materials, mesh
/// collider) and its three green point lights; the root's cube renderer is disabled in the prefab.
fn spawn_item_spawner(commands: &mut Commands, assets: &AssetServer, pos: Vec3, label: &str) {
    const JSON: &str = "assets/data/map/ItemSpawner.json";
    static PREFAB: std::sync::OnceLock<Option<crate::weapons::data::Prefab>> = std::sync::OnceLock::new();
    let Some(prefab) = PREFAB.get_or_init(|| crate::weapons::data::Prefab::load(JSON).map_err(|e| warn!("ItemSpawner prefab: {e}")).ok()) else { return };
    let scene = assets.load(GltfAssetLabel::Scene(0).from_asset("models/map/ItemSpawner.glb"));
    let root = commands
        .spawn((Name::new(format!("ItemSpawner:{label}")), Transform::from_translation(pos), Visibility::default()))
        .with_children(|p| {
            // AssetRipper's glb is X-mirrored; the port mirrors Z (see weapons::spawn_world_weapon).
            p.spawn((bevy::world_serialization::WorldAssetRoot(scene), crate::unity_pbr::PrefabRender(JSON.into()), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI))));
        })
        .id();
    let root_path = prefab.objects.first().map(|o| o.path.clone()).unwrap_or_default();
    for o in &prefab.objects {
        if let Some(l) = o.comp("Light") {
            if l.get("m_Enabled").and_then(|v| v.as_i64()) == Some(0) {
                continue;
            }
            let c = l.get("m_Color");
            let g = |k: &str| c.and_then(|c| c.get(k)).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let i = l.get("m_Intensity").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let range = l.get("m_Range").and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
            let tf = prefab.relative_transform(&o.path, &root_path);
            commands.spawn((
                Name::new(format!("spawner-light:{}", o.path)),
                PointLight {
                    color: Color::srgb(g("r"), g("g"), g("b")),
                    intensity: crate::unity_light::unity_point_lumens(i),
                    range,
                    radius: crate::unity_light::UNITY_LIGHT_RADIUS,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_translation(tf.translation),
                ChildOf(root),
                crate::weapons::markers::world_and_viewmodel(),
            ));
        }
        // SM_Spawner_00 MeshCollider (Default layer): a cylinder of the disc's footprint stands in.
        if o.comp("MeshCollider").is_some() {
            let tf = prefab.relative_transform(&o.path, &root_path);
            commands.spawn((Name::new("spawner-collider"), Collider::cylinder(0.075, 0.55), Transform::from_translation(tf.translation + Vec3::Y * 0.075), ChildOf(root), UnityLayer(layer::DEFAULT)));
        }
    }
}

#[derive(Resource)]
pub struct WeaponRack {
    pub slots: Vec<(String, Vec3, Option<Entity>)>,
    timers: Vec<f32>,
    /// Seconds before an emptied slot refills (test rack 2 s; map ItemSpawners `weaponRespawnTimeInSeconds`).
    delays: Vec<f32>,
    /// Spawn rotation: `Quaternion.LookRotation(spawner.right)`.
    rots: Vec<Quat>,
}

impl WeaponRack {
    /// Adds a weapon slot (an ItemSpawner of a loaded map) that refills `delay` seconds after its weapon is taken.
    pub fn add_slot(&mut self, name: String, pos: Vec3, ent: Option<Entity>, delay: f32, rot: Quat) {
        self.slots.push((name, pos, ent));
        self.timers.push(0.0);
        self.delays.push(delay);
        self.rots.push(rot);
    }
}

/// Test-map aid: a rack slot refills 2 s after its weapon was taken or despawned.
pub fn refill_rack(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<crate::weapons::data::WeaponDb>,
    mut rack: ResMut<WeaponRack>,
    items: Query<&crate::weapons::Item>,
    assets: Res<AssetServer>,
) {
    let dt = time.delta_secs();
    let WeaponRack { slots, timers, delays, rots } = &mut *rack;
    for (i, (name, pos, ent)) in slots.iter_mut().enumerate() {
        let present = ent.and_then(|e| items.get(e).ok()).map(|it| it.hand == crate::weapons::Hand::None && !it.physics && it.bob_origin.is_some()).unwrap_or(false);
        if present {
            timers[i] = 0.0;
            continue;
        }
        timers[i] += dt;
        if timers[i] > delays[i] {
            if let Some(def) = db.get(name) {
                let e = crate::weapons::spawn_world_weapon(&mut commands, def, *pos, &assets);
                let rot = rots[i];
                commands.entity(e).entry::<Transform>().and_modify(move |mut t| t.rotation = rot);
                *ent = Some(e);
            }
            timers[i] = 0.0;
        }
    }
}

/// Training dummy: a real PlayerIK body with PlayerHealth (see health.rs), facing the spawn.
pub fn spawn_training_dummy(mut commands: Commands, assets: Res<AssetServer>) {
    if test_range() {
        crate::health::spawn_dummy(&mut commands, &assets, Vec3::new(-8.0, 0.2, -6.0), 180.0);
    }
}

/// The spawn-room dummy, once the map (its colliders) is set up: the dummy's motor falls under gravity.
pub fn spawn_map_dummy(mut commands: Commands, assets: Res<AssetServer>, spawn: Option<Res<crate::scene_map::MapSpawn>>, mut done: Local<bool>) {
    if *done || test_range() {
        return;
    }
    let Some(sp) = spawn.and_then(|s| s.0) else { return };
    *done = true;
    // Face the initial spawn (Unity yaw: atan2(dx, dz) in Unity space, port z mirrored).
    let d = sp.translation - MAP_DUMMY;
    let yaw = d.x.atan2(-d.z).to_degrees();
    crate::health::spawn_dummy(&mut commands, &assets, MAP_DUMMY, yaw);
}

/// Test-map props: BarrelProps near the spawn and one of each ceramic pot (SM_Props_CeramicPot_*) in a row.
pub fn spawn_props(mut commands: Commands, assets: Res<AssetServer>) {
    if !test_range() {
        return;
    }
    for p in [Vec3::new(2.0, 1.0, 3.0), Vec3::new(3.3, 1.0, 3.0), Vec3::new(2.6, 1.0, 4.3), Vec3::new(-3.0, 1.0, 3.0)] {
        crate::props::spawn_barrel(&mut commands, &assets, p);
    }
    let pots = ["A1", "A2", "A3", "B1", "B2", "B3", "B4", "C1", "C2", "C3", "C4", "D1", "D2", "D3", "D4"];
    for (i, p) in pots.iter().enumerate() {
        crate::props::spawn_pot(&mut commands, &assets, &format!("SM_Props_CeramicPot_{p}"), Vec3::new(2.0 + i as f32 * 1.2, 0.0, -16.0), 0.0);
    }
}
