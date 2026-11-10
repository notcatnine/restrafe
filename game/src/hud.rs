//! Player HUD. STRAFTAT's HUD is 3D geometry under PlayerIK `Camera/UICam` (layer 25 "HUD",
//! rendered by UICam, fov 22.6): heart + health number, left/right bullet counters, fps/ping counter, cables.
//! We spawn PlayerIK.glb, move its UICam subtree under the player camera on render layer 1 and
//! render it with an overlay camera. Inactive GameObjects / disabled renderers of the prefab are hidden.
//! TextMeshPro texts are real 3D TMP meshes (`tmp_text`: SDF shader, the prefab's font asset + material, RectTransform
//! box, margins and alignment) parented to their RectTransform nodes.
//! Ports: HealthTween.cs (heartbeat, colour, punch), HUD.cs (ammo text + parent punch, fps, ping), PlayerHealth health.

use crate::health::PlayerHealth;
use crate::player::fpc::{Fpc, PlayerCamera};
use crate::tmp_text::{tmp_color, TmpAssets, TmpLayout, TmpSdfMaterial, TmpText};
use crate::tween::{Ease, Tweens};
use crate::weapons::data::UField;
use crate::weapons::WeaponUi;
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::world_serialization::WorldInstanceReady;
use std::collections::HashMap;

pub const HUD_LAYER: usize = 1;
/// Prefab path of the UICam GameObject.
const UICAM_PATH: &str = "PlayerIK/CameraLeanPivot/LandBobPivot/CameraHolder/Camera/UICam";
/// HealthTween prefab values.
const TIME_BETWEEN_HEARTBEAT: f32 = 1.3;
const HEART_SCALE: f32 = 0.15;
const LITTLE_HEART_SCALE: f32 = 0.13;
const HEART_SPEED: f32 = 0.2;
const SIZE_TWEEN: f32 = 1.06;
const TWEEN_DURATION: f32 = 0.17;
const FIRST_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const LAST_COLOR: [f32; 4] = [0.7264151, 0.03769135, 0.03769135, 1.0];

#[derive(Resource, Default)]
pub struct HudNodes {
    pub source_root: Option<Entity>,
    pub uicam: Option<Entity>,
    /// hEALTHtext RectTransform node (HealthTween's transform).
    pub health_text: Option<Entity>,
    pub heart: Option<Entity>,
    /// Heart sprite scale at load (base of the heartbeat punches).
    pub heart_base: Vec3,
    pub ammo: [Option<Entity>; 2],
    pub ammo_parent: [Option<Entity>; 2],
    /// HUDTween hudUp / hudDown.
    pub hud_up_down: [Option<Entity>; 2],
    /// Their prefab local positions (where a freshly spawned player's HUD starts before MoveUp).
    pub hud_up_down_start: [Vec3; 2],
    pub backdrops: Vec<Entity>,
    /// glb entities under UICam by path relative to UICam ("Transform/TransformDown/...").
    pub by_path: HashMap<String, Entity>,
    pub attached: bool,
}

/// What drives a HUD TMP text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HudTextKind {
    /// HealthTween: `Mathf.Ceil(health / 4 * 100)`.
    Health,
    /// HUD.ammoLeft (0) / ammoRight (1): PauseManager.left/rightGunAmmo.
    Ammo(usize),
    /// HUD.pingDisplay: PingDisplay.ping (0 offline).
    Ping,
    /// HUD.fpsDisplay: Mathf.Round(1 / unscaledDeltaTime), sampled every 0.1 s.
    Fps,
    /// No script: keeps its serialized m_text.
    Static,
}

/// The TMP mesh entity of a HUD text (child of its RectTransform node).
#[derive(Component)]
pub struct HudTmpText {
    kind: HudTextKind,
    node: Entity,
}

#[derive(Component)]
pub struct HudCamera;

#[derive(Resource)]
struct HealthTweenState {
    timer: f32,
    time_between_func: f32,
    temp_health: f32,
    /// First heartbeat punch running; its OnComplete starts the little one.
    second_beat_pending: bool,
    /// Respawn: recolour the health text for full health without the ChangeState punch (fresh HUD instance).
    recolor: bool,
    fps_timer: f32,
    fps_count: f32,
}

pub fn setup_hud(mut commands: Commands, assets: Res<AssetServer>) {
    // UI camera: drawn last so 2D UI (menus, crosshair) sits above the 3D passes and the overlay canvas camera.
    commands.spawn((Name::new("UiCamera"), Camera2d, Camera { order: 4, clear_color: ClearColorConfig::None, ..default() }, IsDefaultUiCamera));
    let scene = assets.load(GltfAssetLabel::Scene(0).from_asset("models/player/PlayerIK.glb"));
    let root = commands
        .spawn((Name::new("HudSource"), WorldAssetRoot(scene), Transform::default(), Visibility::Hidden))
        .observe(|ev: On<WorldInstanceReady>, children: Query<&Children>, names: Query<&Name>, tfs: Query<&Transform>, mut nodes: ResMut<HudNodes>| {
            // (entity, path relative to UICam once inside it)
            let mut stack: Vec<(Entity, Option<String>)> = vec![(ev.entity, None)];
            while let Some((e, rel)) = stack.pop() {
                let name = names.get(e).map(|n| n.as_str().to_string()).unwrap_or_default();
                let rel = match rel {
                    None if name == "UICam" => {
                        nodes.uicam = Some(e);
                        Some(String::new())
                    }
                    None => None,
                    Some(p) => {
                        let p = if p.is_empty() { name.clone() } else { format!("{p}/{name}") };
                        nodes.by_path.entry(p.clone()).or_insert(e);
                        Some(p)
                    }
                };
                match name.as_str() {
                    "T_Heart_01" => {
                        nodes.heart = Some(e);
                        nodes.heart_base = tfs.get(e).map(|t| t.scale).unwrap_or(Vec3::ONE);
                    }
                    "TransformUp" | "TransformDown" => {
                        let i = (name == "TransformDown") as usize;
                        nodes.hud_up_down[i] = Some(e);
                        nodes.hud_up_down_start[i] = tfs.get(e).map(|t| t.translation).unwrap_or(Vec3::ZERO);
                    }
                    n if n.starts_with("Backdrop") || n.starts_with("Sphere") => nodes.backdrops.push(e),
                    "SM_BulletCounter_L_00" | "SM_BulletCounter_R_00" => {
                        // ammolefttext lives under <counter>/GameObject
                        let i = if name.contains("_L_") { 0 } else { 1 };
                        if let Ok(k) = children.get(e) {
                            for c in k.iter() {
                                if names.get(c).map(|x| x.as_str() == "GameObject").unwrap_or(false) {
                                    nodes.ammo_parent[i] = Some(c);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                if let Ok(k) = children.get(e) {
                    stack.extend(k.iter().map(|c| (c, rel.clone())));
                }
            }
        })
        .id();
    commands.insert_resource(HudNodes { source_root: Some(root), ..default() });
    commands.insert_resource(HealthTweenState { timer: TIME_BETWEEN_HEARTBEAT, time_between_func: TIME_BETWEEN_HEARTBEAT, temp_health: 4.0, second_beat_pending: false, recolor: false, fps_timer: 0.0, fps_count: 0.0 });

}

/// `activeInHierarchy` of a prefab object (own flag and all ancestors).
fn active_in_hierarchy(prefab: &crate::weapons::data::Prefab, path: &str) -> bool {
    let mut p = path;
    loop {
        if let Some(o) = prefab.find(p) {
            if !o.active {
                return false;
            }
        }
        match p.rfind('/') {
            Some(i) => p = &p[..i],
            None => return true,
        }
    }
}

fn vec2_field(f: &crate::weapons::data::Fields, k: &str, default: Vec2) -> Vec2 {
    f.get(k).map(|v| Vec2::new(v["x"].as_f64().unwrap_or(default.x as f64) as f32, v["y"].as_f64().unwrap_or(default.y as f64) as f32)).unwrap_or(default)
}

fn asset_stem(f: &crate::weapons::data::Fields, k: &str) -> Option<String> {
    let a = f.asset(k)?;
    let file = a.rsplit('/').next()?;
    Some(file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file).to_string())
}

/// Moves the UICam subtree under the player camera (once both exist) and sets up the overlay camera.
#[allow(clippy::too_many_arguments)]
pub fn attach_hud(
    mut commands: Commands,
    mut nodes: ResMut<HudNodes>,
    cams: Query<Entity, With<PlayerCamera>>,
    players: Query<Entity, (With<Fpc>, Without<PlayerHealth>)>,
    children: Query<&Children>,
    meshes: Query<(), With<Mesh3d>>,
    assets: Res<AssetServer>,
    node_tf: Query<&Transform>,
    prefab: Res<crate::health::PlayerPrefab>,
    mut mesh_assets: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut tmp: ResMut<TmpAssets>,
    mut images: ResMut<Assets<Image>>,
    mut tmp_mats: ResMut<Assets<TmpSdfMaterial>>,
) {
    for p in &players {
        commands.entity(p).insert(PlayerHealth::default());
    }
    if nodes.attached {
        return;
    }
    let (Some(uicam), Ok(cam)) = (nodes.uicam, cams.single()) else { return };
    nodes.attached = true;
    // Pivot: AssetRipper's glb space is X-mirrored, ours Z-mirrored -> 180 degrees about Y.
    let pivot = commands
        .spawn((Name::new("HudPivot"), Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)), Visibility::default(), ChildOf(cam)))
        .id();
    commands.entity(uicam).insert((ChildOf(pivot), Transform::default(), Visibility::Inherited));
    // Everything visible under UICam goes to the HUD render layer.
    let mut stack = vec![uicam];
    while let Some(e) = stack.pop() {
        commands.entity(e).insert(RenderLayers::layer(HUD_LAYER));
        if let Ok(k) = children.get(e) {
            stack.extend(k.iter());
        }
    }
    if let Some(root) = nodes.source_root {
        commands.entity(root).despawn();
    }
    // Inactive GameObjects (SM_FPS_Counter_00, SM_MatchPoints_00/01/02, Backdrops, Spheres, top cables...) are not
    // drawn; disabled renderers (SM_Heart_00's MeshRenderer) hide only their own mesh primitives.
    let prefab = &prefab.0;
    for (rel, e) in &nodes.by_path {
        let path = format!("{UICAM_PATH}/{rel}");
        let Some(o) = prefab.find(&path) else { continue };
        if !o.active {
            commands.entity(*e).insert(Visibility::Hidden);
            continue;
        }
        let renderer_off = ["MeshRenderer", "SkinnedMeshRenderer"].iter().any(|r| o.comp(r).is_some_and(|c| !c.b("m_Enabled")));
        if renderer_off {
            if let Ok(k) = children.get(*e) {
                for c in k.iter() {
                    let is_node = nodes.by_path.iter().any(|(p, x)| *x == c && prefab.find(&format!("{UICAM_PATH}/{p}")).is_some());
                    if !is_node && meshes.get(c).is_ok() {
                        commands.entity(c).insert(Visibility::Hidden);
                    }
                }
            }
        }
    }
    for b in &nodes.backdrops {
        commands.entity(*b).insert(Visibility::Hidden);
    }
    // Heart: SpriteRenderer (T_Heart_01, 2.56 x 2.56 units, tinted) -> textured quad.
    if let Some(h) = nodes.heart {
        let mat = mats.add(StandardMaterial {
            base_color: Color::srgba(0.7818122, 1.0, 0.759434, 0.54509807),
            base_color_texture: Some(assets.load("hud/T_Heart_01.png")),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        commands.spawn((Mesh3d(mesh_assets.add(Rectangle::new(2.56, 2.56))), MeshMaterial3d(mat), Transform::default(), RenderLayers::layer(HUD_LAYER), ChildOf(h)));
    }
    // TextMeshPro texts (active in hierarchy).
    for o in prefab.objects.iter().filter(|o| o.path.starts_with(UICAM_PATH)) {
        let Some(t) = o.components.iter().find(|(_, f)| f.contains_key("m_fontAsset") && f.contains_key("m_text")).map(|(_, f)| f) else { continue };
        if !active_in_hierarchy(prefab, &o.path) || !t.b("m_Enabled") {
            continue;
        }
        let rel = o.path[UICAM_PATH.len() + 1..].to_string();
        let (Some(&node), Some(rt)) = (nodes.by_path.get(&rel), o.comp("RectTransform")) else {
            warn!("HUD text {rel}: no glb node / RectTransform");
            continue;
        };
        let (Some(font_name), Some(mat_name)) = (asset_stem(t, "m_fontAsset"), asset_stem(t, "m_sharedMaterial")) else { continue };
        let (Some(font), Some(mat)) = (tmp.font(&font_name, &mut images), tmp.material(&mat_name)) else {
            warn!("HUD text {rel}: TMP font '{font_name}' / material '{mat_name}' not found");
            continue;
        };
        let kind = if let Some(h) = o.comp("HUD") {
            if h.b("ammoLeft") {
                HudTextKind::Ammo(0)
            } else if h.b("ammoRight") {
                HudTextKind::Ammo(1)
            } else if h.b("pingDisplay") {
                HudTextKind::Ping
            } else if h.b("fpsDisplay") {
                HudTextKind::Fps
            } else {
                HudTextKind::Static
            }
        } else if o.comp("HealthTween").is_some() {
            HudTextKind::Health
        } else {
            HudTextKind::Static
        };
        // RectTransform under a plain Transform: localPosition.xy = anchoredPosition (the glb node only has
        // m_LocalPosition); glb space mirrors X.
        let ap = vec2_field(rt, "m_AnchoredPosition", Vec2::ZERO);
        if let Ok(tf) = node_tf.get(node) {
            commands.entity(node).insert(Transform { translation: Vec3::new(-ap.x, ap.y, tf.translation.z), ..*tf });
        }
        let fc = t.get("m_fontColor");
        let g = |c: &str| fc.and_then(|v| v[c].as_f64()).unwrap_or(1.0) as f32;
        let text = match t.get("m_text") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        let m = t.get("m_margin");
        let mg = |c: &str| m.and_then(|v| v[c].as_f64()).unwrap_or(0.0) as f32;
        let layout = TmpLayout {
            font_size: t.f("m_fontSize"),
            rect_size: vec2_field(rt, "m_SizeDelta", Vec2::ZERO),
            pivot: vec2_field(rt, "m_Pivot", Vec2::splat(0.5)),
            margin: Vec4::new(mg("x"), mg("y"), mg("z"), mg("w")),
            h_align: t.i("m_HorizontalAlignment") as u32,
            v_align: t.i("m_VerticalAlignment") as u32,
            orthographic: t.b("m_isOrthographic"),
            char_spacing: t.f("m_characterSpacing"),
        };
        let material = tmp_mats.add(TmpSdfMaterial { params: mat.params(), atlas: font.atlas.clone() });
        let e = commands
            .spawn((
                Name::new(format!("{} (TMP mesh)", o.path.rsplit('/').next().unwrap_or(""))),
                TmpText { text, color: [g("r"), g("g"), g("b"), g("a")], font, padding: mat.padding(), layout, mirror_x: true },
                MeshMaterial3d(material),
                Transform::IDENTITY,
                Visibility::Inherited,
                RenderLayers::layer(HUD_LAYER),
                HudTmpText { kind, node },
                ChildOf(node),
            ))
            .id();
        match kind {
            HudTextKind::Health => nodes.health_text = Some(node),
            HudTextKind::Ammo(i) => nodes.ammo[i] = Some(e),
            _ => {}
        }
    }
    for e in [nodes.heart, nodes.health_text].into_iter().flatten().chain(nodes.ammo_parent.into_iter().flatten()) {
        commands.entity(e).insert(Tweens::default());
    }
    // UICam: fov 22.6, near 0.01, far 50, drawn after the world and weapon cameras.
    let hud_cam = commands
        .spawn((
            Name::new("UICam"),
            HudCamera,
            Camera3d::default(),
            Camera { order: 2, clear_color: ClearColorConfig::None, ..default() },
            Projection::Perspective(PerspectiveProjection { fov: 22.6f32.to_radians(), near: 0.01, ..default() }),
            Transform::default(),
            RenderLayers::layer(HUD_LAYER),
            ChildOf(cam),
        ))
        .id();
    // Specular reflections (reflection probe of the map; see hud_environment).
    if let Some(env) = hud_environment(&mut images) {
        commands.entity(hud_cam).insert(env);
    }
    // UICam/Transform/Directional Light (+ (1)): culling mask = HUD layer only, both active (prefab values).
    let unity_to_lux = std::f32::consts::PI * 1.2 * 2f32.powf(crate::ppv2::EV100);
    for o in prefab.objects.iter().filter(|o| o.path.starts_with(UICAM_PATH)) {
        let Some(l) = o.comp("Light") else { continue };
        if !active_in_hierarchy(prefab, &o.path) || !l.b("m_Enabled") || l.i("m_Type") != 1 {
            continue;
        }
        let c = l.get("m_Color");
        let g = |k: &str| c.and_then(|v| v[k].as_f64()).unwrap_or(1.0) as f32;
        // Rotation relative to UICam (all ancestors up to UICam are identity rotations in the prefab).
        let mut rot = Quat::IDENTITY;
        let mut p = o.path.as_str();
        while p.len() > UICAM_PATH.len() {
            if let Some(t) = prefab.find(p).and_then(|x| x.comp("Transform")).and_then(|t| t.get("m_LocalRotation")) {
                rot = crate::weapons::data::u_quat(t) * rot;
            }
            p = &p[..p.rfind('/').unwrap_or(0)];
        }
        // lightsUseLinearIntensity = 1: radiance = colour.linear * intensity; Unity's BRDF has no 1/pi.
        commands.spawn((
            Name::new(format!("HUD {}", o.path.rsplit('/').next().unwrap_or(""))),
            DirectionalLight { color: Color::srgb(g("r"), g("g"), g("b")), illuminance: l.f("m_Intensity") * unity_to_lux, shadow_maps_enabled: false, ..default() },
            Transform::from_rotation(rot),
            RenderLayers::layer(HUD_LAYER),
            ChildOf(cam),
        ));
    }
    // HUDTween.MoveUp: DOLocalMove(zero, 0.4).SetEase(OutSine) (round start / offline transport).
    for e in nodes.hud_up_down.into_iter().flatten() {
        let mut tw = Tweens::default();
        tw.local_move(Vec3::ZERO, 0.4, Ease::OutSine);
        commands.entity(e).insert(tw);
    }
}

/// Color32 quantisation (TMP vertex colours and `outlineColor` are Color32).
fn color32(c: [f32; 4]) -> [f32; 4] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() / 255.0)
}

/// HealthTween.Update/ChangeState + HUD.cs (ammo, fps, ping).
#[allow(clippy::too_many_arguments)]
fn update_hud(
    time: Res<Time>,
    real: Res<Time<bevy::time::Real>>,
    nodes: Res<HudNodes>,
    mut state: ResMut<HealthTweenState>,
    mut reader: MessageReader<WeaponUi>,
    mut texts: Query<(&HudTmpText, &mut TmpText, &MeshMaterial3d<TmpSdfMaterial>)>,
    mut tweens: Query<&mut Tweens>,
    mut tfs: Query<&mut Transform>,
    mut tmp_mats: ResMut<Assets<TmpSdfMaterial>>,
    players: Query<(&Fpc, Option<&PlayerHealth>)>,
) {
    let dt = time.delta_secs();
    let Ok((_, ph)) = players.single() else { return };
    let mut health = ph.map(|h| h.health).unwrap_or(4.0);
    // Debug: RESTRAFE_HUD_HEALTH=<t>:<hp> shows <hp> on the HUD from game time t (HUD only, no gameplay effect).
    if let Some((t, hp)) = std::env::var("RESTRAFE_HUD_HEALTH").ok().and_then(|v| v.split_once(':').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)))) {
        if time.elapsed_secs() >= t {
            health = hp;
        }
    }
    // Heartbeat: heart.DOPunchScale(heartScale, heartSpeed).OnComplete(DOPunchScale(littleHeartScale, heartSpeed)).
    state.timer -= dt;
    if let Some(mut tw) = nodes.heart.and_then(|h| tweens.get_mut(h).ok()) {
        if state.timer < 0.0 {
            state.timer = state.time_between_func;
            tw.punch_scale_from(nodes.heart_base, Vec3::splat(HEART_SCALE), HEART_SPEED, 10, 1.0);
            state.second_beat_pending = true;
        } else if state.second_beat_pending && tw.0.is_empty() {
            state.second_beat_pending = false;
            tw.punch_scale_from(nodes.heart_base, Vec3::splat(LITTLE_HEART_SCALE), HEART_SPEED, 10, 1.0);
        }
    }
    state.time_between_func = crate::unity::lerp(TIME_BETWEEN_HEARTBEAT, 0.43, 1.0 - health / 4.0);
    // PlayerHealth.Update: health changed -> tweenScript.ChangeState().
    let changed = health != state.temp_health;
    let recolor = std::mem::take(&mut state.recolor);
    state.temp_health = health;
    let mut ammo_msgs: [Option<String>; 2] = [None, None];
    for m in reader.read() {
        if let WeaponUi::Ammo { right, text, charged } = m {
            ammo_msgs[*right as usize] = Some(format!("{charged}{text}"));
        }
    }
    // HUD.Start (fpsDisplay): count = 1 / unscaledDeltaTime every 0.1 s.
    state.fps_timer -= real.delta_secs();
    if state.fps_timer <= 0.0 {
        state.fps_timer = 0.1;
        state.fps_count = 1.0 / real.delta_secs().max(1e-6);
    }
    for (h, mut t, mat) in &mut texts {
        let set = |t: &mut Mut<TmpText>, s: String| {
            if t.text != s {
                t.text = s;
            }
        };
        match h.kind {
            HudTextKind::Health => {
                set(&mut t, (health / 4.0 * 100.0).ceil().to_string());
                if changed {
                    // ChangeState: transform.DOKill(); localScale = one; DOPunchScale(sizeTween, duration).
                    if let Ok(mut tw) = tweens.get_mut(h.node) {
                        tw.kill();
                        tw.punch_scale_from(Vec3::ONE, Vec3::splat(SIZE_TWEEN), TWEEN_DURATION, 10, 1.0);
                    }
                    if let Ok(mut tf) = tfs.get_mut(h.node) {
                        tf.scale = Vec3::ONE;
                    }
                }
                if changed || recolor {
                    // Color.Lerp clamps t (health < 0 on the lethal frame -> lastColor).
                    let k = (1.0 - health / 4.0).clamp(0.0, 1.0);
                    let c: [f32; 4] = std::array::from_fn(|i| FIRST_COLOR[i] + (LAST_COLOR[i] - FIRST_COLOR[i]) * k);
                    // text.color (vertex colour), fontSharedMaterial _GlowColor, outlineColor (instance material).
                    t.color = color32(c);
                    if let Some(mut m) = tmp_mats.get_mut(&mat.0) {
                        m.params.glow_color = tmp_color(c);
                        m.params.outline_color = tmp_color(color32(c));
                    }
                }
            }
            HudTextKind::Ammo(i) => {
                let Some(parent) = nodes.ammo_parent[i] else { continue };
                let new = ammo_msgs[i].take().filter(|s| *s != t.text);
                if let Some(s) = new {
                    // text.text != ammo: transform.parent.localScale = one; parent.DOPunchScale(1.15, 0.1).
                    if let Ok(mut tf) = tfs.get_mut(parent) {
                        tf.scale = Vec3::ONE;
                    }
                    if let Ok(mut tw) = tweens.get_mut(parent) {
                        tw.punch_scale_from(Vec3::ONE, Vec3::splat(1.15), 0.1, 10, 1.0);
                    }
                    t.text = s;
                } else if let Ok(mut tw) = tweens.get_mut(parent) {
                    // else parent.DOScale(one, 0.3) every frame (a no-op once at rest, so skipped then).
                    let at_rest = tw.0.is_empty() && tfs.get(parent).is_ok_and(|tf| tf.scale == Vec3::ONE);
                    if !at_rest {
                        tw.scale_to(Vec3::ONE, 0.3, Ease::OutQuad);
                    }
                }
            }
            HudTextKind::Ping => set(&mut t, "0".into()),
            HudTextKind::Fps => set(&mut t, state.fps_count.round().to_string()),
            HudTextKind::Static => {}
        }
    }
}

/// Reflection probe for the HUD camera (see `attach_hud`). In Unity the HUD pieces are scene objects lit by the map's
/// reflection probes (each original map bakes one, e.g. Chateaux_01: box 71 x 30 x 58, intensity 1), falling back to
/// RenderSettings' custom reflection (most maps: Cubemap/Neo_LD_03_371857150_sky, a flat 0.216 grey). Our test map is
/// not an original map, so its "baked probe" is synthesised from the test map itself (map.rs constants): floor below
/// the horizon, perimeter walls up to their angular height, the clear colour above; radiance = albedo / pi *
/// irradiance (sun + ambient), in display units (divided by the camera exposure). Only specular: Unity's diffuse
/// ambient comes from the light probes / ambient, not from the reflection probe.
/// `RESTRAFE_HUD_REFLECTION=default` uses the Unity default custom reflection cubemap instead.
fn hud_environment(images: &mut Assets<Image>) -> Option<bevy::light::EnvironmentMapLight> {
    use crate::map::*;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension};
    let k = 1.2 * 2f32.powf(crate::ppv2::EV100);
    let lin = |c: [f32; 3]| Vec3::new(crate::tmp_text::gamma_to_linear(c[0]), crate::tmp_text::gamma_to_linear(c[1]), crate::tmp_text::gamma_to_linear(c[2]));
    let elev = -SUN_YAW_PITCH.1;
    let pi = std::f32::consts::PI;
    // Floor: sun at elevation `elev`; walls: vertical, mean max(0, cos azimuth) over the four walls = 1/pi.
    let floor = lin(FLOOR_SRGB) * (SUN_LUX * elev.sin() / pi + AMBIENT) / k;
    let wall = lin(WALL_SRGB) * (SUN_LUX * elev.cos() / (pi * pi) + AMBIENT) / k;
    let sky = ClearColor::default().0.to_linear();
    let sky = Vec3::new(sky.red, sky.green, sky.blue);
    // Eye at ~2.1 m in the middle of the 80 x 80 m room.
    let wall_top = ((WALL_HEIGHT - 2.1) / FLOOR_HALF).atan();
    let n = 32usize;
    let use_default = std::env::var("RESTRAFE_HUD_REFLECTION").is_ok_and(|v| v == "default");
    let unity_default = 0.216f32; // Neo_LD_03_371857150_sky.png: every texel 55/255, linear (sRGBTexture 0)
    let mut data = Vec::with_capacity(n * n * 6 * 4);
    for f in 0..6 {
        for y in 0..n {
            for x in 0..n {
                let u = (x as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                let v = (y as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                let d = match f {
                    0 => Vec3::new(1.0, -v, -u),
                    1 => Vec3::new(-1.0, -v, u),
                    2 => Vec3::new(u, 1.0, v),
                    3 => Vec3::new(u, -1.0, -v),
                    4 => Vec3::new(u, -v, 1.0),
                    _ => Vec3::new(-u, -v, -1.0),
                }
                .normalize();
                let e = d.y.asin();
                let c = if use_default {
                    Vec3::splat(unity_default)
                } else if e < 0.0 {
                    floor
                } else if e < wall_top {
                    wall
                } else {
                    sky
                };
                data.extend(c.to_array().map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8));
                data.push(255);
            }
        }
    }
    let cube_of = |w: u32, data: Vec<u8>| {
        let mut img = Image::new(Extent3d { width: w, height: w, depth_or_array_layers: 6 }, TextureDimension::D2, data, TextureFormat::Rgba8Unorm, bevy::asset::RenderAssetUsages::RENDER_WORLD);
        img.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
        img
    };
    let black = cube_of(1, vec![0u8; 24]);
    Some(bevy::light::EnvironmentMapLight {
        diffuse_map: images.add(black),
        specular_map: images.add(cube_of(n as u32, data)),
        // Values are display radiance; the HUD camera's exposure divides by k.
        intensity: k,
        ..default()
    })
}

/// Replaces glTF materials by the Unity `.mat` conversion of the same name (`UnityAssets`). AssetRipper's glTF export
/// maps custom (Amplify) shaders by guessing: for S_Surfaces_00(_NoFog) it used the first texture (_MEtallic, a
/// mask) as base colour, giving the harsh black-and-white look. HUD meshes: every material (S_Surfaces_00_NoFog metal,
/// S_Emissive_Mask_00 screens, S_TransparentWindows_00 glass). Other glTF scenes (weapons: AR15/Kanye/Warden
/// M_RustPanels_00, Propeller M_Toles_00/M_MetalDoor_00/M_MetalRust_00): only the S_Surfaces_00 family and
/// S_TransparentWindows_00, whose conversions are recovered from the compiled shaders.
#[derive(Component)]
struct HudMatDone;

#[allow(clippy::type_complexity)]
fn hud_materials(
    mut commands: Commands,
    q: Query<(Entity, &bevy::gltf::GltfMaterialName, Option<&RenderLayers>, Option<&Mesh3d>), (With<MeshMaterial3d<StandardMaterial>>, Without<HudMatDone>)>,
    mut uassets: ResMut<crate::ugui_assets::UnityAssets>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut fx: ResMut<Assets<crate::ugui_assets::UnityFxMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    tfs: Query<(&Transform, Option<&ChildOf>)>,
    mut shaders: Local<HashMap<String, String>>,
    nodes: Res<HudNodes>,
) {
    // Before attach_hud the HUD subtree has no RenderLayers yet (it would be taken for a non-HUD scene).
    // RESTRAFE_GLTF_MATERIALS=1 keeps AssetRipper's glTF materials (before/after comparisons).
    if !nodes.attached || std::env::var_os("RESTRAFE_GLTF_MATERIALS").is_some() {
        return;
    }
    for (e, name, layers, mesh) in &q {
        let hud = layers.is_some_and(|l| l.intersects(&RenderLayers::layer(HUD_LAYER)));
        if !hud {
            let fam = shaders.entry(name.0.clone()).or_insert_with(|| uassets.material_shader(&name.0).unwrap_or_default());
            if !matches!(fam.as_str(), "S_Surfaces_00.shader" | "S_Surfaces_00_NoFog.shader" | "S_TransparentWindows_00.shader") {
                commands.entity(e).insert(HudMatDone);
                continue;
            }
        }
        let mut ec = commands.entity(e);
        ec.insert(HudMatDone);
        // Debug: RESTRAFE_HUD_HIDE=<material name substring>[,...] hides those HUD renderers (isolate a material).
        if hud && std::env::var("RESTRAFE_HUD_HIDE").is_ok_and(|v| v.split(',').any(|s| !s.is_empty() && name.0.contains(s))) {
            ec.insert(Visibility::Hidden);
        }
        // Negative-determinant hierarchy (SM_FPS_Counter_00 (1) has m_LocalScale.x = -1): Unity flips the triangle
        // winding of mirrored renderers, Bevy does not (back faces would be drawn, front faces culled) -> use a copy
        // of the mesh with reversed winding.
        let mut det = 1.0f32;
        let mut cur = Some(e);
        while let Some(c) = cur {
            let Ok((t, parent)) = tfs.get(c) else { break };
            det *= t.scale.x * t.scale.y * t.scale.z;
            cur = parent.map(|p| p.parent());
        }
        if det < 0.0 {
            if let Some(mut m) = mesh.and_then(|m| meshes.get(&m.0)).cloned() {
                if let Some(ix) = m.indices_mut() {
                    match ix {
                        bevy::mesh::Indices::U16(v) => v.chunks_exact_mut(3).for_each(|t| t.swap(1, 2)),
                        bevy::mesh::Indices::U32(v) => v.chunks_exact_mut(3).for_each(|t| t.swap(1, 2)),
                    }
                    ec.insert(Mesh3d(meshes.add(m)));
                }
            }
        }
        if std::env::var_os("RESTRAFE_DEBUG_HUDMAT").is_some() {
            let tok = crate::owned::Owned::global().and_then(|o| o.find_named(21, &name.0)).map(|(n, p)| crate::owned_docs::token(n, p));
            let shader = uassets.material_shader(&name.0);
            println!("HUDMAT {} hud={hud} -> {tok:?} shader {shader:?}", name.0);
        }
        match uassets.material_by_name(&name.0, &mut images, &mut mats, &mut fx) {
            Some(crate::ugui_assets::UMat::Std(h)) => {
                ec.insert(MeshMaterial3d(h));
            }
            Some(crate::ugui_assets::UMat::Fx(h)) => {
                ec.remove::<MeshMaterial3d<StandardMaterial>>().insert(MeshMaterial3d(h));
            }
            None => warn!("HUD material {} not converted (kept glTF material)", name.0),
        }
    }
}

/// Round respawn (death.rs): the new player prefab brings a fresh HUD — HealthTween at full health (no ChangeState
/// punch) and HUDTween.MoveUp from the prefab positions (test map: PlayerSetup calls MoveUp on spawn).
#[derive(Message)]
pub struct HudRespawn;

fn hud_respawn(mut commands: Commands, mut reader: MessageReader<HudRespawn>, nodes: Res<HudNodes>, mut state: ResMut<HealthTweenState>, mut tfs: Query<&mut Transform>) {
    if reader.read().count() == 0 {
        return;
    }
    state.temp_health = 4.0;
    state.timer = TIME_BETWEEN_HEARTBEAT;
    state.time_between_func = TIME_BETWEEN_HEARTBEAT;
    state.second_beat_pending = false;
    state.recolor = true;
    for (i, e) in nodes.hud_up_down.iter().enumerate() {
        let Some(e) = *e else { continue };
        if let Ok(mut tf) = tfs.get_mut(e) {
            tf.translation = nodes.hud_up_down_start[i];
        }
        let mut tw = Tweens::default();
        tw.local_move(Vec3::ZERO, 0.4, Ease::OutSine);
        commands.entity(e).insert(tw);
    }
}

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HudNodes>()
            .add_message::<HudRespawn>()
            .add_systems(PostUpdate, hud_respawn.before(update_hud))
            .add_systems(Startup, setup_hud)
            .add_systems(Update, (attach_hud, debug_hud, hud_materials.after(attach_hud)))
            // HUD scripts run before DOTween (tween::update_tweens).
            .add_systems(PostUpdate, update_hud.before(crate::tween::update_tweens).before(crate::tmp_text::rebuild_meshes));
    }
}

/// Debug (RESTRAFE_DEBUG_HUD=1): print HUD node positions relative to the HUD camera.
pub fn debug_hud(nodes: Res<HudNodes>, cam: Query<&GlobalTransform, With<HudCamera>>, g: Query<&GlobalTransform>, mut t: Local<f32>, time: Res<Time>) {
    *t += time.delta_secs();
    if std::env::var_os("RESTRAFE_DEBUG_HUD").is_none() || *t < 2.0 {
        return;
    }
    *t = -1000.0;
    let Ok(c) = cam.single() else { println!("HUD: no hud camera; uicam={:?}", nodes.uicam); return };
    let inv = c.affine().inverse();
    for (n, e) in [("uicam", nodes.uicam), ("health", nodes.health_text), ("heart", nodes.heart), ("ammoL", nodes.ammo[0]), ("ammoR", nodes.ammo[1])] {
        if let Some(e) = e {
            if let Ok(x) = g.get(e) {
                println!("HUD {n}: cam-local {:?} scale {:?}", inv.transform_point3(x.translation()), x.scale());
            }
        } else {
            println!("HUD {n}: missing");
        }
    }
}
