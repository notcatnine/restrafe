//! Port of the Unity Input System actions STRAFTAT uses (`PlayerControls` asset defaults).
//!
//! Unity fires action callbacks (`started` / `performed` / `canceled`) before `Update`, so
//! [`gather_input`] runs first each frame and gameplay systems consume the edge flags.
//! Gamepad: the gamepad half of every PlayerControls binding is merged into the same actions ([`map_pad`]).
//! `RESTRAFE_FAKE_PAD="lx,ly,rx,ry,lt,rt[,S+E+W+N+LB+RB+LS+RS+START+SELECT+DU+DD+DL+DR];..."` injects a
//! synthetic pad (one sample per frame, the last one repeats) and prints a FAKE_PAD line per frame.
//! `RESTRAFE_FAKE_PAD_MENU` (same format) is consumed instead while gameplay input is off (loading, menus).
//! Autotests drive [`SimPad`]. Gamepad bindings are rebindable (`Bindings::pad_*`, any device in either column, as
//! PerformInteractiveRebinding); the UI actions (Navigate / Submit / Cancel) are exported for ugui.rs.

use bevy::input::gamepad::{AxisSettings, ButtonAxisSettings, GamepadSettings};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;

/// A key, mouse button or gamepad control binding (rebindable from settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Bind {
    Key(KeyCode),
    Mouse(MouseButton),
    Pad(PadCtl),
}

/// Gamepad ButtonControls an interactive rebind can pick (Gamepad layout: buttons, d-pad, triggers, stick directions).
/// The first 14 are the digital buttons in [`PAD_BUTTONS`] order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PadCtl {
    ButtonSouth,
    ButtonEast,
    ButtonWest,
    ButtonNorth,
    LeftShoulder,
    RightShoulder,
    LeftStickPress,
    RightStickPress,
    Start,
    Select,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    LeftTrigger,
    RightTrigger,
    LeftStickUp,
    LeftStickDown,
    LeftStickLeft,
    LeftStickRight,
    RightStickUp,
    RightStickDown,
    RightStickLeft,
    RightStickRight,
}

pub const PAD_CTLS: [PadCtl; 24] = {
    use PadCtl::*;
    [
        ButtonSouth, ButtonEast, ButtonWest, ButtonNorth, LeftShoulder, RightShoulder, LeftStickPress, RightStickPress, Start, Select, DpadUp, DpadDown, DpadLeft,
        DpadRight, LeftTrigger, RightTrigger, LeftStickUp, LeftStickDown, LeftStickLeft, LeftStickRight, RightStickUp, RightStickDown, RightStickLeft, RightStickRight,
    ]
};

impl PadCtl {
    /// Slot in [`PadSample::analog`] for the analog controls.
    fn analog_slot(self) -> Option<usize> {
        (self as usize).checked_sub(14)
    }

    /// Unity control path.
    pub fn path(self) -> &'static str {
        use PadCtl::*;
        match self {
            ButtonSouth => "<Gamepad>/buttonSouth",
            ButtonEast => "<Gamepad>/buttonEast",
            ButtonWest => "<Gamepad>/buttonWest",
            ButtonNorth => "<Gamepad>/buttonNorth",
            LeftShoulder => "<Gamepad>/leftShoulder",
            RightShoulder => "<Gamepad>/rightShoulder",
            LeftStickPress => "<Gamepad>/leftStickPress",
            RightStickPress => "<Gamepad>/rightStickPress",
            Start => "<Gamepad>/start",
            Select => "<Gamepad>/select",
            DpadUp => "<Gamepad>/dpad/up",
            DpadDown => "<Gamepad>/dpad/down",
            DpadLeft => "<Gamepad>/dpad/left",
            DpadRight => "<Gamepad>/dpad/right",
            LeftTrigger => "<Gamepad>/leftTrigger",
            RightTrigger => "<Gamepad>/rightTrigger",
            LeftStickUp => "<Gamepad>/leftStick/up",
            LeftStickDown => "<Gamepad>/leftStick/down",
            LeftStickLeft => "<Gamepad>/leftStick/left",
            LeftStickRight => "<Gamepad>/leftStick/right",
            RightStickUp => "<Gamepad>/rightStick/up",
            RightStickDown => "<Gamepad>/rightStick/down",
            RightStickLeft => "<Gamepad>/rightStick/left",
            RightStickRight => "<Gamepad>/rightStick/right",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Bindings {
    pub forward: Vec<Bind>,
    pub back: Vec<Bind>,
    pub left: Vec<Bind>,
    pub right: Vec<Bind>,
    pub jump: Vec<Bind>,
    pub run: Vec<Bind>,
    pub crouch: Vec<Bind>,
    pub lean_left: Vec<Bind>,
    pub lean_right: Vec<Bind>,
    pub zoom: Vec<Bind>,
    pub fire: Vec<Bind>,
    pub fire2: Vec<Bind>,
    pub reload: Vec<Bind>,
    pub interact: Vec<Bind>,
    pub move_up: Vec<Bind>,
    /// Gamepad bindings of the same actions (PlayerControls "Gamepad" group / gamepad binding indices).
    pub pad_forward: Vec<Bind>,
    pub pad_back: Vec<Bind>,
    pub pad_left: Vec<Bind>,
    pub pad_right: Vec<Bind>,
    pub pad_jump: Vec<Bind>,
    pub pad_run: Vec<Bind>,
    pub pad_crouch: Vec<Bind>,
    pub pad_lean_left: Vec<Bind>,
    pub pad_lean_right: Vec<Bind>,
    pub pad_zoom: Vec<Bind>,
    pub pad_fire: Vec<Bind>,
    pub pad_fire2: Vec<Bind>,
    pub pad_reload: Vec<Bind>,
    pub pad_interact: Vec<Bind>,
    pub pad_move_up: Vec<Bind>,
    pub pad_drop: Vec<Bind>,
}

impl Default for Bindings {
    fn default() -> Self {
        use Bind::*;
        use PadCtl as P;
        Self {
            pad_forward: vec![Pad(P::LeftStickUp)],
            pad_back: vec![Pad(P::LeftStickDown)],
            pad_left: vec![Pad(P::LeftStickLeft)],
            pad_right: vec![Pad(P::LeftStickRight)],
            pad_jump: vec![Pad(P::ButtonSouth)],
            pad_run: vec![Pad(P::LeftStickPress)],
            pad_crouch: vec![Pad(P::ButtonEast)],
            pad_lean_left: vec![Pad(P::LeftShoulder)],
            pad_lean_right: vec![Pad(P::RightShoulder)],
            pad_zoom: vec![Pad(P::LeftTrigger)],
            pad_fire: vec![Pad(P::RightTrigger)],
            pad_fire2: vec![Pad(P::LeftTrigger)],
            pad_reload: vec![Pad(P::RightStickPress)],
            pad_interact: vec![Pad(P::ButtonWest)],
            pad_move_up: vec![Pad(P::ButtonSouth)],
            pad_drop: vec![Pad(P::ButtonNorth)],
            forward: vec![Key(KeyCode::KeyW)],
            back: vec![Key(KeyCode::KeyS)],
            left: vec![Key(KeyCode::KeyA)],
            right: vec![Key(KeyCode::KeyD)],
            jump: vec![Key(KeyCode::Space)],
            run: vec![Key(KeyCode::ShiftLeft)],
            crouch: vec![
                Key(KeyCode::KeyC),
                Key(KeyCode::ControlLeft),
                Key(KeyCode::ControlRight),
            ],
            lean_left: vec![Key(KeyCode::KeyQ)],
            lean_right: vec![Key(KeyCode::KeyE)],
            zoom: vec![Mouse(MouseButton::Right)],
            fire: vec![Mouse(MouseButton::Left)],
            fire2: vec![Mouse(MouseButton::Right)],
            reload: vec![Key(KeyCode::KeyR)],
            interact: vec![Key(KeyCode::KeyF)],
            move_up: vec![Key(KeyCode::Space)],
        }
    }
}

/// Button state with Input System phase edges.
#[derive(Clone, Copy, Debug, Default)]
pub struct Button {
    /// Button-type action phase `Performed` (`IsPressed()`), or any digital control held.
    pub held: bool,
    /// `started` (and, for digital controls, `performed`) fired this frame.
    pub pressed: bool,
    /// `performed` fired this frame. Same as `pressed` for keys / mouse / pad buttons; differs only for the
    /// analog triggers (started at > 0, performed at the 0.5 press point).
    pub performed: bool,
    /// `canceled` fired this frame.
    pub released: bool,
    /// Analog `ReadValue<float>()` from a gamepad trigger (0 when no analog control is actuated).
    pub analog: f32,
    /// A digital control (key, mouse button, pad button) is held.
    pub digital: bool,
}

impl Button {
    /// `ReadValue<float>()` for a button action.
    pub fn value(&self) -> f32 {
        // Value actions take the most actuated control (an actuated trigger has analog > 0).
        if self.analog > 0.0 {
            self.analog.max(if self.digital { 1.0 } else { 0.0 })
        } else if self.held {
            1.0
        } else {
            0.0
        }
    }

    /// A key / mouse / pad-button state (`performed` = `pressed`).
    pub fn digital(held: bool, pressed: bool, released: bool) -> Self {
        Self { held, pressed, performed: pressed, released, analog: 0.0, digital: held }
    }
}

#[derive(Resource, Default, Debug)]
pub struct PlayerInput {
    /// `Move` action (composite WASD, digital normalized like Unity's 2DVector composite).
    pub move_raw: Vec2,
    /// `MouseX` / `MouseY` pass-through value, Unity convention (+y = up): the mouse delta this frame, or the
    /// deadzoned right stick (which then holds its value, as the pass-through callback only fires on change).
    pub mouse: Vec2,
    /// Raw mouse delta only (legacy `Input.GetAxis("Mouse X/Y")` / 0.1: the old InputManager has no joystick axis).
    pub mouse_delta: Vec2,
    pub scroll: f32,
    pub jump: Button,
    pub run: Button,
    pub crouch: Button,
    pub lean_left: Button,
    pub lean_right: Button,
    pub zoom: Button,
    /// `FireHold` (Value) / `LeftClick`: LMB + right trigger.
    pub fire: Button,
    /// Keyboard/mouse part of `fire` only (legacy `Input.GetMouseButton(0)` call sites).
    pub fire_kbm: Button,
    /// `RightClick`: RMB + left trigger.
    pub fire2: Button,
    pub reload: Button,
    pub interact: Button,
    pub move_up: Button,
    /// `Drop` (gamepad buttonNorth only) -> PlayerPickup.HandleDrop -> SwitchWeapons, like a scroll.
    pub drop: Button,
    /// Number-row keys 1..0 pressed this frame (voice lines / taunts), index 0 = Alpha1.
    pub taunt: [bool; 10],
    /// Input is captured by gameplay (not paused / not in a menu).
    pub enabled: bool,
    /// `PauseManager.gamepad`: set by the `AnyGamepad` action, cleared by `Any` (keyboard key / mouse button).
    /// Persists across frames (x14 look speed, gamepad EndSlide, "X" interact prompt).
    pub gamepad: bool,
    /// PauseManager `menu` action, gamepad part (`<Gamepad>/start`) pressed this frame (escape is read directly).
    pub pad_menu: bool,
    /// UI `Navigate` (CustomUiInput UI map), gamepad part: 2DVector of leftStick + rightStick directions
    /// (DigitalNormalized), or `<Gamepad>/dpad`. Read while paused too.
    pub pad_nav: Vec2,
    /// UI `Submit` (`<Gamepad>/buttonSouth`) / `Cancel` (`*/{Cancel}`: buttonEast) performed this frame.
    pub pad_submit: bool,
    pub pad_cancel: bool,
    /// First gamepad ButtonControl actuated past the press point this frame (interactive rebinding).
    pub pad_pressed: Option<PadCtl>,
}

fn read<'a>(binds: impl IntoIterator<Item = &'a Bind>, keys: &ButtonInput<KeyCode>, mouse: &ButtonInput<MouseButton>) -> Button {
    let mut b = Button::default();
    for bind in binds {
        match *bind {
            Bind::Key(k) => {
                b.held |= keys.pressed(k);
                b.pressed |= keys.just_pressed(k);
                b.released |= keys.just_released(k);
            }
            Bind::Mouse(m) => {
                b.held |= mouse.pressed(m);
                b.pressed |= mouse.just_pressed(m);
                b.released |= mouse.just_released(m);
            }
            // Gamepad controls are merged by `map_pad`.
            Bind::Pad(_) => {}
        }
    }
    // Unity only fires `canceled` when the whole action goes to 0.
    b.released &= !b.held;
    b.performed = b.pressed;
    b.digital = b.held;
    b
}

// ---- Gamepad --------------------------------------------------------------------------------------------
// PlayerControls gamepad bindings (no binding/action processors or interactions except AnyGamepad buttonSouth
// "Press"), evaluated with the game's InputSystem.inputsettings: defaultDeadzoneMin 0.125, defaultDeadzoneMax
// 0.925, defaultButtonPressPoint 0.5, buttonReleaseThreshold 0.75. Stick x / y / up / down / left / right
// controls carry the layout's `axisDeadzone` processor (StickControl); triggers are raw ButtonControls.

const DEADZONE_MIN: f32 = 0.125;
const DEADZONE_MAX: f32 = 0.925;
const PRESS_POINT: f32 = 0.5;
const RELEASE_THRESHOLD: f32 = 0.75;

/// AxisDeadzoneProcessor.Process with the default min / max.
pub fn axis_deadzone(v: f32) -> f32 {
    let a = v.abs();
    if a < DEADZONE_MIN {
        0.0
    } else if a > DEADZONE_MAX {
        v.signum()
    } else {
        v.signum() * ((a - DEADZONE_MIN) / (DEADZONE_MAX - DEADZONE_MIN))
    }
}

/// Raw state of one gamepad (or the merged state of all of them: `<Gamepad>` binds every gamepad).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PadSample {
    pub ls: Vec2,
    pub rs: Vec2,
    pub lt: f32,
    pub rt: f32,
    /// Bit set of [`PAD_BUTTONS`].
    pub buttons: u32,
}

/// Digital pad buttons, bit order of [`PadSample::buttons`] (Unity name, Bevy button, fake-pad token).
pub const PAD_BUTTONS: [(&str, GamepadButton, &str); 14] = [
    ("buttonSouth", GamepadButton::South, "S"),
    ("buttonEast", GamepadButton::East, "E"),
    ("buttonWest", GamepadButton::West, "W"),
    ("buttonNorth", GamepadButton::North, "N"),
    ("leftShoulder", GamepadButton::LeftTrigger, "LB"),
    ("rightShoulder", GamepadButton::RightTrigger, "RB"),
    ("leftStickPress", GamepadButton::LeftThumb, "LS"),
    ("rightStickPress", GamepadButton::RightThumb, "RS"),
    ("start", GamepadButton::Start, "START"),
    ("select", GamepadButton::Select, "SELECT"),
    ("dpad/up", GamepadButton::DPadUp, "DU"),
    ("dpad/down", GamepadButton::DPadDown, "DD"),
    ("dpad/left", GamepadButton::DPadLeft, "DL"),
    ("dpad/right", GamepadButton::DPadRight, "DR"),
];
const SOUTH: u32 = 1 << 0;
const EAST: u32 = 1 << 1;
const START: u32 = 1 << 8;

impl PadSample {
    fn from_gamepad(g: &Gamepad) -> Self {
        let mut buttons = 0;
        for (i, (_, b, _)) in PAD_BUTTONS.iter().enumerate() {
            if g.pressed(*b) {
                buttons |= 1 << i;
            }
        }
        Self {
            ls: g.left_stick(),
            rs: g.right_stick(),
            lt: g.get(GamepadButton::LeftTrigger2).unwrap_or(0.0),
            rt: g.get(GamepadButton::RightTrigger2).unwrap_or(0.0),
            buttons,
        }
    }

    fn merge(&mut self, o: &Self) {
        if o.ls.length_squared() > self.ls.length_squared() {
            self.ls = o.ls;
        }
        if o.rs.length_squared() > self.rs.length_squared() {
            self.rs = o.rs;
        }
        self.lt = self.lt.max(o.lt);
        self.rt = self.rt.max(o.rt);
        self.buttons |= o.buttons;
    }

    /// Stick direction "button" values (StickControl up/down/left/right: clamp then axisDeadzone), order up, down, left, right.
    fn dirs(s: Vec2) -> [f32; 4] {
        [axis_deadzone(s.y.max(0.0)), axis_deadzone((-s.y).max(0.0)), axis_deadzone((-s.x).max(0.0)), axis_deadzone(s.x.max(0.0))]
    }

    /// Analog ButtonControls in [`PadCtl`] order from `LeftTrigger`: triggers, left / right stick directions.
    fn analog(&self) -> [f32; 10] {
        let l = Self::dirs(self.ls);
        let r = Self::dirs(self.rs);
        [self.lt, self.rt, l[0], l[1], l[2], l[3], r[0], r[1], r[2], r[3]]
    }

    /// Value of one control (digital buttons 0 / 1).
    pub fn value(&self, c: PadCtl) -> f32 {
        match c.analog_slot() {
            Some(i) => self.analog()[i],
            None => (self.buttons >> c as u32 & 1) as f32,
        }
    }

    /// Gamepad `dpad` Vector2 (DpadControl: up - down, right - left).
    fn dpad(&self) -> Vec2 {
        let b = |c: PadCtl| (self.buttons >> c as u32 & 1) as f32;
        Vec2::new(b(PadCtl::DpadRight) - b(PadCtl::DpadLeft), b(PadCtl::DpadUp) - b(PadCtl::DpadDown))
    }

    /// `RESTRAFE_FAKE_PAD` frame: `lx,ly,rx,ry,lt,rt[,BTN+BTN...]`.
    fn parse(s: &str) -> Self {
        let parts: Vec<&str> = s.split(',').map(str::trim).collect();
        let f = |i: usize| parts.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
        let mut buttons = 0;
        if let Some(b) = parts.get(6) {
            for t in b.split('+') {
                if let Some(i) = PAD_BUTTONS.iter().position(|(_, _, n)| n.eq_ignore_ascii_case(t)) {
                    buttons |= 1 << i;
                }
            }
        }
        Self { ls: Vec2::new(f(0), f(1)), rs: Vec2::new(f(2), f(3)), lt: f(4), rt: f(5), buttons }
    }
}

/// Default-interaction phase of a Button-type action driven by an analog trigger
/// (InputActionState.ProcessDefaultInteraction).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Waiting,
    Started,
    Performed,
}

#[derive(Default)]
pub struct PadState {
    prev: PadSample,
    /// Default-interaction phase of each analog control ([`PadSample::analog`] order) and this frame's edges.
    phase: [Phase; 10],
    edges: [(bool, bool, bool); 10],
    connected: bool,
    fake: Option<(Vec<PadSample>, usize)>,
    /// RESTRAFE_FAKE_PAD_MENU: samples used while gameplay input is off (menus).
    fake_menu: Option<(Vec<PadSample>, usize)>,
}

/// Synthetic pad driven by autotests (overrides real pads while `active`).
#[derive(Resource, Default)]
pub struct SimPad {
    pub active: bool,
    pub sample: PadSample,
}

impl PadSample {
    /// Builds a sample from a fake-pad button token list ("S+DD").
    pub fn with_buttons(tokens: &str) -> Self {
        Self::parse(&format!("0,0,0,0,0,0,{tokens}"))
    }

    pub fn with_sticks(ls: Vec2, rs: Vec2) -> Self {
        Self { ls, rs, ..default() }
    }
}

/// Steps a trigger Button action; returns (started, performed, canceled).
fn step_trigger(phase: &mut Phase, v: f32) -> (bool, bool, bool) {
    match *phase {
        Phase::Waiting if v >= PRESS_POINT => {
            *phase = Phase::Performed;
            (true, true, false)
        }
        Phase::Waiting if v > 0.0 => {
            *phase = Phase::Started;
            (true, false, false)
        }
        Phase::Started if v >= PRESS_POINT => {
            *phase = Phase::Performed;
            (false, true, false)
        }
        Phase::Started if v <= 0.0 => {
            *phase = Phase::Waiting;
            (false, false, true)
        }
        Phase::Performed if v < PRESS_POINT * RELEASE_THRESHOLD => {
            if v > 0.0 {
                *phase = Phase::Started;
                (true, false, false)
            } else {
                *phase = Phase::Waiting;
                (false, false, true)
            }
        }
        _ => (false, false, false),
    }
}

/// Merges a digital pad button into an action.
fn add_digital(b: &mut Button, cur: &PadSample, prev: &PadSample, bit: u32) {
    let (h, p) = (cur.buttons & bit != 0, prev.buttons & bit != 0);
    b.held |= h;
    b.digital |= h;
    b.pressed |= h && !p;
    b.performed |= h && !p;
    b.released |= !h && p;
}

/// Merges the gamepad controls of a binding list into an action (digital buttons, or analog controls with the
/// default interaction phase).
fn add_pad(b: &mut Button, binds: &[Bind], cur: &PadSample, st: &PadState) {
    for bind in binds {
        let Bind::Pad(c) = *bind else { continue };
        match c.analog_slot() {
            Some(i) => add_trigger(b, st.phase[i], st.edges[i], cur.analog()[i]),
            None => add_digital(b, cur, &st.prev, 1 << c as u32),
        }
    }
}

/// A 2DVector composite part bound to gamepad controls, read as a button (value >= press point).
fn pad_part(binds: &[Bind], cur: &PadSample) -> bool {
    binds.iter().any(|b| matches!(b, Bind::Pad(c) if cur.value(*c) >= PRESS_POINT))
}

/// Merges a trigger-driven Button-type action (phase already stepped).
fn add_trigger(b: &mut Button, phase: Phase, edges: (bool, bool, bool), v: f32) {
    b.held |= phase == Phase::Performed;
    b.pressed |= edges.0;
    b.performed |= edges.1;
    b.released |= edges.2;
    if phase != Phase::Waiting {
        b.analog = b.analog.max(v);
    }
}

/// Maps one frame of gamepad state onto the actions (gamepad half of every PlayerControls binding).
fn map_pad(cur: &PadSample, st: &mut PadState, new: &mut PlayerInput, b: &Bindings) {
    let prev = st.prev;
    let (a, pa) = (cur.analog(), prev.analog());
    for i in 0..10 {
        st.edges[i] = step_trigger(&mut st.phase[i], a[i]);
    }
    // AnyGamepad (Button): any bound control crossing the press point -> ChangeControlSchemeGamepad.
    let any_pad = (cur.buttons & !prev.buttons) != 0 || a.iter().zip(pa.iter()).any(|(c, p)| *c >= PRESS_POINT && *p < PRESS_POINT);
    if any_pad {
        new.gamepad = true;
    }
    new.pad_menu = cur.buttons & START != 0 && prev.buttons & START == 0;
    // UI map (InputSystemUIInputModule, active in menus and in game): Navigate = "Gamepad" 2DVector composite
    // (up = leftStick/up | rightStick/up, ..., DigitalNormalized) + <Gamepad>/dpad (pass-through: the dpad wins
    // while held); Submit = buttonSouth; Cancel = buttonEast.
    let (l, r) = (PadSample::dirs(cur.ls), PadSample::dirs(cur.rs));
    let part = |k: usize| (l[k].max(r[k]) >= PRESS_POINT) as i32 as f32;
    let comp = Vec2::new(part(3) - part(2), part(0) - part(1));
    let dp = cur.dpad();
    new.pad_nav = if dp != Vec2::ZERO { dp.normalize() } else { comp.normalize_or_zero() };
    new.pad_submit = cur.buttons & SOUTH != 0 && prev.buttons & SOUTH == 0;
    new.pad_cancel = cur.buttons & EAST != 0 && prev.buttons & EAST == 0;
    new.pad_pressed = PAD_CTLS.iter().copied().find(|&c| match c.analog_slot() {
        Some(i) => a[i] >= PRESS_POINT && pa[i] < PRESS_POINT,
        None => cur.buttons & !prev.buttons & (1 << c as u32) != 0,
    });
    if !new.enabled {
        return;
    }
    // Move: "Dpad" (= 2DVector) composite of leftStick/up..right, mode DigitalNormalized: each part is a button
    // (deadzoned direction >= press point), then normalized. Value action: keyboard composite wins if active.
    let p = |k: &[Bind], g: &[Bind]| (pad_part(k, cur) || pad_part(g, cur)) as i32 as f32;
    let v = Vec2::new(p(&b.right, &b.pad_right) - p(&b.left, &b.pad_left), p(&b.forward, &b.pad_forward) - p(&b.back, &b.pad_back));
    if new.move_raw == Vec2::ZERO && v != Vec2::ZERO {
        new.move_raw = v.normalize();
    }
    // MouseX <Gamepad>/rightStick/x, MouseY <XInputController>/rightStick/y (pass-through, last change wins: a
    // mouse delta this frame overrides the stick).
    if new.mouse == Vec2::ZERO {
        new.mouse = Vec2::new(axis_deadzone(cur.rs.x), axis_deadzone(cur.rs.y));
    }
    let s = &*st;
    let m = |x: &mut Button, k: &[Bind], g: &[Bind]| {
        add_pad(x, k, cur, s);
        add_pad(x, g, cur, s);
    };
    m(&mut new.jump, &b.jump, &b.pad_jump);
    m(&mut new.move_up, &b.move_up, &b.pad_move_up);
    m(&mut new.run, &b.run, &b.pad_run);
    m(&mut new.crouch, &b.crouch, &b.pad_crouch);
    m(&mut new.interact, &b.interact, &b.pad_interact);
    m(&mut new.drop, &[], &b.pad_drop);
    m(&mut new.reload, &b.reload, &b.pad_reload);
    m(&mut new.lean_left, &b.lean_left, &b.pad_lean_left);
    m(&mut new.lean_right, &b.lean_right, &b.pad_lean_right);
    m(&mut new.fire, &b.fire, &b.pad_fire);
    m(&mut new.fire2, &b.fire2, &b.pad_fire2);
    m(&mut new.zoom, &b.zoom, &b.pad_zoom);
    for b in [&mut new.jump, &mut new.move_up, &mut new.run, &mut new.crouch, &mut new.interact, &mut new.drop, &mut new.reload, &mut new.lean_left, &mut new.lean_right, &mut new.fire, &mut new.fire2, &mut new.zoom] {
        b.released &= !b.held && b.analog == 0.0;
    }
}

/// Bevy filters stick / trigger values (5% deadzone rescale, 0.05..0.95 trigger clamp): pass them through raw so
/// the Input System processors above are the only shaping.
pub fn configure_gamepads(mut q: Query<&mut GamepadSettings, Added<GamepadSettings>>) {
    for mut s in &mut q {
        if let Ok(a) = AxisSettings::new(-1.0, 0.0, 0.0, 1.0, 0.0) {
            s.default_axis_settings = a;
        }
        s.default_button_axis_settings = ButtonAxisSettings { high: 1.0, low: 0.0, threshold: 0.0 };
    }
}

pub fn gather_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    settings: Res<crate::settings::Settings>,
    pads: Query<&Gamepad>,
    mut pad: Local<PadState>,
    mut input: ResMut<PlayerInput>,
    sim: Option<Res<SimPad>>,
) {
    let enabled = input.enabled;
    let b = &settings.bindings;
    let k = &*keys;
    let m = &*mouse_buttons;
    let mut new = PlayerInput {
        enabled,
        gamepad: input.gamepad,
        ..default()
    };
    if enabled {
        let axis = |neg: [&[Bind]; 2], pos: [&[Bind]; 2]| -> f32 {
            (if read(pos.into_iter().flatten(), k, m).held { 1.0 } else { 0.0 })
                - (if read(neg.into_iter().flatten(), k, m).held { 1.0 } else { 0.0 })
        };
        let v = Vec2::new(axis([&b.left, &b.pad_left], [&b.right, &b.pad_right]), axis([&b.back, &b.pad_back], [&b.forward, &b.pad_forward]));
        // Unity's 2DVector composite defaults to mode "DigitalNormalized".
        new.move_raw = if v != Vec2::ZERO {
            v.normalize()
        } else {
            Vec2::ZERO
        };
        new.mouse = Vec2::new(motion.delta.x, -motion.delta.y);
        new.mouse_delta = new.mouse;
        new.scroll = scroll.delta.y;
        new.jump = read(b.jump.iter().chain(&b.pad_jump), k, m);
        new.run = read(b.run.iter().chain(&b.pad_run), k, m);
        new.crouch = read(b.crouch.iter().chain(&b.pad_crouch), k, m);
        new.lean_left = read(b.lean_left.iter().chain(&b.pad_lean_left), k, m);
        new.lean_right = read(b.lean_right.iter().chain(&b.pad_lean_right), k, m);
        new.zoom = read(b.zoom.iter().chain(&b.pad_zoom), k, m);
        new.fire = read(b.fire.iter().chain(&b.pad_fire), k, m);
        new.fire_kbm = new.fire;
        new.fire2 = read(b.fire2.iter().chain(&b.pad_fire2), k, m);
        new.reload = read(b.reload.iter().chain(&b.pad_reload), k, m);
        new.interact = read(b.interact.iter().chain(&b.pad_interact), k, m);
        new.move_up = read(b.move_up.iter().chain(&b.pad_move_up), k, m);
        const DIGITS: [KeyCode; 10] = [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
            KeyCode::Digit0,
        ];
        for (i, d) in DIGITS.iter().enumerate() {
            new.taunt[i] = keys.just_pressed(*d);
        }
    }
    // Any (<Keyboard>/anyKey, <Mouse>/left|middle|rightButton) -> ChangeControlSchemeKeyboard.
    if keys.get_just_pressed().next().is_some()
        || mouse_buttons.any_just_pressed([MouseButton::Left, MouseButton::Middle, MouseButton::Right])
    {
        new.gamepad = false;
    }
    // Gamepad (or the RESTRAFE_FAKE_PAD synthetic one).
    if pad.fake.is_none() {
        if let Ok(s) = std::env::var("RESTRAFE_FAKE_PAD") {
            pad.fake = Some((s.split(';').map(PadSample::parse).collect(), 0));
        } else {
            pad.fake = Some((vec![], 0));
        }
    }
    // RESTRAFE_FAKE_PAD_MENU: same format, consumed only while gameplay input is off (pause / settings menus).
    if pad.fake_menu.is_none() {
        pad.fake_menu = Some((std::env::var("RESTRAFE_FAKE_PAD_MENU").map(|s| s.split(';').map(PadSample::parse).collect()).unwrap_or_default(), 0));
    }
    let step = |f: &mut Option<(Vec<PadSample>, usize)>| {
        f.as_mut().and_then(|(v, i)| {
            if v.is_empty() {
                return None;
            }
            let s = v[(*i).min(v.len() - 1)];
            *i += 1;
            Some((s, *i, v.len()))
        })
    };
    let menu_fake = !enabled && pad.fake_menu.as_ref().map(|f| !f.0.is_empty()).unwrap_or(false);
    let fake = if menu_fake {
        step(&mut pad.fake_menu)
    } else if enabled {
        step(&mut pad.fake)
    } else {
        None
    };
    let mut cur = PadSample::default();
    let mut any = false;
    for g in &pads {
        cur.merge(&PadSample::from_gamepad(g));
        any = true;
    }
    if let Some((s, _, _)) = fake {
        cur = s;
        any = true;
    }
    if let Some(sp) = sim.as_deref().filter(|s| s.active) {
        cur = sp.sample;
        any = true;
    }
    // Gamepad.current == null: AnyGamepad never switches the scheme.
    // (A pad that just disconnected maps once more with a zero sample, releasing its buttons.)
    if any || pad.connected {
        map_pad(&cur, &mut pad, &mut new, b);
    }
    pad.connected = any;
    pad.prev = cur;
    if let Some((s, i, n)) = fake {
        if i <= n + 2 {
            println!(
                "FAKE_PAD f={i} in={s:?} gamepad={} move={:.4?} mouse={:.4?} fire=({},{},{},{},{:.3}) fire2=({},{},{},{:.3}) zoom=({},{},{},{}) jump={} crouch={} interact={} drop={} reload={} run={} lean=({},{}) menu={} nav={:?} submit={} cancel={} pressed={:?}{}",
                new.gamepad, new.move_raw, new.mouse,
                new.fire.held, new.fire.pressed, new.fire.performed, new.fire.released, new.fire.value(),
                new.fire2.held, new.fire2.pressed, new.fire2.released, new.fire2.value(),
                new.zoom.held, new.zoom.pressed, new.zoom.performed, new.zoom.released,
                new.jump.pressed, new.crouch.pressed, new.interact.pressed, new.drop.pressed, new.reload.pressed,
                new.run.value(), new.lean_left.value(), new.lean_right.value(), new.pad_menu, new.pad_nav, new.pad_submit, new.pad_cancel, new.pad_pressed, if menu_fake { " (menu)" } else { "" }
            );
        }
    }
    *input = new;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadzone() {
        assert_eq!(axis_deadzone(0.1), 0.0);
        assert!((axis_deadzone(0.525) - 0.5).abs() < 1e-6);
        assert_eq!(axis_deadzone(-0.95), -1.0);
    }

    #[test]
    fn trigger_phases() {
        let mut p = Phase::Waiting;
        assert_eq!(step_trigger(&mut p, 0.2), (true, false, false));
        assert_eq!(step_trigger(&mut p, 0.6), (false, true, false));
        assert_eq!(step_trigger(&mut p, 0.4), (false, false, false)); // above release point 0.375
        assert_eq!(step_trigger(&mut p, 0.3), (true, false, false)); // back to Started
        assert_eq!(step_trigger(&mut p, 0.0), (false, false, true));
        assert_eq!(step_trigger(&mut p, 1.0), (true, true, false));
    }

    fn frame(st: &mut PadState, s: &str, b: &Bindings, enabled: bool) -> PlayerInput {
        let cur = PadSample::parse(s);
        let mut new = PlayerInput { enabled, ..default() };
        map_pad(&cur, st, &mut new, b);
        st.prev = cur;
        new
    }

    /// Every in-game gamepad action with the default PlayerControls gamepad bindings.
    #[test]
    fn pad_every_game_action() {
        let b = Bindings::default();
        let mut st = PadState::default();
        let i = frame(&mut st, "0,0,0,0,0,0", &b, true);
        assert!(!i.gamepad);
        // Move (left stick, DigitalNormalized) + look (right stick x / y, axisDeadzone) + both triggers.
        let i = frame(&mut st, "-0.9,0.9,0.525,-1,1,1", &b, true);
        assert!(i.gamepad);
        assert!((i.move_raw - Vec2::new(-1.0, 1.0).normalize()).length() < 1e-6);
        assert!((i.mouse - Vec2::new(0.5, -1.0)).length() < 1e-6);
        assert!(i.fire.performed && i.fire.held && i.fire2.performed && i.zoom.performed);
        // Below the stick press point: no move part.
        let i = frame(&mut st, "0.3,0,0,0,0,0", &b, true);
        assert_eq!(i.move_raw, Vec2::ZERO);
        assert!(i.fire.released && i.fire2.released && i.zoom.released);
        let tap = |st: &mut PadState, t: &str| {
            frame(st, "0,0,0,0,0,0", &b, true);
            frame(st, &format!("0,0,0,0,0,0,{t}"), &b, true)
        };
        assert!(tap(&mut st, "S").jump.pressed);
        assert!(tap(&mut st, "E").crouch.pressed);
        assert!(tap(&mut st, "LS").run.pressed);
        assert!(tap(&mut st, "W").interact.pressed);
        assert!(tap(&mut st, "LB").lean_left.pressed);
        assert!(tap(&mut st, "RB").lean_right.pressed);
        assert!(tap(&mut st, "N").drop.pressed);
        assert!(tap(&mut st, "RS").reload.pressed);
        assert!(tap(&mut st, "START").pad_menu);
        // Nothing else fires on a single button.
        let i = tap(&mut st, "RS");
        assert!(!i.jump.pressed && !i.crouch.pressed && !i.interact.pressed && !i.drop.pressed && !i.pad_menu);
        // Paused (input off): only the scheme / menu / UI actions.
        frame(&mut st, "0,0,0,0,0,0", &b, false);
        let i = frame(&mut st, "0,0,0,0,0,0,S+START", &b, false);
        assert!(!i.jump.pressed && i.pad_menu && i.pad_submit);
    }

    #[test]
    fn pad_bindings_and_ui_actions() {
        let mut b = Bindings::default();
        let mut st = PadState::default();
        // Defaults: South = jump, right trigger = fire, left stick = move.
        let i = frame(&mut st, "0,0.9,0,0,0,1,S", &b, true);
        assert!(i.jump.pressed && i.fire.performed && i.move_raw == Vec2::Y && i.pad_submit);
        assert_eq!(i.pad_pressed, Some(PadCtl::ButtonSouth));
        // Rebound gamepad jump (North): South no longer jumps.
        b.pad_jump = vec![Bind::Pad(PadCtl::ButtonNorth)];
        frame(&mut st, "0,0,0,0,0,0", &b, true);
        let i = frame(&mut st, "0,0,0,0,0,0,S+N", &b, true);
        assert!(i.jump.pressed && i.drop.pressed);
        frame(&mut st, "0,0,0,0,0,0", &b, true);
        let i = frame(&mut st, "0,0,0,0,0,0,S", &b, true);
        assert!(!i.jump.pressed && i.move_up.pressed);
        // UI Navigate while paused: right stick composite, the d-pad wins; Cancel = East; analog press for rebinding.
        let i = frame(&mut st, "0,0,0.8,0,0,0", &b, false);
        assert_eq!(i.pad_nav, Vec2::X);
        assert_eq!(i.pad_pressed, Some(PadCtl::RightStickRight));
        let i = frame(&mut st, "0,0,0.8,0,0,0,DD+E", &b, false);
        assert_eq!(i.pad_nav, Vec2::NEG_Y);
        assert!(i.pad_cancel && !i.jump.held);
    }
}
