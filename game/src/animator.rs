//! Mecanim AnimatorController runtime for the third-person body (`SK_Aboubi_00.controller`).
//!
//! Parses the exported controller YAML (layers, state machines, states -> clips / blend trees, transitions with
//! bool/trigger conditions, fixed durations, exit time, offset, interruption source) and evaluates it in muscle
//! space (`humanoid::MPose`), so crossfades / blend trees / layer overrides blend exactly like Mecanim does.
//!
//! Layers (as shipped): Base Layer (locomotion/crouch/slide/lean/jump), Aiming (2D torso-pitch tree on Horizontal/Vertical),
//! Arms (hold poses driven by PlayerPickup bools), plus three weight-0 layers (SpineHead_*, LowerPose) that the game never
//! enables (no SetLayerWeight anywhere) -> skipped.
//! Layer masks: the exported controller has no AvatarMask assets (all `m_Mask` null), but the built controller's
//! m_LayerArray[i].m_BodyMask (read from data.unity3d with UnityPy, 2026-10-26) confirms the masks used here:
//! Aiming / SpineHead_* = DoF 0-20 (spine, chest, upper chest, neck, head, eyes, jaw) + their TDoF, no root;
//! Arms = DoF 37-54 (shoulders, arms, forearms, hands) + hand goals + fingers + arm TDoF; Base Layer = everything.
//! DEVIATION: Any-State transitions never re-enter the state we are already in/fading to (Unity's canTransitionToSelf
//! would restart the lean pose every frame, which is visually the same for the static pose clips).

use crate::humanoid::{Avatar, HumanoidAnimator, HumanoidAssets, HumanoidRig, MPose, apply_pose, pose_from, sample_m};
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const CONTROLLER: &str = "SK_Aboubi_00";

// ---------------------------------------------------------------------------------------------
// Controller data
// ---------------------------------------------------------------------------------------------

enum Motion {
    Clip(String),
    Tree(Tree),
}

struct Tree {
    two_d: bool,
    px: String,
    py: String,
    kids: Vec<Kid>,
}

struct Kid {
    motion: Option<Motion>,
    thr: f32,
    pos: Vec2,
    ts: f32,
}

enum Cond {
    If(String),
    IfNot(String),
    Greater(String, f32),
    Less(String, f32),
    Equals(String, f32),
    NotEqual(String, f32),
}

struct Trans {
    conds: Vec<Cond>,
    dst: usize,
    dur: f32,
    exit: Option<f32>,
    offset: f32,
    interrupt: i64,
}

struct StateDef {
    name: String,
    speed: f32,
    motion: Option<Motion>,
    trans: Vec<Trans>,
}

struct LayerDef {
    name: String,
    weight: f32,
    additive: bool,
    /// Per human node; empty = whole body (+ root).
    mask: Vec<bool>,
    states: Vec<StateDef>,
    any: Vec<Trans>,
    default: usize,
}

#[derive(Resource)]
pub struct AnimatorAssets {
    ctl: Arc<Controller>,
}

pub struct Controller {
    layers: Vec<LayerDef>,
}

fn mask_for(avatar: &Avatar, prefixes: &[&str]) -> Vec<bool> {
    let mut m = vec![false; avatar.nodes.len()];
    for (name, (node, _)) in &avatar.muscles {
        if prefixes.iter().any(|p| name.starts_with(p)) {
            m[*node] = true;
        }
    }
    m
}

impl Controller {
    /// The player's AnimatorController, read from the installation (anim_ctl.rs).
    pub fn load(avatar: &Avatar) -> Controller {
        let ctl = crate::anim_ctl::load(CONTROLLER).expect("player animator controller in the installation");
        fn motion(m: &crate::anim_ctl::Motion) -> Motion {
            match m {
                crate::anim_ctl::Motion::Clip(c) => Motion::Clip(c.clone()),
                crate::anim_ctl::Motion::Tree(t) => Motion::Tree(Tree {
                    two_d: t.blend_type != 0,
                    px: t.px.clone(),
                    py: t.py.clone(),
                    kids: t.kids.iter().map(|k| Kid { motion: k.motion.as_ref().map(motion), thr: k.thr, pos: k.pos, ts: k.ts }).collect(),
                }),
            }
        }
        let trans = |t: &crate::anim_ctl::Trans| -> Option<Trans> {
            let conds = t
                .conds
                .iter()
                .filter_map(|c| {
                    let n = c.param.clone();
                    Some(match c.mode {
                        1 => Cond::If(n),
                        2 => Cond::IfNot(n),
                        3 => Cond::Greater(n, c.threshold),
                        4 => Cond::Less(n, c.threshold),
                        6 => Cond::Equals(n, c.threshold),
                        7 => Cond::NotEqual(n, c.threshold),
                        _ => return None,
                    })
                })
                .collect();
            Some(Trans { conds, dst: t.dst?, dur: t.dur, exit: t.exit, offset: t.offset, interrupt: t.interrupt })
        };
        let layers = ctl
            .layers
            .iter()
            .enumerate()
            .map(|(li, l)| {
                let mask = match l.name.as_str() {
                    "Aiming" => mask_for(avatar, &["Spine", "Chest", "UpperChest", "Neck", "Head", "Jaw", "Left Eye", "Right Eye"]),
                    "Arms" => mask_for(avatar, &["Left Shoulder", "Right Shoulder", "Left Arm", "Right Arm", "Left Forearm", "Right Forearm", "Left Hand", "Right Hand", "LeftHand.", "RightHand."]),
                    _ => Vec::new(),
                };
                LayerDef {
                    name: l.name.clone(),
                    weight: if li == 0 { 1.0 } else { l.weight },
                    additive: l.additive,
                    mask,
                    states: l.states.iter().map(|s| StateDef { name: s.name.clone(), speed: s.speed, motion: s.motion.as_ref().map(motion), trans: s.trans.iter().filter_map(trans).collect() }).collect(),
                    any: l.any.iter().filter_map(trans).collect(),
                    default: l.default,
                }
            })
            .collect();
        Controller { layers }
    }
}

// ---------------------------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct Params {
    vals: HashMap<String, f32>,
    trig: HashSet<String>,
}

impl Params {
    pub fn set_f(&mut self, n: &str, v: f32) {
        self.vals.insert(n.to_string(), v);
    }
    pub fn set_b(&mut self, n: &str, v: bool) {
        self.vals.insert(n.to_string(), v as i32 as f32);
    }
    pub fn set_trigger(&mut self, n: &str) {
        self.trig.insert(n.to_string());
    }
    fn f(&self, n: &str) -> f32 {
        self.vals.get(n).copied().unwrap_or(0.0)
    }
    fn b(&self, n: &str) -> bool {
        self.trig.contains(n) || self.f(n) > 0.5
    }
    fn check(&self, c: &Cond) -> bool {
        match c {
            Cond::If(n) => self.b(n),
            Cond::IfNot(n) => !self.b(n),
            Cond::Greater(n, t) => self.f(n) > *t,
            Cond::Less(n, t) => self.f(n) < *t,
            Cond::Equals(n, t) => (self.f(n) - t).abs() < 1e-5,
            Cond::NotEqual(n, t) => (self.f(n) - t).abs() >= 1e-5,
        }
    }
    fn consume(&mut self, conds: &[Cond]) {
        for c in conds {
            if let Cond::If(n) = c {
                self.trig.remove(n);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Motion evaluation
// ---------------------------------------------------------------------------------------------

fn tree_weights(t: &Tree, p: &Params) -> Vec<f32> {
    let n = t.kids.len();
    let mut w = vec![0.0; n];
    if n == 0 {
        return w;
    }
    if !t.two_d {
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|a, b| t.kids[*a].thr.total_cmp(&t.kids[*b].thr));
        let v = p.f(&t.px).clamp(t.kids[order[0]].thr, t.kids[order[n - 1]].thr);
        let hi = order.iter().position(|&i| t.kids[i].thr >= v).unwrap_or(n - 1);
        if hi == 0 {
            w[order[0]] = 1.0;
        } else {
            let (a, b) = (order[hi - 1], order[hi]);
            let span = t.kids[b].thr - t.kids[a].thr;
            let u = if span > 1e-6 { (v - t.kids[a].thr) / span } else { 1.0 };
            w[a] = 1.0 - u;
            w[b] = u;
        }
        return w;
    }
    // 2D simple directional: angular interpolation between the two neighbouring directions, faded to a uniform
    // blend (or the centre motion) towards the origin.
    let pt = Vec2::new(p.f(&t.px), p.f(&t.py));
    let r = pt.length().min(1.0);
    let ring: Vec<usize> = (0..n).filter(|&i| t.kids[i].pos.length() > 1e-4).collect();
    let center: Option<usize> = (0..n).find(|&i| t.kids[i].pos.length() <= 1e-4);
    let mut ang = vec![0.0f32; n];
    if ring.is_empty() {
        w.iter_mut().for_each(|x| *x = 1.0 / n as f32);
        return w;
    }
    if r > 1e-4 {
        let a = pt.y.atan2(pt.x);
        let mut sorted = ring.clone();
        for &i in &ring {
            ang[i] = t.kids[i].pos.y.atan2(t.kids[i].pos.x);
        }
        sorted.sort_by(|a, b| ang[*a].total_cmp(&ang[*b]));
        let m = sorted.len();
        if m == 1 {
            w[sorted[0]] = 1.0;
        } else {
            // find i with ang[i] <= a < ang[i+1] (wrapping)
            let mut lo = *sorted.last().unwrap();
            let mut hi = sorted[0];
            for k in 0..m {
                if ang[sorted[k]] <= a {
                    lo = sorted[k];
                    hi = sorted[(k + 1) % m];
                }
            }
            let mut span = ang[hi] - ang[lo];
            let mut da = a - ang[lo];
            if span <= 0.0 {
                span += std::f32::consts::TAU;
            }
            if da < 0.0 {
                da += std::f32::consts::TAU;
            }
            let u = (da / span).clamp(0.0, 1.0);
            w[lo] += 1.0 - u;
            w[hi] += u;
        }
    }
    let mut out = vec![0.0; n];
    let angular_total = if r > 1e-4 { r } else { 0.0 };
    for &i in &ring {
        let uniform = (1.0 - r) / if center.is_some() { 1.0 } else { ring.len() as f32 };
        out[i] = w[i] * angular_total + if center.is_some() { 0.0 } else { uniform };
    }
    if let Some(c) = center {
        out[c] = 1.0 - r;
    }
    out
}

fn motion_len(m: &Motion, p: &Params, assets: &mut HumanoidAssets) -> f32 {
    match m {
        Motion::Clip(n) => assets.clip(n).map(|c| c.length).unwrap_or(1.0),
        Motion::Tree(t) => {
            let w = tree_weights(t, p);
            let mut sum = 0.0;
            let mut tot = 0.0;
            for (k, w) in t.kids.iter().zip(&w) {
                if *w > 0.0 {
                    if let Some(km) = &k.motion {
                        sum += w * motion_len(km, p, assets) / if k.ts.abs() > 1e-4 { k.ts } else { 1.0 };
                        tot += w;
                    }
                }
            }
            if tot > 0.0 { sum / tot } else { 1.0 }
        }
    }
}

/// `n` = normalised state time (cycles).
fn motion_pose(m: &Motion, n: f32, p: &Params, assets: &mut HumanoidAssets) -> Option<MPose> {
    match m {
        Motion::Clip(name) => {
            let clip = assets.clip(name)?;
            let t = if clip.loop_time { n.rem_euclid(1.0) } else { n.clamp(0.0, 1.0) } * clip.length;
            Some(sample_m(&assets.avatar, &clip, t))
        }
        Motion::Tree(t) => {
            let w = tree_weights(t, p);
            let mut poses: Vec<(f32, MPose)> = Vec::new();
            for (k, w) in t.kids.iter().zip(&w) {
                if *w > 1e-5 {
                    if let Some(pose) = k.motion.as_ref().and_then(|km| motion_pose(km, n, p, assets)) {
                        poses.push((*w, pose));
                    }
                }
            }
            let refs: Vec<(f32, &MPose)> = poses.iter().map(|(w, p)| (*w, p)).collect();
            MPose::mix(&refs)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------------------------

/// Mecanim exit time: reached when normalized time crosses it during the last update; exitTime < 1 repeats every
/// loop (0 fires on the wrap after one full play), >= 1 fires once (same rule as weapons/anim.rs).
fn exit_crossed(prev: f32, cur: f32, x: f32) -> bool {
    if x >= 1.0 {
        prev < x && cur >= x
    } else {
        (cur - x).floor() > (prev - x).floor()
    }
}

#[derive(Clone, Copy)]
struct StateRun {
    state: usize,
    time: f32,
    /// Normalized time before the last advance (exit-time crossing check).
    prev: f32,
}

enum Src {
    State(StateRun),
    Snap(MPose),
}

struct Fade {
    dst: StateRun,
    dur: f32,
    el: f32,
    interrupt: i64,
}

struct LayerRun {
    src: Src,
    fade: Option<Fade>,
}

#[derive(Component, Default)]
pub struct AnimatorRuntime {
    pub params: Params,
    layers: Vec<LayerRun>,
    pub weights: Vec<f32>,
    dbg_t: f32,
    /// Base Layer pose of the last update (the hips are placed from it, see `animate`).
    pub base: Option<MPose>,
}

impl AnimatorRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Names of the current (target) state per layer, for debugging/tests.
    pub fn state_names(&self, ctl: &Controller) -> Vec<String> {
        self.layers
            .iter()
            .zip(&ctl.layers)
            .map(|(l, d)| {
                let s = match (&l.fade, &l.src) {
                    (Some(f), _) => Some(f.dst.state),
                    (None, Src::State(r)) => Some(r.state),
                    _ => None,
                };
                format!("{}:{}", d.name, s.map(|i| d.states[i].name.as_str()).unwrap_or("?"))
            })
            .collect()
    }

    fn eval_state(def: &LayerDef, r: StateRun, p: &Params, assets: &mut HumanoidAssets) -> Option<MPose> {
        motion_pose(def.states[r.state].motion.as_ref()?, r.time, p, assets)
    }

    fn layer_pose(l: &LayerRun, def: &LayerDef, p: &Params, assets: &mut HumanoidAssets) -> Option<MPose> {
        let a = match &l.src {
            Src::State(r) => Self::eval_state(def, *r, p, assets),
            Src::Snap(m) => Some(m.clone()),
        };
        let Some(f) = &l.fade else { return a };
        let b = Self::eval_state(def, f.dst, p, assets);
        let w = if f.dur > 0.0 { (f.el / f.dur).clamp(0.0, 1.0) } else { 1.0 };
        match (a, b) {
            (Some(a), Some(b)) => MPose::mix(&[(1.0 - w, &a), (w, &b)]),
            (a, None) => a,
            (None, b) => b,
        }
    }

    fn advance(def: &LayerDef, r: &mut StateRun, p: &Params, assets: &mut HumanoidAssets, dt: f32) {
        let st = &def.states[r.state];
        let len = st.motion.as_ref().map(|m| motion_len(m, p, assets)).unwrap_or(1.0).max(1e-3);
        r.prev = r.time;
        r.time += dt * st.speed / len;
    }

    fn step_layer(l: &mut LayerRun, def: &LayerDef, p: &mut Params, assets: &mut HumanoidAssets, dt: f32) {
        // --- transition selection ---
        let target = match (&l.fade, &l.src) {
            (Some(f), _) => Some(f.dst.state),
            (None, Src::State(r)) => Some(r.state),
            _ => None,
        };
        let mut lists: Vec<(Vec<&Trans>, StateRun)> = Vec::new();
        let src_list = |r: StateRun| (def.any.iter().chain(def.states[r.state].trans.iter()).collect::<Vec<_>>(), r);
        match (&l.fade, &l.src) {
            (None, Src::State(r)) => lists.push(src_list(*r)),
            (Some(f), src) => {
                let s = if let Src::State(r) = src { Some(src_list(*r)) } else { None };
                let d = src_list(f.dst);
                match f.interrupt {
                    1 => lists.extend(s),
                    2 => lists.push(d),
                    3 => {
                        lists.extend(s);
                        lists.push(d)
                    }
                    4 => {
                        lists.push(d);
                        lists.extend(s)
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        let mut fired: Option<(usize, usize)> = None; // (list idx, trans idx)
        'outer: for (li, (list, run)) in lists.iter().enumerate() {
            for (ti, t) in list.iter().enumerate() {
                if Some(t.dst) == target && ti < def.any.len() {
                    continue; // no re-entry from Any State
                }
                if t.exit.is_some_and(|e| !exit_crossed(run.prev, run.time, e)) {
                    continue;
                }
                if t.conds.iter().all(|c| p.check(c)) {
                    fired = Some((li, ti));
                    break 'outer;
                }
            }
        }
        if let Some((li, ti)) = fired {
            let t = lists[li].0[ti];
            let (dst, dur, offset, interrupt) = (t.dst, t.dur, t.offset, t.interrupt);
            p.consume(&t.conds);
            drop(lists);
            if l.fade.is_some() {
                if let Some(snap) = Self::layer_pose(l, def, p, assets) {
                    l.src = Src::Snap(snap);
                }
            }
            let run = StateRun { state: dst, time: offset, prev: offset };
            if dur <= 0.0 {
                l.src = Src::State(run);
                l.fade = None;
            } else {
                l.fade = Some(Fade { dst: run, dur, el: 0.0, interrupt });
            }
        }
        // --- advance time ---
        if let Src::State(r) = &mut l.src {
            Self::advance(def, r, p, assets, dt);
        }
        if let Some(f) = &mut l.fade {
            Self::advance(def, &mut f.dst, p, assets, dt);
            f.el += dt;
            if f.el >= f.dur {
                l.src = Src::State(f.dst);
                l.fade = None;
            }
        }
    }

    pub fn update(&mut self, ctl: &Controller, assets: &mut HumanoidAssets, dt: f32) -> MPose {
        if self.layers.is_empty() {
            self.layers = ctl.layers.iter().map(|d| LayerRun { src: Src::State(StateRun { state: d.default, time: 0.0, prev: 0.0 }), fade: None }).collect();
            self.weights = ctl.layers.iter().map(|d| d.weight).collect();
        }
        let dt = dt.min(0.1);
        let mut out = MPose::neutral(&assets.avatar);
        for (li, (l, def)) in self.layers.iter_mut().zip(&ctl.layers).enumerate() {
            let w = self.weights[li];
            if li > 0 && (w <= 0.0 || def.additive) {
                continue; // additive layers are unused by the shipped controller
            }
            Self::step_layer(l, def, &mut self.params, assets, dt);
            let Some(lp) = Self::layer_pose(l, def, &self.params, assets) else { continue };
            if li == 0 {
                self.base = Some(lp.clone());
                out = lp;
                continue;
            }
            let all = def.mask.is_empty();
            for (n, m) in out.musc.iter_mut().enumerate() {
                if all || def.mask[n] {
                    for d in 0..3 {
                        m[d] += (lp.musc[n][d] - m[d]) * w;
                    }
                }
            }
            if all {
                if let Some(mx) = MPose::mix(&[(1.0 - w, &out.clone()), (w, &lp)]) {
                    out.root_t = mx.root_t;
                    out.root_q = mx.root_q;
                }
            }
        }
        self.dbg_t += dt;
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Dummy driving
// ---------------------------------------------------------------------------------------------

/// Animator parameters for the training dummy (what FirstPersonController.HandleAnimation / PlayerPickup would set).
/// The dummy stands still: MovementSpeed 0, Grounded true. Set fields (or `RESTRAFE_DUMMY_POSE`) to pose it.
#[derive(Component, Clone)]
pub struct DummyAnimParams {
    pub crouch: bool,
    pub grounded: bool,
    pub slide: bool,
    pub crouch_move: f32,
    pub movement_speed: f32,
    pub vertical: f32,
    pub lean_right: bool,
    pub lean_left: bool,
    pub two_handed: bool,
    pub double_single: bool,
    pub single_handed: bool,
    pub left_handed: bool,
    /// Set true to fire the "Jump" trigger once.
    pub jump: bool,
}

impl Default for DummyAnimParams {
    fn default() -> Self {
        DummyAnimParams {
            crouch: false,
            grounded: true,
            slide: false,
            crouch_move: 0.0,
            movement_speed: 0.0,
            vertical: 0.0,
            lean_right: false,
            lean_left: false,
            two_handed: false,
            double_single: false,
            single_handed: false,
            left_handed: false,
            jump: false,
        }
    }
}

impl DummyAnimParams {
    /// `RESTRAFE_DUMMY_POSE` = comma list of: crouch, crouchwalk, walk, run, lean (right), leanleft, slide, jump,
    /// twohanded, single, double, left, up, down.
    pub fn from_env() -> Self {
        let mut p = Self::default();
        if let Ok(s) = std::env::var("RESTRAFE_DUMMY_POSE") {
            for k in s.split(',') {
                match k.trim() {
                    "crouch" => p.crouch = true,
                    "crouchwalk" => {
                        p.crouch = true;
                        p.crouch_move = 1.0
                    }
                    "walk" => p.movement_speed = 0.5,
                    "run" => p.movement_speed = 1.0,
                    "lean" | "leanright" => p.lean_right = true,
                    "leanleft" => p.lean_left = true,
                    "slide" => p.slide = true,
                    "jump" => {
                        p.jump = true;
                        p.grounded = false
                    }
                    "twohanded" => p.two_handed = true,
                    "single" | "singlehanded" => p.single_handed = true,
                    "double" | "doublesingle" => p.double_single = true,
                    "left" | "lefthanded" => p.left_handed = true,
                    "up" => p.vertical = -1.0,
                    "down" => p.vertical = 1.0,
                    "" => {}
                    o => warn!("RESTRAFE_DUMMY_POSE: unknown token {o}"),
                }
            }
        }
        p
    }

    fn apply(&mut self, a: &mut Params) {
        a.set_b("Crouch", self.crouch);
        a.set_b("Grounded", self.grounded);
        a.set_b("Slide", self.slide);
        a.set_f("crouchMove", self.crouch_move);
        a.set_f("MovementSpeed", self.movement_speed);
        a.set_f("Vertical", self.vertical);
        a.set_b("LeanRight", self.lean_right);
        a.set_b("LeanLeft", self.lean_left);
        a.set_b("TwoHanded", self.two_handed);
        a.set_b("DoubleSingle", self.double_single);
        a.set_b("SingleHanded", self.single_handed);
        a.set_b("LeftHanded", self.left_handed);
        if self.jump {
            a.set_trigger("Jump");
            self.jump = false;
        }
    }
}

fn animate_controller(
    time: Res<Time>,
    ctl: Res<AnimatorAssets>,
    mut assets: ResMut<HumanoidAssets>,
    mut q: Query<(Entity, &mut AnimatorRuntime, &HumanoidAnimator, &HumanoidRig, Option<&mut DummyAnimParams>)>,
    mut tfs: Query<&mut Transform>,
    mut logged: Local<bool>,
    mut last: Local<HashMap<Entity, Vec<String>>>,
) {
    for (e, mut rt, anim, rig, params) in &mut q {
        if anim.frozen {
            continue;
        }
        if let Some(mut p) = params {
            p.apply(&mut rt.params);
        }
        let m = rt.update(&ctl.ctl, &mut assets, time.delta_secs());
        if !*logged && rt.dbg_t > 2.0 && std::env::var_os("RESTRAFE_ANIM_DEBUG").is_some() {
            info!("animator states: {:?}", rt.state_names(&ctl.ctl));
            *logged = true;
        }
        // RESTRAFE_ANIM_DEBUG=2: log every state change (per animator entity).
        if std::env::var("RESTRAFE_ANIM_DEBUG").is_ok_and(|v| v == "2") {
            let names = rt.state_names(&ctl.ctl);
            if last.get(&e) != Some(&names) {
                info!("animator {e}: t={:.2} {:?}", rt.dbg_t, names);
                last.insert(e, names);
            }
        }
        // The upper layers (Aiming: spine, Arms) only change their masked bones: the hips keep the Base Layer pose's
        // placement. Re-solving the hips from the blended spine (body frame) tipped the slide pose back ~23 deg (knees
        // up into the owner's view); the original's slide view (captured in the shipped game, training ground) shows
        // the legs low and straight like the Aboubi_Slide_00 clip alone.
        let mut pose = pose_from(&assets.avatar, &m);
        if let Some(b) = rt.base.as_ref().filter(|_| std::env::var_os("RESTRAFE_BLENDED_HIPS").is_none()) {
            let bp = pose_from(&assets.avatar, b);
            pose.hips_t = bp.hips_t;
            pose.hips_q = bp.hips_q;
        }
        if std::env::var_os("RESTRAFE_ANIM_ROOT").is_some() && rt.state_names(&ctl.ctl).first().is_some_and(|n| n.contains("Slide")) {
            println!("ANIMROOT {e} t={:.2} root_t={:?} root_q={:?} hips_t={:?} hips_q(unity)={:.3?} leg_l_rot={:.3?}", rt.dbg_t, m.root_t, m.root_q, pose.hips_t, pose.hips_q, assets.avatar.human_bone.get(1).and_then(|&b| (b >= 0).then(|| pose.rot[b as usize])));
        }
        apply_pose(&assets.avatar, rig, &pose, &mut tfs);
    }
}

pub fn build(app: &mut App) {
    let avatar = app.world().resource::<HumanoidAssets>().avatar.clone();
    let ctl = Arc::new(Controller::load(&avatar));
    app.insert_resource(AnimatorAssets { ctl }).add_systems(Update, animate_controller.after(crate::humanoid::animate_humanoid));
}
