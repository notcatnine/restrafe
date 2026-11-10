//! In-game pause + settings UI: the real `--PAUSE MENU--` and `-- OPTIONS MENU --` of the "---USER INTERFACE---"
//! canvas (MainMenu.unity, DontDestroyOnLoad), imported with `ugui.rs`, driven like the original scripts:
//!
//! * `PauseManager.Menu` (Escape): closes the options menu, toggles the pause menu, `firstInterface3D.ChooseState(true)`,
//!   FirstInterface on / SecondInterface off; cursor unlocked + gameplay input off while paused; crosshair hidden.
//! * Pause screen = 3D model (PF_PauseMenu_Prefab_00 rendered by its own camera into PauseMenuHUD.renderTexture, shown
//!   by the "3DRender" RawImage) + invisible buttons (ResumeButton, "SettingsButton (1)", MenuButton) whose
//!   `ModelButtonHandler` push the 3D buttons (`ModelButtonTween`) and `MoveUIObject` slides the panels.
//! * "SettingsButton (1)" opens `-- OPTIONS MENU --` (3D "OPTIONS HUD" rendered by HUDCamera into MainMenuHUD) with the
//!   Controls / Audio / Graphics tabs (PcTab / AudioTab / GraphTab), Exit closes options + pause.
//! * Scripts: ButtonSizeTween (menu sounds + DOScale), ModelButtonHandler/ModelButtonTween, MoveUIObject,
//!   DisableAfterTime, ChangeTextValue, DisplayLoadValues, ReBindUI / ResetAllBindings / InputManager rebinding,
//!   SettingsResolution, ResetScreenResolution; persistent calls to `Settings.*` / `SoundManager.*` update
//!   [`Settings`] (saved to config/settings.ron like PlayerPrefs.Save()).
//! "Leave match" (MenuButton -> SteamLobby.LeaveMatch) quits the game (there is no main menu to return to).
//! Gamepad: ChangeSelectedItem / PlaySoundWithGamepad, the gamepad branches of ButtonSizeTween / ModelButtonHandler,
//! PauseManager.LateUpdate's fallback selection, gamepad-column rebinding (navigation itself: ugui.rs).
//! Out of scope (no-ops): music player, voice chat, custom crosshair images, outline colours (values kept in the UI only).

use crate::input::{Bind, Bindings, PlayerInput};
use crate::settings::Settings;
use crate::tween::Ease;
use crate::ugui::{CallValue, ImportCtx, PointerKind, TweenProp, UiCall, UiPointer, UiTree, UguiAmbient, UguiRender, Widget};
use crate::ugui_assets::UnityAssets;
use crate::ugui_scene::{f, file_ref, int, v3, UScene};
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PresentMode, PrimaryWindow, WindowMode};
use serde_yaml::Value;
use std::collections::HashMap;

/// True while the pause menu is open (`PauseManager.pause`).
#[derive(Resource, Default)]
pub struct MenuOpen(pub bool);

#[derive(Clone)]
struct MoveUi {
    active_pos: Vec3,
    offset: Vec3,
    duration: f32,
    ease: Ease,
    state: bool,
}

#[derive(Clone)]
struct ModelBtn {
    initial: Vec3,
    dir: Vec3,
    hover: f32,
    press: f32,
    duration: f32,
}

#[derive(Clone)]
struct SizeTween {
    target: usize,
    target_scale: Vec3,
    duration: f32,
    ease: Ease,
    init_scale: Vec3,
    close_window: bool,
    bigger_when_selected: bool,
    gamepad_only: bool,
    selected_scale: Vec3,
    duration2: f32,
    ease2: Ease,
    is_selected: bool,
    has_button: bool,
}

#[derive(Clone)]
struct RebindUi {
    action: String,
    index: usize,
    action_text: Option<usize>,
    rebind_btn: Option<usize>,
    rebind_text: Option<usize>,
    reset_btn: Option<usize>,
    sequence: bool,
    /// Text shown by the scene for the default binding (Unity's GetBindingDisplayString).
    default_text: String,
}

struct Rebinding {
    ui: usize,
    part: usize,
    started: f32,
}

#[derive(Resource, Default)]
pub struct PauseUi {
    pub ready: bool,
    pub canvas: usize,
    pub pause_menu: Option<usize>,
    pub options_menu: Option<usize>,
    first_interface: Option<usize>,
    second_interface: Option<usize>,
    first3d: Option<usize>,
    seq_display: Option<usize>,
    time: f32,
    prev_on: Vec<bool>,
    move_ui: HashMap<usize, MoveUi>,
    model_btn: HashMap<usize, ModelBtn>,
    handler_btn: HashMap<usize, usize>,
    size_tween: HashMap<usize, SizeTween>,
    timers: Vec<(f32, usize)>,
    rebind_ui: HashMap<usize, RebindUi>,
    rebinding: Option<Rebinding>,
    clips: HashMap<String, String>,
    resolutions: Vec<UVec3>,
    loaded_values: bool,
    /// UI-only values (colour inputs / outline width etc.).
    pub extra: HashMap<String, f32>,
    /// PauseManager.resumeButton3D (selected when the pause menu opens with a gamepad).
    resume3d: Option<usize>,
    /// PauseManager.gamepad this frame.
    gamepad: bool,
    /// Menu clips requested by persistent calls (PlaySoundWithGamepad).
    sounds: Vec<&'static str>,
}

fn ease(v: i64) -> Ease {
    match Ease::from_i32(v as i32) {
        Ease::Unset => Ease::OutQuad,
        e => e,
    }
}

fn clip_path(assets: &mut UnityAssets, m: &serde_yaml::Mapping, key: &str) -> Option<String> {
    crate::ugui_scene::guid_ref(m.get(key)).and_then(|g| assets.path(&g))
}

/// Loads the canvas and builds the script state (PauseManager.Awake, the scripts' Awake/Start).
#[allow(clippy::too_many_arguments)]
fn setup_pause_ui(
    mut commands: Commands,
    mut tree: ResMut<UiTree>,
    mut ui: ResMut<PauseUi>,
    mut uassets: ResMut<UnityAssets>,
    mut rend: ResMut<UguiRender>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut fxm: ResMut<Assets<crate::ugui_assets::UnityFxMaterial>>,
    settings: Res<Settings>,
) {
    let t0 = std::time::Instant::now();
    let Some(sc) = UScene::load("Scenes/MainMenu/MainMenu.unity") else {
        warn!("pause menu: MainMenu.unity not found (run setup)");
        return;
    };
    let Some(&canvas_go) = sc.find_by_name("---USER INTERFACE---").first() else { return };
    let rend = &mut *rend;
    let mut cx = ImportCtx { assets: &mut uassets, images: &mut images, meshes: &mut meshes, materials: &mut mats, fx_materials: &mut fxm, fonts: &mut rend.fonts, tmp_mats: &mut rend.tmp_props };
    // SceneMotor/Canvas (sorting order -1, drawn below ---USER INTERFACE---): ExploratioNText.
    let scene_motor = UScene::load("GameObject/SceneMotor.prefab");
    let motor_canvas = tree.add_canvas_root("SceneMotor/Canvas");
    let exploration = scene_motor.as_ref().and_then(|sm| {
        let go = *sm.find_by_name("ExploratioNText").first()?;
        let n = tree.import(sm, go, Some(motor_canvas), &mut cx);
        tree.nodes[motor_canvas].children.push(n);
        Some(n)
    });
    let canvas = tree.add_canvas_root("---USER INTERFACE---");
    // In-game screen UI (hierarchy order: Speedometer, then the menus, ---MINIMAL UI--- after the pause menu).
    let mut screen = crate::screen_ui::ScreenUiNodes { exploration, ..default() };
    if let Some(go) = sc.children(canvas_go).into_iter().find(|&c| sc.go_name(c) == "Speedometer") {
        let n = tree.import(&sc, go, Some(canvas), &mut cx);
        tree.nodes[canvas].children.push(n);
        screen.speedometer = Some(n);
    }
    // START ROUND TEXT (deadtext, HealthEnemyText, ...; texts empty until PauseManager writes them).
    if let Some(go) = sc.children(canvas_go).into_iter().find(|&c| sc.go_name(c) == "START ROUND TEXT") {
        let n = tree.import(&sc, go, Some(canvas), &mut cx);
        tree.nodes[canvas].children.push(n);
        // KillCam.Start: GameObject.Find("SwitchCamText" / "PlayerNameText" / "PlayerHpText").
        screen.kill_cam = [tree.find(n, "SwitchCamText"), tree.find(n, "PlayerHpText"), tree.find(n, "PlayerNameText")];
    }
    // --MAIN MENU--/--HATS MENU-- (customization screen, opened with K in game) + FloatTextCanvas.
    let cosmetics_ui = crate::cosmetics::import_hats_menu(&sc, canvas_go, canvas, &mut tree, &mut cx);
    for name in ["-- OPTIONS MENU --", "--PAUSE MENU--"] {
        if let Some(go) = sc.children(canvas_go).into_iter().find(|&c| sc.go_name(c) == name) {
            let n = tree.import(&sc, go, Some(canvas), &mut cx);
            tree.nodes[canvas].children.push(n);
        }
    }
    // -- TAB SCREEN -- (PauseManager.tabScreen, shown while Tab is held; TabScreen texts) with the local player's
    // PlayerListItem at LobbyController.tabhostPosition (LobbyController.CreateNewObjects).
    if let Some(go) = sc.children(canvas_go).into_iter().find(|&c| sc.go_name(c) == "-- TAB SCREEN --") {
        let n = tree.import(&sc, go, Some(canvas), &mut cx);
        tree.nodes[canvas].children.push(n);
        let mut tab = crate::screen_ui::TabNodes { root: n, ..default() };
        if let Some(ts) = sc.components(go).into_iter().find(|c| c.2 == "TabScreen").and_then(|c| sc.doc(c.0)) {
            let t = |k: &str| tree.comp(file_ref(ts.get(k)));
            tab.round = t("roundText");
            tab.scores = [t("scoreOne"), t("scoreTwo"), t("scoreThree"), t("scoreFour")];
            tab.map = t("mapText");
        }
        let host = sc
            .find_by_name("LobbyController")
            .into_iter()
            .flat_map(|g| sc.components(g))
            .find(|c| c.2 == "LobbyController")
            .and_then(|c| sc.doc(c.0))
            .and_then(|lc| tree.comp(file_ref(lc.get("tabhostPosition"))))
            .or_else(|| tree.find(n, "tabHostObject"));
        if let (Some(host), Some(item_sc)) = (host, UScene::load("GameObject/PlayerListItem.prefab")) {
            if let Some(&root_go) = item_sc.find_by_name("PlayerListItem").first() {
                let item = tree.import(&item_sc, root_go, Some(host), &mut cx);
                tree.nodes[host].children.push(item);
                // transform.position = tabhostPosition.position, localScale one: the item's pivot on the host's pivot.
                let host_pivot = tree.nodes[host].rect_t.as_ref().map(|r| r.pivot).unwrap_or(Vec2::splat(0.5));
                if let Some(rt) = tree.nodes[item].rect_t.as_mut() {
                    rt.amin = host_pivot;
                    rt.amax = host_pivot;
                    rt.apos = Vec2::ZERO;
                }
                tree.nodes[item].scale = Vec3::ONE;
                if let Some(pl) = item_sc.components(root_go).into_iter().find(|c| c.2 == "PlayerListItem").and_then(|c| item_sc.doc(c.0)) {
                    let comp = |k: &str| tree.comp(file_ref(pl.get(k)));
                    let node = |k: &str| tree.node(file_ref(pl.get(k)));
                    tab.player_name = comp("PlayerNameText");
                    tab.player_ready = comp("PlayerReadyText");
                    tab.not_ready_color = crate::ugui_scene::rgba(pl.get("notreadyColor"));
                    tab.icon = comp("PlayerIcon");
                    // PlayerListItem.Start (local player, host, no teams): kick / ban / mute / add buttons and the dev
                    // badge off, team dropdown off (GameManager.playingTeams false); Update: cosmetics button scale 0 in game.
                    let mut off: Vec<usize> = ["kickbutton", "banbutton", "mutebutton", "textmutebutton", "addButton", "DevloperIndicator"].into_iter().filter_map(node).collect();
                    off.extend(comp("teamIdDropdown"));
                    tab.cosmetic_button = node("cosmeticbutton");
                    for b in off {
                        tree.set_active(b, false);
                    }
                }
            }
        }
        if let Some(p) = crate::cosmetics::import_tab_preview(&sc, &tree, n) {
            commands.insert_resource(p);
        }
        screen.tab = Some(tab);
    }
    if let Some(go) = sc.children(canvas_go).into_iter().find(|&c| sc.go_name(c) == "---MINIMAL UI---") {
        let n = tree.import(&sc, go, Some(canvas), &mut cx);
        tree.nodes[canvas].children.push(n);
        screen.minimal = Some(n);
    }
    tree.resolve();
    tree.init_widgets();
    if let Some(c) = cosmetics_ui {
        commands.insert_resource(c);
    }
    ui.canvas = canvas;
    // PauseManager.minimalistHealthText / minimalFpsText / minimalPingText / right/leftGunAmmo(Reload).
    if let Some(pm) = sc.components(canvas_go).into_iter().find(|c| c.2 == "PauseManager").and_then(|c| sc.doc(c.0)) {
        let t = |k: &str| tree.comp(file_ref(pm.get(k)));
        screen.health = t("minimalistHealthText");
        screen.fps = t("minimalFpsText");
        screen.ping = t("minimalPingText");
        screen.ammo = [t("leftGunAmmo"), t("rightGunAmmo")];
        screen.dead = t("deadText");
        screen.enemy_health = t("enemyHealthText");
        screen.interact_prompt = [t("interactPromptText"), t("interactPromptTextGamepad")];
    }
    commands.insert_resource(screen);
    // PauseManager fields.
    let pm = sc.components(canvas_go).into_iter().find(|c| c.2 == "PauseManager").and_then(|c| sc.doc(c.0));
    if let Some(pm) = &pm {
        let go = |k: &str| tree.node(file_ref(pm.get(k)));
        ui.pause_menu = go("pauseMenu");
        ui.options_menu = go("optionsMenu");
        ui.first_interface = go("firstInterface");
        ui.second_interface = go("secondInterface");
        ui.first3d = tree.comp(file_ref(pm.get("firstInterface3D")));
        ui.resume3d = go("resumeButton3D");
        ui.seq_display = go("sequenceDisplayGameObject");
        ui.time = pm.get("time").and_then(Value::as_f64).unwrap_or(0.3) as f32;
        for k in ["genericMenuClip", "pressMenuClip", "releaseMenuClip", "closeMenuClip"] {
            if let Some(p) = clip_path(cx.assets, pm, k) {
                ui.clips.insert(k.to_string(), p);
            }
        }
    }
    // Scripts.
    for i in 0..tree.nodes.len() {
        for (id, name, m, enabled) in tree.nodes[i].scripts.clone() {
            if !enabled {
                continue;
            }
            match name.as_str() {
                "MoveUIObject" => {
                    ui.move_ui.insert(i, MoveUi { active_pos: tree.local_position(i), offset: Vec3::from_array(v3(m.get("offset"))), duration: f(m.get("duration")), ease: ease(int(m.get("ease"))), state: false });
                }
                "ModelButtonTween" => {
                    let mut dir = Vec3::from_array(v3(m.get("direction")));
                    if dir == Vec3::ZERO {
                        dir = Vec3::Z;
                    }
                    ui.model_btn.insert(i, ModelBtn { initial: tree.local_position(i), dir, hover: f(m.get("hoverAmount")), press: f(m.get("pressAmount")), duration: f(m.get("tweenDuration")) });
                }
                "ModelButtonHandler" => {
                    if let Some(b) = tree.comp(file_ref(m.get("button"))) {
                        ui.handler_btn.insert(i, b);
                    }
                }
                "ButtonSizeTween" => {
                    let text_only = int(m.get("scaleTextOnly")) != 0;
                    let target = if text_only {
                        tree.text_in_children(i, true).unwrap_or(i)
                    } else if int(m.get("parentScale")) != 0 {
                        tree.nodes[i].parent.unwrap_or(i)
                    } else {
                        i
                    };
                    let init_scale = if int(m.get("takeInitScale")) != 0 { tree.nodes[target].scale } else { Vec3::ONE };
                    ui.size_tween.insert(
                        i,
                        SizeTween {
                            target,
                            target_scale: Vec3::from_array(v3(m.get("targetScale"))),
                            duration: f(m.get("tweenDuration")),
                            ease: ease(int(m.get("easeType"))),
                            init_scale,
                            close_window: int(m.get("closeWindow")) != 0,
                            bigger_when_selected: int(m.get("biggerWhenSelected")) != 0,
                            gamepad_only: int(m.get("biggerWithGamepadOnly")) != 0,
                            selected_scale: Vec3::from_array(v3(m.get("selectedScale"))),
                            duration2: f(m.get("tweenDuration2")),
                            ease2: ease(int(m.get("easeType2"))),
                            is_selected: false,
                            has_button: matches!(tree.nodes[i].widget, Widget::Button { .. }),
                        },
                    );
                }
                "ReBindUI" => {
                    let c = |k: &str| tree.comp(file_ref(m.get(k)));
                    let rebind_text = c("rebindText");
                    let default_text = rebind_text.and_then(|t| tree.text(t).map(str::to_string)).unwrap_or_default();
                    ui.rebind_ui.insert(
                        i,
                        RebindUi {
                            action: m.get("actionName").and_then(Value::as_str).unwrap_or("").to_string(),
                            index: int(m.get("selectedBinding")) as usize,
                            action_text: c("actionText"),
                            rebind_btn: c("rebindButton"),
                            rebind_text,
                            reset_btn: c("resetButton"),
                            sequence: int(m.get("sequenceDisplay")) != 0,
                            default_text,
                        },
                    );
                }
                "DisableAfterTime" => {
                    // Start(): after `time` (0.14-0.15 s, while the options menu is still at scale 0 from
                    // PauseManager.Awake) -> localScale = 1, SetActive(false).
                    tree.nodes[i].scale = Vec3::ONE;
                    tree.set_active(i, false);
                }
                _ => {}
            }
            let _ = id;
        }
    }
    // MoveUIObject.Awake: start slid out.
    for (n, m) in ui.move_ui.clone() {
        tree.apply_prop(n, TweenProp::Pos, m.active_pos + m.offset);
    }
    ui.prev_on = vec![false; tree.nodes.len()];
    for i in 0..tree.nodes.len() {
        ui.prev_on[i] = effective_active(&tree, i);
    }
    // ReBindUI.OnEnable -> UpdateUI.
    for k in ui.rebind_ui.keys().copied().collect::<Vec<_>>() {
        update_rebind_ui(&mut tree, &ui, k, &settings.bindings);
    }
    // Debug: RESTRAFE_NAV_DUMP=1 lists the rebind fields and the selectables' navigation.
    if std::env::var_os("RESTRAFE_NAV_DUMP").is_some() {
        let path = |i: usize| tree.ancestors(i).iter().rev().map(|&a| tree.nodes[a].name.clone()).collect::<Vec<_>>().join("/");
        for (k, r) in &ui.rebind_ui {
            println!("NAVDUMP rebind {} action={} index={} default={:?}", path(*k), r.action, r.index, r.default_text);
        }
        for i in 0..tree.nodes.len() {
            if let Some(s) = &tree.nodes[i].sel {
                let nav: Vec<String> = s.nav.iter().map(|n| n.map(|n| tree.nodes[n].name.clone()).unwrap_or_default()).collect();
                println!("NAVDUMP sel {} mode={} wrap={} nav={nav:?} inter={} colors={:?}", path(i), s.nav_mode, s.nav_wrap, s.interactable, s.colors);
            }
        }
        if let Some(r) = ui.resume3d {
            println!("NAVDUMP resume3d {}", path(r));
        }
    }
    ui.ready = true;
    info!("pause menu: imported {} UI nodes in {:.2} s", tree.nodes.len(), t0.elapsed().as_secs_f32());
}

fn effective_active(tree: &UiTree, n: usize) -> bool {
    tree.ancestors(n).iter().all(|&a| tree.nodes[a].active)
}

/// OnEnable / OnDisable of the script components whose activeInHierarchy changed.
fn sync_enable(tree: &mut UiTree, ui: &mut PauseUi) {
    if ui.prev_on.len() < tree.nodes.len() {
        let n = tree.nodes.len();
        ui.prev_on.resize(n, false);
    }
    for i in 0..tree.nodes.len() {
        let on = effective_active(tree, i);
        if on == ui.prev_on[i] {
            continue;
        }
        ui.prev_on[i] = on;
        if on {
            // MoveUIObject.OnEnable
            if let Some(m) = ui.move_ui.get_mut(&i) {
                m.state = false;
                let p = m.active_pos + m.offset;
                tree.kill_tweens(i);
                tree.apply_prop(i, TweenProp::Pos, p);
            }
            // ButtonSizeTween.OnEnable
            if let Some(s) = ui.size_tween.get(&i) {
                let (t, sc) = (s.target, s.init_scale);
                tree.apply_prop(t, TweenProp::Scale, sc);
            }
        } else if let Some(b) = ui.model_btn.get(&i) {
            // ModelButtonTween.OnDisable
            if b.initial != Vec3::ZERO {
                tree.kill_tweens(i);
                tree.apply_prop(i, TweenProp::Pos, b.initial);
            }
        }
    }
}

fn choose_state(tree: &mut UiTree, ui: &mut PauseUi, n: usize, state: bool) {
    let Some(m) = ui.move_ui.get_mut(&n) else { return };
    if m.state == state {
        return;
    }
    m.state = state;
    let to = if state { m.active_pos } else { m.active_pos + m.offset };
    let (d, e) = (m.duration, m.ease);
    tree.tween(n, TweenProp::Pos, to, d, e);
}

fn change_state(tree: &mut UiTree, ui: &mut PauseUi, n: usize) {
    let s = ui.move_ui.get(&n).map(|m| !m.state).unwrap_or(false);
    if let Some(m) = ui.move_ui.get_mut(&n) {
        m.state = !s;
    }
    choose_state_force(tree, ui, n, s);
}

fn choose_state_force(tree: &mut UiTree, ui: &mut PauseUi, n: usize, state: bool) {
    let Some(m) = ui.move_ui.get_mut(&n) else { return };
    m.state = state;
    let to = if state { m.active_pos } else { m.active_pos + m.offset };
    let (d, e) = (m.duration, m.ease);
    tree.tween(n, TweenProp::Pos, to, d, e);
}

// ------------------------------------------------------------------------------------------------------------------
// Bindings (InputManager / PlayerControls)

fn action_control_type(action: &str) -> &'static str {
    match action {
        "Move" | "ChangeWeapon" => "Vector2",
        "Run" => "Axis",
        _ => "Button",
    }
}

/// Our binding list for (action, binding index); `None` when the action has no rebindable equivalent here.
fn binding_slot<'a>(b: &'a mut Bindings, action: &str, index: usize, part: usize) -> Option<&'a mut Vec<Bind>> {
    Some(match (action, index) {
        ("Jump", 0) => &mut b.jump,
        ("Move", 0) => match part {
            0 => &mut b.forward,
            1 => &mut b.back,
            2 => &mut b.left,
            _ => &mut b.right,
        },
        ("Run", 0) => &mut b.run,
        ("Interact", 0) => &mut b.interact,
        ("LeanRight", 0) => &mut b.lean_right,
        ("LeanLeft", 0) => &mut b.lean_left,
        ("Reload", 0) => &mut b.reload,
        ("Zoom", 0) => &mut b.zoom,
        ("FireHold", 0) => &mut b.fire,
        ("RightClick", 0) => &mut b.fire2,
        ("Crouch", 0) | ("Crouch", 2) => &mut b.crouch,
        // Gamepad binding indices (PlayerControls JSON).
        ("Move", 5) => match part {
            0 => &mut b.pad_forward,
            1 => &mut b.pad_back,
            2 => &mut b.pad_left,
            _ => &mut b.pad_right,
        },
        ("Jump", 1) => &mut b.pad_jump,
        ("Run", 1) => &mut b.pad_run,
        ("Crouch", 1) => &mut b.pad_crouch,
        ("Interact", 1) => &mut b.pad_interact,
        ("Reload", 1) => &mut b.pad_reload,
        ("LeanRight", 1) => &mut b.pad_lean_right,
        ("LeanLeft", 1) => &mut b.pad_lean_left,
        ("Zoom", 1) => &mut b.pad_zoom,
        ("Drop", 0) => &mut b.pad_drop,
        ("FireHold", 4) => &mut b.pad_fire,
        ("RightClick", 4) => &mut b.pad_fire2,
        ("MoveUp", 1) => &mut b.pad_move_up,
        _ => return None,
    })
}

/// Crouch: binding 0 = <Keyboard>/ctrl (our ControlLeft + ControlRight), binding 2 = <Keyboard>/c. Stored as
/// [binding 2, binding 0 keys...].
fn crouch_part(v: &[Bind], index: usize) -> Vec<Bind> {
    if index == 2 { v.first().copied().into_iter().collect() } else { v.iter().skip(1).copied().collect() }
}

fn set_binding(b: &mut Bindings, action: &str, index: usize, part: usize, bind: Option<Bind>) {
    let def = Bindings::default();
    if action == "Crouch" && index != 1 {
        let mut cur = b.crouch.clone();
        let (c, ctrl) = (cur.first().copied().unwrap_or(def.crouch[0]), cur.iter().skip(1).copied().collect::<Vec<_>>());
        cur = match (index, bind) {
            (2, Some(k)) => [vec![k], ctrl].concat(),
            (2, None) => [vec![def.crouch[0]], ctrl].concat(),
            (_, Some(k)) => vec![c, k],
            (_, None) => [vec![c], def.crouch[1..].to_vec()].concat(),
        };
        b.crouch = cur;
        return;
    }
    let mut d = def.clone();
    let default = binding_slot(&mut d, action, index, part).cloned();
    if let Some(slot) = binding_slot(b, action, index, part) {
        *slot = match bind {
            Some(k) => vec![k],
            None => default.unwrap_or_default(),
        };
    }
}

/// Input System display name (short names for mouse buttons, Unity key display names).
fn bind_name(b: &Bind) -> String {
    match b {
        Bind::Mouse(MouseButton::Left) => "LMB".into(),
        Bind::Mouse(MouseButton::Right) => "RMB".into(),
        Bind::Mouse(MouseButton::Middle) => "MMB".into(),
        Bind::Mouse(MouseButton::Back) => "Back".into(),
        Bind::Mouse(MouseButton::Forward) => "Forward".into(),
        Bind::Mouse(m) => format!("{m:?}"),
        Bind::Pad(c) => pad_name(*c).into(),
        Bind::Key(k) => {
            let s = format!("{k:?}");
            match k {
                KeyCode::Space => "Space".into(),
                KeyCode::ShiftLeft => "Left Shift".into(),
                KeyCode::ShiftRight => "Right Shift".into(),
                KeyCode::ControlLeft => "Left Control".into(),
                KeyCode::ControlRight => "Right Control".into(),
                KeyCode::AltLeft => "Left Alt".into(),
                KeyCode::AltRight => "Right Alt".into(),
                KeyCode::Tab => "Tab".into(),
                KeyCode::Enter => "Enter".into(),
                KeyCode::Backspace => "Backspace".into(),
                KeyCode::CapsLock => "Caps Lock".into(),
                KeyCode::ArrowUp => "Up Arrow".into(),
                KeyCode::ArrowDown => "Down Arrow".into(),
                KeyCode::ArrowLeft => "Left Arrow".into(),
                KeyCode::ArrowRight => "Right Arrow".into(),
                _ => s.trim_start_matches("Key").trim_start_matches("Digit").to_string(),
            }
        }
    }
}

/// GetBindingDisplayString of a Gamepad control (short display names; the scene's gamepad column shows "A", "B",
/// "X", "Y", "LB", "RB", "LT", "RT", "Left Stick Press", "D-Pad/Up" for the defaults).
fn pad_name(c: crate::input::PadCtl) -> &'static str {
    use crate::input::PadCtl::*;
    match c {
        ButtonSouth => "A",
        ButtonEast => "B",
        ButtonWest => "X",
        ButtonNorth => "Y",
        LeftShoulder => "LB",
        RightShoulder => "RB",
        LeftTrigger => "LT",
        RightTrigger => "RT",
        LeftStickPress => "Left Stick Press",
        RightStickPress => "Right Stick Press",
        Start => "Start",
        Select => "Select",
        DpadUp => "D-Pad/Up",
        DpadDown => "D-Pad/Down",
        DpadLeft => "D-Pad/Left",
        DpadRight => "D-Pad/Right",
        LeftStickUp => "LS/Up",
        LeftStickDown => "LS/Down",
        LeftStickLeft => "LS/Left",
        LeftStickRight => "LS/Right",
        RightStickUp => "RS/Up",
        RightStickDown => "RS/Down",
        RightStickLeft => "RS/Left",
        RightStickRight => "RS/Right",
    }
}

fn binding_text(b: &Bindings, r: &RebindUi) -> String {
    let def = Bindings::default();
    let mut bb = b.clone();
    let mut dd = def.clone();
    if r.action == "Crouch" && r.index != 1 {
        let (cur, dfl) = (crouch_part(&b.crouch, r.index), crouch_part(&def.crouch, r.index));
        if cur == dfl {
            return r.default_text.clone();
        }
        return cur.first().map(bind_name).unwrap_or_default();
    }
    if r.action == "Move" {
        let parts: Vec<Vec<Bind>> = (0..4).map(|p| binding_slot(&mut bb, "Move", r.index, p).cloned().unwrap_or_default()).collect();
        let dparts: Vec<Vec<Bind>> = (0..4).map(|p| binding_slot(&mut dd, "Move", r.index, p).cloned().unwrap_or_default()).collect();
        if parts == dparts {
            return r.default_text.clone();
        }
        return parts.iter().map(|p| p.first().map(bind_name).unwrap_or_default()).collect::<Vec<_>>().join("/");
    }
    match (binding_slot(&mut bb, &r.action, r.index, 0), binding_slot(&mut dd, &r.action, r.index, 0)) {
        (Some(cur), Some(dfl)) if cur != dfl => cur.first().map(bind_name).unwrap_or_default(),
        _ => r.default_text.clone(),
    }
}

fn update_rebind_ui(tree: &mut UiTree, ui: &PauseUi, n: usize, b: &Bindings) {
    let Some(r) = ui.rebind_ui.get(&n).cloned() else { return };
    if let Some(s) = ui.seq_display {
        tree.set_active(s, false);
    }
    if let Some(t) = r.action_text {
        tree.set_text(t, &r.action);
    }
    if let Some(t) = r.rebind_text {
        let s = binding_text(b, &r);
        tree.set_text(t, &s);
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Settings bindings

/// `ChangeTextValue` formatting (Mathf.Round is round-half-even; `x + ".0"` for integers, else ToString("F2")).
/// Mono evaluates `Mathf.Round(v * factor2) * factor % 2f` with double intermediates: 200 * 0.01f is 1.99999996, not
/// 2, so whole values with factor 0.01 print "2.00" (the original shows 2.00 for the sensitivities).
fn change_text_value(tree: &mut UiTree, holder: usize, v: f32) {
    let Some(m) = tree.script(holder, "ChangeTextValue").cloned() else { return };
    let factor = m.get("factor").and_then(Value::as_f64).unwrap_or(0.01) as f32;
    let factor2 = m.get("factor2").and_then(Value::as_f64).unwrap_or(100.0) as f32;
    let r = (v * factor2).round_ties_even() as f64 * factor as f64;
    let s = if r % 2.0 == 0.0 || r % 2.0 == 1.0 { format!("{}.0", r as f32) } else { format!("{:.2}", r as f32) };
    if let Some(t) = tree.text_in_children(holder, true) {
        tree.set_text(t, &s);
    }
}

/// DisplayLoadValues: the flag set on the component selects the Settings value shown by its widget.
fn display_load_values(tree: &mut UiTree, s: &Settings) {
    for i in 0..tree.nodes.len() {
        let Some(m) = tree.script(i, "DisplayLoadValues").cloned() else { continue };
        let flag = |k: &str| int(m.get(k)) != 0;
        let toggles: [(&str, bool); 18] = [
            ("toggleTwoAxis", s.toggle_two_axis),
            ("invertMouseX", s.invert_mouse_x),
            ("invertMouseY", s.invert_mouse_y),
            ("sprintToggle", s.sprint_toggle),
            ("aimToggle", s.aim_toggle),
            ("leanToggle", s.lean_toggle),
            ("crouchToggle", s.crouch_toggle),
            ("reverseSprintBind", s.reverse_sprint_bind),
            ("enableVoiceChat", s.enable_voice_chat),
            ("isFullscreen", s.is_fullscreen),
            ("minimalistUi", s.minimalist_ui),
            ("motionBlur", s.motion_blur),
            ("inverseFireBinding", s.inverse_fire_binding),
            ("inGameMusic", s.in_game_music),
            ("enableFixedCrosshair", s.enable_fixed_crosshair),
            ("showSpeedometer", s.show_speedometer),
            ("disableCrosshair", s.disable_crosshair),
            ("exclusiveFullscreen", s.exclusive_fullscreen),
        ];
        for (k, v) in toggles {
            if flag(k) {
                tree.toggle_set(i, v, true, false);
            }
        }
        if flag("targetFpsToggle") {
            tree.toggle_set(i, s.target_fps_toggle, true, false);
        }
        if flag("useVsync") {
            tree.toggle_set(i, s.use_vsync == 1, true, false);
        }
        if flag("reduceVFX") {
            tree.toggle_set(i, s.reduce_vfx, true, false);
        }
        let sliders: [(&str, f32); 13] = [
            ("mouseSensitivity", s.mouse_sensitivity),
            ("mouseAimScopeSensitivity", s.mouse_aim_scope_sensitivity),
            ("mouseAimSensitivity", s.mouse_aim_sensitivity),
            ("mouseXSensitivity", s.horizontal_sensitivity),
            ("mouseYSensitivity", s.vertical_sensitivity),
            ("fovValue", s.normal_fov_value),
            ("brightness", s.brightness),
            ("damageIntensity", s.damage_intensity),
            ("voiceChatVolume", s.voice_chat_volume),
            ("effectsVolume", s.effects_volume),
            ("musicVolume", s.music_volume),
            ("masterVolume", s.master_volume),
            ("menuMusicVolume", s.menu_music_volume),
        ];
        for (k, v) in sliders {
            if flag(k) {
                tree.slider_set(i, v, true);
            }
        }
        if flag("ambientVolume") {
            tree.slider_set(i, s.ambient_volume, true);
        }
        if flag("voiceChatMode") {
            tree.dropdown_set(i, s.voice_chat_mode, true);
        }
        if flag("graphics") {
            tree.dropdown_set(i, s.quality_setting, true);
        }
        if flag("targetFps") {
            let v = s.target_fps.max(30);
            tree.input_set(i, &v.to_string(), true);
        }
    }
}

fn arg_node(tree: &UiTree, c: &crate::ugui_scene::PCall) -> Option<usize> {
    tree.comp(c.object_arg)
}

#[allow(clippy::too_many_arguments)]
/// Set by the pause menu's "Leave match" button; [`leave_match`] quits the app.
static LEAVE_MATCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn leave_match(mut exit: MessageWriter<AppExit>) {
    if LEAVE_MATCH.swap(false, std::sync::atomic::Ordering::Relaxed) {
        exit.write(AppExit::Success);
    }
}

fn handle_call(tree: &mut UiTree, ui: &mut PauseUi, s: &mut Settings, call: &UiCall, rend: &mut UguiRender, windows: &mut Query<&mut Window, With<PrimaryWindow>>) -> bool {
    let c = &call.call;
    let slider = |tree: &UiTree| arg_node(tree, c).and_then(|n| tree.slider_value(n)).or(match call.value {
        CallValue::Float(v) => Some(v),
        _ => None,
    });
    let toggle = |tree: &UiTree| arg_node(tree, c).and_then(|n| tree.toggle_value(n)).or(match call.value {
        CallValue::Bool(v) => Some(v),
        _ => None,
    });
    let mut changed = true;
    match (c.type_name.as_str(), c.method.as_str()) {
        // "Leave match" (MenuButton -> SteamLobby.LeaveMatch): no main menu in the port, so it quits the game.
        ("SteamLobby", "LeaveMatch") => {
            LEAVE_MATCH.store(true, std::sync::atomic::Ordering::Relaxed);
            changed = false;
        }
        ("MoveUIObject", "ChooseState") => {
            if let Some(n) = tree.comp(c.target) {
                let st = match (&call.value, c.mode) {
                    (CallValue::Bool(b), 0) => *b,
                    _ => c.bool_arg,
                };
                choose_state(tree, ui, n, st);
            }
            changed = false;
        }
        ("MoveUIObject", "ChangeState") => {
            if let Some(n) = tree.comp(c.target) {
                change_state(tree, ui, n);
            }
            changed = false;
        }
        ("PauseManager", "SetActiveAfterSecond") => {
            if let Some(n) = tree.node(c.object_arg) {
                ui.timers.push((ui.time, n));
            }
            changed = false;
        }
        ("PauseManager", "SetActiveOpposite") => {
            if let Some(n) = tree.node(c.object_arg) {
                let a = tree.nodes[n].active;
                tree.set_active(n, !a);
            }
            changed = false;
        }
        // PauseManager.ChangeSelectedItem(obj): with a gamepad, EventSystem first selected = selected = obj.
        ("PauseManager", "ChangeSelectedItem") => {
            if ui.gamepad {
                if let Some(n) = tree.node(c.object_arg) {
                    tree.set_selected(Some(n));
                }
            }
            changed = false;
        }
        ("PauseManager", "PlaySoundWithGamepad") => {
            if ui.gamepad {
                ui.sounds.push("genericMenuClip");
            }
            changed = false;
        }
        ("PauseManager", _) => changed = false,
        ("ChangeTextValue", "ChangeIntValue") => {
            if let (Some(h), Some(v)) = (tree.comp(c.target), slider(tree)) {
                change_text_value(tree, h, v);
            }
            changed = false;
        }
        ("Settings", m) => match m {
            "ChangeMouseSensitivity" => s.mouse_sensitivity = slider(tree).unwrap_or(s.mouse_sensitivity),
            "ChangeAimScopeMouseSensitivity" => s.mouse_aim_scope_sensitivity = slider(tree).unwrap_or(s.mouse_aim_scope_sensitivity),
            "ChangeAimMouseSensitivity" => s.mouse_aim_sensitivity = slider(tree).unwrap_or(s.mouse_aim_sensitivity),
            "ChangeMouseXSensitivity" => s.horizontal_sensitivity = slider(tree).unwrap_or(s.horizontal_sensitivity),
            "ChangeMouseYSensitivity" => s.vertical_sensitivity = slider(tree).unwrap_or(s.vertical_sensitivity),
            "ChangeFov" => s.normal_fov_value = slider(tree).unwrap_or(s.normal_fov_value),
            "ChangeBrightness" => s.brightness = slider(tree).unwrap_or(s.brightness),
            "ChangeDamageIntensity" => s.damage_intensity = slider(tree).unwrap_or(s.damage_intensity),
            "ChangeVoiceChatVolume" => s.voice_chat_volume = slider(tree).unwrap_or(s.voice_chat_volume),
            "ToggleTwoAxisMouse" => s.toggle_two_axis = toggle(tree).unwrap_or(s.toggle_two_axis),
            "ToggleVerticalInvert" => s.invert_mouse_y = toggle(tree).unwrap_or(s.invert_mouse_y),
            "ToggleHorizontalInvert" => s.invert_mouse_x = toggle(tree).unwrap_or(s.invert_mouse_x),
            "ToggleSprint" => s.sprint_toggle = toggle(tree).unwrap_or(s.sprint_toggle),
            "ToggleAim" => s.aim_toggle = toggle(tree).unwrap_or(s.aim_toggle),
            "ToggleLean" => s.lean_toggle = toggle(tree).unwrap_or(s.lean_toggle),
            "ToggleCrouch" => s.crouch_toggle = toggle(tree).unwrap_or(s.crouch_toggle),
            "ToggleReverseSprintBind" => s.reverse_sprint_bind = toggle(tree).unwrap_or(s.reverse_sprint_bind),
            "ToggleMinimalistUi" => s.minimalist_ui = toggle(tree).unwrap_or(s.minimalist_ui),
            "ToggleMotionBlur" => s.motion_blur = toggle(tree).unwrap_or(s.motion_blur),
            "InverseFireBinding" => s.inverse_fire_binding = toggle(tree).unwrap_or(s.inverse_fire_binding),
            "ChangeFullscreen" => s.is_fullscreen = toggle(tree).unwrap_or(s.is_fullscreen),
            "ChangeVSync" => s.use_vsync = toggle(tree).map(|b| b as i32).unwrap_or(s.use_vsync),
            "ChangeVoiceChat" => s.enable_voice_chat = toggle(tree).unwrap_or(s.enable_voice_chat),
            "ChangePushToTalk" => {
                if let CallValue::Int(v) = call.value {
                    s.voice_chat_mode = v;
                }
            }
            "EnableFixedCrosshair" => s.enable_fixed_crosshair = toggle(tree).unwrap_or(s.enable_fixed_crosshair),
            "EnableSpeedometer" => s.show_speedometer = toggle(tree).unwrap_or(s.show_speedometer),
            "DisableCrosshair" => s.disable_crosshair = toggle(tree).unwrap_or(s.disable_crosshair),
            "InGameMusic" => s.in_game_music = toggle(tree).unwrap_or(s.in_game_music),
            "ChangeExclusiveFullscreen" => s.exclusive_fullscreen = toggle(tree).unwrap_or(s.exclusive_fullscreen),
            "ReduceVFX" => s.reduce_vfx = toggle(tree).unwrap_or(s.reduce_vfx),
            "ChangeTargetFpsToggle" => {
                s.target_fps_toggle = toggle(tree).unwrap_or(s.target_fps_toggle);
                s.target_frame_rate = if s.target_fps_toggle { s.target_fps } else { -1 };
            }
            "ChangeFrameRate" => {
                // int.Parse (fallback 120), min 30 (SetTextWithoutNotify).
                if let Some(n) = arg_node(tree, c) {
                    let txt = tree.input_text(n).unwrap_or("").to_string();
                    let mut v = match txt.trim().parse::<i32>() {
                        Ok(v) => v,
                        Err(_) => {
                            tree.input_set(n, "120", true);
                            120
                        }
                    };
                    if v < 30 {
                        v = 30;
                        tree.input_set(n, "30", false);
                    }
                    s.target_fps = v;
                    s.target_frame_rate = if s.target_fps_toggle { v } else { -1 };
                }
            }
            "SetQualityLevelDropdown" => {
                // qualityDropdown = the Graphics Settings dropdown (the event's sender).
                if let Some(v) = tree.dropdown_value(call.node) {
                    s.quality_setting = v;
                }
            }
            _ => {
                debug!("settings menu: Settings.{m} not ported");
                changed = false;
            }
        },
        ("SoundManager", m) => match m {
            "ChangeMasterVolumeSlider" => s.master_volume = slider(tree).unwrap_or(s.master_volume),
            "ChangeEffectVolumeSlider" => s.effects_volume = slider(tree).unwrap_or(s.effects_volume),
            "ChangeAmbientVolumeSlider" => s.ambient_volume = slider(tree).unwrap_or(s.ambient_volume),
            "ChangeMusicVolumeSlider" => s.music_volume = slider(tree).unwrap_or(s.music_volume),
            "ChangeMenuMusicVolumeSlider" => s.menu_music_volume = slider(tree).unwrap_or(s.menu_music_volume),
            _ => changed = false,
        },
        ("ResetAllBindings", "ResetAll") => {
            if let Some(n) = tree.comp(c.target) {
                let mut stack = vec![n];
                while let Some(k) = stack.pop() {
                    stack.extend(tree.nodes[k].children.clone());
                    if let Some(r) = ui.rebind_ui.get(&k).cloned() {
                        let parts = if r.action == "Move" { 4 } else { 1 };
                        for p in 0..parts {
                            set_binding(&mut s.bindings, &r.action, r.index, p, None);
                        }
                        update_rebind_ui(tree, ui, k, &s.bindings);
                    }
                }
            }
        }
        ("SettingsResolution", "SetResolutionReal") => {
            if let Some(n) = arg_node(tree, c).or(Some(call.node)) {
                let idx = tree.dropdown_value(n).unwrap_or(0);
                set_resolution(ui, s, idx, windows);
            }
        }
        ("ResetScreenResolution", "ResetResolution") => {
            if let Some(dd) = tree.comp(c.target).and_then(|n| tree.script(n, "ResetScreenResolution").cloned()).and_then(|m| tree.comp(file_ref(m.get("resolutionDropdown")))) {
                tree.dropdown_set(dd, 0, false);
                set_resolution(ui, s, 0, windows);
            }
        }
        (t, m) => {
            // UI-only values (outline colours / widths, crosshair images, music player, lobby).
            match call.value {
                CallValue::Float(v) => {
                    ui.extra.insert(format!("{t}.{m}"), v);
                }
                CallValue::Str(ref v) => {
                    if let Ok(x) = v.parse::<f32>() {
                        ui.extra.insert(format!("{t}.{m}"), x);
                    }
                }
                _ => {}
            }
            debug!("settings menu: {t}.{m} not ported (UI only)");
            changed = false;
        }
    }
    let _ = rend;
    changed
}

/// SettingsResolution.SetResolution (Screen.SetResolution) + Settings.ChangeResolution.
fn set_resolution(ui: &PauseUi, s: &mut Settings, idx: i32, windows: &mut Query<&mut Window, With<PrimaryWindow>>) {
    if idx < 0 || idx as usize >= ui.resolutions.len() {
        warn!("Invalid resolution index: {idx}");
        return;
    }
    let r = ui.resolutions[idx as usize];
    s.resolution = idx;
    if std::env::var_os("RESTRAFE_AUTOTEST").is_some() {
        return;
    }
    if let Ok(mut w) = windows.single_mut() {
        w.resolution.set_physical_resolution(r.x, r.y);
    }
}

fn play(commands: &mut Commands, assets: &AssetServer, settings: &Settings, ui: &PauseUi, key: &str) {
    if let Some(p) = ui.clips.get(key) {
        crate::audio::play(commands, assets, settings, p, 1.0);
    }
}

/// PauseManager.Menu (Escape).
fn menu(tree: &mut UiTree, ui: &mut PauseUi) {
    if let Some(o) = ui.options_menu {
        if tree.nodes[o].active {
            tree.set_active(o, false);
        }
    }
    let Some(p) = ui.pause_menu else { return };
    let a = tree.nodes[p].active;
    tree.set_active(p, !a);
    sync_enable(tree, ui);
    if let Some(f3) = ui.first3d {
        choose_state(tree, ui, f3, true);
    }
    if let Some(fi) = ui.first_interface {
        tree.set_active(fi, true);
    }
    if let Some(si) = ui.second_interface {
        tree.set_active(si, false);
    }
    sync_enable(tree, ui);
    // if (pauseMenu.activeSelf) ChangeSelectedItem(resumeButton3D) (gamepad only).
    if !a && ui.gamepad {
        if let Some(r) = ui.resume3d {
            tree.set_selected(Some(r));
        }
    }
}

/// Player HUD light: PlayerIK .../Camera/UICam/Transform/Directional Light (layer HUD only, colour (0,1,0.294),
/// intensity 0.58). In a match it is the light of the options menu's 3D HUD (also on the HUD layer).
#[derive(Component)]
struct HudLayerLight(Quat);

fn spawn_hud_light(mut commands: Commands) {
    let path = "GameObject/PlayerIK.prefab".to_string();
    let Some(sc) = UScene::load(&path) else { return };
    let Some(&light) = sc.find_by_name("Directional Light").first() else { return };
    // Rotation of the light relative to the player Camera (Unity space).
    let mut rot = Quat::IDENTITY;
    let mut g = light;
    while sc.go_name(g) != "Camera" {
        let Some(t) = sc.transform_of(g) else { break };
        let q = crate::ugui_scene::v4(sc.doc(t).and_then(|m| m.get("m_LocalRotation").cloned()).as_ref());
        rot = Quat::from_xyzw(q[0], q[1], q[2], q[3]) * rot;
        match sc.parent(g) {
            Some(p) => g = p,
            None => break,
        }
    }
    let l = sc.components(light).into_iter().find(|c| c.1 == 108).and_then(|c| sc.doc(c.0));
    let (color, intensity) = l.map(|m| (crate::ugui_scene::rgba(m.get("m_Color")), f(m.get("m_Intensity")))).unwrap_or(([0.0, 1.0, 0.294, 1.0], 0.58));
    let local = crate::ugui_assets::unity_quat(rot.to_array());
    commands.spawn((
        Name::new("HudLayerLight"),
        HudLayerLight(local),
        DirectionalLight { color: Color::srgb(color[0], color[1], color[2]), illuminance: intensity * std::f32::consts::PI * 1.2, shadow_maps_enabled: false, ..default() },
        Transform::default(),
        RenderLayers::layer(25),
    ));
}

fn update_hud_light(cam: Query<&GlobalTransform, With<crate::player::fpc::PlayerCamera>>, mut l: Query<(&HudLayerLight, &mut Transform)>, global: Res<GlobalAmbientLight>, mut amb: ResMut<UguiAmbient>) {
    if let Ok(c) = cam.single() {
        for (h, mut t) in &mut l {
            t.rotation = c.rotation() * h.0;
        }
    }
    // Scene ambient in Unity units (the world camera exposure, ppv2::EV100) -> menu cameras (ev100 0).
    let unity = global.brightness / (1.2 * 2f32.powf(crate::ppv2::EV100));
    amb.color = global.color;
    amb.brightness = unity * 1.2;
}

#[allow(clippy::too_many_arguments)]
pub fn menu_update(
    mut commands: Commands,
    (keys, mouse, time, assets): (Res<ButtonInput<KeyCode>>, Res<ButtonInput<MouseButton>>, Res<Time>, Res<AssetServer>),
    mut open: ResMut<MenuOpen>,
    mut tree: ResMut<UiTree>,
    mut ui: ResMut<PauseUi>,
    mut rend: ResMut<UguiRender>,
    mut settings: ResMut<Settings>,
    (mut calls, mut ptrs): (MessageReader<UiCall>, MessageReader<UiPointer>),
    (mut cursor, mut windows): (Query<&mut CursorOptions, With<PrimaryWindow>>, Query<&mut Window, With<PrimaryWindow>>),
    mut input: ResMut<PlayerInput>,
    monitors: Query<&bevy::window::Monitor>,
    sim: Res<crate::ugui::SimPointer>,
    cos_open: Res<crate::cosmetics::CosmeticsMenuOpen>,
    weapon_menu: Res<crate::weapon_menu::WeaponMenu>,
) {
    if !ui.ready {
        return;
    }
    let tree = &mut *tree;
    let ui = &mut *ui;
    // Display values once (DisplayLoadValues.Start, 0.1 s after the options menu first ran).
    if !ui.loaded_values {
        ui.loaded_values = true;
        // SettingsResolution.Start: Screen.resolutions sorted by width, height, refresh rate (descending).
        let mut res: Vec<UVec3> = vec![];
        for m in &monitors {
            for v in &m.video_modes {
                let r = UVec3::new(v.physical_size.x, v.physical_size.y, (v.refresh_rate_millihertz as f32 / 1000.0).round() as u32);
                if !res.contains(&r) {
                    res.push(r);
                }
            }
        }
        res.sort_by(|a, b| b.x.cmp(&a.x).then(b.y.cmp(&a.y)).then(b.z.cmp(&a.z)));
        ui.resolutions = res.clone();
        let gcd = |mut a: u32, mut b: u32| {
            while b != 0 {
                let t = b;
                b = a % b;
                a = t;
            }
            a.max(1)
        };
        let opts: Vec<String> = res.iter().map(|r| format!("{}x{} ({}:{}) {} Hz", r.x, r.y, r.x / gcd(r.x, r.y), r.y / gcd(r.x, r.y), r.z)).collect();
        for i in 0..tree.nodes.len() {
            if tree.script(i, "SettingsResolution").is_some() {
                tree.dropdown_set_options(i, opts.clone());
                tree.dropdown_set(i, settings.resolution, false);
            }
        }
        let s = settings.clone();
        display_load_values(tree, &s);
        // ChangeTextValue.Start
        for i in 0..tree.nodes.len() {
            if tree.script(i, "ChangeTextValue").is_some() {
                if let Some(sl) = tree.widget_in_children(i, &|w| matches!(w, Widget::Slider { .. })) {
                    let v = tree.slider_value(sl).unwrap_or(0.0);
                    change_text_value(tree, i, v);
                }
            }
        }
    }
    // Escape: PauseManager.Menu (also cancels a running rebind, InputManager WithCancelingThrough escape).
    let sim_escape = sim.active && sim.wheel == -999.0;
    // Escape closes the customization menu instead (cosmetics.rs, runs after this system).
    // Menu action: <Keyboard>/escape, <Gamepad>/start.
    ui.gamepad = input.gamepad;
    // (A running rebind takes the gamepad's Start as the new binding instead.)
    let pad_menu = input.pad_menu && ui.rebinding.is_none();
    if (keys.just_pressed(KeyCode::Escape) || sim_escape || pad_menu) && !cos_open.0 && !weapon_menu.open && !weapon_menu.esc_consumed {
        if let Some(r) = ui.rebinding.take() {
            update_rebind_ui(tree, ui, r.ui, &settings.bindings);
        }
        menu(tree, ui);
    }
    // SetActiveAfterSecond timers.
    let dt = time.delta_secs();
    let mut fire = vec![];
    ui.timers.retain_mut(|(t, n)| {
        *t -= dt;
        if *t <= 0.0 {
            fire.push(*n);
            false
        } else {
            true
        }
    });
    for n in fire {
        tree.set_active(n, true);
    }
    // Rebinding: next key / mouse button.
    if let Some(r) = ui.rebinding.as_ref() {
        let elapsed = time.elapsed_secs() - r.started;
        // PerformInteractiveRebinding takes any device's ButtonControl (keyboard, mouse or gamepad, in either column).
        let pressed = keys
            .get_just_pressed()
            .find(|k| **k != KeyCode::Escape)
            .map(|k| Bind::Key(*k))
            .or_else(|| if elapsed > 0.05 { mouse.get_just_pressed().next().map(|m| Bind::Mouse(*m)) } else { None })
            .or_else(|| if elapsed > 0.0 { input.pad_pressed.map(Bind::Pad) } else { None });
        if let Some(b) = pressed {
            let r = ui.rebinding.take().unwrap();
            let rb = ui.rebind_ui[&r.ui].clone();
            if let Bind::Pad(c) = b {
                info!("rebind {}[{}] part {} -> {}", rb.action, rb.index, r.part, c.path());
            }
            set_binding(&mut settings.bindings, &rb.action, rb.index, r.part, Some(b));
            if rb.action == "Move" && r.part < 3 {
                if rb.sequence {
                    if let Some(s) = ui.seq_display {
                        tree.set_active(s, true);
                    }
                }
                ui.rebinding = Some(Rebinding { ui: r.ui, part: r.part + 1, started: time.elapsed_secs() });
            } else {
                update_rebind_ui(tree, ui, r.ui, &settings.bindings);
            }
            settings.save();
        }
    }
    // Pointer events -> scripts.
    let focused = windows.single().map(|w| w.focused).unwrap_or(true) || sim.active;
    let evs: Vec<UiPointer> = ptrs.read().copied().collect();
    for e in evs {
        let n = e.node;
        if let Some(st) = ui.size_tween.get(&n).cloned() {
            // `PauseManager.Instance.gamepad || !biggerWithGamepadOnly`
            let scale_ok = ui.gamepad || !st.gamepad_only;
            match e.kind {
                PointerKind::Enter if focused => {
                    play(&mut commands, &assets, &settings, ui, "genericMenuClip");
                    tree.kill_tweens(st.target);
                    if scale_ok {
                        tree.tween(st.target, TweenProp::Scale, st.target_scale, st.duration, st.ease);
                    }
                }
                PointerKind::Exit if focused && !st.is_selected => {
                    tree.kill_tweens(st.target);
                    if scale_ok {
                        tree.tween(st.target, TweenProp::Scale, st.init_scale, st.duration, st.ease);
                    }
                }
                PointerKind::Down => play(&mut commands, &assets, &settings, ui, "pressMenuClip"),
                PointerKind::Click if st.has_button => play(&mut commands, &assets, &settings, ui, if st.close_window { "closeMenuClip" } else { "releaseMenuClip" }),
                PointerKind::Select => {
                    if st.bigger_when_selected {
                        ui.size_tween.get_mut(&n).unwrap().is_selected = true;
                        if scale_ok {
                            tree.tween(st.target, TweenProp::Scale, st.selected_scale, st.duration2, st.ease2);
                        }
                    }
                    // Gamepad: hover sound + DOKill + DOScale(targetScale).
                    if ui.gamepad {
                        play(&mut commands, &assets, &settings, ui, "genericMenuClip");
                        tree.kill_tweens(st.target);
                        tree.tween(st.target, TweenProp::Scale, st.target_scale, st.duration, st.ease);
                    }
                }
                PointerKind::Deselect => {
                    let mut is_sel = st.is_selected;
                    if st.bigger_when_selected {
                        ui.size_tween.get_mut(&n).unwrap().is_selected = false;
                        is_sel = false;
                        if scale_ok {
                            tree.tween(st.target, TweenProp::Scale, st.init_scale, st.duration2, st.ease2);
                        }
                    }
                    if ui.gamepad && !is_sel {
                        tree.kill_tweens(st.target);
                        tree.tween(st.target, TweenProp::Scale, st.init_scale, st.duration, st.ease);
                    }
                }
                _ => {}
            }
        }
        if let Some(&b) = ui.handler_btn.get(&n) {
            if let Some(mb) = ui.model_btn.get(&b).cloned() {
                let to = match e.kind {
                    PointerKind::Enter | PointerKind::Up => Some(mb.initial - mb.dir * mb.hover),
                    PointerKind::Exit => Some(mb.initial),
                    // OnSelect / OnDeselect: Hover / Leave with a gamepad.
                    PointerKind::Select if ui.gamepad => Some(mb.initial - mb.dir * mb.hover),
                    PointerKind::Deselect if ui.gamepad => Some(mb.initial),
                    PointerKind::Down => Some(mb.initial - mb.dir * mb.press),
                    _ => None,
                };
                if let Some(to) = to {
                    tree.tween(b, TweenProp::Pos, to, mb.duration, Ease::OutQuad);
                }
            }
        }
        if e.kind == PointerKind::Click {
            // ReBindUI listeners on its buttons.
            let hit = ui.rebind_ui.iter().find_map(|(k, r)| if r.rebind_btn == Some(n) { Some((*k, true)) } else if r.reset_btn == Some(n) { Some((*k, false)) } else { None });
            if let Some((k, rebind)) = hit {
                let r = ui.rebind_ui[&k].clone();
                if rebind {
                    if r.sequence {
                        if let Some(s) = ui.seq_display {
                            tree.set_active(s, true);
                        }
                    }
                    if let Some(t) = r.rebind_text {
                        tree.set_text(t, &format!("Press a {}", action_control_type(&r.action)));
                    }
                    ui.rebinding = Some(Rebinding { ui: k, part: 0, started: time.elapsed_secs() });
                } else {
                    let parts = if r.action == "Move" { 4 } else { 1 };
                    for p in 0..parts {
                        set_binding(&mut settings.bindings, &r.action, r.index, p, None);
                    }
                    update_rebind_ui(tree, ui, k, &settings.bindings);
                    settings.save();
                }
            }
        }
    }
    // Persistent calls.
    let cs: Vec<UiCall> = calls.read().cloned().collect();
    let mut save = false;
    for c in &cs {
        save |= handle_call(tree, ui, &mut settings, c, &mut rend, &mut windows);
    }
    if save {
        settings.save();
    }
    for k in std::mem::take(&mut ui.sounds) {
        play(&mut commands, &assets, &settings, ui, k);
    }
    // ResetScreenResolution.Update: shown only when a non-default resolution is selected.
    for i in 0..tree.nodes.len() {
        if tree.script(i, "ResetScreenResolution").is_some() {
            tree.nodes[i].scale = if settings.resolution != 0 { Vec3::ONE } else { Vec3::ZERO };
        }
    }
    sync_enable(tree, ui);
    // PauseManager.Update: pause = pauseMenu.activeSelf; cursor + gameplay input.
    let pause = ui.pause_menu.map(|p| tree.nodes[p].active).unwrap_or(false);
    if pause != open.0 {
        open.0 = pause;
        if let Ok(mut c) = cursor.single_mut() {
            c.grab_mode = if pause { CursorGrabMode::None } else { CursorGrabMode::Locked };
            c.visible = pause;
        }
        input.enabled = !pause;
    }
    // PauseManager.LateUpdate (gamepad): nothing selected, or the selection went inactive -> select
    // FindObjectsOfType<Button>()[0] (Unity's order is its internal object list: the first active Button in
    // hierarchy order here).
    if ui.gamepad {
        let sel_ok = tree.selected.map(|s| effective_active(tree, s)).unwrap_or(false);
        if !sel_ok {
            if let Some(b) = (0..tree.nodes.len()).find(|&i| matches!(tree.nodes[i].widget, Widget::Button { .. }) && tree.nodes[i].sel.is_some() && effective_active(tree, i)) {
                tree.set_selected(Some(b));
            }
        }
    }
    // Interactive rebinding owns the input: no UI navigation / submit until it completes.
    tree.nav_block = ui.rebinding.is_some();
}

/// Window options from Settings (fullscreen / vsync).
fn apply_video(settings: Res<Settings>, mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    if !settings.is_changed() {
        return;
    }
    let Ok(mut w) = windows.single_mut() else { return };
    let pm = if settings.use_vsync != 0 { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync };
    if w.present_mode != pm {
        w.present_mode = pm;
    }
    // Autotests keep their off-screen window (present mode only).
    if std::env::var_os("RESTRAFE_AUTOTEST").is_some() {
        return;
    }
    let mode = if !settings.is_fullscreen {
        WindowMode::Windowed
    } else if settings.exclusive_fullscreen {
        WindowMode::Fullscreen(MonitorSelection::Current, VideoModeSelection::Current)
    } else {
        WindowMode::BorderlessFullscreen(MonitorSelection::Current)
    };
    if w.mode != mode {
        w.mode = mode;
    }
    let pm = if settings.use_vsync != 0 { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync };
    if w.present_mode != pm {
        w.present_mode = pm;
    }
}

/// PlayerSetup.ChangeMotionBlur(Settings.motionBlur): the Camera Profile MotionBlur override (shutterAngle 270,
/// sampleCount 10) on the world camera. Bevy samples per direction: 5 each way.
fn apply_motion_blur(
    mut commands: Commands,
    settings: Res<Settings>,
    cams: Query<(Entity, Has<bevy::post_process::motion_blur::MotionBlur>), With<crate::player::fpc::PlayerCamera>>,
) {
    for (e, has) in &cams {
        if settings.motion_blur && !has {
            commands.entity(e).insert(bevy::post_process::motion_blur::MotionBlur { shutter_angle: 270.0 / 360.0, samples: 5 });
        } else if !settings.motion_blur && has {
            commands.entity(e).remove::<bevy::post_process::motion_blur::MotionBlur>();
        }
    }
}

/// Settings.targetFpsToggle / targetFps -> Application.targetFrameRate (ignored by Unity while vsync is on; values
/// <= 30 leave it unchanged at startup). Runs first in the frame, before the time update: frame start to frame start
/// >= 1 / targetFps (no catch-up after slow frames). Coarse sleep, then a short spin.
fn frame_limiter(
    settings: Res<Settings>,
    real: Res<Time<bevy::time::Real>>,
    mut strategy: ResMut<bevy::time::TimeUpdateStrategy>,
    mut last: Local<Option<std::time::Instant>>,
    mut stats: Local<(f32, f32, u32, u32)>,
) {
    // Debug: RESTRAFE_FPS_LOG=1 prints, every second, the shortest frame and how many frames beat the cap.
    if std::env::var_os("RESTRAFE_FPS_LOG").is_some() && real.delta_secs() > 0.0 {
        let cap = 1.0 / settings.target_fps.max(1) as f32;
        let d = real.delta_secs();
        stats.0 += d;
        stats.1 = if stats.2 == 0 { d } else { stats.1.min(d) };
        stats.2 += 1;
        if d < cap * 0.99 {
            stats.3 += 1;
        }
        if stats.0 >= 1.0 {
            println!("FPSLOG frames={} min_dt={:.4} max_fps={:.1} over_cap={}", stats.2, stats.1, 1.0 / stats.1, stats.3);
            *stats = (0.0, 0.0, 0, 0);
        }
    }
    if settings.target_frame_rate > 0 && settings.use_vsync == 0 {
        if let Some(prev) = *last {
            let deadline = prev + std::time::Duration::from_secs_f64(1.0 / settings.target_frame_rate as f64);
            let now = std::time::Instant::now();
            if deadline > now {
                let left = deadline - now;
                if left > std::time::Duration::from_millis(2) {
                    std::thread::sleep(left - std::time::Duration::from_millis(1));
                }
                while std::time::Instant::now() < deadline {
                    std::hint::spin_loop();
                }
            }
        }
    }
    let now = std::time::Instant::now();
    *last = Some(now);
    // Unity measures deltaTime on its main loop: Time<Real> takes this (paced) instant instead of the render world's
    // end-of-render stamp, which jittered around the cap.
    *strategy = bevy::time::TimeUpdateStrategy::ManualInstant(now);
}

pub struct SettingsMenuPlugin;

impl Plugin for SettingsMenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(crate::ugui::UguiPlugin)
            .init_resource::<MenuOpen>()
            .init_resource::<PauseUi>()
            .add_systems(Startup, (setup_pause_ui, spawn_hud_light))
            // Right before Time<Real> is sampled: the measured frame time is the paced interval.
            .add_systems(First, frame_limiter.before(bevy::time::TimeSystems))
            .add_systems(Update, (menu_update.after(crate::ugui::ugui_input).before(crate::input::gather_input), apply_video, apply_motion_blur, update_hud_light, leave_match.after(menu_update)));
    }
}

#[allow(dead_code)]
fn unused(_: &HashMap<u8, u8>) {
    let _ = Bind::Key(KeyCode::KeyA);
}
