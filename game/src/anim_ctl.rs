//! AnimatorControllers read from the installation. Player builds keep the baked `ControllerConstant` (layers,
//! state machines, states, blend trees, transitions and parameters by hash) plus the controller's name table
//! (`m_TOS`) and clip list; this turns it back into named layers / states / transitions the game code runs.

use crate::owned::Owned;
use bevy::math::Vec2;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Cond {
    /// Unity AnimatorConditionMode: 1 If, 2 IfNot, 3 Greater, 4 Less, 6 Equals, 7 NotEqual
    pub mode: i64,
    pub param: String,
    pub threshold: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trans {
    /// destination state index in the same state machine (None: exit / another machine)
    pub dst: Option<usize>,
    pub conds: Vec<Cond>,
    pub dur: f32,
    pub offset: f32,
    pub exit: Option<f32>,
    pub fixed: bool,
    pub to_self: bool,
    pub interrupt: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Motion {
    /// AnimationClip name (as anim_clip::load takes it)
    Clip(String),
    Tree(Tree),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tree {
    /// 0 = 1D, 1..3 = 2D (simple directional / freeform directional / freeform cartesian), 4 = direct
    pub blend_type: i64,
    pub px: String,
    pub py: String,
    pub kids: Vec<Kid>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Kid {
    pub motion: Option<Motion>,
    pub thr: f32,
    pub pos: Vec2,
    /// playback speed of the child (the editor's m_TimeScale; baked as the leaf's m_Duration = 1 / time scale)
    pub ts: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub name: String,
    pub speed: f32,
    pub motion: Option<Motion>,
    pub trans: Vec<Trans>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub name: String,
    pub weight: f32,
    pub additive: bool,
    pub states: Vec<State>,
    pub any: Vec<Trans>,
    pub default: usize,
}

#[derive(Clone, Debug)]
pub struct Controller {
    pub layers: Vec<Layer>,
}

fn num(v: &Value) -> f32 {
    v.as_f64().unwrap_or(0.0) as f32
}

fn flag(v: &Value) -> bool {
    v.as_u64() == Some(1) || v.as_bool() == Some(true)
}

/// The controller an exported file stem / asset path names ("SK_Aboubi_00", "AnimatorController/X.controller").
pub fn load(name: &str) -> Option<Controller> {
    let stem = name.rsplit('/').next().unwrap_or(name).trim_end_matches(".controller");
    let o = Owned::global()?;
    let (node, pid) = o.find_named(91, stem)?;
    let (_, f) = crate::prefab_json::script_object(o, o.assemblies()?, node, pid).map_err(|e| bevy::log::warn!("controller {stem}: {e}")).ok()?;
    from_fields(stem, &f)
}

fn from_fields(name: &str, f: &serde_json::Map<String, Value>) -> Option<Controller> {
    let tos: HashMap<u32, String> = f
        .get("m_TOS")?
        .as_array()?
        .iter()
        .filter_map(|p| Some((p.get("first")?.as_u64()? as u32, p.get("second")?.as_str()?.to_string())))
        .collect();
    let named = |v: &Value| v.as_u64().and_then(|h| tos.get(&(h as u32)).cloned()).unwrap_or_default();
    let clips: Vec<String> = f
        .get("m_AnimationClips")?
        .as_array()?
        .iter()
        .map(|c| c.as_str().unwrap_or("").rsplit('/').next().unwrap_or("").trim_end_matches(".anim").to_string())
        .collect();
    let c = f.get("m_Controller")?;
    let machines = c["m_StateMachineArray"].as_array()?;

    let trans = |t: &Value, n_states: usize| -> Trans {
        let t = &t["data"];
        let dst = t["m_DestinationState"].as_u64().map(|d| d as usize).filter(|&d| d < n_states);
        Trans {
            dst,
            conds: t["m_ConditionConstantArray"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|cc| {
                    let cc = &cc["data"];
                    Cond { mode: cc["m_ConditionMode"].as_i64().unwrap_or(0), param: named(&cc["m_EventID"]), threshold: num(&cc["m_EventThreshold"]) }
                })
                .collect(),
            dur: num(&t["m_TransitionDuration"]),
            offset: num(&t["m_TransitionOffset"]),
            exit: flag(&t["m_HasExitTime"]).then(|| num(&t["m_ExitTime"])),
            fixed: flag(&t["m_HasFixedDuration"]),
            to_self: flag(&t["m_CanTransitionToSelf"]),
            interrupt: t["m_InterruptionSource"].as_i64().unwrap_or(0),
        }
    };
    // a blend tree node: a leaf (clip) or a tree of child nodes
    fn node(nodes: &[Value], i: usize, clips: &[String], named: &dyn Fn(&Value) -> String) -> Option<Motion> {
        let n = &nodes.get(i)?["data"];
        let kids_idx: Vec<usize> = n["m_ChildIndices"].as_array().into_iter().flatten().filter_map(|x| x.as_u64()).map(|x| x as usize).collect();
        if kids_idx.is_empty() {
            let clip = n["m_ClipID"].as_u64().filter(|&c| c != u32::MAX as u64)?;
            return clips.get(clip as usize).map(|c| Motion::Clip(c.clone()));
        }
        let thr: Vec<f32> = n["m_Blend1dData"]["data"]["m_ChildThresholdArray"].as_array().into_iter().flatten().map(num).collect();
        let pos: Vec<Vec2> = n["m_Blend2dData"]["data"]["m_ChildPositionArray"].as_array().into_iter().flatten().map(|p| Vec2::new(num(&p["x"]), num(&p["y"]))).collect();
        let kids = kids_idx
            .iter()
            .enumerate()
            .map(|(k, &ci)| {
                let d = nodes.get(ci).map(|c| num(&c["data"]["m_Duration"])).unwrap_or(1.0);
                Kid { motion: node(nodes, ci, clips, named), thr: thr.get(k).copied().unwrap_or(0.0), pos: pos.get(k).copied().unwrap_or(Vec2::ZERO), ts: if d > 0.0 { 1.0 / d } else { 1.0 } }
            })
            .collect();
        let y = n["m_BlendEventYID"].as_u64().filter(|&h| h != u32::MAX as u64).map(|h| named(&Value::from(h))).unwrap_or_default();
        Some(Motion::Tree(Tree { blend_type: n["m_BlendType"].as_i64().unwrap_or(0), px: named(&n["m_BlendEventID"]), py: y, kids }))
    }

    let mut layers = Vec::new();
    for l in c["m_LayerArray"].as_array()? {
        let l = &l["data"];
        let sm = &machines.get(l["m_StateMachineIndex"].as_u64()? as usize)?["data"];
        // synchronized layers pick their motion set from the states of the machine they follow
        let motion_set = l["m_StateMachineSynchronizedLayerIndex"].as_u64().unwrap_or(0) as usize;
        let states_v = sm["m_StateConstantArray"].as_array()?;
        let n_states = states_v.len();
        let states = states_v
            .iter()
            .map(|s| {
                let s = &s["data"];
                let tree_idx = s["m_BlendTreeConstantIndexArray"].as_array().and_then(|a| a.get(motion_set)).and_then(|x| x.as_i64()).unwrap_or(-1);
                let motion = (tree_idx >= 0)
                    .then(|| s["m_BlendTreeConstantArray"].as_array().and_then(|a| a.get(tree_idx as usize)))
                    .flatten()
                    .and_then(|t| t["data"]["m_NodeArray"].as_array().and_then(|nodes| node(nodes, 0, &clips, &named)));
                State {
                    name: named(&s["m_NameID"]),
                    speed: num(&s["m_Speed"]),
                    motion,
                    trans: s["m_TransitionConstantArray"].as_array().into_iter().flatten().map(|t| trans(t, n_states)).collect(),
                }
            })
            .collect();
        layers.push(Layer {
            name: named(&l["m_Binding"]),
            weight: num(&l["m_DefaultWeight"]),
            additive: l["(int&)m_LayerBlendingMode"].as_i64() == Some(1),
            states,
            any: sm["m_AnyStateTransitionConstantArray"].as_array().into_iter().flatten().map(|t| trans(t, n_states)).collect(),
            default: sm["m_DefaultState"].as_u64().unwrap_or(0) as usize,
        });
    }
    let _ = name;
    Some(Controller { layers })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../ripped/ExportedProject/Assets");

    /// The old exported controller YAML in the same structure (the reference).
    fn from_yaml(path: &str, clip_guids: &HashMap<String, String>) -> Controller {
        use serde_yaml::Value as Y;
        let text = std::fs::read_to_string(path).unwrap();
        let mut docs: HashMap<i64, Y> = HashMap::new();
        let mut root = Y::Null;
        for part in text.split("\n--- !u!").skip(1) {
            let (head, body) = part.split_once('\n').unwrap();
            let id: i64 = head.split('&').nth(1).and_then(|s| s.split_whitespace().next()).and_then(|s| s.parse().ok()).unwrap_or(0);
            let v: Y = serde_yaml::from_str(body).unwrap_or(Y::Null);
            let inner = v.as_mapping().and_then(|m| m.values().next()).cloned().unwrap_or(Y::Null);
            if head.starts_with("91 ") {
                root = inner.clone();
            }
            docs.insert(id, inner);
        }
        let fid = |v: &Y| v["fileID"].as_i64().unwrap_or(0);
        let f = |v: &Y| v.as_f64().unwrap_or(0.0) as f32;
        fn motion(v: &Y, docs: &HashMap<i64, Y>, guids: &HashMap<String, String>) -> Option<Motion> {
            let id = v["fileID"].as_i64().unwrap_or(0);
            if id == 0 {
                return None;
            }
            if let Some(g) = v["guid"].as_str() {
                return guids.get(g).map(|n| Motion::Clip(n.clone()));
            }
            let t = docs.get(&id)?;
            let kids = t["m_Childs"]
                .as_sequence()?
                .iter()
                .map(|c| Kid {
                    motion: motion(&c["m_Motion"], docs, guids),
                    thr: c["m_Threshold"].as_f64().unwrap_or(0.0) as f32,
                    pos: Vec2::new(c["m_Position"]["x"].as_f64().unwrap_or(0.0) as f32, c["m_Position"]["y"].as_f64().unwrap_or(0.0) as f32),
                    ts: c["m_TimeScale"].as_f64().unwrap_or(1.0) as f32,
                })
                .collect();
            Some(Motion::Tree(Tree { blend_type: t["m_BlendType"].as_i64().unwrap_or(0), px: t["m_BlendParameter"].as_str().unwrap_or("").to_string(), py: t["m_BlendParameterY"].as_str().unwrap_or("").to_string(), kids }))
        }
        let mut layers = Vec::new();
        for l in root["m_AnimatorLayers"].as_sequence().cloned().unwrap_or_default() {
            let sm = &docs[&fid(&l["m_StateMachine"])];
            let ids: Vec<i64> = sm["m_ChildStates"].as_sequence().map(|s| s.iter().map(|c| fid(&c["m_State"])).collect()).unwrap_or_default();
            let idx: HashMap<i64, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
            let tr = |id: i64| -> Trans {
                let t = &docs[&id];
                Trans {
                    dst: idx.get(&fid(&t["m_DstState"])).copied(),
                    conds: t["m_Conditions"].as_sequence().into_iter().flatten().map(|c| Cond { mode: c["m_ConditionMode"].as_i64().unwrap_or(0), param: c["m_ConditionEvent"].as_str().unwrap_or("").to_string(), threshold: f(&c["m_EventTreshold"]) }).collect(),
                    dur: f(&t["m_TransitionDuration"]),
                    offset: f(&t["m_TransitionOffset"]),
                    exit: (t["m_HasExitTime"].as_i64() == Some(1)).then(|| f(&t["m_ExitTime"])),
                    fixed: t["m_HasFixedDuration"].as_i64() == Some(1),
                    to_self: t["m_CanTransitionToSelf"].as_i64() == Some(1),
                    interrupt: t["m_InterruptionSource"].as_i64().unwrap_or(0),
                }
            };
            let states = ids
                .iter()
                .map(|id| {
                    let s = &docs[id];
                    State { name: s["m_Name"].as_str().unwrap_or("").to_string(), speed: f(&s["m_Speed"]), motion: motion(&s["m_Motion"], &docs, clip_guids), trans: s["m_Transitions"].as_sequence().into_iter().flatten().map(|t| tr(fid(t))).collect() }
                })
                .collect();
            layers.push(Layer {
                name: l["m_Name"].as_str().unwrap_or("").to_string(),
                weight: f(&l["m_DefaultWeight"]),
                additive: l["m_BlendingMode"].as_i64() == Some(1),
                states,
                any: sm["m_AnyStateTransitions"].as_sequence().into_iter().flatten().map(|t| tr(fid(t))).collect(),
                default: idx.get(&fid(&sm["m_DefaultState"])).copied().unwrap_or(0),
            });
        }
        Controller { layers }
    }

    /// Every controller read in place equals the old export: layers, states, blend trees, transitions, conditions.
    #[test]
    fn controllers_match_export() {
        if Owned::global().is_none() {
            return;
        }
        let mut guids = HashMap::new();
        for e in std::fs::read_dir(format!("{ASSETS}/AnimationClip")).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if let Some(stem) = n.strip_suffix(".anim.meta") {
                if let Some(g) = std::fs::read_to_string(e.path()).ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("guid: ").map(str::to_string))) {
                    guids.insert(g, stem.to_string());
                }
            }
        }
        let (mut n, mut states) = (0, 0);
        for e in std::fs::read_dir(format!("{ASSETS}/AnimatorController")).into_iter().flatten().flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            let Some(stem) = file.strip_suffix(".controller") else { continue };
            let want = from_yaml(&e.path().to_string_lossy(), &guids);
            let got = load(stem).unwrap_or_else(|| panic!("{stem}: not read in place"));
            assert_eq!(want.layers.len(), got.layers.len(), "{stem}: layers");
            for (a, b) in want.layers.iter().zip(&got.layers) {
                assert_eq!(a.name, b.name, "{stem}: layer name");
                assert_eq!((a.additive, a.default, a.states.len()), (b.additive, b.default, b.states.len()), "{stem} {}: layer", a.name);
                assert!((a.weight - b.weight).abs() < 1e-5, "{stem} {}: weight", a.name);
                assert_eq!(a.any, b.any, "{stem} {}: any-state transitions", a.name);
                for (sa, sb) in a.states.iter().zip(&b.states) {
                    assert_eq!(sa, sb, "{stem} {}: state", a.name);
                    states += 1;
                }
            }
            n += 1;
        }
        println!("controllers: {n} controllers, {states} states match");
    }
}
