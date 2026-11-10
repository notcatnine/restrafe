//! Port of the cosmetics system: `CosmeticsManager` lists (suits / hats / cigs), `PlayerSetup.ChangeDress`
//! (hat + cig instantiated under `hatToWearPosition` == the "HatPivot" bone, `mats[mat]` on the body
//! SkinnedMeshRenderers, `fparmsMats[mat]` on fpArmsSuits) and the original customization screen (`--HATS MENU--`,
//! imported with ugui.rs, opened with K in game, 3D AboubiPreview included; see the "menu" section below).
//!
//! Data: `assets/data/cosmetics.json` (extracted from MainMenu.unity's CosmeticsManager, see that file's `_doc`).
//! Persisted selection: `config/cosmetics.ron` (replaces the SaveData hat/suit/cig indices).
//! Env override for tests: `RESTRAFE_COSMETICS=suit,hat,cig`.
//!
//! The PlayerIK.glb has no skinned meshes (AssetRipper exports them separately), so the body parts
//! (`Mesh/SK_Aboubi00_*.glb`, bind pose) are attached here as static meshes under the matching renderer
//! nodes of the dummy's "Graphics" subtree; their materials are what ChangeDress swaps.

use crate::health::Dummy;
use crate::input::PlayerInput;
use bevy::image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use bevy::world_serialization::WorldAssetRoot;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const SAVE_PATH: &str = "config/cosmetics.ron";
const DATA_PATH: &str = "assets/data/cosmetics.json";

// ---------------------------------------------------------------- data

#[derive(Deserialize, Clone, Debug)]
pub struct MatInfo {
    /// Unity material path relative to the ripped Assets ("Material/M_Garage_00.mat").
    #[serde(default)]
    pub material: String,
    pub texture: String,
    pub color: Vec<f32>,
    pub normal: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Suit {
    pub name: String,
    pub body: MatInfo,
    /// `fparmsMats[suit]` (first-person arms).
    pub arms: MatInfo,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Item {
    pub name: String,
    /// glb scene path relative to `assets/`.
    pub model: String,
    /// Hats: the Unity Mesh asset (relative to `assets/`) of the prefab root's convex MeshCollider (`m_Mesh`, which
    /// is not always the rendered mesh). None: no collider (No Hat, the 5 skinned hats; Fishy's MeshCollider has no mesh).
    #[serde(default)]
    pub collider: Option<String>,
    /// Prefab path relative to ripped/ExportedProject/Assets ("GameObject/PF_X.prefab").
    #[serde(default)]
    pub prefab: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct BodyPart {
    pub node: String,
    pub model: String,
    /// "suit" (swapped by ChangeDress) or "face" (fixed M_Aboubi_Face_00).
    pub material: String,
}

#[derive(Deserialize, Resource, Clone, Debug)]
pub struct CosmeticsData {
    pub hat_pivot: String,
    pub face_texture: String,
    pub body: Vec<BodyPart>,
    pub suits: Vec<Suit>,
    pub hats: Vec<Item>,
    pub cigs: Vec<Item>,
}

/// Current selection (CosmeticsManager.suitIndex / hatIndex / cigIndex).
#[derive(Resource, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Cosmetics {
    pub suit: usize,
    pub hat: usize,
    pub cig: usize,
}

impl Cosmetics {
    fn load() -> Self {
        let mut c: Cosmetics = std::fs::read_to_string(SAVE_PATH).ok().and_then(|s| ron::from_str(&s).ok()).unwrap_or_default();
        if let Ok(v) = std::env::var("RESTRAFE_COSMETICS") {
            let n: Vec<usize> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            c.suit = n.first().copied().unwrap_or(c.suit);
            c.hat = n.get(1).copied().unwrap_or(c.hat);
            c.cig = n.get(2).copied().unwrap_or(c.cig);
        }
        c
    }
    fn save(&self) {
        // Autotests must not overwrite the user's selection (same rule as Settings::save).
        if std::env::var_os("RESTRAFE_AUTOTEST").is_some() {
            return;
        }
        let _ = std::fs::create_dir_all("config");
        if let Ok(s) = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()) {
            let _ = std::fs::write(SAVE_PATH, s);
        }
    }
    /// `ChangeDress` index wrapping: `(i + len) % len`.
    fn wrap(&mut self, d: &CosmeticsData) {
        self.suit %= d.suits.len();
        self.hat %= d.hats.len();
        self.cig %= d.cigs.len();
    }
}

/// Lazily built materials (public so the first-person arms can use `arms[suit]` once they are rendered).
#[derive(Resource, Default)]
pub struct CosmeticMaterials {
    pub body: Vec<Option<Handle<StandardMaterial>>>,
    pub arms: Vec<Option<Handle<StandardMaterial>>>,
    pub face: Option<Handle<StandardMaterial>>,
}

fn make_material(info: &MatInfo, assets: &AssetServer, mats: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
    // Unity textures repeat by default.
    let load = |p: &str, srgb: bool| -> Handle<Image> {
        assets
            .load_builder()
            .with_settings(move |s: &mut ImageLoaderSettings| {
                s.is_srgb = srgb;
                s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                    address_mode_u: ImageAddressMode::Repeat,
                    address_mode_v: ImageAddressMode::Repeat,
                    ..ImageSamplerDescriptor::linear()
                });
            })
            .load(p.to_string())
    };
    let c = &info.color;
    // S_StunnedAboubi_MULTISUITS_00 / _NEON / _Gloss (every suit body + arms material; the property `_texcoord2`):
    // the vertex shader passes TEXCOORD1 (the mesh's second UV set) * _texcoord2_ST as the _BC / _normal UV (DXBC,
    // MULTISUITS_00 VS blob 29: `mad o1.xy, v4.xyxx, cb0[9]`); the body / arm meshes carry both UV sets and only the
    // second one is laid out for the suit textures. Standard metallic BRDF with _Metallic / _Smoothness (all 0):
    // dielectric F0 0.04 (Bevy reflectance 0.5).
    // the suit material's saved properties, read from the installation
    let props = (!info.material.is_empty())
        .then(|| {
            let stem = info.material.rsplit('/').next()?.trim_end_matches(".mat");
            let (n, p) = crate::owned::Owned::global()?.find_named(21, stem)?;
            crate::owned_docs::asset_at(n, p).and_then(|(_, m)| m.get("m_SavedProperties").cloned())
        })
        .flatten();
    let second_uv = props.as_ref().and_then(|p| p.get("m_TexEnvs")).is_some_and(|t| t.get("_texcoord2").is_some());
    let channel = if second_uv { bevy::mesh::UvChannel::Uv1 } else { bevy::mesh::UvChannel::Uv0 };
    let float = |k: &str| props.as_ref().and_then(|p| p.get("m_Floats")).and_then(|f| f.get(k)).and_then(|v| v.as_f64()).map(|v| v as f32);
    mats.add(StandardMaterial {
        base_color: Color::srgba(c[0], c[1], c[2], c.get(3).copied().unwrap_or(1.0)),
        base_color_texture: Some(load(&info.texture, true)),
        base_color_channel: channel.clone(),
        // normal maps unpacked like Unity's UnpackNormal (ugui_assets::normal_image), not in their stored layout
        normal_map_texture: info.normal.as_ref().map(|n| {
            let stem = n.rsplit('/').next().unwrap_or(n).trim_end_matches(".png");
            crate::owned::Owned::global().and_then(|o| o.find_named(28, stem)).and_then(|(tn, tp)| crate::ugui_assets::normal_image(tn, tp)).map(|img| assets.add(img)).unwrap_or_else(|| load(n, false))
        }),
        normal_map_channel: channel,
        perceptual_roughness: if second_uv { 1.0 - float("_Smoothness").unwrap_or(0.0) } else { 1.0 },
        metallic: if second_uv { float("_Metallic").unwrap_or(0.0) } else { 0.0 },
        reflectance: if second_uv { 0.5 } else { 0.0 },
        ..default()
    })
}

impl CosmeticMaterials {
    pub fn body(&mut self, i: usize, d: &CosmeticsData, a: &AssetServer, m: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
        self.body.resize(d.suits.len(), None);
        self.body[i].get_or_insert_with(|| make_material(&d.suits[i].body, a, m)).clone()
    }
    pub fn arms(&mut self, i: usize, d: &CosmeticsData, a: &AssetServer, m: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
        self.arms.resize(d.suits.len(), None);
        self.arms[i].get_or_insert_with(|| make_material(&d.suits[i].arms, a, m)).clone()
    }
    pub fn face(&mut self, d: &CosmeticsData, a: &AssetServer, m: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
        self.face
            .get_or_insert_with(|| make_material(&MatInfo { material: String::new(), texture: d.face_texture.clone(), color: vec![1.0; 4], normal: None }, a, m))
            .clone()
    }
}

// ---------------------------------------------------------------- applying

/// Per-dummy dress state.
#[derive(Component)]
pub struct DressState {
    suit_meshes: Vec<Entity>,
    pub hat_pivot: Entity,
    pub hat: Option<Entity>,
    cig: Option<Entity>,
    applied: Option<Cosmetics>,
}

impl DressState {
    /// Selection last applied to this body.
    pub fn applied(&self) -> Option<Cosmetics> {
        self.applied
    }
}

/// Finds (by node name) the body part anchors + hat pivot below `root`, once the glb scene has spawned.
fn find_nodes(root: Entity, children: &Query<&Children>, names: &Query<&Name>, wanted: &[&str]) -> Vec<Option<Entity>> {
    let mut found = vec![None; wanted.len()];
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if let Ok(n) = names.get(e) {
            if let Some(i) = wanted.iter().position(|w| *w == n.as_str()) {
                // Several nodes share names ("SK_Aboubi_00"); the wanted ones are unique.
                found[i].get_or_insert(e);
            }
        }
        if let Ok(k) = children.get(e) {
            stack.extend(k.iter());
        }
    }
    found
}

fn setup_dress(
    mut commands: Commands,
    data: Res<CosmeticsData>,
    assets: Res<AssetServer>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut cm: ResMut<CosmeticMaterials>,
    cos: Res<Cosmetics>,
    // The local player's (hidden) body too: its torso / legs show while sliding (health.rs local_slide_body).
    dummies: Query<Entity, (Or<(With<Dummy>, With<crate::health::PreviewBody>, With<crate::health::LocalBody>, With<crate::health::CorpseBody>)>, Without<DressState>)>,
    children: Query<&Children>,
    names: Query<&Name>,
    meshes: Query<(), With<Mesh3d>>,
) {
    for dummy in &dummies {
        let mut wanted: Vec<&str> = data.body.iter().map(|b| b.node.as_str()).collect();
        wanted.push(data.hat_pivot.as_str());
        let found = find_nodes(dummy, &children, &names, &wanted);
        if found.iter().any(|f| f.is_none()) {
            continue; // scene not spawned yet
        }
        // The `*_Col` hit-collider meshes are disabled MeshRenderers in the prefab; AssetRipper's glb still has them.
        let mut stack = vec![dummy];
        while let Some(e) = stack.pop() {
            if meshes.contains(e) && names.get(e).is_ok_and(|n| n.ends_with("_Col")) {
                commands.entity(e).insert(Visibility::Hidden);
            }
            if let Ok(k) = children.get(e) {
                stack.extend(k.iter());
            }
        }
        let face = cm.face(&data, &assets, &mut mats);
        let suit_mat = cm.body(cos.suit, &data, &assets, &mut mats);
        let mut suit_meshes = Vec::new();
        for (i, part) in data.body.iter().enumerate() {
            let anchor = found[i].unwrap();
            let mesh: Handle<Mesh> = assets.load(GltfAssetLabel::Primitive { mesh: 0, primitive: 0 }.from_asset(part.model.clone()));
            let is_suit = part.material == "suit";
            let mat = if is_suit { suit_mat.clone() } else { face.clone() };
            let e = commands
                .spawn((Name::new(format!("{}_mesh", part.node)), Mesh3d(mesh), MeshMaterial3d(mat), Transform::default(), Visibility::default()))
                .id();
            commands.entity(anchor).add_child(e);
            commands.entity(e).insert(crate::skinning::PendingSkin { model: part.model.clone(), dummy });
            if is_suit {
                suit_meshes.push(e);
            }
        }
        commands.entity(dummy).insert(DressState { suit_meshes, hat_pivot: found[data.body.len()].unwrap(), hat: None, cig: None, applied: None });
    }
}

/// `PlayerSetup.RpcLogic___ChangeDress`: replace hat + cig under the pivot, swap body materials.
fn apply_dress(
    mut commands: Commands,
    data: Res<CosmeticsData>,
    cos: Res<Cosmetics>,
    assets: Res<AssetServer>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut cm: ResMut<CosmeticMaterials>,
    mut dressed: Query<&mut DressState>,
) {
    for mut st in &mut dressed {
        if st.applied == Some(*cos) {
            continue;
        }
        let prev = st.applied;
        if prev.map(|p| p.suit) != Some(cos.suit) {
            let m = cm.body(cos.suit, &data, &assets, &mut mats);
            for &e in &st.suit_meshes {
                commands.entity(e).insert(MeshMaterial3d(m.clone()));
            }
        }
        // Hat / cig are instantiated at the pivot with identity local transform (HatPosition copies the
        // pivot's world pose every frame); the glb root of each prefab carries no transform.
        let pivot = st.hat_pivot;
        let respawn = |old: &mut Option<Entity>, item: &Item, name: &str, commands: &mut Commands| {
            if let Some(o) = old.take() {
                commands.entity(o).despawn();
            }
            let scene = assets.load(GltfAssetLabel::Scene(0).from_asset(item.model.clone()));
            let e = commands.spawn((Name::new(name.to_string()), WorldAssetRoot(scene), Transform::default(), Visibility::default(), UnityItemMaterials(0))).id();
            // Skinned hats (SkinnedMeshRenderer + VerletSpring; the glb holds only their bones): see hud_verlet.rs.
            if let Some(stem) = item.prefab.as_deref().and_then(|p| p.rsplit('/').next()).map(|p| p.trim_end_matches(".prefab")) {
                let json = format!("assets/data/hats/{stem}.json");
                if crate::owned::data_exists(&json) {
                    commands.entity(e).insert(crate::hud_verlet::PrefabSkin(json));
                }
            }
            commands.entity(pivot).add_child(e);
            *old = Some(e);
        };
        if prev.map(|p| p.hat) != Some(cos.hat) {
            let mut o = st.hat;
            respawn(&mut o, &data.hats[cos.hat], "Hat", &mut commands);
            st.hat = o;
        }
        if prev.map(|p| p.cig) != Some(cos.cig) {
            let mut o = st.cig;
            respawn(&mut o, &data.cigs[cos.cig], "Cig", &mut commands);
            st.cig = o;
        }
        st.applied = Some(*cos);
    }
}

/// Hat/cig hang below the (hidden) `Head_Col` collider node, so they get an explicit visibility that
/// follows the dummy's Graphics node (PlayerHealth.graphics, hidden while dead).
/// While ragdolled the body stands for the `aboubiRagdoll` instance, which has no cig (the cig stays under the
/// deactivated player's hatToWearPosition); the hat pops off (PlayerHealth.Hat) and stays visible.
fn sync_dress_visibility(dressed: Query<(&DressState, &crate::health::DummyGraphics, Has<crate::ragdoll::HasRagdoll>, Has<crate::health::Corpse>)>, mut vis: Query<&mut Visibility>) {
    for (st, g, ragdolled, corpse) in &dressed {
        // A corpse is always the ragdoll instance (also on its first frame, before HasRagdoll lands).
        let ragdolled = ragdolled || corpse;
        let want = match vis.get(g.0) {
            Ok(Visibility::Hidden) => Visibility::Hidden,
            _ => Visibility::Visible,
        };
        for (e, want) in [(st.hat, want), (st.cig, if ragdolled { Visibility::Hidden } else { want })] {
            let Some(e) = e else { continue };
            if let Ok(mut v) = vis.get_mut(e) {
                if *v != want {
                    *v = want;
                }
            }
        }
    }
}

// ---------------------------------------------------------------- menu (--HATS MENU--)
//
// The original customization screen is `---USER INTERFACE---/--MAIN MENU--/--HATS MENU--` (MainMenu.unity), opened
// by `MenuController.ActivateMenu(hatsMenu)` from the main menu's button bar. It is imported with ugui.rs by
// settings_menu.rs (same canvas as the pause/options menus) through [`import_hats_menu`]:
//
// * "HATS MENU HUD" (3D frame SM_MainMenu_Cosmetics_00 + point light, layer HUD) rendered by `--MAIN MENU--/HUDCamera`
//   into MainMenuHUD and shown by `--MAIN MENU--/MenuHUDDisplayTexture` (both are siblings of the menu in the original
//   and active with every main menu screen; here they are parented to the hats menu so they only show with it).
// * TITLE "COSMETICS MENU", tabs 1stTab "Hats" / 2ndTab "Suits" / 3rdTab "Smoke" (persistent SetActive calls on
//   HatsView / SuitsView / CigsView), one ScrollRect grid of `CosmeticInstance` buttons per view, OKButton
//   (`MenuController.ActivateMenu(playMenu)` -> closes the menu here), "Name Gradient" + GradientDropDown.
// * Preview: RawImage showing the CosmeticPreview RenderTexture drawn by `COSMETICSSS/Camera` (layer 23 only) looking
//   at `COSMETICSSS/PreviewAboubi` (AboubiPreview: rotates rotateSpeed deg/s, wears the selection; own point light).
// * `FloatTextCanvas/FloatingText` (FloatingName): hover text next to the cursor (CosmeticInstance.OnPointerEnter).
//
// GridLayoutGroup / ContentSizeFitter are applied here once (the importer does not run layout groups and the
// serialized item positions are stale).
//
// Unlocks: CosmeticInstance.acquired is serialized per item; Start() unlocks DLC items when SteamLobby.ownDlc0
// (DLC) / ownDlc1 (Supporter Edition) is true, CosmeticsManager.UpdateUnlockable unlocks ProgressManager XP rewards,
// challenge items are unlocked by achievements. Steam is not available here, so every item is treated as acquired
// (= owning both DLCs, max XP, all challenges) unless `RESTRAFE_COSMETICS_LOCKED=1` (serialized flags + 0-xp rewards:
// the original offline state).

use crate::ugui::{GKind, ImportCtx, PointerKind, UiCall, UiPointer, UiTree};
use crate::ugui_scene::{f, file_ref, guid_ref, int, rgba, v2, v3, v4, UScene};
use bevy::camera::visibility::RenderLayers;

/// Unity layer of the preview model / camera (AboubiPreview.SetGameLayerRecursive(.., 23)).
pub const PREVIEW_LAYER: usize = 23;

/// Render layer of the customization menu's 3D frame (`HATS MENU HUD`, Unity layer 25 like the options menu) and
/// of its HUDCamera in the port. The original only shows this menu in the main menu, where no player exists: the
/// frame (light probes off) is lit by MainMenu's ambient probe alone. In a match the port's HUD layer also carries the
/// player's green UICam light (settings_menu.rs HudLayerLight), which must not reach this frame.
pub const HATS_HUD_LAYER: usize = 28;

/// MainMenu (build level 0) RenderSettings.m_AmbientProbe, L0 + L1 (r: sh[0..3], g: sh[9..12], b: sh[18..21]; read
/// with UnityPy from data.unity3d, not in the YAML export): the ambient of renderers with light probes off.
const MAIN_MENU_AMBIENT_PROBE: [[f32; 3]; 4] = [
    [0.170_512_77, 0.165_429_41, 0.160_051_45],
    [0.037_263_57, 0.037_067_51, 0.035_940_86],
    [0.024_427_9, 0.023_473_72, 0.022_846_89],
    [0.001_169_193_3, 0.001_178_254_4, 0.001_212_871],
];

/// True while the customization menu is open (read by settings_menu.rs: Escape closes it instead of pausing).
#[derive(Resource, Default)]
pub struct CosmeticsMenuOpen(pub bool);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Suit,
    Hat,
    Cig,
}

#[derive(Clone, Debug)]
struct ItemUi {
    node: usize,
    kind: Kind,
    index: usize,
    acquired: bool,
    dlc: bool,
    supporter: bool,
    challenge: String,
}

#[derive(Resource)]
pub struct CosmeticsUi {
    pub hats_menu: usize,
    /// The menu's HUDCamera node (renders HATS MENU HUD into the menu HUD texture).
    hud_camera: Option<usize>,
    items: Vec<ItemUi>,
    preview_rt: Option<Handle<Image>>,
    float_text: Option<usize>,
    float_display: Option<usize>,
    float_label: Option<usize>,
    name_to_show: String,
    /// GradientDropDown options, set once the tree is resolved.
    gradients: Option<(usize, Vec<String>)>,
    /// Unity world transforms (position, rotation) of COSMETICSSS/PreviewAboubi, its point light (local), the camera.
    preview: (Vec3, [f32; 4]),
    light: (Vec3, [f32; 4], f32, f32),
    camera: (Vec3, [f32; 4], f32, f32, f32, f32),
    rotate_speed: f32,
    /// MainMenu RenderSettings ambient (the preview lives in the main menu scene): Flat mode -> m_AmbientSkyColor.
    ambient: [f32; 4],
}

fn rect_width(tree: &UiTree, n: usize) -> f32 {
    match tree.nodes[n].rect_t.as_ref() {
        Some(r) if r.amin.x != r.amax.x => tree.nodes[n].parent.map(|p| rect_width(tree, p)).unwrap_or(crate::ugui::REF_RES.x) * (r.amax.x - r.amin.x) + r.size_delta.x,
        Some(r) => r.size_delta.x,
        None => 0.0,
    }
}

/// GridLayoutGroup (upper-left start, horizontal axis, flexible constraint) + ContentSizeFitter (vertical preferred).
fn grid_layout(tree: &mut UiTree, content: usize, m: &serde_yaml::Mapping) {
    let pad = m.get("m_Padding");
    let p = |k: &str| int(pad.and_then(|p| p.get(k))) as f32;
    let (left, right, top, bottom) = (p("m_Left"), p("m_Right"), p("m_Top"), p("m_Bottom"));
    let cell = Vec2::from_array(v2(m.get("m_CellSize")));
    let sp = Vec2::from_array(v2(m.get("m_Spacing")));
    let width = rect_width(tree, content);
    let cols = (((width - left - right + sp.x + 0.001) / (cell.x + sp.x)).floor() as usize).max(1);
    let kids: Vec<usize> = tree.nodes[content].children.iter().copied().filter(|&c| tree.nodes[c].active).collect();
    for (k, &c) in kids.iter().enumerate() {
        let (col, row) = ((k % cols) as f32, (k / cols) as f32);
        if let Some(r) = tree.nodes[c].rect_t.as_mut() {
            r.amin = Vec2::new(0.0, 1.0);
            r.amax = Vec2::new(0.0, 1.0);
            r.size_delta = cell;
            r.apos = Vec2::new(left + col * (cell.x + sp.x) + cell.x * r.pivot.x, -(top + row * (cell.y + sp.y) + cell.y * (1.0 - r.pivot.y)));
        }
    }
    let rows = kids.len().div_ceil(cols) as f32;
    if let Some(r) = tree.nodes[content].rect_t.as_mut() {
        r.size_delta.y = top + bottom + rows * cell.y + (rows - 1.0).max(0.0) * sp.y;
    }
}

fn local_tr(sc: &UScene, go: i64) -> (Vec3, Quat) {
    let Some(m) = sc.transform_of(go).and_then(|t| sc.doc(t)) else { return (Vec3::ZERO, Quat::IDENTITY) };
    let q = v4(m.get("m_LocalRotation"));
    (Vec3::from_array(v3(m.get("m_LocalPosition"))), Quat::from_xyzw(q[0], q[1], q[2], q[3]))
}

/// Unity world position / rotation of a (non-UI) scene object.
fn world_tr(sc: &UScene, go: i64) -> (Vec3, Quat) {
    let (mut p, mut r) = local_tr(sc, go);
    let mut g = go;
    while let Some(par) = sc.parent(g) {
        let (pp, pr) = local_tr(sc, par);
        p = pp + pr * p;
        r = pr * r;
        g = par;
    }
    (p, r)
}

/// Imports `--HATS MENU--` (+ the main menu HUD render it is shown with, + FloatTextCanvas) below `canvas` and runs
/// the CosmeticInstance / CosmeticsManager setup. Called by settings_menu.rs before `tree.resolve()`.
pub fn import_hats_menu(sc: &UScene, canvas_go: i64, canvas: usize, tree: &mut UiTree, cx: &mut ImportCtx) -> Option<CosmeticsUi> {
    let main = sc.children(canvas_go).into_iter().find(|&c| sc.go_name(c) == "--MAIN MENU--")?;
    let hats_go = sc.children(main).into_iter().find(|&c| sc.go_name(c) == "--HATS MENU--")?;
    let hats = tree.import(sc, hats_go, Some(canvas), cx);
    tree.nodes[canvas].children.push(hats);
    // MenuController.ActivateMenu(hatsMenu) also activates buttonBar (ButtonsBAR--): its BAR HUD/Lights (HUD-layer point
    // lights, all culling layers) light the menu frame (the teal tint). ButtonsBAR-- / BAR HUD / Lights sit at the
    // main menu's centre like --HATS MENU--, unscaled, so the Lights subtree keeps its world placement below it.
    // DlcStorePageButton.UpdateUI with both DLCs owned (the port's unlock state): notOwnedObjects (DlcNotOwnedLight,
    // ...) inactive.
    if let Some(lights_go) = sc.find_by_name("Lights").into_iter().find(|&g| sc.path(g).ends_with("ButtonsBAR--/BAR HUD/Lights")) {
        let n = tree.import(sc, lights_go, Some(hats), cx);
        tree.nodes[hats].children.push(n);
        if let Some(off) = tree.find(n, "DlcNotOwnedLight") {
            tree.set_active(off, false);
        }
    }
    for (k, name) in ["MenuHUDDisplayTexture", "HUDCamera"].into_iter().enumerate() {
        if let Some(go) = sc.children(main).into_iter().find(|&c| sc.go_name(c) == name) {
            let n = tree.import(sc, go, Some(hats), cx);
            tree.nodes[hats].children.retain(|&c| c != n);
            tree.nodes[hats].children.insert(k, n);
        }
    }
    // ProgressManager XP rewards (challengeDescription "Reach N xp to unlock").
    let mut xp_of: HashMap<i64, i64> = HashMap::new();
    for id in sc.ids_of_class(114) {
        if let Some(m) = sc.doc(id) {
            if m.contains_key("xpPopupPrefab") {
                if let Some(seq) = m.get("instances").and_then(|v| v.as_sequence()) {
                    for inst in seq {
                        let c = file_ref(inst.get("cosmetic"));
                        if c != 0 {
                            xp_of.insert(c, int(inst.get("xpToUnlock")));
                        }
                    }
                }
            }
        }
    }
    let unlock_all = std::env::var_os("RESTRAFE_COSMETICS_LOCKED").is_none();
    let mut items = vec![];
    for (view, kind) in [("HatsView", Kind::Hat), ("SuitsView", Kind::Suit), ("CigsView", Kind::Cig)] {
        let Some(content) = tree.find(hats, &format!("{view}/Scroll View/Viewport/Content")) else { continue };
        let content_go = tree.nodes[content].go;
        let grid = sc.components(content_go).into_iter().find(|c| c.2 == "GridLayoutGroup").and_then(|c| sc.doc(c.0));
        if let Some(m) = &grid {
            grid_layout(tree, content, m);
        }
        // ScrollRect scrollbar visibility (AutoHide = 1, AutoHideAndExpandViewport = 2): a scrollbar that is not
        // needed is deactivated and, for 2, the viewport takes its space back (ScrollRect.UpdateScrollbarVisibility).
        let sv = tree.find(hats, &format!("{view}/Scroll View"));
        let sr = sv.and_then(|s| sc.components(tree.nodes[s].go).into_iter().find(|c| c.2 == "ScrollRect")).and_then(|c| sc.doc(c.0));
        if let (Some(sv), Some(sr), Some(vp)) = (sv, sr, tree.nodes[content].parent) {
            let sv_h = tree.nodes[sv].rect_t.as_ref().map(|r| r.size_delta.y).unwrap_or(0.0);
            if let Some(hb) = tree.find(sv, "Scrollbar Horizontal") {
                // Horizontal scrolling is off (m_Horizontal 0) and the grid never exceeds the viewport width.
                let needed = int(sr.get("m_Horizontal")) != 0 && tree.nodes[content].rect_t.as_ref().is_some_and(|r| r.size_delta.x > 0.0);
                let vis = int(sr.get("m_HorizontalScrollbarVisibility"));
                if !needed && vis != 0 {
                    tree.set_active(hb, false);
                    if vis == 2 {
                        if let Some(r) = tree.nodes[vp].rect_t.as_mut() {
                            r.size_delta.y = 0.0;
                        }
                    }
                }
            }
            if let Some(vb) = tree.find(sv, "Scrollbar Vertical") {
                let vp_h = sv_h + tree.nodes[vp].rect_t.as_ref().map(|r| r.size_delta.y).unwrap_or(0.0);
                let content_h = tree.nodes[content].rect_t.as_ref().map(|r| r.size_delta.y).unwrap_or(0.0);
                let needed = int(sr.get("m_Vertical")) != 0 && content_h > vp_h;
                let vis = int(sr.get("m_VerticalScrollbarVisibility"));
                if !needed && vis != 0 {
                    tree.set_active(vb, false);
                    if vis == 2 {
                        if let Some(r) = tree.nodes[vp].rect_t.as_mut() {
                            r.size_delta.x = 0.0;
                        }
                        if let Some(m) = &grid {
                            grid_layout(tree, content, m);
                        }
                    }
                }
            }
        }
        let mut order = 0;
        for c in tree.nodes[content].children.clone() {
            let Some((id, _, m, _)) = tree.nodes[c].scripts.iter().find(|s| s.1 == "CosmeticInstance").cloned() else { continue };
            if !tree.nodes[c].active {
                continue;
            }
            // CosmeticsManager.Start: hats get their index from the (active) child order, suits/cigs keep theirs.
            let index = if kind == Kind::Hat { order } else { int(m.get("index")).max(0) as usize };
            order += 1;
            let xp = int(m.get("unlockWithXp")) != 0;
            let xp_needed = xp_of.get(&id).copied();
            let challenge = match (xp, xp_needed) {
                (true, Some(n)) => format!("Reach {n} xp to unlock"),
                _ => m.get("challengeDescription").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            };
            let acquired = unlock_all || int(m.get("acquired")) != 0 || (xp && xp_needed.is_some_and(|n| n <= 0));
            // CosmeticInstance.Start / Update: Image sprite + colour, button.interactable = acquired.
            if let Some(img) = tree.find(c, "Image") {
                let sp = m.get("sprite");
                let sprite = match (guid_ref(sp), file_ref(sp)) {
                    (Some(g), fid) if fid != 0 => cx.assets.sprite(&g, fid, cx.images),
                    _ => None,
                };
                if let Some(g) = tree.nodes[img].graphic.as_mut() {
                    if let (GKind::Image { sprite: s, .. }, Some(sp)) = (&mut g.kind, sprite) {
                        *s = Some(sp);
                    }
                    g.color = rgba(m.get(if acquired { "unlockedColor" } else { "lockedColor" }));
                }
            }
            if let Some(sel) = tree.nodes[c].sel.as_mut() {
                sel.interactable = acquired;
            }
            if std::env::var_os("RESTRAFE_DEBUG_COS").is_some() && !challenge.is_empty() {
                println!("COS {} {:?} {:?} order={}", tree.nodes[c].name, kind as u8, challenge, order);
            }
            items.push(ItemUi { node: c, kind, index, acquired, dlc: int(m.get("unlockWithDlc")) != 0, supporter: int(m.get("unlockWithSupporterDlc")) != 0, challenge });
        }
    }
    // PlayerNameGradientManager (Steam achievement-gated name colours; its code is not in the decompiled assemblies):
    // the dropdown lists the gradient presets. The name tag it colours only exists in multiplayer.
    let mut gradients = None;
    for (id, _, _) in sc.components(main) {
        if let Some(m) = sc.doc(id) {
            if let (Some(seq), Some(dd)) = (m.get("gradients").and_then(|v| v.as_sequence()), tree.comp(file_ref(m.get("dropdown")))) {
                let names: Vec<String> = seq
                    .iter()
                    .filter_map(|g| guid_ref(g.get("gradient")).and_then(|guid| cx.assets.path(&guid)))
                    .map(|p| std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
                    .collect();
                gradients = Some((dd, names));
            }
        }
    }
    // FloatTextCanvas (FloatingName).
    let (mut float_text, mut float_display, mut float_label) = (None, None, None);
    if let Some(go) = sc.children(canvas_go).into_iter().find(|&c| sc.go_name(c) == "FloatTextCanvas") {
        let n = tree.import(sc, go, Some(canvas), cx);
        tree.nodes[canvas].children.push(n);
        tree.nodes[n].scale = Vec3::ONE;
        for i in n..tree.nodes.len() {
            if let Some((_, _, m, _)) = tree.nodes[i].scripts.iter().find(|s| s.1 == "FloatingName").cloned() {
                float_text = Some(i);
                float_display = tree.node(file_ref(m.get("display")));
                float_label = tree.comp(file_ref(m.get("text")));
            }
        }
    }
    // COSMETICSSS: preview camera, AboubiPreview and its light.
    let preview_rt = tree.find(hats, "Preview/RawImage").and_then(|n| match tree.nodes[n].graphic.as_ref().map(|g| &g.kind) {
        Some(GKind::Raw { tex, .. }) => tex.clone(),
        _ => None,
    });
    let pick = |name: &str| sc.find_by_name(name).into_iter().find(|&g| sc.path(g).starts_with("COSMETICSSS"));
    // HATS MENU HUD meshes + the HUDCamera rendering them: own layer (see HATS_HUD_LAYER).
    let hud_camera = tree.nodes[hats].children.get(1).copied().filter(|&n| tree.nodes[n].name == "HUDCamera");
    if let Some(hud) = tree.find(hats, "HATS MENU HUD") {
        let mut stack = vec![hud];
        while let Some(n) = stack.pop() {
            // GameObject layer (UiTree::resolve copies it onto the mesh).
            tree.nodes[n].layer = HATS_HUD_LAYER as _;
            stack.extend(tree.nodes[n].children.iter().copied());
        }
    }
    if let Some(crate::ugui::Obj3d::Camera { mask, .. }) = hud_camera.and_then(|c| tree.nodes[c].obj3d.as_mut()) {
        *mask = 1 << HATS_HUD_LAYER;
    }
    let mut ui = CosmeticsUi {
        hats_menu: hats,
        hud_camera,
        items,
        preview_rt,
        float_text,
        float_display,
        float_label,
        name_to_show: String::new(),
        gradients,
        preview: (Vec3::ZERO, [0.0, 0.0, 0.0, 1.0]),
        light: (Vec3::ZERO, [1.0; 4], 0.5, 10.0),
        camera: (Vec3::ZERO, [0.0, 0.0, 0.0, 1.0], 41.54, 0.3, 1000.0, 0.0),
        rotate_speed: 20.0,
        ambient: main_menu_ambient(),
    };
    if let Some(go) = pick("PreviewAboubi") {
        let (p, r) = world_tr(sc, go);
        ui.preview = (p, r.to_array());
        if let Some(m) = sc.components(go).into_iter().find(|c| c.2 == "AboubiPreview").and_then(|c| sc.doc(c.0)) {
            ui.rotate_speed = f(m.get("rotateSpeed"));
        }
        if let Some(l) = sc.children(go).into_iter().find(|&c| sc.components(c).iter().any(|k| k.1 == 108)) {
            let (lp, _) = local_tr(sc, l);
            let m = sc.components(l).into_iter().find(|k| k.1 == 108).and_then(|k| sc.doc(k.0));
            if let Some(m) = m {
                ui.light = (lp, rgba(m.get("m_Color")), f(m.get("m_Intensity")), f(m.get("m_Range")));
            }
        }
    }
    if let Some(go) = pick("Camera") {
        let (p, r) = world_tr(sc, go);
        if let Some(m) = sc.components(go).into_iter().find(|c| c.1 == 20).and_then(|c| sc.doc(c.0)) {
            ui.camera = (p, r.to_array(), f(m.get("field of view")), f(m.get("near clip plane")), f(m.get("far clip plane")), f(m.get("m_Depth")));
        }
    }
    info!("cosmetics menu: {} items ({} hats), preview rt {}", ui.items.len(), ui.items.iter().filter(|i| i.kind == Kind::Hat).count(), ui.preview_rt.is_some());
    Some(ui)
}

#[derive(Component)]
struct PreviewCamera;

/// AboubiPreview / AboubiPreviewLobby: (base rotation, current angle, rotateSpeed deg/s).
#[derive(Component)]
struct PreviewSpin(Quat, f32, f32);

/// The Tab screen's player preview: `GameManager/HostGameCamera` (layer 23 only) renders the spinning
/// `LobbyController.previews[0]` (AboubiPreviewLobby "Preview" under the camera, rotateSpeed 123) into the
/// PlayersPreview RenderTexture shown by `-- TAB SCREEN --/RawImage (2)`.
#[derive(Resource)]
pub struct TabPreviewUi {
    rt: Handle<Image>,
    /// Camera field of view, near, far, depth.
    camera: (f32, f32, f32, f32),
    /// Preview pose relative to the camera (Unity local position / rotation).
    local: (Vec3, Quat),
    rotate_speed: f32,
    /// The Tab screen root (the camera renders only while it is shown).
    pub tab_root: usize,
}

#[derive(Component)]
struct TabPreviewCamera;

/// The Tab screen's preview body (ClientInstance.MenuAnimation drives its Animator).
#[derive(Component)]
struct TabPreviewBody;

/// ClientInstance.Update -> MenuAnimation (owner, every frame): keys 1..6 -> MenuAnimationObservers(i) on
/// LobbyController.previews[PlayerId]: 0 idle, 1 crouch, 2 slide, 3 walk (MovementSpeed 0.5), 4 run (1), 5 crouch walk.
/// (The same keys also play the voice lines, FirstPersonController.HandleTaunt.)
fn tab_preview_pose(keys: Res<ButtonInput<KeyCode>>, mut bodies: Query<&mut crate::animator::DummyAnimParams, With<TabPreviewBody>>) {
    const KEYS: [KeyCode; 6] = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6];
    let Some(i) = KEYS.iter().position(|k| keys.just_pressed(*k)) else { return };
    // (crouch, slide, MovementSpeed, crouchMove)
    const POSES: [(bool, bool, f32, f32); 6] = [(false, false, 0.0, 0.0), (true, false, 0.0, 0.0), (false, true, 0.0, 0.0), (false, false, 0.5, 0.0), (false, false, 1.0, 0.0), (true, false, 0.0, 1.0)];
    let (crouch, slide, speed, crouch_move) = POSES[i];
    for mut p in &mut bodies {
        p.crouch = crouch;
        p.slide = slide;
        p.movement_speed = speed;
        p.crouch_move = crouch_move;
    }
}

/// Where the Tab preview rig lives (far from the cosmetics preview, which renders layer 23 as well).
const TAB_PREVIEW_ORIGIN: Vec3 = Vec3::new(0.0, -3000.0, 0.0);

/// Reads the Tab screen preview set-up from MainMenu.unity (LobbyController.previews[0], its camera, the RawImage).
pub fn import_tab_preview(sc: &UScene, tree: &UiTree, tab_root: usize) -> Option<TabPreviewUi> {
    let raw = tree.find(tab_root, "RawImage (2)")?;
    let rt = match tree.nodes[raw].graphic.as_ref().map(|g| &g.kind) {
        Some(GKind::Raw { tex, .. }) => tex.clone()?,
        _ => return None,
    };
    let lc = sc.find_by_name("LobbyController").into_iter().flat_map(|g| sc.components(g)).find(|c| c.2 == "LobbyController").and_then(|c| sc.doc(c.0))?;
    let first = lc.get("previews").and_then(|v| v.as_sequence()).and_then(|s| s.first()).map(|v| crate::ugui_scene::file_ref(Some(v)))?;
    let preview_go = sc.doc(first).map(|m| crate::ugui_scene::file_ref(m.get("m_GameObject"))).filter(|g| *g != 0)?;
    let speed = sc.doc(first).map(|m| f(m.get("rotateSpeed"))).unwrap_or(20.0);
    let cam_go = sc.parent(preview_go)?;
    let cam = sc.components(cam_go).into_iter().find(|c| c.1 == 20).and_then(|c| sc.doc(c.0))?;
    Some(TabPreviewUi {
        rt,
        camera: (f(cam.get("field of view")), f(cam.get("near clip plane")), f(cam.get("far clip plane")), f(cam.get("m_Depth"))),
        local: local_tr(sc, preview_go),
        rotate_speed: speed,
        tab_root,
    })
}

/// Spawns the Tab preview rig once imported: camera -> PlayersPreview RT, a dressed spinning preview body in front of
/// it, fully lit (white ambient: albedo as is; user request).
fn spawn_tab_preview(mut commands: Commands, ui: Option<Res<TabPreviewUi>>, assets: Res<AssetServer>, mut done: Local<bool>) {
    let Some(ui) = ui else { return };
    if *done {
        return;
    }
    *done = true;
    let layers = RenderLayers::layer(PREVIEW_LAYER);
    let (fov, near, far, depth) = ui.camera;
    let cam = commands
        .spawn((
            Name::new("TabPreviewCamera"),
            TabPreviewCamera,
            Camera3d::default(),
            Camera { order: -20 + depth as isize, clear_color: ClearColorConfig::Custom(Color::srgba(0.0, 0.0, 0.0, 0.0)), is_active: false, ..default() },
            Projection::Perspective(PerspectiveProjection { fov: fov.to_radians(), near, far, ..default() }),
            Transform::from_translation(TAB_PREVIEW_ORIGIN),
            layers,
            Msaa::Off,
            bevy::core_pipeline::tonemapping::Tonemapping::None,
            bevy::camera::Exposure { ev100: 0.0 },
            // Ambient brightness 1.2 at EV 0 = irradiance 1: the albedo unshaded.
            AmbientLight { color: Color::WHITE, brightness: 1.2, affects_lightmapped_meshes: true },
            bevy::camera::RenderTarget::Image(ui.rt.clone().into()),
        ))
        .id();
    let _ = cam;
    let (lp, lr) = ui.local;
    let base = crate::ugui_assets::unity_quat(lr.to_array());
    let spin = commands
        .spawn((Name::new("TabPreviewSpin"), PreviewSpin(base, 0.0, ui.rotate_speed), Transform::from_translation(TAB_PREVIEW_ORIGIN + unity_pos(lp)).with_rotation(base), Visibility::default()))
        .id();
    let body = crate::health::spawn_preview_body(&mut commands, &assets, Transform::default());
    commands.entity(body).insert((ChildOf(spin), TabPreviewBody));
}

/// The Tab preview camera renders only while the Tab screen is shown.
fn tab_preview_camera(ui: Option<Res<TabPreviewUi>>, tree: Res<UiTree>, mut cams: Query<&mut Camera, With<TabPreviewCamera>>) {
    let Some(ui) = ui else { return };
    let on = tree.nodes[ui.tab_root].active;
    for mut c in &mut cams {
        if c.is_active != on {
            c.is_active = on;
        }
    }
}

fn unity_pos(p: Vec3) -> Vec3 {
    Vec3::new(p.x, p.y, -p.z)
}

/// Spawns COSMETICSSS once the menu is imported: preview camera -> CosmeticPreview RT, spinning PreviewAboubi.
fn spawn_preview(mut commands: Commands, ui: Option<Res<CosmeticsUi>>, assets: Res<AssetServer>, mut images: ResMut<Assets<Image>>, mut done: Local<bool>) {
    let Some(ui) = ui else { return };
    if *done {
        return;
    }
    *done = true;
    let layers = RenderLayers::layer(PREVIEW_LAYER);
    let (cp, cr, fov, near, far, depth) = ui.camera;
    let mut cam = commands.spawn((
        Name::new("CosmeticsPreviewCamera"),
        PreviewCamera,
        Camera3d::default(),
        Camera { order: -20 + depth as isize, clear_color: ClearColorConfig::Custom(Color::srgba(0.0, 0.0, 0.0, 0.0)), is_active: false, ..default() },
        Projection::Perspective(PerspectiveProjection { fov: fov.to_radians(), near, far, ..default() }),
        Transform::from_translation(unity_pos(cp)).with_rotation(crate::ugui_assets::unity_quat(cr)),
        layers.clone(),
        Msaa::Off,
        bevy::core_pipeline::tonemapping::Tonemapping::None,
        bevy::camera::Exposure { ev100: 0.0 },
        AmbientLight { color: Color::srgb(ui.ambient[0], ui.ambient[1], ui.ambient[2]), brightness: 1.2, affects_lightmapped_meshes: true },
    ));
    if let Some(t) = &ui.preview_rt {
        cam.insert(bevy::camera::RenderTarget::Image(t.clone().into()));
    }
    // MainMenu's baked light probes (8, ~45 m away: Unity extrapolates through the hull's outer cells) light the preview
    // body, evaluated at its bounds centre (~1 m above PreviewAboubi). Without them the side facing away from the
    // PreviewAboubi point light was pitch black. Exposure of this camera: ev100 0 -> 1 / 1.2.
    let at = unity_pos(ui.preview.0) + Vec3::Y;
    if let Some(env) = crate::scene_map::probe_environment(&mut images, "assets/maps/MainMenu_LightProbes.json", at, 1.0 / 1.2) {
        cam.insert(env);
    }

    let (pp, pr) = ui.preview;
    let base = crate::ugui_assets::unity_quat(pr);
    let spin = commands.spawn((Name::new("PreviewAboubiSpin"), PreviewSpin(base, 0.0, ui.rotate_speed), Transform::from_translation(unity_pos(pp)).with_rotation(base), Visibility::default())).id();
    let (lp, lc, li, lr) = ui.light;
    // Same Unity intensity -> Bevy conversion as the ugui menu lights (camera ev100 = 0).
    let lux = li * std::f32::consts::PI * 1.2;
    commands.spawn((
        Name::new("PreviewAboubiLight"),
        // Unity built-in falloff (unity_light.rs; the lumen conversion equals unity_point_lumens at this camera's ev100 0).
        PointLight { color: Color::srgb(lc[0], lc[1], lc[2]), intensity: lux * 4.0 * std::f32::consts::PI, range: lr, radius: crate::unity_light::UNITY_LIGHT_RADIUS, shadow_maps_enabled: false, ..default() },
        Transform::from_translation(unity_pos(lp)),
        layers,
        ChildOf(spin),
    ));
    let body = crate::health::spawn_preview_body(&mut commands, &assets, Transform::default());
    commands.entity(body).insert(ChildOf(spin));
}

/// AboubiPreview.Update (transform.Rotate(0, rotateSpeed * dt, 0)) + SetGameLayerRecursive(.., 23) for everything
/// below the preview (body parts, hat, cig spawn asynchronously).
fn preview_update(
    mut commands: Commands,
    time: Res<Time>,
    ui: Option<Res<CosmeticsUi>>,
    mut spin: Query<(&mut PreviewSpin, &mut Transform)>,
    roots: Query<Entity, With<crate::health::PreviewBody>>,
    children: Query<&Children>,
    unlayered: Query<(), (With<Mesh3d>, Without<RenderLayers>)>,
) {
    let _ = ui;
    for (mut s, mut t) in &mut spin {
        s.1 = (s.1 + s.2 * time.delta_secs()) % 360.0;
        t.rotation = s.0 * crate::unity::unity_euler(0.0, s.1, 0.0);
    }
    for r in &roots {
        let mut stack = vec![r];
        while let Some(e) = stack.pop() {
            if unlayered.contains(e) {
                commands.entity(e).insert(RenderLayers::layer(PREVIEW_LAYER));
            }
            if let Ok(k) = children.get(e) {
                stack.extend(k.iter());
            }
        }
    }
}

/// CosmeticInstance.OnPointerEnter text for FloatingName.
fn hover_text(it: &ItemUi) -> String {
    let what = match it.kind {
        Kind::Hat => "Hat",
        Kind::Cig => "Cig",
        Kind::Suit => "Suit",
    };
    if it.supporter {
        if !it.acquired { "Get the Supporter Edition DLC !".into() } else { format!("Supporter DLC {what} unlocked !") }
    } else if it.dlc {
        if !it.acquired { "Get the DLC !".into() } else { format!("DLC {what} unlocked !") }
    } else if !it.acquired {
        it.challenge.clone()
    } else if !it.challenge.is_empty() {
        format!("<s>{}</s> Done!", it.challenge)
    } else {
        String::new()
    }
}

/// CosmeticsManager.ChangeDress (index wrap) -> AboubiPreview + local player / dummy dress (apply_dress).
fn change_dress(cos: &mut Cosmetics, data: &CosmeticsData, it: &ItemUi) {
    match it.kind {
        Kind::Hat => cos.hat = it.index,
        Kind::Cig => cos.cig = it.index,
        Kind::Suit => cos.suit = it.index,
    }
    cos.wrap(data);
}

/// K opens / closes the customization screen (MenuController.ActivateMenu(hatsMenu) in the main menu), Escape and
/// OK close it; item clicks dress; FloatingName hover text; preview camera enabled while shown (AboubiCameraControl).
#[allow(clippy::too_many_arguments)]
fn hats_menu_update(
    keys: Res<ButtonInput<KeyCode>>,
    ui: Option<ResMut<CosmeticsUi>>,
    mut tree: ResMut<UiTree>,
    mut open: ResMut<CosmeticsMenuOpen>,
    paused: Res<crate::settings_menu::MenuOpen>,
    data: Res<CosmeticsData>,
    mut cos: ResMut<Cosmetics>,
    (mut calls, mut ptrs): (MessageReader<UiCall>, MessageReader<UiPointer>),
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut input: ResMut<PlayerInput>,
    mut cams: Query<&mut Camera, With<PreviewCamera>>,
) {
    let Some(mut ui) = ui else { return };
    let ui = &mut *ui;
    let tree = &mut *tree;
    if let Some((dd, names)) = ui.gradients.take() {
        tree.dropdown_set_options(dd, names);
    }
    let mut want = open.0;
    if keys.just_pressed(KeyCode::KeyK) && !paused.0 {
        want = !want;
    }
    // The PauseManager `Menu` action (<Keyboard>/escape, <Gamepad>/start) closes it (no script in the original
    // closes the hats menu by key: it is a main-menu page left through MenuController.ActivateMenu / the back button).
    if (keys.just_pressed(KeyCode::Escape) || input.pad_menu) && open.0 {
        want = false;
    }
    let before = *cos;
    for c in calls.read() {
        match (c.call.type_name.as_str(), c.call.method.as_str()) {
            ("CosmeticInstance", "ChangeDress") => {
                let n = tree.script_node(c.call.target);
                if let Some(it) = ui.items.iter().find(|i| Some(i.node) == n) {
                    change_dress(&mut cos, &data, it);
                    info!("cosmetics: ChangeDress {:?} {} -> suit {} hat {} cig {}", it.kind, it.index, cos.suit, cos.hat, cos.cig);
                }
            }
            ("MenuController", "ActivateMenu") if open.0 => want = false,
            _ => {}
        }
    }
    if *cos != before {
        cos.save();
    }
    for p in ptrs.read() {
        let Some(it) = ui.items.iter().find(|i| i.node == p.node) else { continue };
        match p.kind {
            PointerKind::Enter => ui.name_to_show = hover_text(it),
            PointerKind::Exit => ui.name_to_show.clear(),
            _ => {}
        }
    }
    if want != open.0 {
        open.0 = want;
        tree.set_active(ui.hats_menu, want);
        if !want {
            ui.name_to_show.clear();
        }
        if let Ok(mut c) = cursor.single_mut() {
            c.grab_mode = if want { CursorGrabMode::None } else { CursorGrabMode::Locked };
            c.visible = want;
        }
        input.enabled = !want;
    }
    if open.0 {
        input.enabled = false;
    }
    for mut c in &mut cams {
        if c.is_active != open.0 {
            c.is_active = open.0;
        }
    }
    float_name(tree, ui, open.0);
}

/// FloatingName.Update: display active when there is a text, text at the mouse position. The canvas uses
/// ConstantPixelSize (scale 1) -> the node is scaled by 1 / canvas scale. The display box is sized like its
/// ContentSizeFitter + VerticalLayoutGroup (padding 5 left/right) would.
fn float_name(tree: &mut UiTree, ui: &CosmeticsUi, open: bool) {
    let name = if open { ui.name_to_show.as_str() } else { "" };
    if let Some(d) = ui.float_display {
        if tree.nodes[d].active != !name.is_empty() {
            tree.set_active(d, !name.is_empty());
        }
    }
    if name.is_empty() {
        return;
    }
    // `<s>` (completed challenges) is drawn by tmp_text::layout_ugui; the width ignores the tags.
    let shown = name.to_string();
    let Some(t) = ui.float_label else { return };
    if tree.text(t) != Some(shown.as_str()) {
        tree.set_text(t, &shown);
    }
    let size = match tree.nodes[t].graphic.as_ref().map(|g| &g.kind) {
        Some(GKind::Text(tg)) => {
            let l = crate::tmp_text::TmpUiLayout {
                font_size: tg.size,
                rect_size: Vec2::splat(10000.0),
                pivot: Vec2::splat(0.5),
                margin: tg.margin,
                h_align: tg.h_align,
                v_align: tg.v_align,
                char_spacing: tg.char_spacing,
                word_spacing: tg.word_spacing,
                line_spacing: tg.line_spacing,
                paragraph_spacing: 0.0,
                wrap: false,
                style: tg.style,
                grad_ratio: tg.props.grad_ratio(),
            };
            let w = crate::tmp_text::ui_line_width(&tg.font, &l, &shown);
            let h = (tg.font.ascent - tg.font.descent) * crate::tmp_text::ui_font_scale(&tg.font, tg.size);
            Some(Vec2::new(w, h))
        }
        _ => None,
    };
    if let Some(s) = size {
        if let Some(r) = ui.float_display.and_then(|d| tree.nodes[d].rect_t.as_mut()) {
            r.size_delta = Vec2::new(s.x + 10.0, s.y);
        }
        if let Some(r) = tree.nodes[t].rect_t.as_mut() {
            r.amin = Vec2::new(0.0, 1.0);
            r.amax = Vec2::new(0.0, 1.0);
            r.size_delta = s;
            r.apos = Vec2::new(5.0 + s.x * r.pivot.x, -s.y * (1.0 - r.pivot.y));
        }
    }
    if let (Some(ft), Some(p)) = (ui.float_text, tree.pointer) {
        let (half_h, inv) = (tree.canvas_size.y * 0.5, 1.0 / tree.scale.max(1e-4));
        tree.nodes[ft].scale = Vec3::splat(inv);
        if let Some(r) = tree.nodes[ft].rect_t.as_mut() {
            r.apos = Vec2::new(p.x, p.y + half_h);
        }
    }
}

// ---------------------------------------------------------------- plugin

pub struct CosmeticsPlugin;

impl Plugin for CosmeticsPlugin {
    fn build(&self, app: &mut App) {
        // the old generated file, else the list built from the installation's MainMenu scene
        let json = std::fs::read_to_string(DATA_PATH).ok().unwrap_or_else(|| {
            let owned = crate::owned::Owned::global().expect("STRAFTAT installation");
            crate::owned_cosmetics::cosmetics_json(owned).map(|v| v.to_string()).unwrap_or_else(|e| panic!("cosmetics: {e}"))
        });
        let data: CosmeticsData = serde_json::from_str(&json).expect("cosmetics data");
        let mut cos = Cosmetics::load();
        cos.wrap(&data);
        app.insert_resource(data)
            .insert_resource(cos)
            .init_resource::<CosmeticMaterials>()
            .init_resource::<CosmeticsMenuOpen>()
            .add_systems(
                Update,
                (
                    hats_menu_update.after(crate::settings_menu::menu_update).before(crate::input::gather_input),
                    (spawn_preview, spawn_tab_preview, setup_dress, apply_dress, sync_dress_visibility, preview_update, tab_preview_camera).chain(),
                    preview_forward_materials.after(preview_update),
                    unity_item_materials.before(preview_forward_materials),
                    tab_preview_pose.after(crate::input::gather_input),
                    hats_hud_ambient,
                ),
            );
    }
}

/// The customization menu's HUDCamera: MainMenu's ambient probe as its (diffuse) environment, no flat ambient.
fn hats_hud_ambient(ui: Option<Res<CosmeticsUi>>, tree: Res<UiTree>, mut cams: Query<&mut AmbientLight, Without<bevy::light::EnvironmentMapLight>>, mut images: ResMut<Assets<Image>>, mut commands: Commands) {
    let Some(e) = ui.and_then(|u| u.hud_camera).and_then(|n| tree.nodes[n].entity) else { return };
    let Ok(mut amb) = cams.get_mut(e) else { return };
    amb.brightness = 0.0;
    // Menu cameras: ev100 0 (view exposure 1 / 1.2).
    commands.entity(e).insert(crate::scene_map::sh_environment(&mut images, MAIN_MENU_AMBIENT_PROBE.map(Vec3::from_array), 1.0 / 1.2));
}

/// On a hat / cig instance: its glTF materials (AssetRipper's guess: base colour only, glTF default metallic 1 /
/// roughness 1) are replaced by the converted Unity materials of the same name (S_Hats_00 = Standard metallic, see
/// ugui_assets.rs). `.0` counts the frames waited for the scene; given up after 600 (No Hat: no mesh at all).
#[derive(Component)]
pub struct UnityItemMaterials(u32);

fn unity_item_materials(
    mut commands: Commands,
    mut roots: Query<(Entity, &mut UnityItemMaterials)>,
    children: Query<&Children>,
    gltf: Query<(&bevy::gltf::GltfMaterialName, Has<MeshMaterial3d<StandardMaterial>>)>,
    mut umats: crate::unity_pbr::UnityMatParams,
) {
    for (root, mut st) in &mut roots {
        st.0 += 1;
        let mut found = false;
        for e in children.iter_descendants(root) {
            let Ok((name, is_std)) = gltf.get(e) else { continue };
            found = true;
            if !is_std {
                continue;
            }
            if let Some(m) = umats.material(&name.0) {
                commands.entity(e).remove::<MeshMaterial3d<StandardMaterial>>().insert(MeshMaterial3d(m));
            }
        }
        if found || st.0 > 600 {
            commands.entity(root).remove::<UnityItemMaterials>();
        }
    }
}

/// `RenderSettings` of MainMenu.unity: ambient colour for m_AmbientMode 3 (Flat) = m_AmbientSkyColor (black in the
/// shipped scene, so the preview is lit by its point light only). Other modes fall back to the equator colour.
fn main_menu_ambient() -> [f32; 4] {
    // MainMenu's RenderSettings, read from the installation
    let docs = crate::owned_docs::scene("MainMenu").unwrap_or_default();
    let Some(rs) = docs.iter().find(|d| d.1 == 104).map(|d| serde_yaml::Value::Mapping((*d.2).clone())) else { return [0.0; 4] };
    let m = Some(&rs);
    let mode = int(m.and_then(|m| m.get("m_AmbientMode")));
    rgba(m.and_then(|m| m.get(if mode == 3 { "m_AmbientSkyColor" } else { "m_AmbientEquatorColor" })))
}

/// The preview cameras (cosmetics menu, Tab screen) render forward with their own light and ambient, like the MainMenu
/// scene's cameras; opaque materials default to the deferred path, which those cameras skip (and lit black with a
/// G-buffer). Preview-layer meshes get forward copies of their (shared with the dummy) materials.
#[allow(clippy::type_complexity)]
fn preview_forward_materials(
    mut unity: Query<(&mut MeshMaterial3d<crate::unity_pbr::UnityPbr>, &RenderLayers)>,
    mut standard: Query<(&mut MeshMaterial3d<StandardMaterial>, &RenderLayers)>,
    mut unity_mats: ResMut<Assets<crate::unity_pbr::UnityPbr>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
    mut cache: Local<std::collections::HashMap<bevy::asset::UntypedAssetId, bevy::asset::UntypedHandle>>,
) {
    if std::env::var_os("RESTRAFE_FORWARD").is_some() {
        return;
    }
    use bevy::material::OpaqueRendererMethod as M;
    for (mut m, l) in &mut unity {
        if !l.intersects(&RenderLayers::layer(PREVIEW_LAYER)) {
            continue;
        }
        let Some(src) = unity_mats.get(&m.0) else { continue };
        if src.base.opaque_render_method == M::Forward {
            continue;
        }
        let id = m.0.id().untyped();
        let h = match cache.get(&id) {
            Some(h) => h.clone().typed::<crate::unity_pbr::UnityPbr>(),
            None => {
                let mut c = src.clone();
                c.base.opaque_render_method = M::Forward;
                let h = unity_mats.add(c);
                cache.insert(id, h.clone().untyped());
                h
            }
        };
        m.0 = h;
    }
    for (mut m, l) in &mut standard {
        if !l.intersects(&RenderLayers::layer(PREVIEW_LAYER)) {
            continue;
        }
        let Some(src) = std_mats.get(&m.0) else { continue };
        if src.opaque_render_method == M::Forward {
            continue;
        }
        let id = m.0.id().untyped();
        let h = match cache.get(&id) {
            Some(h) => h.clone().typed::<StandardMaterial>(),
            None => {
                let mut c = src.clone();
                c.opaque_render_method = M::Forward;
                let h = std_mats.add(c);
                cache.insert(id, h.clone().untyped());
                h
            }
        };
        m.0 = h;
    }
}
