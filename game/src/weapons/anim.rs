//! Generic Unity Animator for weapon prefabs: MeleeWeapon's `attackAnimator` (a NetworkAnimator on the weapon root,
//! e.g. `Katana.controller`: KatanaIdle -> BaseAttack/SecondAttack triggers -> exit time back, OutOfHand bool) and
//! Weapon.animator (Glock slide "Shoot", gun "Reload" clips).
//! Parses the `.controller` (single layer: states, trigger/bool conditions, exit-time transitions, any-state with
//! m_CanTransitionToSelf, transition durations) and samples each clip's Euler/Position curves (Hermite) onto the
//! weapon glb nodes. Transitions cross-fade linearly over m_TransitionDuration (not interruptible). Every state has
//! m_WriteDefaultValues = 1, so channels the current clip doesn't animate get the node's default (bind) value.
//! Clip paths are relative to the Animator's GameObject (not always the prefab root: Glock, Kanye).
//! Controllers and clips are read from the installation (anim_ctl.rs / anim_clip.rs); clip paths resolve against the
//! weapon prefab's hierarchy.

use super::data::UField;
use super::melee::MeleeState;
use super::{Hand, Item};
use bevy::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};


struct Curve3 {
    path: String,
    euler: bool,
    /// Index into `Ctl::channels`.
    ch: usize,
    /// x, y, z keys (each component keeps its own key times)
    comps: [Vec<crate::anim_clip::Key>; 3],
}

struct Clip {
    len: f32,
    looping: bool,
    curves: Vec<Curve3>,
}

struct Cond {
    param: String,
    /// 1 = If (true / trigger set), 2 = IfNot.
    mode: i32,
}

struct Trans {
    dst: usize,
    conds: Vec<Cond>,
    exit: Option<f32>,
    /// m_TransitionDuration (seconds if `fixed`, else fraction of the source state's length).
    dur: f32,
    fixed: bool,
    /// m_CanTransitionToSelf (only meaningful for any-state transitions).
    to_self: bool,
}

struct State {
    clip: Option<Arc<Clip>>,
    speed: f32,
    trans: Vec<Trans>,
}

pub struct Ctl {
    states: Vec<State>,
    any: Vec<Trans>,
    default: usize,
    /// Every animated (path, euler) binding over all clips of the controller.
    channels: Vec<(String, bool)>,
}

impl Ctl {
    /// State length used for normalized time (empty states count as 1 s).
    fn len(&self, s: usize) -> f32 {
        self.states[s].clip.as_ref().map(|c| c.len.max(1e-4)).unwrap_or(1.0)
    }
}

/// Euler / position curves of a clip from the installation, paths resolved against the weapon prefab.
fn load_clip(name: &str, resolver: &dyn Fn(u32) -> Option<String>) -> Option<Clip> {
    use crate::anim_clip::Kind;
    let c = crate::anim_clip::load(name, resolver)?;
    let curves = c
        .curves
        .into_iter()
        .filter(|cv| matches!(cv.kind, Kind::Euler | Kind::Position))
        .map(|cv| {
            let euler = cv.kind == Kind::Euler;
            let mut it = cv.comps.into_iter();
            Curve3 { path: cv.path, euler, ch: 0, comps: [it.next().unwrap_or_default(), it.next().unwrap_or_default(), it.next().unwrap_or_default()] }
        })
        .collect();
    Some(Clip { len: c.stop, looping: c.loop_time, curves })
}

fn load_ctl(asset: &str, prefab: &super::data::Prefab) -> Option<Ctl> {
    let c = crate::anim_ctl::load(asset)?;
    let layer = c.layers.first()?;
    let resolver = crate::anim_clip::hierarchy_resolver(prefab.objects.iter().map(|o| o.path.as_str()));
    let trans = |t: &crate::anim_ctl::Trans| -> Option<Trans> {
        Some(Trans {
            dst: t.dst?,
            conds: t.conds.iter().map(|c| Cond { param: c.param.clone(), mode: c.mode as i32 }).collect(),
            exit: t.exit,
            dur: t.dur,
            fixed: t.fixed,
            to_self: t.to_self,
        })
    };
    let mut channels: Vec<(String, bool)> = Vec::new();
    let mut states = Vec::new();
    for st in &layer.states {
        let clip = match &st.motion {
            Some(crate::anim_ctl::Motion::Clip(name)) => load_clip(name, &resolver).map(|mut c| {
                for cv in &mut c.curves {
                    let key = (cv.path.clone(), cv.euler);
                    cv.ch = channels.iter().position(|x| *x == key).unwrap_or_else(|| {
                        channels.push(key);
                        channels.len() - 1
                    });
                }
                Arc::new(c)
            }),
            _ => None,
        };
        states.push(State { clip, speed: st.speed, trans: st.trans.iter().filter_map(trans).collect() });
    }
    let any = layer.any.iter().filter_map(trans).collect();
    Some(Ctl { states, any, default: layer.default, channels })
}

fn sample(c: &Curve3, t: f32) -> Vec3 {
    Vec3::new(crate::anim_clip::eval(&c.comps[0], t), crate::anim_clip::eval(&c.comps[1], t), crate::anim_clip::eval(&c.comps[2], t))
}

/// Unity-space sampled values of every channel for `state` at state time `t` (None = not animated by its clip).
fn pose(ctl: &Ctl, state: usize, t: f32) -> Vec<Option<Vec3>> {
    let mut out = vec![None; ctl.channels.len()];
    if let Some(clip) = &ctl.states[state].clip {
        let t = if clip.looping && clip.len > 0.0 { t % clip.len } else { t.min(clip.len) };
        for c in &clip.curves {
            out[c.ch] = Some(sample(c, t));
        }
    }
    out
}

/// Exit time reached between normalized times `prev` and `cur`: exitTime < 1 repeats every loop (an exit time of 0
/// therefore fires when the state wraps, after one full play, not on the first frame), >= 1 fires once.
fn exit_crossed(prev: f32, cur: f32, x: f32) -> bool {
    if x >= 1.0 {
        prev < x && cur >= x
    } else {
        (cur - x).floor() > (prev - x).floor()
    }
}

struct Blend {
    from: usize,
    from_t: f32,
    elapsed: f32,
    dur: f32,
}

#[derive(Component)]
pub struct WeaponAnimator {
    ctl: Arc<Ctl>,
    state: usize,
    t: f32,
    /// Normalized time of `state` at the last transition evaluation (exit-time crossing check).
    eval_norm: f32,
    blend: Option<Blend>,
    triggers: Vec<String>,
    /// Animator GameObject path relative to the prefab root ("" = root).
    base: String,
    /// Resolved glb node and its default (bind) local transform, per channel.
    nodes: Vec<Option<(Entity, Transform)>>,
    /// Current local transform of every animated path (relative to the weapon root) in the prefab-JSON convention
    /// (`PrefabObject::local_transform`), so fparms can follow animated Grip parents.
    pub live: HashMap<String, Transform>,
}

#[derive(Component)]
pub struct NoWeaponAnimator;

/// Adds a WeaponAnimator to every weapon whose prefab has an Animator controller.
pub fn attach(mut commands: Commands, q: Query<(Entity, &Item), (Without<WeaponAnimator>, Without<NoWeaponAnimator>)>, mut cache: Local<HashMap<String, Option<Arc<Ctl>>>>) {
    for (e, item) in &q {
        let found = item.def.prefab.first_with("Animator");
        let asset = found.and_then(|(_, a)| a.asset("m_Controller"));
        let ctl = asset.and_then(|a| cache.entry(a.clone()).or_insert_with(|| load_ctl(&a, &item.def.prefab).map(Arc::new)).clone());
        match ctl {
            Some(ctl) => {
                let root = item.def.prefab.objects.first().map(|o| o.path.as_str()).unwrap_or("");
                let base = found.map(|(o, _)| o.path.strip_prefix(root).unwrap_or("").trim_start_matches('/').to_string()).unwrap_or_default();
                let n = ctl.channels.len();
                let state = ctl.default;
                commands.entity(e).insert(WeaponAnimator {
                    ctl,
                    state,
                    t: 0.0,
                    eval_norm: 0.0,
                    blend: None,
                    triggers: Vec::new(),
                    base,
                    nodes: vec![None; n],
                    live: HashMap::new(),
                });
            }
            None => {
                commands.entity(e).insert(NoWeaponAnimator);
            }
        }
    }
}

/// Resolves a prefab path ("Glock/ElbowPivotPoint/.../SM_Glock17_Slide_00") in the weapon's glb instance: the root
/// segment is searched breadth-first below the weapon entity (the glb adds scene wrappers; a deeper node may share the
/// root's name, e.g. DF_Cyst/DF_Cyst), the rest are direct children.
pub fn find_path(root: Entity, path: &str, children: &Query<&Children>, names: &Query<&Name>) -> Option<Entity> {
    let mut segs = path.split('/');
    let first = segs.next()?;
    let rest: Vec<&str> = segs.collect();
    let mut queue = VecDeque::from([root]);
    while let Some(e) = queue.pop_front() {
        if names.get(e).is_ok_and(|n| n.as_str() == first) {
            let walk = rest.iter().try_fold(e, |cur, s| children.get(cur).ok()?.iter().find(|c| names.get(*c).is_ok_and(|n| n.as_str() == *s)));
            if walk.is_some() {
                return walk;
            }
        }
        if let Ok(k) = children.get(e) {
            queue.extend(k.iter());
        }
    }
    None
}

/// Unity-space channel value -> glb-local (AssetRipper X-mirrored) position / rotation.
fn to_glb(euler: bool, v: Vec3) -> (Vec3, Quat) {
    if euler {
        // Unity Euler (ZXY: q = Qy * Qx * Qz) -> quaternion, then X-mirror: (x, -y, -z, w).
        let q = Quat::from_euler(EulerRot::YXZ, v.y.to_radians(), v.x.to_radians(), v.z.to_radians());
        (Vec3::ZERO, Quat::from_xyzw(q.x, -q.y, -q.z, q.w))
    } else {
        (Vec3::new(-v.x, v.y, v.z), Quat::IDENTITY)
    }
}

pub fn update(
    time: Res<Time>,
    mut q: Query<(Entity, &mut WeaponAnimator, &mut Item, Option<&mut MeleeState>)>,
    children: Query<&Children>,
    names: Query<&Name>,
    mut tfs: Query<&mut Transform, Without<Item>>,
) {
    let dt = time.delta_secs();
    let debug_on = std::env::var_os("RESTRAFE_DEBUG_ANIM").is_some();
    for (e, mut a, mut item, ms) in &mut q {
        let a = &mut *a;
        // Unity triggers are flags (set until a transition consumes them), not a queue.
        for t in std::mem::take(&mut item.anim_triggers) {
            if !a.triggers.iter().any(|x| x == t) {
                a.triggers.push(t.to_string());
            }
        }
        if let Some(t) = ms.and_then(|mut m| m.anim_trigger.take()) {
            if !t.is_empty() && !a.triggers.contains(&t) {
                a.triggers.push(t);
            }
        }
        // MeleeWeapon.Update: SetBool(outofhandsAnim, layer == 7) (dropped / not held).
        let ooh = item.def.item.s("outofhandsAnim");
        let held = item.hand != Hand::None;
        let debug = debug_on && held;
        let ctl = a.ctl.clone();

        // Advance the current (and fading-out) state.
        a.t += dt * ctl.states[a.state].speed;
        if let Some(b) = a.blend.as_mut() {
            b.from_t += dt * ctl.states[b.from].speed;
            b.elapsed += dt;
            if b.elapsed >= b.dur {
                a.blend = None;
            }
        }
        // Transitions are not interruptible (m_InterruptionSource ignored): evaluate only outside a cross-fade.
        if a.blend.is_none() {
            let norm = a.t / ctl.len(a.state);
            let prev = a.eval_norm;
            let ok = |tr: &Trans, trig: &[String]| {
                tr.exit.is_none_or(|x| exit_crossed(prev, norm, x))
                    && (tr.exit.is_some() || !tr.conds.is_empty())
                    && tr.conds.iter().all(|c| {
                        let on = if !ooh.is_empty() && c.param == ooh { !held } else { trig.contains(&c.param) };
                        if c.mode == 2 { !on } else { on }
                    })
            };
            let next = ctl.any.iter().filter(|tr| tr.to_self || tr.dst != a.state).chain(ctl.states[a.state].trans.iter()).find(|tr| ok(tr, &a.triggers));
            a.eval_norm = norm;
            if let Some(tr) = next {
                for c in &tr.conds {
                    a.triggers.retain(|t| *t != c.param);
                }
                let dur = if tr.fixed { tr.dur } else { tr.dur * ctl.len(a.state) / ctl.states[a.state].speed.max(1e-4) };
                if debug {
                    println!("ANIM state {} -> {} (blend {dur:.3}s)", a.state, tr.dst);
                }
                a.blend = (dur > 1e-3).then(|| Blend { from: a.state, from_t: a.t, elapsed: 0.0, dur });
                a.state = tr.dst;
                a.t = 0.0;
                a.eval_norm = 0.0;
            }
        }

        let to = pose(&ctl, a.state, a.t);
        let from = a.blend.as_ref().map(|b| (pose(&ctl, b.from, b.from_t), (b.elapsed / b.dur).clamp(0.0, 1.0)));
        let root = item.def.prefab.objects.first().map(|o| o.path.clone()).unwrap_or_default();
        for (ch, (path, euler)) in ctl.channels.iter().enumerate() {
            let rel = if a.base.is_empty() { path.clone() } else { format!("{}/{path}", a.base) };
            if a.nodes[ch].is_none() {
                // Only cache hits: the glb scene may not be instantiated yet.
                a.nodes[ch] = find_path(e, &format!("{root}/{rel}"), &children, &names).and_then(|n| tfs.get(n).ok().map(|tf| (n, *tf)));
            }
            let Some((node, def)) = a.nodes[ch] else { continue };
            let Ok(mut tf) = tfs.get_mut(node) else { continue };
            // Write defaults: channels the clip doesn't animate get the node's bind value.
            let value = |p: &[Option<Vec3>]| p[ch].map(|v| to_glb(*euler, v)).unwrap_or((def.translation, def.rotation));
            let (mut pos, mut rot) = value(&to);
            if let Some((fp, w)) = &from {
                let (fpos, frot) = value(fp);
                pos = fpos.lerp(pos, *w);
                rot = frot.slerp(rot, *w);
            }
            if debug && a.t < 0.05 {
                println!("ANIM {rel} euler={euler} t={:.3} pos={pos:?} rot={rot:?} def={:?}/{:?}", a.t, def.translation, def.rotation);
            }
            // glb (X-mirrored, under a Y-180 root) -> prefab convention (Z-mirrored): conjugate by Y 180.
            let live = a.live.entry(rel.clone()).or_insert_with(|| item.def.prefab.find(&format!("{root}/{rel}")).map(|o| o.local_transform()).unwrap_or_default());
            if *euler {
                tf.rotation = rot;
                live.rotation = Quat::from_xyzw(-rot.x, rot.y, -rot.z, rot.w);
            } else {
                tf.translation = pos;
                live.translation = Vec3::new(-pos.x, pos.y, -pos.z);
            }
        }
    }
}

/// The same Animator for a scene object (map props: Pigeon's SM_A_Pigeon00.controller): bool parameters instead of
/// item triggers, clip paths relative to the entity itself. The root channel (path "") is skipped: these Animators
/// apply root motion, so constant root curves move nothing.
#[derive(Component)]
pub struct NodeAnimator {
    ctl: Arc<Ctl>,
    state: usize,
    t: f32,
    eval_norm: f32,
    blend: Option<Blend>,
    pub bools: HashMap<String, bool>,
    nodes: Vec<Option<(Entity, Transform)>>,
}

impl NodeAnimator {
    /// `asset`: the controller ("AnimatorController/X.controller"); `prefab`: the hierarchy its clips animate.
    pub fn load(asset: &str, prefab: &super::data::Prefab) -> Option<NodeAnimator> {
        static CACHE: OnceLock<std::sync::Mutex<HashMap<String, Option<Arc<Ctl>>>>> = OnceLock::new();
        let ctl = CACHE.get_or_init(Default::default).lock().ok()?.entry(asset.to_string()).or_insert_with(|| load_ctl(asset, prefab).map(Arc::new)).clone()?;
        let n = ctl.channels.len();
        Some(NodeAnimator { state: ctl.default, ctl, t: 0.0, eval_norm: 0.0, blend: None, bools: HashMap::new(), nodes: vec![None; n] })
    }
}

pub fn node_animator_update(time: Res<Time>, mut q: Query<(Entity, &mut NodeAnimator)>, children: Query<&Children>, names: Query<&Name>, mut tfs: Query<&mut Transform, Without<NodeAnimator>>) {
    let dt = time.delta_secs();
    for (e, mut a) in &mut q {
        let a = &mut *a;
        let ctl = a.ctl.clone();
        a.t += dt * ctl.states[a.state].speed;
        if let Some(b) = a.blend.as_mut() {
            b.from_t += dt * ctl.states[b.from].speed;
            b.elapsed += dt;
            if b.elapsed >= b.dur {
                a.blend = None;
            }
        }
        if a.blend.is_none() {
            let norm = a.t / ctl.len(a.state);
            let prev = a.eval_norm;
            let bools = &a.bools;
            let ok = |tr: &Trans| {
                tr.exit.is_none_or(|x| exit_crossed(prev, norm, x))
                    && (tr.exit.is_some() || !tr.conds.is_empty())
                    && tr.conds.iter().all(|c| {
                        let on = bools.get(&c.param).copied().unwrap_or(false);
                        if c.mode == 2 { !on } else { on }
                    })
            };
            let next = ctl.any.iter().filter(|tr| tr.to_self || tr.dst != a.state).chain(ctl.states[a.state].trans.iter()).find(|tr| ok(tr));
            a.eval_norm = norm;
            if let Some(tr) = next {
                let dur = if tr.fixed { tr.dur } else { tr.dur * ctl.len(a.state) / ctl.states[a.state].speed.max(1e-4) };
                a.blend = (dur > 1e-3).then(|| Blend { from: a.state, from_t: a.t, elapsed: 0.0, dur });
                a.state = tr.dst;
                a.t = 0.0;
                a.eval_norm = 0.0;
            }
        }
        let to = pose(&ctl, a.state, a.t);
        let from = a.blend.as_ref().map(|b| (pose(&ctl, b.from, b.from_t), (b.elapsed / b.dur).clamp(0.0, 1.0)));
        for (ch, (path, euler)) in ctl.channels.iter().enumerate() {
            if path.is_empty() {
                continue;
            }
            if a.nodes[ch].is_none() {
                let walk = path.split('/').try_fold(e, |cur, s| children.get(cur).ok()?.iter().find(|c| names.get(*c).is_ok_and(|n| n.as_str() == s)));
                a.nodes[ch] = walk.and_then(|n| tfs.get(n).ok().map(|tf| (n, *tf)));
            }
            let Some((node, def)) = a.nodes[ch] else { continue };
            let Ok(mut tf) = tfs.get_mut(node) else { continue };
            let value = |p: &[Option<Vec3>]| p[ch].map(|v| to_glb(*euler, v)).unwrap_or((def.translation, def.rotation));
            let (mut pos, mut rot) = value(&to);
            if let Some((fp, w)) = &from {
                let (fpos, frot) = value(fp);
                pos = fpos.lerp(pos, *w);
                rot = frot.slerp(rot, *w);
            }
            if *euler {
                tf.rotation = rot;
            } else {
                tf.translation = pos;
            }
        }
    }
}
