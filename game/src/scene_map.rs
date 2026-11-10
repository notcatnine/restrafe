//! The Exploration Mode map `Scenes/TrainingRange/TrainingRange_00.unity` ("Training Ground" in the Play menu), loaded
//! from AssetRipper's scene glb + the scene extracted to JSON, next to the test area.
//!
//! * Rendering: the glb under a [`crate::unity_pbr::PrefabRender`] (materials, inactive objects, disabled renderers).
//! * Bakery lightmaps (`!ftraceLightmaps` ftLightmapsStorage: maps / bakedRenderers / bakedIDs / bakedScaleOffset):
//!   BC6H HDR textures (manifest `rawtex`) applied as Bevy `Lightmap`s on the renderers' primitives.
//! * Colliders: every enabled Box / Sphere / Capsule / Mesh collider of an active object, with the object's layer and tag
//!   (footsteps, ladders, Killz); MeshColliders decode their exact Mesh asset (`maps/mesh/<Mesh>.asset`; the glTF export
//!   numbers duplicate mesh names differently).
//! * Realtime lights (`m_Lightmapping` 4) as Unity-attenuated point / spot lights; directional lights with the HUD layer
//!   in their culling mask (the sun, the two HUD-only ones) also light the 3D HUD (shadowless, HUD render layer).
//! * Gameplay objects: ItemSpawners (weapons, `weaponRespawnTimeInSeconds`), BarrelProps and Pots (replaced by the live
//!   props.rs objects at the same pose, keeping their lightmaps), SpawnPoints (player spawn).
//! * Left out on request: the time-attack parkour (`---OFFICIAL RACES---` TimerTrigger gates, TimerManager, the
//!   DisableTimeAttackParkour cube, race texts) plus the scene's manager / UI objects (menus, cameras, network).

use crate::physics::{UnityLayer, UnityTag};
use crate::unity_pbr::{PrefabRender, PrefabRenderDone};
use crate::weapons::data::{Prefab, PrefabObject, UField};
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::Lightmap;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::world_serialization::WorldAssetRoot;
use bevy_rapier3d::prelude::*;
use std::collections::HashMap;

pub const MAP: &str = "TrainingRange_00";
const JSON: &str = "assets/data/maps/TrainingRange_00.json";
/// Where the map sits relative to the test area (port space).
pub const MAP_OFFSET: Vec3 = Vec3::new(0.0, 0.0, -60.0);

/// Roots / components left out (parkour time attack, scene managers and UI).
const EXCLUDED_ROOTS: [&str; 7] = ["---OFFICIAL RACES---", "MENU_CUBE (1)", "3D CANVAS", "---USER INTERFACE---", "NetworkManager", "GameManager", "Main Camera"];
const EXCLUDED_COMPONENTS: [&str; 5] = ["TimerTrigger", "TimerManager", "DisableTimeAttackParkour", "ChangeTextValue", "DisplayLoadValues"];

#[derive(Component)]
pub struct MapGlb {
    frames_ready: u32,
}

#[derive(Component)]
pub struct MapSetupDone;

/// Whether the map's colliders are usable by character-controller queries: the map is set up and a few physics steps
/// have run since (rapier's query pipeline only sees colliders added before the last step). True without a map.
pub fn map_physics_ready(maps: &Query<Has<MapSetupDone>, With<MapGlb>>, frames: &mut u32) -> bool {
    if maps.is_empty() {
        return true;
    }
    if maps.iter().any(|done| done) {
        *frames += 1;
    }
    *frames > 3
}

/// Player spawn of the loaded map (world pose; the camera pitch starts at 0).
#[derive(Resource, Default, Clone, Copy)]
pub struct MapSpawn(pub Option<Transform>);

/// The map's 1v1 SpawnPoints in PlayerManager order (first = the initial spawn, TakeIndex 0, player 0; respawns pick a
/// shuffled one: ReturnSpawnPoint's free-sphere test always overlaps the floor, so it falls back to the first).
#[derive(Resource, Default)]
pub struct MapSpawnPoints {
    pub points: Vec<Transform>,
    snapped: bool,
}

/// SpawnPoint.Awake SnapToGround: Physics.Raycast down 10 m -> hit point + 0.2 up. Runs once the map colliders are in
/// the physics world, then publishes the initial spawn.
fn snap_spawn_points(rapier: ReadRapierContext, points: Option<ResMut<MapSpawnPoints>>, mut spawn: ResMut<MapSpawn>, mut frames: Local<u32>) {
    let Some(mut points) = points else { return };
    if points.snapped {
        return;
    }
    *frames += 1;
    if *frames < 3 {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    for p in &mut points.points {
        if let Some(h) = crate::physics::raycast(&ctx, p.translation, Vec3::NEG_Y, 10.0, crate::physics::DEFAULT_RAYCAST_LAYERS) {
            p.translation = h.point + Vec3::Y * 0.2;
        }
    }
    points.snapped = true;
    spawn.0 = points.points.first().copied();
    info!("{MAP}: spawn points {:?}", points.points.iter().map(|p| p.translation).collect::<Vec<_>>());
}

#[derive(Resource, Default)]
struct MapLightmaps(Vec<Handle<Image>>);

/// A Bakery lightmap to apply to this entity's primitives once they exist (live props replacing scene objects).
#[derive(Component, Clone, Copy)]
pub struct PendingLightmap {
    pub index: usize,
    pub scale_offset: Vec4,
}

/// Loads `maps/lightmaps/<name>.utex` (see setup.rs `rawtex`): BC6H (Unity TextureFormat 24) only.
fn load_utex(file: &str) -> Option<Image> {
    if std::env::var_os("RESTRAFE_LM_DEBUG").is_some() {
        // Debug: a 1x1 half-float white lightmap instead of the BC6H data.
        let one = 0x3c00u16.to_le_bytes(); // f16 1.0
        let px: Vec<u8> = [one, one, one, one].concat();
        return Some(Image::new(Extent3d { width: 1, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, px, TextureFormat::Rgba16Float, RenderAssetUsages::RENDER_WORLD));
    }
    let Ok(b) = std::fs::read(file) else {
        // Not exported: the lightmap Texture2D read in place, level 0 as stored (rows bottom-up, which the
        // lightmap UV transform expects).
        let stem = file.rsplit('/').next()?.trim_end_matches(".utex");
        let (t, data) = crate::owned::Owned::global()?.texture_object(stem).map_err(|e| warn!("{file}: {e}")).ok()?;
        let gf = crate::unity_fs::GpuFormat::from_unity(t.format)?;
        if gf != crate::unity_fs::GpuFormat::Bc6h {
            warn!("{file}: unsupported texture format {}", t.format);
            return None;
        }
        let len = gf.level_size(t.width as u32, t.height as u32);
        let mut img = Image::new(Extent3d { width: t.width as u32, height: t.height as u32, depth_or_array_layers: 1 }, TextureDimension::D2, data.get(..len)?.to_vec(), TextureFormat::Bc6hRgbUfloat, RenderAssetUsages::RENDER_WORLD);
        img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::linear());
        return Some(img);
    };
    if b.len() < 20 || &b[0..4] != b"UTEX" {
        return None;
    }
    let u = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let (w, h, fmt) = (u(4), u(8), u(12));
    let format = match fmt {
        24 => TextureFormat::Bc6hRgbUfloat,
        _ => {
            warn!("{file}: unsupported texture format {fmt}");
            return None;
        }
    };
    let mut img = Image::new(Extent3d { width: w, height: h, depth_or_array_layers: 1 }, TextureDimension::D2, b[20..].to_vec(), format, RenderAssetUsages::RENDER_WORLD);
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::linear());
    Some(img)
}

#[cfg(test)]
mod lm_tests {
    /// With the old export present: the in-place lightmap equals the exported .utex payload (RESTRAFE_LM_CHECK=1).
    #[test]
    fn owned_lightmap_matches_export() {
        if !crate::owned::ref_checks("RESTRAFE_LM_CHECK") {
            return;
        }
        for name in ["TrainingRange_00_LM0_final", "TrainingRange_00_LMA22_final"] {
            let b = std::fs::read(crate::owned::reference_file(&format!("assets/maps/lightmaps/{name}.utex"))).unwrap();
            let (t, data) = crate::owned::Owned::open().unwrap().texture_object(name).unwrap();
            let len = crate::unity_fs::GpuFormat::Bc6h.level_size(t.width as u32, t.height as u32);
            println!("{name}: {}x{} format {} mips {}, exported {} bytes, in place {} bytes, equal {}", t.width, t.height, t.format, t.mip_count, b.len() - 20, len, b[20..] == data[..len]);
            assert_eq!(&b[20..], &data[..len]);
        }
    }
}

fn storage(prefab: &Prefab) -> Option<&crate::weapons::data::Fields> {
    prefab.objects.iter().find_map(|o| o.components.iter().find(|(_, f)| f.contains_key("bakedRenderers")).map(|(_, f)| f))
}

pub fn spawn_map(mut commands: Commands, assets: Res<AssetServer>, mut images: ResMut<Assets<Image>>) {
    if std::env::var_os("RESTRAFE_NO_MAP").is_some() {
        return;
    }
    let Ok(prefab) = Prefab::load(JSON).map_err(|e| warn!("{MAP}: {e} (run setup)")) else { return };
    // Lightmaps in ftLightmapsStorage.maps order (= lightmap index).
    let mut lm = Vec::new();
    if let Some(st) = storage(&prefab) {
        for m in st.get("maps").and_then(|m| m.as_array()).into_iter().flatten() {
            let name = m.as_str().unwrap_or("").rsplit('/').next().unwrap_or("").trim_end_matches(".texture2D").trim_end_matches(".png").to_string();
            let img = load_utex(&format!("assets/maps/lightmaps/{name}.utex")).unwrap_or_else(|| Image::default());
            lm.push(images.add(img));
        }
    }
    commands.insert_resource(MapLightmaps(lm));
    commands.init_resource::<MapSpawn>();
    let scene = assets.load(GltfAssetLabel::Scene(0).from_asset(format!("maps/{MAP}.glb")));
    let vis = if std::env::var_os("RESTRAFE_MAP_HIDDEN").is_some() { Visibility::Hidden } else { Visibility::default() };
    commands.spawn((Name::new(MAP), Transform::from_translation(MAP_OFFSET), vis)).with_children(|p| {
        // AssetRipper's glb is X-mirrored; the port mirrors Z (see weapons::spawn_world_weapon).
        p.spawn((WorldAssetRoot(scene), PrefabRender(JSON.into()), MapGlb { frames_ready: 0 }, Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)), Visibility::default()));
    });
}

/// Unity local position -> glb node local (AssetRipper mirrors X).
fn glb_pos(v: Option<&serde_json::Value>) -> Vec3 {
    let g = |c: &str| v.and_then(|v| v.get(c)).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
    Vec3::new(-g("x"), g("y"), g("z"))
}

fn is_excluded(path: &str, objects: &HashMap<&str, &PrefabObject>) -> bool {
    let mut p = path;
    loop {
        if EXCLUDED_ROOTS.contains(&p) {
            return true;
        }
        if objects.get(p).is_some_and(|o| o.components.iter().any(|(t, _)| EXCLUDED_COMPONENTS.contains(&t.as_str()))) {
            return true;
        }
        match p.rsplit_once('/') {
            Some((parent, _)) => p = parent,
            None => return false,
        }
    }
}

fn active_in_hierarchy(path: &str, objects: &HashMap<&str, &PrefabObject>) -> bool {
    let mut p = path;
    loop {
        if objects.get(p).is_some_and(|o| !o.active) {
            return false;
        }
        match p.rsplit_once('/') {
            Some((parent, _)) => p = parent,
            None => return true,
        }
    }
}

/// Pose for a live prop root replacing a scene object whose glb node has world transform `g` (its glb child carries the
/// same Y180 as the map's glb root).
fn prop_root_pose(g: &GlobalTransform) -> Transform {
    let (s, r, t) = g.to_scale_rotation_translation();
    Transform { translation: t, rotation: r * Quat::from_rotation_y(std::f32::consts::PI).inverse(), scale: s }
}

#[allow(clippy::too_many_arguments)]
fn setup_map(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut glbs: Query<(Entity, &mut MapGlb), (With<PrefabRenderDone>, Without<MapSetupDone>)>,
    children: Query<&Children>,
    names: Query<&Name>,
    prims: Query<&Mesh3d>,
    globals: Query<&GlobalTransform>,
    lightmaps: Res<MapLightmaps>,
    db: Res<crate::weapons::data::WeaponDb>,
    mut rack: Option<ResMut<crate::map::WeaponRack>>,
    spawn: ResMut<MapSpawn>,
    parents_q: Query<&ChildOf>,
) {
    let Ok((root, mut mg)) = glbs.single_mut() else { return };
    mg.frames_ready += 1;
    if mg.frames_ready == 1 {
        // AssetRipper's scene glb drops the transforms of the scene's root objects: restore them from the scene
        // (glb convention: X mirrored -> position (-x, y, z), rotation (x, -y, -z, w)).
        let Ok(prefab) = Prefab::load(JSON) else { return };
        let mut fixed = 0;
        let mut stack: Vec<Entity> = children.get(root).map(|c| c.iter().collect()).unwrap_or_default();
        while let Some(e) = stack.pop() {
            let Ok(name) = names.get(e) else { continue };
            let Some(o) = prefab.objects.iter().find(|o| o.path == name.as_str()) else {
                // the glTF scene entity above the root nodes
                stack.extend(children.get(e).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default());
                continue;
            };
            if let Some(t) = o.comp("Transform").or_else(|| o.comp("RectTransform")) {
                let g = |k: &str, c: &str| t.get(k).and_then(|v| v.get(c)).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                // A root RectTransform keeps its x / y in m_AnchoredPosition (no parent rect: anchoredPosition = localPosition).
                let rect = o.comp("Transform").is_none();
                let (px, py) = if rect { (g("m_AnchoredPosition", "x"), g("m_AnchoredPosition", "y")) } else { (g("m_LocalPosition", "x"), g("m_LocalPosition", "y")) };
                let tf = Transform {
                    translation: Vec3::new(-px, py, g("m_LocalPosition", "z")),
                    rotation: Quat::from_xyzw(g("m_LocalRotation", "x"), -g("m_LocalRotation", "y"), -g("m_LocalRotation", "z"), g("m_LocalRotation", "w")).normalize(),
                    scale: Vec3::new(g("m_LocalScale", "x"), g("m_LocalScale", "y"), g("m_LocalScale", "z")),
                };
                commands.entity(e).insert(tf);
                fixed += 1;
            }
        }
        debug!("{MAP}: restored {fixed} root transforms");
        return;
    }
    // Let transform propagation run over the restored roots first.
    if mg.frames_ready < 4 {
        return;
    }
    commands.entity(root).insert(MapSetupDone);
    let Ok(prefab) = Prefab::load(JSON) else { return };
    let objects: HashMap<&str, &PrefabObject> = prefab.objects.iter().map(|o| (o.path.as_str(), o)).collect();
    // glb node entity per object path (same walk as PrefabRender).
    let mut nodes: HashMap<String, Entity> = HashMap::new();
    let mut stack: Vec<(Entity, String)> = children.get(root).map(|c| c.iter().map(|e| (e, String::new())).collect()).unwrap_or_default();
    while let Some((e, parent)) = stack.pop() {
        if prims.contains(e) {
            continue;
        }
        let Ok(name) = names.get(e) else { continue };
        let path = if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") };
        if parent.is_empty() && !objects.contains_key(path.as_str()) {
            for k in children.get(e).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default() {
                stack.push((k, String::new()));
            }
            continue;
        }
        for k in children.get(e).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default() {
            stack.push((k, path.clone()));
        }
        nodes.entry(path).or_insert(e);
    }
    let primitives_of = |e: Entity| -> Vec<Entity> { children.get(e).map(|c| c.iter().filter(|k| prims.contains(*k)).collect()).unwrap_or_default() };

    // Bakery lightmaps per renderer path.
    let mut lm_of: HashMap<String, (usize, Vec4)> = HashMap::new();
    if let Some(st) = storage(&prefab) {
        let rends: Vec<String> = st.get("bakedRenderers").and_then(|r| r.as_array()).into_iter().flatten().map(|r| r.as_str().unwrap_or("").trim_start_matches("ref:").split("::").next().unwrap_or("").to_string()).collect();
        let ids_hex = st.get("bakedIDs").and_then(|v| v.as_str()).unwrap_or("");
        let ids: Vec<i32> = (0..ids_hex.len() / 8).map(|k| u32::from_str_radix(&ids_hex[k * 8..k * 8 + 8], 16).map(|v| i32::from_le_bytes(v.to_be_bytes())).unwrap_or(-1)).collect();
        let sos: Vec<Vec4> = st.get("bakedScaleOffset").and_then(|v| v.as_array()).into_iter().flatten().map(|v| {
            let g = |c: &str| v.get(c).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
            Vec4::new(g("x"), g("y"), g("z"), g("w"))
        }).collect();
        for (k, r) in rends.iter().enumerate() {
            if let (Some(&id), Some(&so)) = (ids.get(k), sos.get(k)) {
                if id >= 0 && (id as usize) < lightmaps.0.len() {
                    lm_of.insert(r.clone(), (id as usize, so));
                }
            }
        }
    }
    let (mut n_col, mut n_light, mut n_lm, mut n_props, mut n_items, mut n_hidden) = (0, 0, 0, 0, 0, 0);
    let mut spawn_points: Vec<(i32, Transform)> = Vec::new();
    let mut pending_texts: Vec<PendingText> = Vec::new();
    let mut mesh_cache: HashMap<(String, bool), Option<Collider>> = HashMap::new();
    for o in &prefab.objects {
        let Some(&node) = nodes.get(&o.path) else { continue };
        if is_excluded(&o.path, &objects) {
            commands.entity(node).try_insert(Visibility::Hidden);
            n_hidden += 1;
            continue;
        }
        if !active_in_hierarchy(&o.path, &objects) {
            continue;
        }
        let g = globals.get(node).copied().unwrap_or_default();
        // Live props at the scene object's pose (the scene's copy is hidden).
        if o.comp("PhysicsProp").is_some() && std::env::var_os("RESTRAFE_MAP_NOBARRELS").is_none() {
            commands.entity(node).try_insert(Visibility::Hidden);
            crate::props::spawn_barrel_at(&mut commands, &assets, prop_root_pose(&g));
            n_props += 1;
            continue;
        }
        if o.comp("Pot").is_some() {
            commands.entity(node).try_insert(Visibility::Hidden);
            let base = o.path.rsplit('/').next().unwrap_or("").split(" (").next().unwrap_or("").to_string();
            if let Some(e) = crate::props::spawn_pot_at(&mut commands, &assets, &base, prop_root_pose(&g)) {
                // RESTRAFE_POT_NOLM: probe-lit pots (debug: compare probe vs lightmap brightness).
                if let Some(&(index, so)) = lm_of.get(&o.path).filter(|_| std::env::var_os("RESTRAFE_POT_NOLM").is_none()) {
                    commands.entity(e).insert(PendingLightmap { index, scale_offset: so });
                }
            }
            n_props += 1;
            continue;
        }
        // ItemSpawner.Spawn: Instantiate(itemToSpawn, position + up * 0.5, LookRotation(right)).
        if let Some(sp) = o.comp("ItemSpawner") {
            let name = sp.asset("itemToSpawn").map(|a| a.rsplit('/').next().unwrap_or("").trim_end_matches(".prefab").to_string()).unwrap_or_default();
            if let (Some(def), Some(rack)) = (db.get(&name), rack.as_mut()) {
                let pos = g.translation() + Vec3::Y * 0.5;
                let e = crate::weapons::spawn_world_weapon(&mut commands, def, pos, &assets);
                // LookRotation(transform.right): the item's forward is the spawner's right.
                let rot = Transform::IDENTITY.looking_to(g.right().as_vec3(), Vec3::Y).rotation;
                commands.entity(e).entry::<Transform>().and_modify(move |mut t| t.rotation = rot);
                let delay = sp.get("weaponRespawnTimeInSeconds").and_then(|v| v.as_f64()).unwrap_or(3.0) as f32;
                if std::env::var_os("RESTRAFE_DEBUG_SPAWNERS").is_some() {
                    let disc = nodes.get(&format!("{}/SM_Spawner_00", o.path)).and_then(|d| globals.get(*d).ok()).map(|d| d.translation());
                    let dn = nodes.get(&format!("{}/SM_Spawner_00", o.path)).copied();
                    let pg: Vec<Vec3> = dn.map(|d| primitives_of(d)).unwrap_or_default().iter().filter_map(|p| globals.get(*p).ok().map(|g| g.translation())).collect();
                    let names_k: Vec<String> = dn.and_then(|d| children.get(d).ok()).map(|c| c.iter().map(|k| names.get(k).map(|n| n.to_string()).unwrap_or("?".into())).collect()).unwrap_or_default();
                    println!("SPAWNER {} {name} at {pos:?} disc {disc:?} prims {pg:?} kids {names_k:?}", o.path);
                }
                rack.add_slot(name, pos, Some(e), delay, rot);
                n_items += 1;
            }
        }
        // PlayerManager.PopulateSpawnPoints: the SpawnPoints under the object tagged "Spawnpoints" (1v1 set), in
        // GetComponentsInChildren order; spawned with Quaternion.Euler(0, eulerAngles.y, 0).
        if o.comp("SpawnPoint").is_some() {
            let parent_tag = o.path.rsplit_once('/').and_then(|(p, _)| objects.get(p)).map(|p| p.tag.as_str());
            if parent_tag == Some("Spawnpoints") {
                let order = o.comp("Transform").map(|t| t.i("m_RootOrder")).unwrap_or(0);
                let fwd = (g.rotation() * Quat::from_rotation_y(std::f32::consts::PI).inverse()) * Vec3::NEG_Z;
                let rot = Transform::IDENTITY.looking_to(Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::NEG_Z), Vec3::Y).rotation;
                spawn_points.push((order, Transform::from_translation(g.translation()).with_rotation(rot)));
            }
        }
        // Lightmap on this renderer's primitives.
        // Unity Ultra lights each renderer with its 4 most important realtime lights per pixel: the big lightmapped level
        // meshes get practically none of the 81 small spawner lights (map lights: affects_lightmapped_mesh_diffuse
        // false), while every spawner disc is its own renderer lit by its 3 lights: the discs stay unlightmapped.
        let own_lights = o.path.ends_with("/SM_Spawner_00");
        if let Some(&(index, so)) = lm_of.get(&o.path).filter(|_| std::env::var_os("RESTRAFE_MAP_NOLM").is_none() && !own_lights) {
            // ftLightmaps.RefreshScene: `if (!renderer.isPartOfStaticBatch) renderer.lightmapScaleOffset = bakedScaleOffset`.
            // Static batching already baked the scale/offset into the combined mesh's UV2, so those sample it as is.
            let combined = o.comp("MeshFilter").and_then(|m| m.get("m_Mesh")).and_then(|m| m.as_str()).is_some_and(|m| m.contains("Combined Mesh"));
            let so = if combined { Vec4::new(1.0, 1.0, 0.0, 0.0) } else { so };
            let skip = std::env::var("RESTRAFE_LM_SKIP").map(|v| v.split(',').any(|k| k.parse() == Ok(index))).unwrap_or(false);
            if std::env::var_os("RESTRAFE_LM_DEBUG").is_some() { println!("LMI {index} {}", o.path); }
            for p in primitives_of(node).into_iter().filter(|_| !skip) {
                commands.entity(p).try_insert(unity_lightmap(&lightmaps.0[index], so));
                n_lm += 1;
            }
        }
        // Looping AudioSources (playOnAwake): ambience bed and lamp hum (SetAmbientVolume -> ambient volume).
        if let Some(a) = o.comp("AudioSource") {
            if a.i("m_PlayOnAwake") != 0 && a.i("Loop") != 0 && a.i("m_Enabled") != 0 {
                if let Some(clip) = a.asset("m_audioClip") {
                    let snd = crate::vfx::def::SoundDef::parse(&serde_json::json!({ "hitClips": [format!("asset:{clip}")] }), Some(&serde_json::Value::Object(a.clone())));
                    if let Some(sd) = snd {
                        let child = commands.spawn((Name::new(format!("ambience:{}", o.path)), Transform::default(), crate::audio::LoopEmitter {
                            clip,
                            linear_max: sd.linear_max,
                            volume: sd.volume * a.asset("OutputAudioMixerGroup").map(|m| mixer_gain(&m)).unwrap_or(1.0),
                            two_d: !sd.spatial,
                            ambient: o.comp("SetAmbientVolume").is_some(),
                            rolloff: sd.rolloff.clone(),
                        })).id();
                        commands.entity(node).add_child(child);
                    }
                }
            }
        }
        // The scene's Directional Light: Mixed in the editor but not baked (m_BakingOutput.isBaked false, bake type
        // Realtime in the build), so a realtime sun with soft shadows over the whole map, lightmapped surfaces included.
        if let Some(l) = o.comp("Light").filter(|l| l.i("m_Type") == 1 && l.i("m_Enabled") != 0) {
            let hud_only = l.get("m_CullingMask").and_then(|m| m.get("m_Bits")).and_then(|b| b.as_u64()) == Some(1 << 25);
            if !hud_only {
                let c = l.get("m_Color");
                let ch = |k: &str| c.and_then(|c| c.get(k)).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                let r = g.rotation() * Quat::from_rotation_y(std::f32::consts::PI).inverse();
                commands.insert_resource(MapSun { rotation: r, color: Color::srgb(ch("r"), ch("g"), ch("b")), intensity: l.f("m_Intensity"), shadows: l.get("m_Shadows").and_then(|s| s.get("m_Type")).and_then(|v| v.as_i64()).unwrap_or(0) != 0 });
            }
            // Directional lights whose culling mask has the HUD layer (the sun: everything; HUD-LIGHT-Directionnal (+ (1)):
            // HUD only, lavender, intensity 1) also light the 3D HUD drawn by UICam. UICam culls everything but the HUD,
            // so no world geometry shadows it: a shadowless copy on the port's HUD layer.
            let bits = l.get("m_CullingMask").and_then(|m| m.get("m_Bits")).and_then(|b| b.as_u64()).unwrap_or(u64::MAX);
            if bits >> 25 & 1 == 1 && std::env::var_os("RESTRAFE_NO_MAP_HUD_LIGHTS").is_none() {
                let c = l.get("m_Color");
                let ch = |k: &str| c.and_then(|c| c.get(k)).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                let r = g.rotation() * Quat::from_rotation_y(std::f32::consts::PI).inverse();
                commands.spawn((
                    Name::new(format!("hud light:{}", o.path)),
                    DirectionalLight {
                        color: Color::srgb(ch("r"), ch("g"), ch("b")),
                        illuminance: l.f("m_Intensity") * std::f32::consts::PI / crate::unity_light::view_exposure(),
                        shadow_maps_enabled: false,
                        ..default()
                    },
                    Transform::from_rotation(r),
                    bevy::camera::visibility::RenderLayers::layer(crate::hud::HUD_LAYER),
                ));
            }
        }
        // Realtime lights (baked ones are in the lightmaps).
        if let Some(l) = o.comp("Light") {
            let realtime = l.i("m_Lightmapping") == 4;
            let hud_only = l.get("m_CullingMask").and_then(|m| m.get("m_Bits")).and_then(|b| b.as_u64()) == Some(1 << 25);
            if realtime && !hud_only && l.i("m_Enabled") != 0 && std::env::var_os("RESTRAFE_MAP_NOLIGHTS").is_none() {
                if let Some(b) = light_bundle(l) {
                    let child = commands.spawn((Name::new(format!("light:{}", o.path)), Transform::default(), crate::weapons::markers::world_and_viewmodel())).id();
                    match b {
                        LightKind::Point(p) => commands.entity(child).insert(p),
                        LightKind::Spot(s) => commands.entity(child).insert(s),
                    };
                    commands.entity(node).add_child(child);
                    n_light += 1;
                }
            }
        }
        // World-space TextMeshPro (the fly-mode sign): built by `map_texts` once the fonts load.
        if o.comp("MeshRenderer").is_some() {
            if let Some((_, f)) = o.components.iter().find(|(_, f)| f.contains_key("m_text")) {
                let rt = o.comp("RectTransform");
                let v2 = |k: &str, d: Vec2| rt.and_then(|r| r.get(k)).map(|v| Vec2::new(v["x"].as_f64().unwrap_or(d.x as f64) as f32, v["y"].as_f64().unwrap_or(d.y as f64) as f32)).unwrap_or(d);
                pending_texts.push(PendingText { node, fields: f.clone(), size: v2("m_SizeDelta", Vec2::ZERO), pivot: v2("m_Pivot", Vec2::splat(0.5)) });
            }
        }
        // Doors / pigeons (scene_props.rs).
        if let Some(d) = o.comp("Door") {
            if std::env::var_os("RESTRAFE_DEBUG_DOORS").is_some() {
                let mut chain = vec![];
                let mut cur = Some(node);
                while let Some(c) = cur {
                    chain.push(format!("{}@{:?}", names.get(c).map(|n| n.to_string()).unwrap_or_default(), globals.get(c).map(|g| g.translation()).ok()));
                    cur = parents_q.get(c).ok().map(|p| p.parent());
                }
                println!("DOORNODE {} -> {}", o.path, chain.join(" <- "));
            }
            commands.entity(node).insert(crate::scene_props::Door::from_fields(d, o.comp("AudioSource")));
        }
        if let Some(pg) = o.comp("Pigeon") {
            commands.entity(node).insert(crate::scene_props::Pigeon::from_fields(pg));
            // Animator (SM_A_Pigeon00.controller: idle / fly wing clips on the "fly" bool).
            if let Some(c) = o.comp("Animator").and_then(|a| a.asset("m_Controller")) {
                if let Some(an) = crate::weapons::anim::NodeAnimator::load(&c, &prefab) {
                    commands.entity(node).insert(an);
                }
            }
        }
        // Colliders.
        if std::env::var_os("RESTRAFE_MAP_NOCOLLIDERS").is_some() {
            continue;
        }
        // Dynamic Rigidbodies (SM_Table00, three SM_Trashbag_01): mass 1, drag 0, angularDrag 0.05, gravity. Nothing
        // scripted pushes them; loose weapons, thrown props and ragdolls knock them over.
        let mut body_mass = None;
        if let Some(rb) = o.comp("Rigidbody").filter(|rb| rb.i("m_IsKinematic") == 0) {
            let mass = rb.get("m_Mass").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let n = o.components.iter().filter(|(t, f)| t.ends_with("Collider") && f.get("m_IsTrigger").and_then(|v| v.as_i64()) != Some(1)).count().max(1);
            body_mass = Some(mass / n as f32);
            commands.entity(node).insert((
                RigidBody::Dynamic,
                Damping { linear_damping: rb.f("m_Drag"), angular_damping: rb.f("m_AngularDrag") },
                GravityScale(if rb.i("m_UseGravity") != 0 { 1.0 } else { 0.0 }),
            ));
        }
        // InteractEnvironment.Awake: gameObject.layer = 19 (InteractEnvironment).
        let interact_env = o.comp("Door").is_some() || o.comp("PhysicsProp").is_some();
        let layer = UnityLayer(if interact_env { crate::physics::layer::INTERACT_ENVIRONMENT } else { o.layer });
        // The collider's own GameObject carries the script: map the collider entity back to the node.
        let target: Option<(bool, bool)> = Some((o.comp("Door").is_some(), o.comp("Pigeon").is_some()));
        let tag = UnityTag(o.tag.clone());
        for (t, f) in &o.components {
            if f.get("m_Enabled").and_then(|v| v.as_i64()) == Some(0) {
                continue;
            }
            let trigger = f.get("m_IsTrigger").and_then(|v| v.as_i64()) == Some(1);
            let shape: Option<(Collider, Vec3)> = match t.as_str() {
                "BoxCollider" => {
                    let s = f.v3("m_Size").abs() * 0.5;
                    Some((Collider::cuboid(s.x.max(1e-3), s.y.max(1e-3), s.z.max(1e-3)), glb_pos(f.get("m_Center"))))
                }
                "SphereCollider" => Some((Collider::ball(f.f("m_Radius")), glb_pos(f.get("m_Center")))),
                "CapsuleCollider" => {
                    let (r, h) = (f.f("m_Radius"), f.f("m_Height"));
                    let half = (h * 0.5 - r).max(0.0);
                    let c = match f.i("m_Direction") {
                        0 => Collider::capsule_x(half, r),
                        2 => Collider::capsule_z(half, r),
                        _ => Collider::capsule_y(half, r),
                    };
                    Some((c, glb_pos(f.get("m_Center"))))
                }
                "MeshCollider" => {
                    // The exact Mesh asset, decoded in port space (Z mirrored): the 180 degree turn puts it in the glb
                    // node's X-mirrored space.
                    let mesh = f.get("m_Mesh").and_then(|m| m.as_str()).map(|m| m.rsplit('/').next().unwrap_or("").to_string());
                    if let Some(c) = mesh.filter(|m| !m.is_empty()).and_then(|m| mesh_collider(&m, f.i("m_Convex") != 0, &mut mesh_cache)) {
                        let mut e = commands.spawn((Name::new(format!("collider:{}", o.path)), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)), c, layer, tag.clone()));
                        if trigger {
                            e.insert(Sensor);
                        } else if let Some(m) = body_mass {
                            e.insert(ColliderMassProperties::Mass(m));
                        }
                        link_collider(&mut e, node, target);
                        let id = e.id();
                        commands.entity(node).add_child(id);
                        n_col += 1;
                    }
                    None
                }
                _ => None,
            };
            if let Some((col, center)) = shape {
                let mut c = commands.spawn((Name::new(format!("collider:{}", o.path)), Transform::from_translation(center), col, layer, tag.clone()));
                link_collider(&mut c, node, target);
                if o.tag == "Killz" && t == "BoxCollider" {
                    c.insert(KillZone { half: f.v3("m_Size").abs() * 0.5 });
                }
                if trigger {
                    c.insert(Sensor);
                } else if let Some(m) = body_mass {
                    c.insert(ColliderMassProperties::Mass(m));
                }
                let id = c.id();
                commands.entity(node).add_child(id);
                n_col += 1;
            }
        }
    }
    spawn_points.sort_by_key(|(o, _)| *o);
    commands.insert_resource(PendingTexts(pending_texts));
    commands.insert_resource(MapSpawnPoints { points: spawn_points.into_iter().map(|(_, t)| t).collect(), snapped: false });
    info!("{MAP}: {} nodes, {n_col} colliders, {n_light} lights, {n_lm} lightmapped primitives, {n_props} props, {n_items} item spawners, {n_hidden} excluded, spawn {:?}", nodes.len(), spawn.0.map(|t| t.translation));
    println!("MAP {MAP} ready: colliders={n_col} lights={n_light} lightmapped={n_lm} props={n_props} items={n_items} excluded={n_hidden}");
}

enum LightKind {
    Point(PointLight),
    Spot(SpotLight),
}

fn light_bundle(l: &crate::weapons::data::Fields) -> Option<LightKind> {
    let c = l.get("m_Color");
    let g = |k: &str| c.and_then(|c| c.get(k)).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let color = Color::srgb(g("r"), g("g"), g("b"));
    let intensity = crate::unity_light::unity_point_lumens(l.f("m_Intensity"));
    let range = l.f("m_Range");
    match l.i("m_Type") {
        2 => Some(LightKind::Point(PointLight { color, intensity, range, radius: crate::unity_light::UNITY_LIGHT_RADIUS, shadow_maps_enabled: false, affects_lightmapped_mesh_diffuse: false, ..default() })),
        0 => {
            let angle = l.f("m_SpotAngle").to_radians() * 0.5;
            Some(LightKind::Spot(SpotLight { color, intensity, range, radius: crate::unity_light::UNITY_LIGHT_RADIUS, outer_angle: angle, inner_angle: angle * 0.8, shadow_maps_enabled: false, affects_lightmapped_mesh_diffuse: false, ..default() }))
        }
        _ => None,
    }
}

/// Unity lightmap UV = uv2 * scaleOffset.xy + scaleOffset.zw (texture rows bottom-up = GPU memory order); the glb stores
/// v flipped (1 - v), so the rect runs from (zw.x, zw.y + s.y) to (zw.x + s.x, zw.y).
fn unity_lightmap(image: &Handle<Image>, so: Vec4) -> Lightmap {
    Lightmap { image: image.clone(), uv_rect: Rect { min: Vec2::new(so.z, so.w + so.y), max: Vec2::new(so.z + so.x, so.w) }, bicubic_sampling: false }
}

fn apply_pending_lightmaps(
    mut commands: Commands,
    q: Query<(Entity, &PendingLightmap)>,
    kids: Query<&Children>,
    prims: Query<(), With<Mesh3d>>,
    lightmaps: Option<Res<MapLightmaps>>,
) {
    let Some(lightmaps) = lightmaps else { return };
    for (e, p) in &q {
        let found: Vec<Entity> = kids.iter_descendants(e).filter(|d| prims.contains(*d)).collect();
        if found.is_empty() {
            continue;
        }
        for d in found {
            if let Some(img) = lightmaps.0.get(p.index) {
                commands.entity(d).try_insert(unity_lightmap(img, p.scale_offset));
            }
        }
        commands.entity(e).remove::<PendingLightmap>();
    }
}

pub struct SceneMapPlugin;

impl Plugin for SceneMapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (spawn_map, load_environment, setup_probe_env)).add_systems(Update, (setup_sky, map_lighting, place_player_at_spawn, snap_spawn_points, map_texts, kill_zones, ray_test, height_map, wall_scan, whats_at, lightmap_uv_check)).add_systems(Update, (setup_map, apply_pending_lightmaps, bake_irradiance_volume, probe_balls)).add_systems(PostUpdate, respecialize_lightmapped).add_systems(Update, irradiance_view_anchor);
    }
}

/// Bevy caches each mesh's pipeline specialization and only redoes it when the mesh or material changes: a Lightmap
/// added after the material (the map's materials are set by PrefabRender first) must touch the material.
fn respecialize_lightmapped(
    mut unity: Query<&mut MeshMaterial3d<crate::unity_pbr::UnityPbr>, Added<Lightmap>>,
    mut standard: Query<&mut MeshMaterial3d<StandardMaterial>, Added<Lightmap>>,
) {
    for mut m in &mut unity {
        m.set_changed();
    }
    for mut m in &mut standard {
        m.set_changed();
    }
}

// ---------------------------------------------------------------------------------------------
// Sky + lighting environment
// ---------------------------------------------------------------------------------------------

const SKY: &str = "maps/sky/Haze_WarmRainCloudsOnHorizon_EquiRect_flat.png";

/// RenderSettings.skybox (`Skybox/Cubemap`, _Tint 0.5 x unity_ColorSpaceDouble ~= 1, _Exposure 1): the cubemap exported
/// as a 1024 x 6144 strip of the six faces (+X, -X, +Y, -Y, +Z, -Z).
#[derive(Resource)]
struct MapSky {
    image: Handle<Image>,
    ready: bool,
}

/// The map's realtime Directional Light (port-space rotation: Bevy forward = Unity light direction).
#[derive(Resource, Clone, Copy)]
pub struct MapSun {
    pub rotation: Quat,
    pub color: Color,
    pub intensity: f32,
    pub shadows: bool,
}

/// Baked light probes (LightProbes.asset): Unity world positions + the L0 (direction-independent) RGB term.
#[derive(Resource, Default)]
pub(crate) struct MapProbes(Vec<(Vec3, [Vec3; 4])>);

/// LightProbes m_Tetrahedralization inner tetrahedra (probe indices); Unity blends the 4 probes of the containing one.
#[derive(Resource, Default, Clone)]
pub(crate) struct MapProbeTets {
    inner: Vec<[usize; 4]>,
    /// Outer cells: hull triangle (probe indices) extruded along the probes' hull rays.
    outer: Vec<[usize; 3]>,
    /// m_HullRays per probe (port space; zero for interior probes).
    rays: Vec<Vec3>,
}

/// Last resort (no cell found): the 4 nearest probes, inverse-distance weighted.
fn nearest_probes(probes: &[(Vec3, [Vec3; 4])], at: Vec3) -> [Vec3; 4] {
    let mut near: Vec<(f32, [Vec3; 4])> = probes.iter().map(|(q, sh)| (q.distance_squared(at), *sh)).collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut sum, mut wsum) = ([Vec3::ZERO; 4], 0.0);
    for (d2, sh) in near.iter().take(4) {
        let w = 1.0 / (d2.sqrt() + 0.1);
        for k in 0..4 {
            sum[k] += sh[k] * w;
        }
        wsum += w;
    }
    sum.map(|v| v / wsum.max(1e-6))
}

/// Unity's outside-the-hull lookup: find t >= 0 with p in the triangle (v_i + t r_i) and return its barycentric weights.
fn outer_cell_weights(p: Vec3, v: [Vec3; 3], r: [Vec3; 3]) -> Option<[f32; 3]> {
    let at = |t: f32| [v[0] + r[0] * t, v[1] + r[1] * t, v[2] + r[2] * t];
    let f = |t: f32| {
        let q = at(t);
        (q[1] - q[0]).cross(q[2] - q[0]).dot(p - q[0])
    };
    let (mut t0, mut f0) = (0.0f32, f(0.0));
    let mut root = None;
    for k in 1..=128 {
        let t1 = (k as f32 / 128.0).powi(2) * 500.0;
        let f1 = f(t1);
        if f0 == 0.0 || f0.signum() != f1.signum() {
            let (mut a, mut b, mut fa) = (t0, t1, f0);
            for _ in 0..40 {
                let m = 0.5 * (a + b);
                let fm = f(m);
                if fa.signum() == fm.signum() {
                    a = m;
                    fa = fm;
                } else {
                    b = m;
                }
            }
            root = Some(0.5 * (a + b));
            break;
        }
        (t0, f0) = (t1, f1);
    }
    let q = at(root?);
    let n = (q[1] - q[0]).cross(q[2] - q[0]);
    let area = n.length_squared();
    if area < 1e-9 {
        return None;
    }
    let w0 = (q[1] - p).cross(q[2] - p).dot(n) / area;
    let w1 = (q[2] - p).cross(q[0] - p).dot(n) / area;
    let w2 = 1.0 - w0 - w1;
    (w0 >= -1e-3 && w1 >= -1e-3 && w2 >= -1e-3).then_some([w0, w1, w2])
}

/// Barycentric weights of `p` in the tetrahedron (a, b, c, d), None when degenerate.
fn barycentric(p: Vec3, a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> Option<[f32; 4]> {
    let m = Mat3::from_cols(a - d, b - d, c - d);
    if m.determinant().abs() < 1e-6 {
        return None;
    }
    let w = m.inverse() * (p - d);
    Some([w.x, w.y, w.z, 1.0 - w.x - w.y - w.z])
}

fn load_environment(mut commands: Commands, assets: Res<AssetServer>) {
    if std::env::var_os("RESTRAFE_NO_MAP").is_some() {
        return;
    }
    commands.insert_resource(MapSky { image: assets.load(SKY), ready: false });
    let (probes, tets) = load_light_probes(&format!("assets/maps/{MAP}_LightProbes.json"), MAP_OFFSET);
    commands.insert_resource(probes);
    commands.insert_resource(tets);
}

/// A scene's baked light probes (AssetRipper LightProbes.json: positions, SH, tetrahedralization) in port space
/// (Unity z mirrored, + `offset`).
pub(crate) fn load_light_probes(path: &str, offset: Vec3) -> (MapProbes, MapProbeTets) {
    let mut probes = MapProbes::default();
    let mut tets = MapProbeTets::default();
    // the exported file, else the scene's LightProbes read from the installation
    let text = std::fs::read_to_string(path).ok().or_else(|| {
        let scene = path.rsplit('/').next()?.strip_suffix("_LightProbes.json")?;
        crate::owned::Owned::global()?.light_probes_json(scene).map(|v| v.to_string())
    });
    if let Some(t) = &text {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            let pos = v["m_Data"]["m_Positions"].as_array().cloned().unwrap_or_default();
            let sh = v["m_BakedCoefficients"].as_array().cloned().unwrap_or_default();
            for (p, c) in pos.iter().zip(sh.iter()) {
                let g = |k: &str| p[k].as_f64().unwrap_or(0.0) as f32;
                let s = |k: &str| c[k].as_f64().unwrap_or(0.0) as f32;
                // SphericalHarmonicsL2 per channel (r 0..8, g 9..17, b 18..26): [L00, L1-1 (y), L10 (z), L11 (x), L2...]
                // (the L2 band is empty here). Kept as radiance; convolved in `probe_irradiance`.
                let band = |k: usize| Vec3::new(s(&format!("m_Sh{:_>3}_", k)), s(&format!("m_Sh{:_>3}_", k + 9)), s(&format!("m_Sh{:_>3}_", k + 18)));
                // Unity world -> port (z mirrored) + map offset.
                probes.0.push((Vec3::new(g("m_X"), g("m_Y"), -g("m_Z")) + offset, [band(0), band(1), band(2), band(3)]));
            }
        }
    }
    if let Some(t) = &text {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            for tet in v["m_Data"]["m_Tetrahedralization"]["m_Tetrahedra"].as_array().into_iter().flatten() {
                let idx = [0, 1, 2, 3].map(|k| tet[format!("m_Indices_{k}_")].as_i64().unwrap_or(-1));
                if idx.iter().all(|i| *i >= 0) {
                    tets.inner.push(idx.map(|i| i as usize));
                } else if idx[..3].iter().all(|i| *i >= 0) {
                    tets.outer.push([idx[0] as usize, idx[1] as usize, idx[2] as usize]);
                }
            }
            for r in v["m_Data"]["m_Tetrahedralization"]["m_HullRays"].as_array().into_iter().flatten() {
                let g = |k: &str| r[k].as_f64().unwrap_or(0.0) as f32;
                tets.rays.push(Vec3::new(g("m_X"), g("m_Y"), -g("m_Z")));
            }
        }
    }
    (probes, tets)
}

/// Diffuse environment light for a camera whose renderers sample a scene's light probes at `at` (port space): Unity
/// evaluates the probes once per renderer (bounds centre). `exposure`: the camera's view exposure (Bevy multiplies the
/// environment light by it; Unity's probe light is not exposed). Specular: black (no reflection probe).
pub(crate) fn probe_environment(images: &mut Assets<Image>, path: &str, at: Vec3, exposure: f32) -> Option<bevy::light::EnvironmentMapLight> {
    let (probes, tets) = load_light_probes(path, Vec3::ZERO);
    let sh = light_probe_sh(&probes, &tets, at)?;
    Some(sh_environment(images, sh, exposure))
}

/// Diffuse environment light of a light-probe SH (see [`probe_environment`]).
pub(crate) fn sh_environment(images: &mut Assets<Image>, sh: [Vec3; 4], exposure: f32) -> bevy::light::EnvironmentMapLight {
    use bevy::render::render_resource::{TextureViewDescriptor, TextureViewDimension};
    let mut cube = |n: usize, data: Vec<u8>| {
        let mut img = Image::new(Extent3d { width: n as u32, height: n as u32, depth_or_array_layers: 6 }, TextureDimension::D2, data, TextureFormat::Rgba16Float, RenderAssetUsages::default());
        img.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
        images.add(img)
    };
    let diffuse = cube(PROBE_CUBE, irradiance_cube_data(sh));
    let specular = cube(1, vec![0u8; 6 * 8]);
    bevy::light::EnvironmentMapLight { diffuse_map: diffuse, specular_map: specular, intensity: 1.0 / exposure, ..default() }
}

/// Turns the loaded strip into a cube texture and puts it on the world camera.
fn setup_sky(
    mut commands: Commands,
    sky: Option<ResMut<MapSky>>,
    mut images: ResMut<Assets<Image>>,
    cams: Query<Entity, With<crate::player::fpc::PlayerCamera>>,
) {
    let Some(mut sky) = sky else { return };
    if sky.ready {
        return;
    }
    let Ok(cam) = cams.single() else { return };
    let Some(mut img) = images.get_mut(&sky.image) else { return };
    // Unity face order +X -X +Y -Y +Z -Z; the port mirrors Z: sampling direction (x, y, z) must read Unity's
    // (x, y, -z). With the D3D / wgpu face layout that swaps +Z and -Z, and flips the X and Z faces horizontally and
    // the Y faces vertically (otherwise the face seams don't meet: a visible square around the sun face).
    let w = img.width() as usize;
    let face = w * w * 4;
    if let Some(data) = img.data.as_mut() {
        if data.len() >= face * 6 {
            let (a, b) = data.split_at_mut(face * 5);
            a[face * 4..face * 5].swap_with_slice(&mut b[..face]);
            for f in 0..6 {
                let fd = &mut data[f * face..(f + 1) * face];
                if f == 2 || f == 3 {
                    for r in 0..w / 2 {
                        let (top, bot) = fd.split_at_mut((w - 1 - r) * w * 4);
                        top[r * w * 4..(r + 1) * w * 4].swap_with_slice(&mut bot[..w * 4]);
                    }
                } else {
                    for r in 0..w {
                        let row = &mut fd[r * w * 4..(r + 1) * w * 4];
                        for c in 0..w / 2 {
                            for k in 0..4 {
                                row.swap(c * 4 + k, (w - 1 - c) * 4 + k);
                            }
                        }
                    }
                }
            }
        }
    }
    let _ = img.reinterpret_stacked_2d_as_array(6);
    img.texture_view_descriptor = Some(bevy::render::render_resource::TextureViewDescriptor {
        dimension: Some(bevy::render::render_resource::TextureViewDimension::Cube),
        ..default()
    });
    commands.entity(cam).insert(bevy::core_pipeline::Skybox {
        image: Some(sky.image.clone()),
        brightness: 1.0 / crate::unity_light::view_exposure(),
        ..default()
    });
    sky.ready = true;
}

/// Lighting by location: the test area keeps its sun and ambient; inside the map there is no realtime sun and
/// RenderSettings ambient is flat black, so dynamic objects get the light probes (L0 of the nearest probes).
fn map_lighting(
    players: Query<&GlobalTransform, With<crate::player::fpc::Fpc>>,
    probes: Option<Res<MapProbes>>,
    tets: Res<MapProbeTets>,
    time_secs: Res<Time>,
    mut sun: Query<(&mut DirectionalLight, &mut Transform, &mut bevy::light::CascadeShadowConfig), With<crate::map::TestSun>>,
    map_sun: Option<Res<MapSun>>,
    env_cams: Query<(Entity, Has<bevy::light::EnvironmentMapLight>), Or<(With<crate::player::fpc::PlayerCamera>, With<crate::weapons::markers::WeaponCam>)>>,
    mut env: ResMut<ProbeEnv>,
    hud_cams: Query<(Entity, Option<&bevy::light::EnvironmentMapLight>, &GlobalTransform), With<crate::hud::HudCamera>>,
    mut images: ResMut<Assets<Image>>,
    mut commands: Commands,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut was_in_test: Local<Option<bool>>,
    (settings, mut last_quality): (Res<crate::settings::Settings>, Local<Option<i32>>),
    mut shadow_map: ResMut<bevy::light::DirectionalLightShadowMap>,
) {
    let Some(probes) = probes else { return };
    let Ok(p) = players.single() else { return };
    let pos = p.translation();
    let in_test = crate::map::test_range() && pos.x.abs() < crate::map::TEST_AREA_HALF && pos.z.abs() < crate::map::TEST_AREA_HALF && pos.y > -5.0;
    let quality_changed = *last_quality != Some(settings.quality_setting);
    *last_quality = Some(settings.quality_setting);
    let switched = *was_in_test != Some(in_test) || map_sun.as_ref().is_some_and(|m| m.is_added()) || quality_changed;
    *was_in_test = Some(in_test);
    if switched && std::env::var_os("RESTRAFE_DEBUG_REFL").is_some() {
        println!("LIGHTSWITCH t={:.2} in_test={in_test} pos={pos:?}", time_secs.elapsed_secs());
    }
    if std::env::var_os("RESTRAFE_DEBUG_LIGHT").is_some() {
        for (s, tf, _) in &sun {
            println!("LIGHT t={:.2} sun={} shadows={} rot={:?} ambient={} envcams={:?}", time_secs.elapsed_secs(), s.illuminance, s.shadow_maps_enabled, tf.rotation, ambient.brightness, env_cams.iter().map(|(_, h)| h).collect::<Vec<_>>());
        }
    }
    if in_test || probes.0.is_empty() {
        for (e, has) in &env_cams {
            if has {
                commands.entity(e).remove::<bevy::light::EnvironmentMapLight>();
            }
        }
        if env.current_reflection.take().is_some() {
            if let Some(d) = env.hud_default.clone() {
                for (e, _, _) in &hud_cams {
                    commands.entity(e).insert(d.clone());
                }
            }
        }
        if switched {
            for (mut s, mut tf, mut cascades) in &mut sun {
                s.illuminance = crate::map::SUN_LUX;
                s.color = Color::WHITE;
                s.shadow_maps_enabled = std::env::var_os("RESTRAFE_NO_SHADOWS").is_none();
                tf.rotation = Quat::from_euler(EulerRot::YXZ, crate::map::SUN_YAW_PITCH.0, crate::map::SUN_YAW_PITCH.1, 0.0);
                *cascades = bevy::light::CascadeShadowConfigBuilder::default().build();
            }
        }
        ambient.color = Color::WHITE;
        ambient.brightness = crate::map::AMBIENT;
        return;
    }
    // Unity diffuse from a directional light: albedo * colour * intensity * NdotL (no 1/pi); Bevy: lux * exposure *
    // albedo / pi * NdotL. Shadows per QualitySettings level (Settings.qualitySetting -> SetQualityLevel):
    // Low / Medium: distance 20, 1 cascade, resolution Low; High: 40, 2, Medium; Ultra: 150, 4, High.
    let (shadow_distance, cascades_n, shadow_size) = match settings.quality_setting {
        0 | 1 => (20.0, 1, 1024),
        2 => (40.0, 2, 2048),
        _ => (150.0, 4, 4096),
    };
    if shadow_map.size != shadow_size {
        shadow_map.size = shadow_size;
    }
    if switched {
        for (mut s, mut tf, mut cascades) in &mut sun {
            match map_sun.as_deref() {
                Some(m) => {
                    s.illuminance = m.intensity * std::f32::consts::PI / crate::unity_light::view_exposure();
                    s.color = m.color;
                    s.shadow_maps_enabled = m.shadows && std::env::var_os("RESTRAFE_NO_SHADOWS").is_none();
                    tf.rotation = m.rotation;
                    info!("{MAP}: realtime sun {} lux, forward {:?}", s.illuminance, tf.forward());
                    *cascades = bevy::light::CascadeShadowConfigBuilder { num_cascades: cascades_n, maximum_distance: shadow_distance, ..default() }.build();
                }
                None => {
                    // No sun in the map (its Directional Light is inactive): no shadow cascades to render either.
                    s.illuminance = 0.0;
                    s.shadow_maps_enabled = false;
                }
            }
        }
    }
    // Inverse-distance weighting of the 4 nearest probes (Unity interpolates within the probe tetrahedron).
    // Unity: barycentric blend of the probe tetrahedron containing the object (here the player's chest / held items);
    // outside the hull (no inner tetrahedron) the 4 nearest probes, inverse-distance weighted.
    let at = pos + Vec3::Y;
    let (sh, inside) = probe_sh(&probes.0, &tets, at, 0..tets.inner.len());
    // Light probes are per pixel in Unity (ShadeSH9 with each surface normal): an irradiance cubemap of the interpolated
    // SH (ShadeSH9, see shade_sh9; n in Unity space, port z mirrored), used as the
    // world and weapon cameras' diffuse environment light (lightmapped meshes excluded, no specular: the map has no
    // reflection cubemap). Ambient is off inside the map (RenderSettings flat black).
    ambient.brightness = 0.0;
    if std::env::var_os("RESTRAFE_DEBUG_PROBES").is_some() && (switched || (time_secs.elapsed_secs() % 1.0) < 0.017) {
        let e = |d: Vec3| shade_sh9(&sh, Vec3::new(d.x, d.y, -d.z));
        println!("PROBES at {pos:?} inner={:?}: L0 {:?} up {:?} down {:?} +x {:?} -x {:?} +z {:?} -z {:?}", inside, sh[0], e(Vec3::Y), e(Vec3::NEG_Y), e(Vec3::X), e(Vec3::NEG_X), e(Vec3::Z), e(Vec3::NEG_Z));
    }
    if let Some(mut img) = images.get_mut(&env.diffuse) {
        img.data = Some(irradiance_cube_data(sh));
    }
    // Baked reflection probe around the camera: the smallest probe box containing it (Unity: equal importance, the
    // smaller volume wins), else the map-wide one.
    let cam_pos = hud_cams.iter().next().map(|(_, _, g)| g.translation()).unwrap_or(at);
    let probe = env
        .reflections
        .iter()
        .enumerate()
        .filter(|(_, r)| (cam_pos - r.0).abs().cmple(r.1).all())
        .min_by(|a, b| a.1 .1.length_squared().total_cmp(&b.1 .1.length_squared()))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let specular = env.reflections.get(probe).map(|r| r.2.clone()).unwrap_or_else(|| env.specular.clone());
    let reflection_changed = env.current_reflection != Some(probe);
    if reflection_changed && std::env::var_os("RESTRAFE_DEBUG_REFL").is_some() {
        println!("REFL t={:.2} probe {:?} -> {probe} cam={cam_pos:?}", time_secs.elapsed_secs(), env.current_reflection);
    }
    env.current_reflection = Some(probe);
    // HUD (UICam): light probes off (RenderSettings ambient probe) + the reflection probe; its test-area environment is
    // put back outside the map.
    for (e, hud_env, _) in &hud_cams {
        if env.hud_default.is_none() {
            env.hud_default = hud_env.cloned();
        }
        if reflection_changed || switched {
            commands.entity(e).insert(bevy::light::EnvironmentMapLight {
                diffuse_map: env.ambient.clone(),
                specular_map: specular.clone(),
                intensity: 1.0 / crate::unity_light::view_exposure(),
                ..default()
            });
        }
    }
    let light = bevy::light::EnvironmentMapLight {
        diffuse_map: env.diffuse.clone(),
        specular_map: specular,
        intensity: std::env::var("RESTRAFE_PROBE_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0) / crate::unity_light::view_exposure(),
        affects_lightmapped_mesh_diffuse: false,
        ..default()
    };
    for (e, has) in &env_cams {
        if !has || reflection_changed {
            commands.entity(e).insert(light.clone());
        }
    }
}

/// Light probe SH at `at`: barycentric blend of the probe tetrahedron containing it (candidates `inner`, indices into
/// `tets.inner`); outside the probe hull Unity's outer cells (hull triangles extruded along their hull rays); else the 4
/// nearest probes, inverse-distance weighted. Also returns the containing inner tetrahedron.
fn probe_sh(probes: &[(Vec3, [Vec3; 4])], tets: &MapProbeTets, at: Vec3, inner: impl Iterator<Item = usize>) -> ([Vec3; 4], Option<[usize; 4]>) {
    let mut inner = inner;
    let inside = inner.find_map(|k| {
        let t = tets.inner[k];
        let w = barycentric(at, probes[t[0]].0, probes[t[1]].0, probes[t[2]].0, probes[t[3]].0)?;
        w.iter().all(|x| *x >= -1e-4).then_some((t, w))
    });
    let sh = match inside {
        Some((t, w)) => {
            let mut sum = [Vec3::ZERO; 4];
            for i in 0..4 {
                for k in 0..4 {
                    sum[k] += probes[t[i]].1[k] * w[i];
                }
            }
            sum
        }
        None if !tets.outer.is_empty() && tets.rays.len() == probes.len() => {
            let cell = tets.outer.iter().find_map(|c| {
                let w = outer_cell_weights(at, c.map(|i| probes[i].0), c.map(|i| tets.rays[i]))?;
                Some((*c, w))
            });
            match cell {
                Some((c, w)) => {
                    let mut sum = [Vec3::ZERO; 4];
                    for i in 0..3 {
                        for k in 0..4 {
                            sum[k] += probes[c[i]].1[k] * w[i];
                        }
                    }
                    sum
                }
                None => nearest_probes(probes, at),
            }
        }
        None => nearest_probes(probes, at),
    };
    (sh, inside.map(|i| i.0))
}

/// Light-probe SH (L0 + L1, Unity's interpolation) at a port-space position, for per-renderer lighting outside Bevy's
/// lighting (FXVille blood's ShadeSH9 vertex term).
pub(crate) fn light_probe_sh(probes: &MapProbes, tets: &MapProbeTets, at: Vec3) -> Option<[Vec3; 4]> {
    if probes.0.is_empty() {
        return None;
    }
    Some(probe_sh(&probes.0, tets, at, 0..tets.inner.len()).0)
}

/// TrainingRange_00.unity RenderSettings m_AmbientSkyColor / m_AmbientEquatorColor / m_AmbientGroundColor (sRGB; flat
/// mode): Unity's unity_AmbientSky / Equator / Ground.
pub(crate) const MAP_AMBIENT_SRGB: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [0.114, 0.125, 0.133], [0.047, 0.043, 0.035]];

/// Unity samples the light probes per renderer (at its bounds centre), so a dynamic object's light does not depend on
/// where the player stands. The interpolated probe SH is baked once into a Bevy irradiance volume over the probe bounds
/// (ambient cube per voxel: E(+-X), E(+-Y), E(+-Z) of the L0 + L1 SH, sampled per fragment and trilinear between
/// voxels); lightmapped meshes keep ignoring it. The per-camera environment diffuse (player position) only remains the
/// fallback outside the volume.
fn bake_irradiance_volume(
    probes: Option<Res<MapProbes>>,
    tets: Res<MapProbeTets>,
    mut images: ResMut<Assets<Image>>,
    mut commands: Commands,
    mut job: Local<Option<std::sync::mpsc::Receiver<ProbeVolumeBake>>>,
    mut done: Local<bool>,
    rapier: ReadRapierContext,
    map_ready: Query<(), With<MapSetupDone>>,
) {
    if *done {
        return;
    }
    // Baked on a worker thread (a few seconds of probe interpolation) so the map load does not stall.
    if job.is_none() {
        let Some(probes) = probes else { return };
        if probes.0.is_empty() || tets.inner.is_empty() || map_ready.is_empty() {
            return;
        }
        let Ok(ctx) = rapier.single() else { return };
        // The volume must cover every place a dynamic object can be: inside a view with an irradiance volume, Bevy
        // gives fragments outside all volumes no diffuse light at all (black). So it spans the map's colliders, not
        // just the probes (outside the probe hull the bake extrapolates like Unity: outer cells, nearest probes).
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for (p, _) in &probes.0 {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        // Playable space: the probe bounds grown by 20 m sideways / 6 m vertically (barrels stand up to ~8 m outside
        // the probe hull), clipped to the map's colliders (distant terrain and skirt colliders would make the voxels
        // coarse).
        let (plo, phi) = (lo - Vec3::new(20.0, 6.0, 20.0), hi + Vec3::new(20.0, 6.0, 20.0));
        let (mut clo, mut chi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for (_, c) in ctx.colliders.colliders.iter() {
            let a = c.compute_aabb();
            clo = clo.min(Vec3::new(a.mins.x, a.mins.y, a.mins.z));
            chi = chi.max(Vec3::new(a.maxs.x, a.maxs.y, a.maxs.z));
        }
        let bounds = (plo.max(clo).min(lo), phi.min(chi).max(hi));
        let (mut slo, mut shi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for (_, c) in ctx.colliders.colliders.iter() {
            let a = c.compute_aabb();
            let (mn, mx) = (Vec3::new(a.mins.x, a.mins.y, a.mins.z), Vec3::new(a.maxs.x, a.maxs.y, a.maxs.z));
            if (mx - mn).max_element() < 150.0 {
                slo = slo.min(mn);
                shi = shi.max(mx);
            }
        }
        info!("{MAP}: probe bounds {lo:?}..{hi:?}, collider union {clo:?}..{chi:?}, small colliders {slo:?}..{shi:?}");
        let (tx, rx) = std::sync::mpsc::channel();
        let (p, t) = (probes.0.clone(), tets.clone());
        std::thread::spawn(move || {
            let _ = tx.send(bake_probe_volume(&p, &t, bounds));
        });
        *job = Some(rx);
        return;
    }
    let Ok(b) = job.as_ref().unwrap().try_recv() else { return };
    *done = true;
    let img = Image::new(
        Extent3d { width: b.res.x, height: 2 * b.res.y, depth_or_array_layers: 3 * b.res.z },
        TextureDimension::D3,
        b.data,
        TextureFormat::Rgba16Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    let debug_scale = std::env::var("RESTRAFE_PROBE_VOLUME_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
    let intensity = debug_scale * std::env::var("RESTRAFE_PROBE_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0) / crate::unity_light::view_exposure();
    if std::env::var_os("RESTRAFE_NO_PROBE_VOLUME").is_none() {
        commands.spawn((
            Name::new("LightProbeVolume"),
            bevy::light::LightProbe::default(),
            bevy::light::IrradianceVolume { voxels: images.add(img), intensity, affects_lightmapped_meshes: false },
            Transform::from_translation((b.lo + b.hi) * 0.5).with_scale(b.hi - b.lo),
        ));
    }
    info!("{MAP}: light probe volume {:?} voxels ({:.2} m) over {:?}..{:?}, baked in {:.0} ms", b.res, b.spacing, b.lo, b.hi, b.ms);
}

/// Irradiance-volume texture data (see [`bake_irradiance_volume`]).
struct ProbeVolumeBake {
    data: Vec<u8>,
    res: UVec3,
    lo: Vec3,
    hi: Vec3,
    spacing: f32,
    ms: f32,
}

/// Bevy irradiance-volume layout: a 3D texture (Rx, 2Ry, 3Rz), +X/-X, +Y/-Y, +Z/-Z ambient-cube sides stacked (negative
/// side in the upper half of t, as the shader samples it; axis slab in p).
fn bake_probe_volume(probes: &[(Vec3, [Vec3; 4])], tets: &MapProbeTets, (lo, hi): (Vec3, Vec3)) -> ProbeVolumeBake {
    let t0 = std::time::Instant::now();
    let size = hi - lo;
    // Voxel edge: ~1 m, coarser for huge bounds (at most ~800k voxels, each 3D texture side <= 2048).
    let spacing = (size.x * size.y * size.z / 800_000.0).cbrt().max(1.0);
    let res = (size / spacing).ceil().max(Vec3::ONE).as_uvec3().min(UVec3::new(2048, 1024, 682));
    // Tetrahedron buckets on a coarse grid (cell 4 m) so a voxel only tests nearby tetrahedra.
    const CELL: f32 = 4.0;
    let cres = (size / CELL).ceil().max(Vec3::ONE).as_uvec3();
    let cidx = |p: Vec3| -> UVec3 { ((p - lo) / CELL).floor().max(Vec3::ZERO).as_uvec3().min(cres - UVec3::ONE) };
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); (cres.x * cres.y * cres.z) as usize];
    for (k, t) in tets.inner.iter().enumerate() {
        let ps = t.map(|i| probes[i].0);
        let (a, b) = (cidx(ps.iter().fold(Vec3::splat(f32::MAX), |m, p| m.min(*p))), cidx(ps.iter().fold(Vec3::splat(f32::MIN), |m, p| m.max(*p))));
        for z in a.z..=b.z {
            for y in a.y..=b.y {
                for x in a.x..=b.x {
                    buckets[((z * cres.y + y) * cres.x + x) as usize].push(k);
                }
            }
        }
    }
    let (rx, ry, rz) = (res.x as usize, res.y as usize, res.z as usize);
    let (w, h) = (rx, 2 * ry);
    let mut data = vec![0u8; rx * 2 * ry * 3 * rz * 8];
    let mut put = |s: usize, t: usize, p: usize, c: Vec3| {
        let i = ((p * h + t) * w + s) * 8;
        for (j, v) in [c.x, c.y, c.z, 1.0].into_iter().enumerate() {
            data[i + j * 2..i + j * 2 + 2].copy_from_slice(&half::f16::from_f32(v).to_le_bytes());
        }
    };
    // Port direction d -> Unity normal (d.x, d.y, -d.z).
    let e = |sh: &[Vec3; 4], d: Vec3| shade_sh9(sh, Vec3::new(d.x, d.y, -d.z));
    let step = size / res.as_vec3();
    for z in 0..rz {
        for y in 0..ry {
            for x in 0..rx {
                let at = lo + (Vec3::new(x as f32, y as f32, z as f32) + 0.5) * step;
                let c = cidx(at);
                let cand = &buckets[((c.z * cres.y + c.y) * cres.x + c.x) as usize];
                let (sh, _) = probe_sh(probes, tets, at, cand.iter().copied());
                // irradiance_volume.wgsl reads the negative side from the upper half (neg_offset 0.5 for N < 0); the
                // module docs state the opposite.
                put(x, y, z, e(&sh, Vec3::X));
                put(x, y + ry, z, e(&sh, Vec3::NEG_X));
                put(x, y, z + rz, e(&sh, Vec3::Y));
                put(x, y + ry, z + rz, e(&sh, Vec3::NEG_Y));
                put(x, y, z + 2 * rz, e(&sh, Vec3::Z));
                put(x, y + ry, z + 2 * rz, e(&sh, Vec3::NEG_Z));
            }
        }
    }
    ProbeVolumeBake { data, res, lo, hi, spacing, ms: t0.elapsed().as_secs_f32() * 1000.0 }
}

/// Built-in ShadeSH9 of a baked SphericalHarmonicsL2 (L0 + L1; the L2 band is empty in this map): unity_SHAr/g/b =
/// (sh[3], sh[1], sh[2], sh[0] - sh[6]), so E(n) = L00 + L1-1 n.y + L10 n.z + L11 n.x with n in Unity space. The stored
/// coefficients are already normalized for irradiance (SphericalHarmonicsL2.AddAmbientLight adds the colour to L00).
pub(crate) fn shade_sh9(sh: &[Vec3; 4], n: Vec3) -> Vec3 {
    (sh[0] + sh[1] * n.y + sh[2] * n.z + sh[3] * n.x).max(Vec3::ZERO)
}

/// Irradiance cubemap data (Rgba16Float, PROBE_CUBE^2 x 6) of an L0 + L1 SH ([`shade_sh9`]).
fn irradiance_cube_data(sh: [Vec3; 4]) -> Vec<u8> {
    let n = PROBE_CUBE;
    let mut data = Vec::with_capacity(n * n * 6 * 8);
    for f in 0..6 {
        for y in 0..n {
            for x in 0..n {
                // Bevy samples cubemaps with the world direction's z negated (left-handed cube space): a texel
                // direction is the port direction with z flipped, i.e. the Unity direction itself.
                let d = cube_dir(f, x, y, n);
                let e = shade_sh9(&sh, d);
                for c in [e.x, e.y, e.z, 1.0] {
                    data.extend_from_slice(&half::f16::from_f32(c).to_le_bytes());
                }
            }
        }
    }
    data
}

/// Direction of texel (x, y) of face f (wgpu / D3D cube layout +X -X +Y -Y +Z -Z).
fn cube_dir(f: usize, x: usize, y: usize, n: usize) -> Vec3 {
    let u = (x as f32 + 0.5) / n as f32 * 2.0 - 1.0;
    let v = (y as f32 + 0.5) / n as f32 * 2.0 - 1.0;
    match f {
        0 => Vec3::new(1.0, -v, -u),
        1 => Vec3::new(-1.0, -v, u),
        2 => Vec3::new(u, 1.0, v),
        3 => Vec3::new(u, -1.0, -v),
        4 => Vec3::new(u, -v, 1.0),
        _ => Vec3::new(-u, -v, -1.0),
    }
    .normalize()
}

/// RenderSettings.m_AmbientProbe of TrainingRange_00 (L00 / L1-1 / L10 / L11, rgb; read from the built scene, it is not
/// in the YAML export): the ambient of renderers with light probes off (most HUD parts).
const AMBIENT_PROBE: [[f32; 3]; 4] = [
    [0.136_833_65, 0.129_256_13, 0.113_024_876],
    [0.075_196_564, 0.070_982_29, 0.062_011_424],
    [0.043_230_17, 0.037_694_67, 0.030_290_753],
    [0.005_089_358_4, 0.005_204_553, 0.004_646_137],
];

/// Baked reflection probes (Unity world box centre, box size): `Reflection Probe` (map-wide, ReflectionProbe-0) and
/// `---LIGHTING---/Reflection Probe` (ReflectionProbe-1). Box sizes from the built ReflectionProbe components (m_BoxSize,
/// not in the YAML export).
const REFLECTION_PROBES: [(&str, [f32; 3], [f32; 3]); 2] = [
    ("TrainingRange_00_ReflectionProbe-0", [-29.145_952, -5.413_74, 109.600_12], [491.22, 498.56, 806.65]),
    ("TrainingRange_00_ReflectionProbe-1", [-3.882_33, 9.239_784, -1.582_353_5], [67.11, 19.92, 58.0]),
];

/// A baked reflection cubemap (AssetRipper strip: 6 faces stacked +X -X +Y -Y +Z -Z) as a linear Rgba16Float cubemap with
/// the port's z mirror (the skybox face fix) and a box-filtered mip chain (rough surfaces sample the low mips).
fn load_reflection_cube(path: &str) -> Option<Image> {
    use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
    use bevy::render::render_resource::{TextureViewDescriptor, TextureViewDimension};
    let src = match std::fs::read(path) {
        Ok(bytes) => Image::from_buffer(&bytes, ImageType::Extension("png"), CompressedImageFormats::NONE, true, ImageSampler::Default, RenderAssetUsages::default()).ok()?.convert(TextureFormat::Rgba8UnormSrgb)?,
        Err(_) => {
            // Not exported: the scene's baked probe Cubemap read in place ("<scene>_ReflectionProbe-<n>" -> the
            // "ReflectionProbe-<n>" stored with that scene's level).
            let stem = path.rsplit('/').next()?.trim_end_matches(".png");
            let (scene, probe) = stem.split_once("_Reflection").map(|(a, b)| (a, format!("Reflection{b}")))?;
            let owned = crate::owned::Owned::global()?;
            let file = owned.scene_level(scene).map(|l| owned.0.bundle.nodes[l].path.replace("level", "sharedassets") + ".assets");
            let (w, strip, _) = owned.cubemap_strip(&probe, file.as_deref()).map_err(|e| warn!("{path}: {e}")).ok()?;
            Image::new(Extent3d { width: w, height: w * 6, depth_or_array_layers: 1 }, TextureDimension::D2, strip, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default())
        }
    };
    let w = src.width() as usize;
    if src.height() as usize != w * 6 {
        return None;
    }
    let data = src.data.as_ref()?;
    let lin = |c: u8| crate::tmp_text::gamma_to_linear(c as f32 / 255.0);
    // Port face f texel (x, y) <- Unity face (+Z/-Z swapped) texel mirrored (X and Z faces horizontally, Y vertically).
    let mut faces: Vec<Vec<Vec3>> = vec![vec![Vec3::ZERO; w * w]; 6];
    for (f, face) in faces.iter_mut().enumerate() {
        let uf = match f {
            4 => 5,
            5 => 4,
            k => k,
        };
        for y in 0..w {
            for x in 0..w {
                let (ux, uy) = if f == 2 || f == 3 { (x, w - 1 - y) } else { (w - 1 - x, y) };
                let i = ((uf * w + uy) * w + ux) * 4;
                face[y * w + x] = Vec3::new(lin(data[i]), lin(data[i + 1]), lin(data[i + 2]));
            }
        }
    }
    let mut levels = vec![faces];
    loop {
        let prev = levels.last().unwrap();
        let pw = (prev[0].len() as f32).sqrt().round() as usize;
        if pw <= 1 {
            break;
        }
        let nw = pw / 2;
        let next: Vec<Vec<Vec3>> = prev
            .iter()
            .map(|face| {
                (0..nw * nw)
                    .map(|i| {
                        let (x, y) = (i % nw * 2, i / nw * 2);
                        (face[y * pw + x] + face[y * pw + x + 1] + face[(y + 1) * pw + x] + face[(y + 1) * pw + x + 1]) * 0.25
                    })
                    .collect()
            })
            .collect();
        levels.push(next);
    }
    // Layer-major: each face with all of its mips.
    let mut out = Vec::new();
    for f in 0..6 {
        for level in &levels {
            for c in &level[f] {
                for v in [c.x, c.y, c.z, 1.0] {
                    out.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
                }
            }
        }
    }
    // Image::new checks the data against the base level only: build it at that size, then put in the full chain.
    let mut img = Image::new_fill(
        Extent3d { width: w as u32, height: w as u32, depth_or_array_layers: 6 },
        TextureDimension::D2,
        &[0u8; 8],
        TextureFormat::Rgba16Float,
        RenderAssetUsages::default(),
    );
    img.data = Some(out);
    img.texture_descriptor.mip_level_count = levels.len() as u32;
    img.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
    Some(img)
}

const PROBE_CUBE: usize = 8;

/// The light-probe irradiance cubemap (diffuse) and a black specular cubemap for the map's dynamic objects.
#[derive(Resource)]
struct ProbeEnv {
    diffuse: Handle<Image>,
    specular: Handle<Image>,
    /// RenderSettings ambient probe irradiance (HUD diffuse).
    ambient: Handle<Image>,
    /// (box centre, half size, cubemap) in port space.
    reflections: Vec<(Vec3, Vec3, Handle<Image>)>,
    current_reflection: Option<usize>,
    hud_default: Option<bevy::light::EnvironmentMapLight>,
}

fn setup_probe_env(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    use bevy::render::render_resource::{TextureViewDescriptor, TextureViewDimension};
    let mut cube = |n: usize| {
        let mut img = Image::new(
            Extent3d { width: n as u32, height: n as u32, depth_or_array_layers: 6 },
            TextureDimension::D2,
            vec![0u8; n * n * 6 * 8],
            TextureFormat::Rgba16Float,
            RenderAssetUsages::default(),
        );
        img.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
        images.add(img)
    };
    let (diffuse, specular, ambient) = (cube(PROBE_CUBE), cube(1), cube(PROBE_CUBE));
    if let Some(mut img) = images.get_mut(&ambient) {
        img.data = Some(irradiance_cube_data(AMBIENT_PROBE.map(Vec3::from_array)));
    }
    let reflections = REFLECTION_PROBES
        .iter()
        .filter_map(|(name, c, size)| {
            let Some(img) = load_reflection_cube(&format!("assets/maps/reflection/{name}.png")) else {
                warn!("{MAP}: reflection probe {name} missing (run setup)");
                return None;
            };
            Some((Vec3::new(c[0], c[1], -c[2]) + MAP_OFFSET, Vec3::from_array(*size) * 0.5, images.add(img)))
        })
        .collect();
    commands.insert_resource(ProbeEnv { diffuse, specular, ambient, reflections, current_reflection: None, hud_default: None });
}

/// Whether the player (re)spawns at the map's SpawnPoint: always in normal play; autotests keep the test area spawn
/// (their scripted positions), except `mapview`.
pub fn spawn_in_map() -> bool {
    match std::env::var("RESTRAFE_AUTOTEST") {
        Ok(name) => name == "mapview" || name == "door" || name == "pigeon" || name == "wallshot" || name == "walk" || name == "weaponmenu" || name == "steep" || name == "mapdisc",
        Err(_) => true,
    }
}

/// Moves the player to the map's SpawnPoint once the map is set up (the player is created at startup).
/// Debug: RESTRAFE_PROBE_BALLS=x,y,z[;x,y,z..]: white diffuse spheres (0.5 m) once the map is set up, to see the light
/// probe lighting's direction.
fn probe_balls(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>, ready: Query<(), With<MapSetupDone>>, mut done: Local<bool>) {
    let Ok(spec) = std::env::var("RESTRAFE_PROBE_BALLS") else { return };
    if *done || ready.is_empty() {
        return;
    }
    *done = true;
    let mesh = meshes.add(Sphere::new(0.5).mesh().uv(48, 24));
    let mat = mats.add(StandardMaterial { base_color: Color::WHITE, perceptual_roughness: 1.0, reflectance: 0.0, ..default() });
    for p in spec.split(';') {
        let v: Vec<f32> = p.split(',').filter_map(|x| x.parse().ok()).collect();
        if v.len() == 3 {
            commands.spawn((Name::new("ProbeBall"), Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), Transform::from_xyz(v[0], v[1], v[2])));
        }
    }
}

fn place_player_at_spawn(
    spawn: Option<Res<MapSpawn>>,
    mut players: Query<(&mut Transform, &mut crate::player::fpc::Fpc)>,
    mut done: Local<bool>,
) {
    if *done || !spawn_in_map() {
        return;
    }
    let Ok((mut tf, mut fpc)) = players.single_mut() else { return };
    // Until the map is set up there is nothing to stand on: hold the player still (no fall, no landing sound).
    fpc.move_direction = Vec3::ZERO;
    fpc.fall_distance = 0.0;
    let Some(sp) = spawn.and_then(|s| s.0) else {
        tf.translation = crate::death::PLAYER_SPAWN;
        return;
    };
    tf.translation = sp.translation;
    tf.rotation = sp.rotation;
    fpc.rotation_x = 0.0;
    *done = true;
}

/// A `Killz` trigger box (half extents in its local space).
#[derive(Component)]
struct KillZone {
    half: Vec3,
}

/// FirstPersonController.OnTriggerEnter(Killz): suicide stat, fellVoid, DespawnObject; Weapon.OnTriggerEnter(Killz):
/// Invoke("DespawnObject", 0.65) (the Weapon is destroyed; no respawn).
fn kill_zones(
    zones: Query<(Entity, &KillZone, &GlobalTransform)>,
    mut players: Query<(&GlobalTransform, &mut crate::health::PlayerHealth, &crate::player::fpc::Fpc)>,
    mut items: Query<&mut crate::weapons::Item>,
    rapier: ReadRapierContext,
) {
    // The CharacterController capsule overlapping the (scaled, rotated) trigger box. The map's Killz is a 0.01 m thick
    // slab: a point test misses it at falling speeds, the capsule (2 m) overlaps it on some frame as in PhysX.
    let capsule_inside = |a: Vec3, b: Vec3, r: f32| zones.iter().any(|(_, z, g)| {
        let (scale, rot, t) = g.to_scale_rotation_translation();
        let half = z.half * scale.abs();
        let inv = rot.inverse();
        let (la, lb) = (inv * (a - t), inv * (b - t));
        let d2 = |s: f32| {
            let p = la.lerp(lb, s);
            (p.abs() - half).max(Vec3::ZERO).length_squared()
        };
        // Distance from the segment to the box is convex along the segment: ternary search.
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let (m1, m2) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
            if d2(m1) <= d2(m2) {
                hi = m2;
            } else {
                lo = m1;
            }
        }
        d2(0.5 * (lo + hi)).min(d2(0.0)).min(d2(1.0)) <= r * r
    });
    if zones.is_empty() {
        return;
    }
    if let Ok((g, mut ph, f)) = players.single_mut() {
        let r = f.p.cc_radius;
        let c = g.translation() + f.cc_center;
        let h = (f.cc_height * 0.5 - r).max(0.0);
        if ph.health > 0.0 && capsule_inside(c - Vec3::Y * h, c + Vec3::Y * h, r) {
            ph.fell_void = true;
            ph.killed = true;
            ph.health = -8.0;
        }
    }
    // Items: the Killz sensor's narrow-phase intersections, i.e. the item's collider overlapping the box at a physics
    // step (PhysX trigger pairs; CCD never applies to triggers, and nothing is swept between steps), filtered by the
    // layer collision matrix through the colliders' groups. The item collider sits on the Item entity.
    let Ok(ctx) = rapier.single() else { return };
    for (ze, _, _) in &zones {
        for (a, b, hit) in ctx.intersection_pairs_with(ze) {
            let other = if a == ze { b } else { a };
            if let (true, Ok(mut it)) = (hit, items.get_mut(other)) {
                if it.physics && it.despawn_in.is_none() {
                    if std::env::var_os("RESTRAFE_KILLZ_LOG").is_some() {
                        println!("KILLZ item {} despawn in 0.65 s", it.def.name);
                    }
                    it.despawn_in = Some(0.65);
                }
            }
        }
    }
}

/// Debug: RESTRAFE_RAYTEST=x,y,z casts down from that point every 2 s once the map is set up and prints the hit.
fn ray_test(
    time: Res<Time>,
    rapier: ReadRapierContext,
    names: Query<&Name>,
    parents: Query<&ChildOf>,
) {
    let Ok(spec) = std::env::var("RESTRAFE_RAYTEST") else { return };
    if (time.elapsed_secs() / 2.0).floor() == ((time.elapsed_secs() - time.delta_secs()) / 2.0).floor() {
        return;
    }
    let v: Vec<f32> = spec.split(',').filter_map(|s| s.parse().ok()).collect();
    if v.len() != 3 {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let o = Vec3::new(v[0], v[1], v[2]);
    match crate::physics::raycast(&ctx, o, Vec3::NEG_Y, 200.0, u32::MAX) {
        Some(h) => {
            let mut n = names.get(h.entity).map(|n| n.to_string()).unwrap_or_default();
            if let Ok(p) = parents.get(h.entity) {
                n = format!("{} <- {}", n, names.get(p.parent()).map(|n| n.to_string()).unwrap_or_default());
            }
            println!("RAYTEST t={:.1} hit {:?} n {:?} dist {:.2} {}", time.elapsed_secs(), h.point, h.normal, h.distance, n);
        }
        None => println!("RAYTEST t={:.1} no hit", time.elapsed_secs()),
    }
}

/// MeshCollider shape from `maps/mesh/<Mesh>.asset` (the exact Mesh asset; the glTF export names duplicates differently).
fn mesh_collider(file: &str, convex: bool, cache: &mut HashMap<(String, bool), Option<Collider>>) -> Option<Collider> {
    cache
        .entry((file.to_string(), convex))
        .or_insert_with(|| {
            let Some(meshes) = crate::ugui_assets::decode_mesh(&format!("assets/maps/mesh/{file}")) else {
                warn!("map MeshCollider {file}: mesh not decoded");
                return None;
            };
            let mut verts: Vec<Vec3> = Vec::new();
            let mut tris: Vec<[u32; 3]> = Vec::new();
            for mesh in &meshes {
                let Some(bevy::mesh::VertexAttributeValues::Float32x3(pos)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else { continue };
                let base = verts.len() as u32;
                verts.extend(pos.iter().map(|p| Vec3::from_array(*p)));
                let idx: Vec<u32> = match mesh.indices() {
                    Some(i) => i.iter().map(|v| v as u32).collect(),
                    None => (0..pos.len() as u32).collect(),
                };
                tris.extend(idx.chunks_exact(3).map(|c| [base + c[0], base + c[1], base + c[2]]));
            }
            if verts.is_empty() {
                warn!("map MeshCollider {file}: no vertices");
                return None;
            }
            // Internal edges of the big level trimeshes otherwise catch the character controller (ramps you cannot walk up,
            // launches when walking down): merge the split vertices and let parry fix internal-edge normals.
            if convex { Collider::convex_hull(&verts) } else { Collider::trimesh_with_flags(verts, tris, TriMeshFlags::MERGE_DUPLICATE_VERTICES | TriMeshFlags::FIX_INTERNAL_EDGES).ok() }
        })
        .clone()
}

/// Links a map collider to the scripted node it belongs to: doors (InteractEnvironment focus / interact) and pigeons
/// (Weapon.TriggerEnvironment: the hit collider's GameObject must carry the script).
fn link_collider(c: &mut EntityCommands, node: Entity, target: Option<(bool, bool)>) {
    match target {
        Some((true, _)) => {
            c.insert(crate::props::EnvTarget(node));
        }
        Some((_, true)) => {
            c.insert(crate::props::PropPart(node));
        }
        _ => {}
    }
}

/// `RESTRAFE_HEIGHTMAP=x0,z0,x1,z1,step`: prints a grid of downward-ray hit heights (debug, once).
fn height_map(rapier: ReadRapierContext, done: Option<Single<&MapGlb, With<MapSetupDone>>>, mut printed: Local<u32>, names: Query<&Name>) {
    let Ok(spec) = std::env::var("RESTRAFE_HEIGHTMAP") else { return };
    if done.is_none() {
        return;
    }
    *printed += 1;
    if *printed != 30 {
        return;
    }
    let v: Vec<f32> = spec.split(',').filter_map(|s| s.parse().ok()).collect();
    let Ok(ctx) = rapier.single() else { return };
    let mut z = v[1];
    while z <= v[3] {
        let mut row = format!("z={z:>7.1}:");
        let mut x = v[0];
        while x <= v[2] {
            match crate::physics::raycast(&ctx, Vec3::new(x, v.get(5).copied().unwrap_or(300.0), z), Vec3::NEG_Y, 600.0, 1) {
                Some(h) => {
                    let n = names.get(h.entity).map(|n| n.to_string()).unwrap_or_default();
                    let tag = if n.starts_with("collider:") { "" } else { "*" };
                    row += &format!(" {:>6.1}{tag}", h.point.y);
                }
                None => row += "      -",
            }
            x += v[4];
        }
        println!("HMAP {row}");
        z += v[4];
    }
    println!("HMAP x from {} step {}", v[0], v[4]);
}

/// `RESTRAFE_WALLSCAN=x,y,z`: prints 24 horizontal ray hits around that point (debug, once).
fn wall_scan(rapier: ReadRapierContext, done: Option<Single<&MapGlb, With<MapSetupDone>>>, mut printed: Local<u32>, names: Query<&Name>) {
    let Ok(spec) = std::env::var("RESTRAFE_WALLSCAN") else { return };
    if done.is_none() {
        return;
    }
    *printed += 1;
    if *printed != 30 {
        return;
    }
    let v: Vec<f32> = spec.split(',').filter_map(|s| s.parse().ok()).collect();
    let Ok(ctx) = rapier.single() else { return };
    let o = Vec3::new(v[0], v[1], v[2]);
    for k in 0..24 {
        let a = (k as f32 * 15.0).to_radians();
        let d = Vec3::new(a.sin(), 0.0, -a.cos());
        match crate::physics::raycast(&ctx, o, d, 100.0, 1) {
            Some(h) => println!("WALL dir {:>3} deg -> {:.2} m at {:?} n {:?} {}", k * 15, h.distance, h.point, h.normal, names.get(h.entity).map(|n| n.to_string()).unwrap_or_default()),
            None => println!("WALL dir {:>3} deg -> none", k * 15),
        }
    }
}

/// `RESTRAFE_WHATS_AT=x,y,z`: prints the rendered map primitives whose world AABB contains that point (debug, once).
fn whats_at(
    done: Option<Single<&MapGlb, With<MapSetupDone>>>,
    mut printed: Local<u32>,
    prims: Query<(Entity, &bevy::camera::primitives::Aabb, &GlobalTransform, &ChildOf), With<Mesh3d>>,
    names: Query<&Name>,
    parents: Query<&ChildOf>,
) {
    let Ok(spec) = std::env::var("RESTRAFE_WHATS_AT") else { return };
    if done.is_none() {
        return;
    }
    *printed += 1;
    if *printed != 30 {
        return;
    }
    let v: Vec<f32> = spec.split(',').filter_map(|s| s.parse().ok()).collect();
    let p = Vec3::new(v[0], v[1], v[2]);
    for (e, aabb, g, parent) in &prims {
        let l = g.affine().inverse().transform_point3(p) - Vec3::from(aabb.center);
        let h = Vec3::from(aabb.half_extents) + Vec3::splat(0.3);
        if l.abs().cmple(h).all() {
            let mut chain = vec![names.get(e).map(|n| n.to_string()).unwrap_or_default()];
            let mut cur = Some(parent.parent());
            while let Some(c) = cur {
                chain.push(names.get(c).map(|n| n.to_string()).unwrap_or_default());
                cur = parents.get(c).ok().map(|p| p.parent());
                if chain.len() > 4 {
                    break;
                }
            }
            println!("WHATSAT {}", chain.join(" <- "));
        }
    }
}

/// `RESTRAFE_LM_DEBUG`: lists lightmapped primitives whose mesh has no second UV set (debug, once).
fn lightmap_uv_check(mut printed: Local<u32>, q: Query<(&Mesh3d, &ChildOf), With<Lightmap>>, meshes: Res<Assets<Mesh>>, names: Query<&Name>) {
    if std::env::var_os("RESTRAFE_LM_DEBUG").is_none() || q.is_empty() {
        return;
    }
    *printed += 1;
    if *printed != 60 {
        return;
    }
    let (mut ok, mut missing) = (0, 0);
    for (m, p) in &q {
        match meshes.get(&m.0) {
            Some(mesh) if mesh.attribute(Mesh::ATTRIBUTE_UV_1).is_some() => ok += 1,
            Some(_) => {
                missing += 1;
                println!("LMUV missing uv1: {}", names.get(p.parent()).map(|n| n.to_string()).unwrap_or_default());
            }
            None => {}
        }
    }
    println!("LMUV ok={ok} missing={missing}");
}

struct PendingText {
    node: Entity,
    fields: crate::weapons::data::Fields,
    size: Vec2,
    pivot: Vec2,
}

#[derive(Resource)]
struct PendingTexts(Vec<PendingText>);

/// TextMeshPro (3D) components of the map: text, font asset + material, size, alignment, margins, spacing and
/// word wrapping (TMP_Text.GenerateTextMesh via `layout_ugui`; non-orthographic texts scale fonts by 0.1).
fn map_texts(
    mut commands: Commands,
    pending: Option<ResMut<PendingTexts>>,
    mut tmp: ResMut<crate::tmp_text::TmpAssets>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<crate::tmp_text::TmpSdfMaterial>>,
) {
    use crate::tmp_text::*;
    let Some(mut pending) = pending else { return };
    for t in std::mem::take(&mut pending.0) {
        let f = &t.fields;
        let asset = |k: &str| f.get(k).and_then(|v| v.as_str()).and_then(|s| s.rsplit('/').next()).map(|s| s.trim_end_matches(".asset").trim_end_matches(".mat").to_string());
        let (Some(font), Some(mat)) = (asset("m_fontAsset").and_then(|n| tmp.font(&n, &mut images)), asset("m_sharedMaterial").and_then(|n| tmp.material(&n))) else {
            warn!("{MAP}: TMP font / material missing for {:?}", f.get("m_text"));
            continue;
        };
        let num = |k: &str| f.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let world = if f.get("m_isOrthographic").and_then(|v| v.as_i64()) == Some(1) { 1.0 } else { 0.1 };
        let m = f.get("m_margin");
        let mg = |k: &str| m.and_then(|m| m.get(k)).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let l = TmpUiLayout {
            font_size: num("m_fontSize") * world,
            rect_size: t.size,
            pivot: t.pivot,
            margin: Vec4::new(mg("x"), mg("y"), mg("z"), mg("w")),
            h_align: f.get("m_HorizontalAlignment").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
            v_align: f.get("m_VerticalAlignment").and_then(|v| v.as_u64()).unwrap_or(256) as u32,
            char_spacing: num("m_characterSpacing") * world,
            word_spacing: num("m_wordSpacing") * world,
            line_spacing: num("m_lineSpacing") * world,
            paragraph_spacing: num("m_paragraphSpacing") * world,
            wrap: f.get("m_enableWordWrapping").and_then(|v| v.as_i64()) != Some(0),
            style: f.get("m_fontStyle").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            grad_ratio: mat.grad_ratio(),
        };
        let text = f.get("m_text").and_then(|v| v.as_str()).unwrap_or("");
        let rgba = f.get("m_fontColor32").and_then(|c| c.get("rgba")).and_then(|v| v.as_u64()).unwrap_or(0xffff_ffff) as u32;
        let color = [(rgba & 255) as f32 / 255.0, ((rgba >> 8) & 255) as f32 / 255.0, ((rgba >> 16) & 255) as f32 / 255.0, (rgba >> 24) as f32 / 255.0];
        let quads = layout_ugui(&font, mat.padding(), &l, text);
        let material = mats.add(TmpSdfMaterial { params: mat.params(), atlas: font.atlas.clone() });
        let child = commands.spawn((Name::new("tmp"), Mesh3d(meshes.add(quads_mesh(&quads, color, true))), MeshMaterial3d(material), Transform::default(), Visibility::default())).id();
        commands.entity(t.node).add_child(child);
    }
    commands.remove_resource::<PendingTexts>();
}

/// Linear gain of an AudioMixer's Master group in its (first) snapshot (`AudioMixerController/X.mixer`: the group's
/// m_Volume parameter GUID -> snapshot m_FloatValues dB). Lamps.mixer: +8 dB.
pub fn mixer_gain(asset: &str) -> f32 {
    // "AudioMixerController/<name>.mixer": the mixer's master volume (dB) from the installation
    let name = asset.rsplit('/').next().unwrap_or(asset).trim_end_matches(".mixer");
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, f32>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(g) = cache.lock().unwrap().get(name) {
        return *g;
    }
    let g = crate::owned::Owned::global().and_then(|o| o.mixer_master_db(name)).map(|db| 10f32.powf(db / 20.0)).unwrap_or(1.0);
    cache.lock().unwrap().insert(name.to_string(), g);
    g
}

/// Bevy enables IRRADIANCE_VOLUME per view only while some irradiance volume intersects its frustum; toggling it
/// re-specializes every mesh pipeline and the deferred lighting pass, and the scene is drawn unlit (dark flash) for the
/// frames those take to compile: when the map's volume finished baking and whenever it entered / left the view. A
/// 1 cm volume 5 cm in front of the player camera (placeholder black voxels) keeps the key constant from the first
/// frame; no geometry is inside it, and only a containing volume lights a fragment (unity_light.rs patch).
fn irradiance_view_anchor(mut commands: Commands, cams: Query<Entity, Added<crate::player::fpc::PlayerCamera>>, mut images: ResMut<Assets<Image>>) {
    for cam in &cams {
        if std::env::var_os("RESTRAFE_NO_PROBE_VOLUME").is_some() {
            return;
        }
        let img = Image::new(
            Extent3d { width: 1, height: 2, depth_or_array_layers: 3 },
            TextureDimension::D3,
            vec![0u8; 1 * 2 * 3 * 8],
            TextureFormat::Rgba16Float,
            RenderAssetUsages::RENDER_WORLD,
        );
        commands.spawn((
            Name::new("LightProbeVolumeViewAnchor"),
            bevy::light::LightProbe::default(),
            bevy::light::IrradianceVolume { voxels: images.add(img), intensity: 0.0, affects_lightmapped_meshes: false },
            Transform::from_xyz(0.0, 0.0, -0.05).with_scale(Vec3::splat(0.01)),
            ChildOf(cam),
        ));
    }
}
