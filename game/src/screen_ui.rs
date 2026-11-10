//! In-game screen-space UI imported from the scenes (nodes created in `settings_menu::setup_pause_ui`):
//! * `---USER INTERFACE---/Speedometer` (TMP, bottom centre): FirstPersonController.Update writes
//!   "Speed : F2" while Settings.showSpeedometer (the text is only cleared on OnDisable, so it freezes when the
//!   setting is turned off mid-game, as in the original).
//! * `---MINIMAL UI---`: PlayerSetup.HideHUD(minimalistUi) -> hudObject (UICam, the 3D HUD) off + minimal UI on.
//!   PlayerHealth.Update writes "health : N", "<ping> ms", "<fps> fps"; PauseManager.ChangeAmmoText writes the
//!   GUN_LEFT / GUN_RIGHT texts (reloadText + text); PauseManager.MoveAmmoDisplay (grab: up, drop / spawn: down)
//!   DOMoves their y to AmmoDisplay/Position{Right,Left}{Up,Down} (bottom-anchored) in 0.3 s OutSine.
//! * `SceneMotor/Canvas/ExploratioNText` ("Exploration Mode"): SceneMotor.Update sets it active = testMap, which
//!   EnterSceneForAll sets when a map is entered alone through the map preview (exploration). Our test map is that
//!   case (single player, no rounds), so it is always shown.

use crate::player::fpc::Fpc;
use crate::ugui::UiTree;
use crate::weapons::WeaponUi;
use bevy::prelude::*;

#[derive(Resource, Default)]
pub struct ScreenUiNodes {
    pub speedometer: Option<usize>,
    pub minimal: Option<usize>,
    pub health: Option<usize>,
    pub fps: Option<usize>,
    pub ping: Option<usize>,
    /// [left, right] PauseManager.leftGunAmmo / rightGunAmmo.
    pub ammo: [Option<usize>; 2],
    pub exploration: Option<usize>,
    /// PauseManager.deadText / enemyHealthText (START ROUND TEXT/deadtext, HealthEnemyText).
    pub dead: Option<usize>,
    pub enemy_health: Option<usize>,
    /// KillCam switchCamText / playerHpText / playerNameText (START ROUND TEXT children).
    pub kill_cam: [Option<usize>; 3],
    /// PauseManager.interactPromptText / interactPromptTextGamepad (rebind UI letters): PauseManager.InteractPromptLetter.
    pub interact_prompt: [Option<usize>; 2],
    pub tab: Option<TabNodes>,
}

/// `-- TAB SCREEN --` (PauseManager.tabScreen + TabScreen texts + the local PlayerListItem).
#[derive(Default, Clone)]
pub struct TabNodes {
    pub root: usize,
    pub round: Option<usize>,
    pub scores: [Option<usize>; 4],
    pub map: Option<usize>,
    pub player_name: Option<usize>,
    pub player_ready: Option<usize>,
    pub not_ready_color: [f32; 4],
    pub icon: Option<usize>,
    pub cosmetic_button: Option<usize>,
}

#[derive(Default)]
struct State {
    fps_timer: f32,
    fps_count: f32,
    /// Applied (HUD hidden, minimal UI active).
    minimal_applied: Option<(bool, bool)>,
    /// PlayerSetup.HideHUD state (`hide`), driven by spawn / Settings.ToggleMinimalistUi / SetQualityLevelDropdown.
    hud_hidden: Option<bool>,
    /// Last seen (minimalistUi, qualitySetting) and death state, to detect those calls.
    last_minimal_quality: Option<(bool, i32)>,
    was_dead: bool,
    /// MoveAmmoDisplay state per hand [left, right].
    ammo_up: [Option<bool>; 2],
    /// deadtext ScaleTweenLoop (loopOnStart): seconds since the UI was set up, and the start scale.
    dead_loop_t: f32,
    dead_base_scale: Option<Vec3>,
    /// ShowEnemyHealthCoroutine: seconds since the local player object was disabled (death).
    dead_since: Option<f32>,
    /// PlayerSetup.EnemyHealth(): `enemyHealth` captured in OnDisable (the death frame), shown 0.2 s later.
    enemy_health_at_death: f32,
}

/// AmmoDisplay/PositionLeftUp/Down, PositionRightUp/Down anchoredPosition.y (anchored to the canvas bottom).
const AMMO_Y: [[f32; 2]; 2] = [[-109.0, 196.0], [-100.0, 228.0]];

#[allow(clippy::too_many_arguments)]
fn update_screen_ui(
    mut st: Local<State>,
    nodes: Option<Res<ScreenUiNodes>>,
    mut tree: ResMut<UiTree>,
    settings: Res<crate::settings::Settings>,
    real: Res<Time<bevy::time::Real>>,
    players: Query<(&Fpc, Option<&crate::health::PlayerHealth>, Option<&crate::weapons::pickup::Hands>)>,
    hud: Option<Res<crate::hud::HudNodes>>,
    mut vis: Query<&mut Visibility>,
    mut reader: MessageReader<WeaponUi>,
    time: Res<Time>,
    death: Res<crate::death::LocalDeath>,
    pad: Res<crate::input::PlayerInput>,
    dummies: Query<&crate::health::PlayerHealth, (With<crate::health::Dummy>, Without<crate::health::Corpse>)>,
    locals: Query<&crate::health::PlayerHealth, (With<crate::player::fpc::Fpc>, Without<crate::health::Dummy>)>,
) {
    let Some(n) = nodes else { return };
    let Ok((fpc, ph, hands)) = players.single() else { return };
    if let Some(e) = n.exploration {
        if !tree.nodes[e].active {
            tree.set_active(e, true);
        }
    }
    if let Some(s) = n.speedometer {
        if settings.show_speedometer || std::env::var_os("RESTRAFE_SPEEDO").is_some() {
            tree.set_text(s, &format!("Speed : {:.2}", fpc.player_speed));
        }
    }
    // PlayerSetup.HideHUD(hide): hudObject.SetActive(!hide) (here: UICam hidden) + minimalistUi.SetActive(hide).
    // * Spawn (OnStartClient): HideHUD(minimalistUi), then HideHUD(true) at quality Low (UICam also off there).
    // * Settings.ToggleMinimalistUi: HideHUD(minimalistUi) only if qualitySetting != 0.
    // * Settings.SetQualityLevelDropdown: HideHUD(quality == 0), so leaving Low shows the HUD even with minimalistUi.
    // * PlayerSetup.OnDisable (death): minimalistUi.SetActive(false); no localPlayer until the respawn.
    // Debug: RESTRAFE_MINIMAL_UI=1 forces the minimalist UI (RESTRAFE_SPEEDO=1 the speedometer) without settings.ron.
    let mu = settings.minimalist_ui || std::env::var_os("RESTRAFE_MINIMAL_UI").is_some();
    let q = settings.quality_setting;
    if st.hud_hidden.is_none() || (st.was_dead && !death.dead) {
        st.hud_hidden = Some(mu || q == 0);
    } else if !death.dead {
        if let Some((last_mu, last_q)) = st.last_minimal_quality {
            if last_q != q {
                st.hud_hidden = Some(q == 0);
            } else if last_mu != mu && q != 0 {
                st.hud_hidden = Some(mu);
            }
        }
    }
    st.last_minimal_quality = Some((mu, q));
    st.was_dead = death.dead;
    let hidden = st.hud_hidden.unwrap_or(false);
    let want = (hidden, hidden && !death.dead);
    let uicam = hud.as_ref().and_then(|h| h.uicam);
    if st.minimal_applied != Some(want) && hud.as_ref().is_some_and(|h| h.attached) && uicam.is_some_and(|u| vis.get(u).is_ok()) {
        st.minimal_applied = Some(want);
        if let Some(m) = n.minimal {
            tree.set_active(m, want.1);
        }
        if let Some(mut v) = uicam.and_then(|u| vis.get_mut(u).ok()) {
            *v = if want.0 { Visibility::Hidden } else { Visibility::Inherited };
        }
    }
    // PlayerHealth.Start coroutine: count = 1 / unscaledDeltaTime every 0.1 s.
    st.fps_timer -= real.delta_secs();
    if st.fps_timer <= 0.0 {
        st.fps_timer = 0.1;
        st.fps_count = 1.0 / real.delta_secs().max(1e-6);
    }
    let health = ph.map(|h| h.health).unwrap_or(4.0);
    if let Some(t) = n.health {
        tree.set_text(t, &format!("health : {}", (health / 4.0 * 100.0).ceil()));
    }
    if let Some(t) = n.ping {
        tree.set_text(t, "0 ms");
    }
    if let Some(t) = n.fps {
        tree.set_text(t, &format!("{} fps", st.fps_count.round()));
    }
    // MoveAmmoDisplay(up, right): text.transform.DOMove(y = target.position.y, 0.3).SetEase(OutSine).
    let held = [hands.is_some_and(|h| h.left.is_some()), hands.is_some_and(|h| h.right.is_some())];
    for i in 0..2 {
        let Some(t) = n.ammo[i] else { continue };
        if st.ammo_up[i] != Some(held[i]) {
            st.ammo_up[i] = Some(held[i]);
            // ---MINIMAL UI--- sits at the canvas centre, unscaled: its local space = canvas space (origin centre).
            let cur = tree.local_position(t);
            let y = -tree.canvas_size.y * 0.5 + AMMO_Y[i][held[i] as usize];
            tree.tween(t, crate::ugui::TweenProp::Pos, Vec3::new(cur.x, y, cur.z), 0.3, crate::tween::Ease::OutSine);
        }
    }
    // ScaleTweenLoop.Start (loopOnStart): DOScale(localScale * size, time).SetEase(ease).SetLoops(-1, Yoyo).
    if let Some(d) = n.dead {
        if let Some(stl) = tree.nodes[d].scripts.iter().find(|s| s.1 == "ScaleTweenLoop").map(|s| s.2.clone()) {
            use crate::ugui_scene::{f, int};
            let base = *st.dead_base_scale.get_or_insert(tree.nodes[d].scale);
            let (dur, size) = (f(stl.get("time")).max(1e-3), f(stl.get("size")));
            st.dead_loop_t += time.delta_secs();
            let cycle = (st.dead_loop_t / dur) as u32;
            let mut k = (st.dead_loop_t / dur).fract();
            if cycle % 2 == 1 {
                k = 1.0 - k;
            }
            let e = crate::tween::Ease::from_i32(int(stl.get("ease")) as i32).eval(k);
            tree.tween(d, crate::ugui::TweenProp::Scale, base.lerp(base * size, e), 0.0, crate::tween::Ease::Linear);
        }
    }
    // PlayerSetup.OnDisable (owner, the player object is despawned on death): EnemyHealth() + ShowEnemyHealth:
    // after 0.2 s, if dead (and the round was not won; no rounds here): "ENEMY HEALTH : <ceil(h/4*100)> HP" + "DEAD";
    // both cleared 2.7 s later. enemyHealth = the other "Player"-tagged PlayerHealth (the training dummy), else 0.
    if death.dead && st.dead_since.is_none() {
        st.dead_since = Some(0.0);
        // KillCam.enemy = PlayerHealth.killer: the own root after an own explosive (HandGrenade/Obus/... set killer to
        // the thrower), so EnemyHealth() reads the local player's dying health; else (no killer, e.g. the void) the
        // other Player-tagged PlayerHealth.
        let own = locals.iter().next().filter(|p| p.suicide).map(|p| p.health);
        st.enemy_health_at_death = own.or_else(|| dummies.iter().next().map(|d| d.health)).unwrap_or(0.0);
    }
    let h = st.enemy_health_at_death;
    if let Some(s) = st.dead_since.as_mut() {
        let prev = *s;
        *s += time.delta_secs();
        if prev < 0.2 && *s >= 0.2 && death.dead {
            if let Some(t) = n.enemy_health {
                tree.set_text(t, &format!("ENEMY HEALTH : {} HP", (h / 4.0 * 100.0).ceil()));
            }
            if let Some(t) = n.dead {
                tree.set_text(t, "DEAD");
            }
        }
        if *s >= 2.9 {
            for t in [n.enemy_health, n.dead].into_iter().flatten() {
                tree.set_text(t, "");
            }
            if !death.dead {
                st.dead_since = None;
            }
        }
    }
    // KillCam.Update texts (death.rs decides the branch; an alive player / respawn = a fresh KillCam: empty).
    // PauseManager.Update: gamepad && interactPromptTextGamepad ? its text : interactPromptText ? its text : "?".
    let prompt = if pad.gamepad && n.interact_prompt[1].is_some() { n.interact_prompt[1] } else { n.interact_prompt[0] };
    let letter = prompt.and_then(|p| tree.text(p)).map(str::to_string).unwrap_or_else(|| "?".into());
    let switch = format!("Mouse Buttons to switch player. {letter} to toggle free cam. H to hide HUD");
    let texts: [String; 3] = match death.cam_text {
        crate::death::KillCamText::Empty => Default::default(),
        crate::death::KillCamText::FreeCam => [switch, "Free Camera".into(), String::new()],
        // PlayerValues.playerClient.PlayerName: the dummy has no Steam client; "Dummy" stands in for it.
        crate::death::KillCamText::Spectate { health } => [switch, format!("Health : {}", (health / 4.0 * 100.0).ceil()), "Player : Dummy".into()],
    };
    for (node, s) in n.kill_cam.iter().zip(texts.iter()) {
        if let Some(t) = *node {
            tree.set_text(t, s);
        }
    }
    for m in reader.read() {
        if let WeaponUi::Ammo { right, text, charged } = m {
            if let Some(t) = n.ammo[*right as usize] {
                tree.set_text(t, &format!("{charged}{text}"));
            }
        }
    }
}

pub struct ScreenUiPlugin;

impl Plugin for ScreenUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (update_screen_ui, tab_screen.after(crate::input::gather_input)));
    }
}

/// PauseManager: `tabScreen.SetActive(Input.GetKey(KeyCode.Tab))` in game; while it is open `otherPauseBools` stops
/// mouse look (FirstPersonController.Update) and frees the cursor. TabScreen.Update texts: exploration mode never sets
/// SceneMotor.sceneIndex / roundAmount (0, 0) and every ScoreManager team has 0 points; the map is the active scene.
/// The local PlayerListItem: PlayerName (Steam persona name: "You" here), Ready false ("not ready",
/// notreadyColor), no Steam avatar (icon hidden).
#[allow(clippy::too_many_arguments)]
fn tab_screen(
    keys: Res<ButtonInput<KeyCode>>,
    nodes: Option<Res<ScreenUiNodes>>,
    mut tree: ResMut<UiTree>,
    paused: Res<crate::settings_menu::MenuOpen>,
    weapon_menu: Res<crate::weapon_menu::WeaponMenu>,
    cosmetics: Res<crate::cosmetics::CosmeticsMenuOpen>,
    mut input: ResMut<crate::input::PlayerInput>,
    mut cursor: Query<&mut bevy::window::CursorOptions, With<bevy::window::PrimaryWindow>>,
    mut was_open: Local<bool>,
    mut filled: Local<bool>,
) {
    let Some(tab) = nodes.and_then(|n| n.tab.clone()) else { return };
    if !*filled {
        *filled = true;
        let set = |tree: &mut UiTree, n: Option<usize>, s: &str| {
            if let Some(n) = n {
                tree.set_text(n, s);
            }
        };
        set(&mut tree, tab.round, "round 0   First to 0 wins");
        set(&mut tree, tab.map, &format!("current map :  {}", crate::scene_map::MAP));
        for sc in tab.scores {
            set(&mut tree, sc, "0");
        }
        set(&mut tree, tab.player_name, "You");
        set(&mut tree, tab.player_ready, "not ready");
        if let Some(g) = tab.player_ready.and_then(|n| tree.nodes[n].graphic.as_mut()) {
            g.color = tab.not_ready_color;
        }
        if let Some(g) = tab.icon.and_then(|n| tree.nodes[n].graphic.as_mut()) {
            g.color[3] = 0.0;
        }
        if let Some(c) = tab.cosmetic_button {
            tree.nodes[c].scale = Vec3::ZERO;
        }
    }
    let open = keys.pressed(KeyCode::Tab) || std::env::var_os("RESTRAFE_TAB").is_some();
    if tree.nodes[tab.root].active != open {
        tree.set_active(tab.root, open);
    }
    if open {
        input.mouse = Vec2::ZERO;
    }
    if open != *was_open {
        *was_open = open;
        if !paused.0 && !weapon_menu.open && !cosmetics.0 {
            if let Ok(mut c) = cursor.single_mut() {
                c.grab_mode = if open { bevy::window::CursorGrabMode::None } else { bevy::window::CursorGrabMode::Locked };
                c.visible = open;
            }
        }
    }
}
