//! AnimationClips read from the installation. Player builds keep a clip's curves packed in
//! `m_MuscleClip.m_Clip` (streamed Hermite segments, densely sampled frames and constants) with hashed bindings
//! (`m_ClipBindingConstant`); this rebuilds the editor view the game code works with: per binding a path, a kind
//! (position / rotation / scale / Euler / float attribute / humanoid muscle) and keyframes with tangents.
//!
//! Streamed keys store the cubic of the segment that follows them (`value(u) = a u^3 + b u^2 + c u + d`): a key's
//! value is `d`, its out tangent `c`, and the next key's in tangent the cubic's slope at the segment end
//! (`3 a dt^2 + 2 b dt + c`), so the Hermite keys reproduce the stored curve exactly. A segment with
//! `a = b = c = 0` that ends on a different value is stepped (out tangent +inf, as Unity's editor stores it).
//! Dense curves are linear between samples; constant curves are flat.

use crate::owned::Owned;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub t: f32,
    pub v: f32,
    pub in_s: f32,
    pub out_s: f32,
}

/// Value of a key list at `t` (Unity's Hermite evaluation, clamped outside the keys; +inf tangents step).
pub fn eval(keys: &[Key], t: f32) -> f32 {
    let Some(first) = keys.first() else { return 0.0 };
    if t <= first.t {
        return first.v;
    }
    let last = keys[keys.len() - 1];
    if t >= last.t {
        return last.v;
    }
    let i = keys.partition_point(|k| k.t <= t).max(1) - 1;
    let (a, b) = (keys[i], keys[i + 1]);
    let dt = b.t - a.t;
    if dt <= 0.0 {
        return b.v;
    }
    if !a.out_s.is_finite() || !b.in_s.is_finite() {
        return a.v;
    }
    let u = (t - a.t) / dt;
    let (m0, m1) = (a.out_s * dt, b.in_s * dt);
    let (u2, u3) = (u * u, u * u * u);
    (2.0 * u3 - 3.0 * u2 + 1.0) * a.v + (u3 - 2.0 * u2 + u) * m0 + (-2.0 * u3 + 3.0 * u2) * b.v + (u3 - u2) * m1
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Position,
    /// quaternion x, y, z, w
    Rotation,
    Scale,
    /// localEulerAnglesRaw x, y, z (degrees)
    Euler,
    /// a float property: its attribute name (resolved through the name tables, else "#<hash>") and class id
    Float { attribute: String, class_id: i32 },
    /// a humanoid curve (root / IK goal / muscle) by Unity's clip index, with its name ("RootT.x", "Left Arm Down-Up")
    Muscle { index: u32, name: String },
}

#[derive(Clone, Debug)]
pub struct Curve {
    /// transform path relative to the animator ("" = the animator itself; "#<hash>" when the name is unknown)
    pub path: String,
    pub kind: Kind,
    /// one key list per component (3 for vectors, 4 for quaternions, 1 for floats)
    pub comps: Vec<Vec<Key>>,
}

#[derive(Clone, Debug)]
pub struct Clip {
    pub start: f32,
    pub stop: f32,
    pub loop_time: bool,
    pub curves: Vec<Curve>,
}

/// Unity's humanoid clip channels: MotionT/Q, RootT/Q, the four IK goals (T xyz, Q xyzw), then the muscles in
/// HumanTrait.MuscleName order.
pub fn muscle_name(index: u32) -> Option<String> {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    let names = NAMES.get_or_init(|| {
        let mut v: Vec<String> = Vec::new();
        for p in ["Motion", "Root", "LeftFoot", "RightFoot", "LeftHand", "RightHand"] {
            for c in ["x", "y", "z"] {
                v.push(format!("{p}T.{c}"));
            }
            for c in ["x", "y", "z", "w"] {
                v.push(format!("{p}Q.{c}"));
            }
        }
        let mut m = |s: &str| v.push(s.to_string());
        for s in ["Spine", "Chest", "UpperChest"] {
            m(&format!("{s} Front-Back"));
            m(&format!("{s} Left-Right"));
            m(&format!("{s} Twist Left-Right"));
        }
        for s in ["Neck", "Head"] {
            m(&format!("{s} Nod Down-Up"));
            m(&format!("{s} Tilt Left-Right"));
            m(&format!("{s} Turn Left-Right"));
        }
        for s in ["Left", "Right"] {
            m(&format!("{s} Eye Down-Up"));
            m(&format!("{s} Eye In-Out"));
        }
        m("Jaw Close");
        m("Jaw Left-Right");
        for s in ["Left", "Right"] {
            for x in ["Upper Leg Front-Back", "Upper Leg In-Out", "Upper Leg Twist In-Out", "Lower Leg Stretch", "Lower Leg Twist In-Out", "Foot Up-Down", "Foot Twist In-Out", "Toes Up-Down"] {
                m(&format!("{s} {x}"));
            }
        }
        for s in ["Left", "Right"] {
            for x in ["Shoulder Down-Up", "Shoulder Front-Back", "Arm Down-Up", "Arm Front-Back", "Arm Twist In-Out", "Forearm Stretch", "Forearm Twist In-Out", "Hand Down-Up", "Hand In-Out"] {
                m(&format!("{s} {x}"));
            }
        }
        for s in ["LeftHand", "RightHand"] {
            for f in ["Thumb", "Index", "Middle", "Ring", "Little"] {
                for x in ["1 Stretched", "Spread", "2 Stretched", "3 Stretched"] {
                    m(&format!("{s}.{f}.{x}"));
                }
            }
        }
        v
    });
    names.get(index as usize).cloned()
}

/// hash -> name of every Avatar's and AnimatorController's `m_TOS` table (transform paths, attribute names ...).
fn name_table(o: &Owned) -> &'static HashMap<u32, String> {
    static T: OnceLock<HashMap<u32, String>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        let Some(asm) = o.assemblies() else { return m };
        let b = &o.0.bundle;
        for (i, n) in b.nodes.iter().enumerate() {
            let Ok(sf) = crate::unity_fs::SerializedFile::open(b, n) else { continue };
            for obj in sf.objects.iter().filter(|x| x.class_id == 90 || x.class_id == 91) {
                let Ok((_, f)) = crate::prefab_json::script_object(o, asm, i, obj.path_id) else { continue };
                for pair in f.get("m_TOS").and_then(|v| v.as_array()).into_iter().flatten() {
                    let (h, s) = match pair {
                        Value::Array(_) => (pair.get(0), pair.get(1)),
                        _ => (pair.get("first"), pair.get("second")),
                    };
                    if let (Some(h), Some(s)) = (h.and_then(|x| x.as_u64()), s.and_then(|x| x.as_str())) {
                        m.insert(h as u32, s.to_string());
                    }
                }
            }
        }
        m
    })
}

/// A transform path or attribute name by its hash (`extra` first, then the name tables).
fn name_of(o: &Owned, hash: u32, extra: &dyn Fn(u32) -> Option<String>) -> String {
    if hash == 0 {
        return String::new();
    }
    extra(hash).or_else(|| name_table(o).get(&hash).cloned()).unwrap_or_else(|| format!("#{hash}"))
}

/// Loads the clip of that name. `paths` resolves path / attribute hashes the name tables do not hold: the CRC32 of
/// the animated hierarchy's relative paths (see [`path_resolver`]) or of attribute names.
pub fn load(name: &str, paths: &dyn Fn(u32) -> Option<String>) -> Option<Clip> {
    let o = Owned::global()?;
    let (node, pid) = o.find_named(74, name)?;
    let (_, f) = crate::prefab_json::script_object(o, o.assemblies()?, node, pid).map_err(|e| bevy::log::warn!("clip {name}: {e}")).ok()?;
    from_fields(o, name, &f, paths)
}

#[cfg(test)]
/// A resolver for [`load`] over candidate names (relative transform paths, attribute names): CRC32 -> name.
pub fn path_resolver<'a>(names: impl IntoIterator<Item = &'a str>) -> impl Fn(u32) -> Option<String> {
    let m: HashMap<u32, String> = names.into_iter().map(|n| (crate::humanoid::crc32(n), n.to_string())).collect();
    move |h| m.get(&h).cloned()
}

/// A resolver over every relative sub-path of a hierarchy's object paths ("A/B/C" gives "C", "B/C" and "A/B/C"), so
/// clips of an Animator anywhere in it resolve.
pub fn hierarchy_resolver<'a>(object_paths: impl IntoIterator<Item = &'a str>) -> impl Fn(u32) -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    for p in object_paths {
        let parts: Vec<&str> = p.split('/').collect();
        for i in 0..parts.len() {
            names.push(parts[i..].join("/"));
        }
    }
    let m: HashMap<u32, String> = names.into_iter().map(|n| (crate::humanoid::crc32(&n), n)).collect();
    move |h| m.get(&h).cloned()
}

fn num(v: &Value) -> f32 {
    v.as_f64().unwrap_or(0.0) as f32
}

/// Legacy clips (the `Animation` component) keep the editor curves themselves.
fn legacy_curves(f: &serde_json::Map<String, Value>) -> Vec<Curve> {
    let key = |k: &Value, c: Option<&str>| {
        let g = |v: &Value| num(match c {
            Some(c) => &v[c],
            None => v,
        });
        Key { t: num(&k["time"]), v: g(&k["value"]), in_s: g(&k["inSlope"]), out_s: g(&k["outSlope"]) }
    };
    let mut out = Vec::new();
    for (sec, kind, comps) in [("m_PositionCurves", Kind::Position, &["x", "y", "z"][..]), ("m_EulerCurves", Kind::Euler, &["x", "y", "z"][..]), ("m_ScaleCurves", Kind::Scale, &["x", "y", "z"][..]), ("m_RotationCurves", Kind::Rotation, &["x", "y", "z", "w"][..])] {
        for c in f.get(sec).and_then(|v| v.as_array()).into_iter().flatten() {
            let keys = c["curve"]["m_Curve"].as_array().cloned().unwrap_or_default();
            out.push(Curve { path: c["path"].as_str().unwrap_or("").to_string(), kind: kind.clone(), comps: comps.iter().map(|comp| keys.iter().map(|k| key(k, Some(comp))).collect()).collect() });
        }
    }
    for c in f.get("m_FloatCurves").and_then(|v| v.as_array()).into_iter().flatten() {
        let keys = c["curve"]["m_Curve"].as_array().cloned().unwrap_or_default();
        out.push(Curve {
            path: c["path"].as_str().unwrap_or("").to_string(),
            kind: Kind::Float { attribute: c["attribute"].as_str().unwrap_or("").to_string(), class_id: c["classID"].as_i64().unwrap_or(0) as i32 },
            comps: vec![keys.iter().map(|k| key(k, None)).collect()],
        });
    }
    out
}

fn from_fields(o: &Owned, name: &str, f: &serde_json::Map<String, Value>, paths: &dyn Fn(u32) -> Option<String>) -> Option<Clip> {
    if f.get("m_Legacy").is_some_and(|v| v.as_u64() == Some(1) || v.as_bool() == Some(true)) {
        let curves = legacy_curves(f);
        let stop = curves.iter().flat_map(|c| c.comps.iter()).filter_map(|k| k.last()).map(|k| k.t).fold(0.0f32, f32::max);
        // legacy clips have no muscle clip settings: they loop by m_WrapMode (2 = Loop)
        let _ = name;
        return Some(Clip { start: 0.0, stop, loop_time: f.get("m_WrapMode").and_then(|v| v.as_i64()) == Some(2), curves });
    }
    let mc = f.get("m_MuscleClip")?;
    let data = &mc["m_Clip"]["data"];
    let (start, stop) = (num(&mc["m_StartTime"]), num(&mc["m_StopTime"]));

    // --- every float curve of the clip, in Unity's order: streamed, dense, constant ---
    let mut curves: Vec<Vec<Key>> = Vec::new();
    // streamed: frames of (time, key count, keys of (curve index, a, b, c, d))
    let s = &data["m_StreamedClip"];
    let n_streamed = s["curveCount"].as_u64().unwrap_or(0) as usize;
    let words: Vec<u32> = s["data"].as_array().map(|a| a.iter().map(|x| x.as_u64().unwrap_or(0) as u32).collect()).unwrap_or_default();
    let mut segs: Vec<Vec<(f32, [f32; 4])>> = vec![Vec::new(); n_streamed];
    let mut p = 0;
    while p + 1 < words.len() {
        let time = f32::from_bits(words[p]);
        let count = words[p + 1] as usize;
        p += 2;
        for _ in 0..count {
            if p + 5 > words.len() {
                break;
            }
            let idx = words[p] as usize;
            let c = [f32::from_bits(words[p + 1]), f32::from_bits(words[p + 2]), f32::from_bits(words[p + 3]), f32::from_bits(words[p + 4])];
            p += 5;
            // the dummy frames at -/+ FLT_MAX only carry the curves' tails
            if time.abs() < 1e30 {
                if let Some(v) = segs.get_mut(idx) {
                    v.push((time, c));
                }
            }
        }
    }
    for seg in segs {
        let mut keys: Vec<Key> = Vec::with_capacity(seg.len());
        for (i, &(t, [a, b, c, d])) in seg.iter().enumerate() {
            let in_s = match i.checked_sub(1).map(|j| seg[j]) {
                Some((pt, [pa, pb, pc, _])) => {
                    let dt = t - pt;
                    3.0 * pa * dt * dt + 2.0 * pb * dt + pc
                }
                None => c,
            };
            let stepped = a == 0.0 && b == 0.0 && c == 0.0 && seg.get(i + 1).is_some_and(|n| n.1[3] != d);
            keys.push(Key { t, v: d, in_s, out_s: if stepped { f32::INFINITY } else { c } });
        }
        curves.push(keys);
    }
    // dense: frameCount x curveCount samples from m_BeginTime at m_SampleRate, linear in between
    let dn = &data["m_DenseClip"];
    let (n_dense, frames) = (dn["m_CurveCount"].as_u64().unwrap_or(0) as usize, dn["m_FrameCount"].as_u64().unwrap_or(0) as usize);
    let (begin, rate) = (num(&dn["m_BeginTime"]), num(&dn["m_SampleRate"]).max(1e-6));
    let samples: Vec<f32> = dn["m_SampleArray"].as_array().map(|a| a.iter().map(num).collect()).unwrap_or_default();
    for c in 0..n_dense {
        let vals: Vec<(f32, f32)> = (0..frames).filter_map(|i| samples.get(i * n_dense + c).map(|&v| (begin + i as f32 / rate, v))).collect();
        let keys = (0..vals.len())
            .map(|i| {
                let slope = |a: (f32, f32), b: (f32, f32)| (b.1 - a.1) / (b.0 - a.0).max(1e-6);
                let in_s = if i > 0 { slope(vals[i - 1], vals[i]) } else { 0.0 };
                let out_s = if i + 1 < vals.len() { slope(vals[i], vals[i + 1]) } else { 0.0 };
                Key { t: vals[i].0, v: vals[i].1, in_s, out_s }
            })
            .collect();
        curves.push(keys);
    }
    // constant: one value over the clip
    for v in data["m_ConstantClip"]["data"].as_array().into_iter().flatten() {
        let v = num(v);
        curves.push(vec![Key { t: start, v, in_s: 0.0, out_s: 0.0 }, Key { t: stop, v, in_s: 0.0, out_s: 0.0 }]);
    }

    // --- bindings consume the curves in order ---
    let mut out = Vec::new();
    let mut next = 0usize;
    for b in f.get("m_ClipBindingConstant")?["genericBindings"].as_array()? {
        if b["isPPtrCurve"].as_u64() == Some(1) {
            continue;
        }
        let (type_id, attr, custom) = (b["typeID"].as_i64().unwrap_or(0) as i32, b["attribute"].as_u64().unwrap_or(0) as u32, b["customType"].as_u64().unwrap_or(0));
        let path_hash = b["path"].as_u64().unwrap_or(0) as u32;
        let (kind, n) = match (type_id, attr, custom) {
            (4, 1, _) => (Kind::Position, 3),
            (4, 2, _) => (Kind::Rotation, 4),
            (4, 3, _) => (Kind::Scale, 3),
            (4, 4, _) => (Kind::Euler, 3),
            (_, _, 8) => (Kind::Muscle { index: attr, name: muscle_name(attr).unwrap_or_else(|| format!("#{attr}")) }, 1),
            _ => (Kind::Float { attribute: name_of(o, attr, paths), class_id: type_id }, 1),
        };
        let comps: Vec<Vec<Key>> = (0..n).map(|k| curves.get(next + k).cloned().unwrap_or_default()).collect();
        next += n;
        out.push(Curve { path: name_of(o, path_hash, paths), kind, comps });
    }
    let _ = name;
    Some(Clip { start, stop, loop_time: mc["m_LoopTime"].as_u64() == Some(1) || mc["m_LoopTime"].as_bool() == Some(true), curves: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../ripped/ExportedProject/Assets/AnimationClip");

    fn yaml_keys(curve: &serde_yaml::Value, comp: Option<&str>) -> Vec<Key> {
        let g = |v: &serde_yaml::Value| -> f32 {
            let v = match comp {
                Some(c) => &v[c],
                None => v,
            };
            match v {
                serde_yaml::Value::String(s) if s.contains("Infinity") => if s.starts_with('-') { f32::NEG_INFINITY } else { f32::INFINITY },
                v => v.as_f64().unwrap_or(0.0) as f32,
            }
        };
        curve["m_Curve"].as_sequence().into_iter().flatten().map(|k| Key { t: k["time"].as_f64().unwrap_or(0.0) as f32, v: g(&k["value"]), in_s: g(&k["inSlope"]), out_s: g(&k["outSlope"]) }).collect()
    }

    /// Every curve of every old exported clip evaluates the same as the clip decoded in place (60 samples each).
    #[test]
    fn clips_match_export() {
        if !crate::owned::ref_checks("RESTRAFE_CLIP_CHECK") || Owned::global().is_none() {
            return;
        }
        let (mut clips, mut curves, mut worst, mut unnamed) = (0, 0, 0.0f32, 0);
        let mut files: Vec<_> = std::fs::read_dir(DIR).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "anim")).collect();
        files.sort();
        for p in files {
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            let text = std::fs::read_to_string(&p).unwrap();
            let body: String = text.lines().filter(|l| !l.starts_with('%') && !l.starts_with("---")).collect::<Vec<_>>().join("\n");
            let y: serde_yaml::Value = serde_yaml::from_str(&body).unwrap();
            let c = &y["AnimationClip"];
            // names the game resolves from its hierarchies: here the old export's own paths / attributes
            let names: Vec<String> = text.lines().filter_map(|l| l.trim().strip_prefix("path: ").or_else(|| l.trim().strip_prefix("attribute: "))).map(str::to_string).collect();
            let resolver = path_resolver(names.iter().map(String::as_str));
            let clip = load(&stem, &resolver).unwrap_or_else(|| panic!("{stem}: not decoded"));
            let st = &c["m_AnimationClipSettings"];
            let (start, stop) = (st["m_StartTime"].as_f64().unwrap_or(0.0) as f32, st["m_StopTime"].as_f64().unwrap_or(0.0) as f32);
            if c["m_Legacy"].as_i64() != Some(1) {
                assert!((clip.start - start).abs() < 1e-4 && (clip.stop - stop).abs() < 1e-4, "{stem}: times {}..{} vs {start}..{stop}", clip.start, clip.stop);
                assert_eq!(clip.loop_time, st["m_LoopTime"].as_i64() == Some(1), "{stem} loop");
            }
            // values at every stored key time (between keys the old export's tangents were approximations: zero for
            // dense curves, which Unity interpolates linearly)
            let mut check = |want: Vec<Key>, got: &[Key], what: String| {
                for k in &want {
                    let t = k.t;
                    let (a, b) = (k.v, eval(got, t));
                    let d = (a - b).abs() / a.abs().max(1.0);
                    worst = worst.max(d);
                    assert!(d < 2e-3, "{stem} {what} t={t}: {a} vs {b}");
                }
                curves += 1;
            };
            for (sec, kind, comps) in [("m_PositionCurves", Kind::Position, &["x", "y", "z"][..]), ("m_EulerCurves", Kind::Euler, &["x", "y", "z"][..]), ("m_ScaleCurves", Kind::Scale, &["x", "y", "z"][..]), ("m_RotationCurves", Kind::Rotation, &["x", "y", "z", "w"][..])] {
                for yc in c[sec].as_sequence().into_iter().flatten() {
                    let path = yc["path"].as_str().unwrap_or("");
                    let Some(dc) = clip.curves.iter().find(|d| d.kind == kind && d.path == path) else {
                        if clip.curves.iter().any(|d| d.kind == kind && d.path.starts_with('#')) {
                            unnamed += 1;
                            continue;
                        }
                        panic!("{stem}: {sec} {path} missing");
                    };
                    for (k, comp) in comps.iter().enumerate() {
                        check(yaml_keys(&yc["curve"], Some(comp)), &dc.comps[k], format!("{sec} {path}.{comp}"));
                    }
                }
            }
            for yc in c["m_FloatCurves"].as_sequence().into_iter().flatten() {
                let (path, attr) = (yc["path"].as_str().unwrap_or(""), yc["attribute"].as_str().unwrap_or(""));
                let found = clip.curves.iter().find(|d| match &d.kind {
                    Kind::Muscle { name, .. } => name == attr,
                    Kind::Float { attribute, .. } => attribute == attr && d.path == path,
                    _ => false,
                });
                let Some(dc) = found else {
                    if clip.curves.iter().any(|d| matches!(&d.kind, Kind::Float { attribute, .. } if attribute.starts_with('#')) || d.path.starts_with('#')) {
                        unnamed += 1;
                        continue;
                    }
                    panic!("{stem}: float curve {path}:{attr} missing");
                };
                check(yaml_keys(&yc["curve"], None), &dc.comps[0], format!("float {path}:{attr}"));
            }
            clips += 1;
        }
        println!("clips: {clips} clips, {curves} curve components match at every key (worst relative difference {worst:.2e}), {unnamed} curves with unresolved names");
    }
}

#[cfg(test)]
mod debug_tests {
    #[test]
    fn clip_keys_debug() {
        let Ok(spec) = std::env::var("RESTRAFE_CLIP_KEYS") else { return };
        let (clip, attr) = spec.split_once(':').unwrap();
        let c = super::load(clip, &|_| None).unwrap();
        println!("start {} stop {}", c.start, c.stop);
        for cv in &c.curves {
            if let super::Kind::Muscle { name, .. } = &cv.kind {
                if name == attr {
                    for k in cv.comps[0].iter().rev().take(5).collect::<Vec<_>>().into_iter().rev() {
                        println!("{k:?}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod value_dump {
    #[test]
    fn clip_values_dump() {
        let Ok(name) = std::env::var("RESTRAFE_CLIP_DUMP") else { return };
        let c = super::load(&name, &|_| None).unwrap();
        println!("{name}: start {} stop {} loop {}", c.start, c.stop, c.loop_time);
        for cv in &c.curves {
            if let super::Kind::Muscle { name, .. } = &cv.kind {
                let k = &cv.comps[0];
                let v0 = super::eval(k, c.start);
                let v1 = super::eval(k, c.stop);
                if v0.abs() > 1e-4 || v1.abs() > 1e-4 {
                    println!("  {name:32} {v0:8.4} {v1:8.4} keys {}", k.len());
                }
            }
        }
    }
}
