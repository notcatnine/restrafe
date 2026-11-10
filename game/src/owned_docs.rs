//! Unity-YAML-shaped documents built from the installation, for the code that works on scene / prefab / asset
//! documents (the UI importer, the material layer): GameObjects, Transforms and components with their Unity field
//! names and `{fileID}` links; references to objects outside a document set carry `guid: "u:<node>:<path id>"`, a
//! token naming the object in the installation (Unity's built-in resources keep their built-in guid and id).

use crate::owned::Owned;
use serde_yaml::Mapping;
use std::sync::Arc;

/// Unity's `unity_builtin_extra` guid.
pub const BUILTIN_EXTRA: &str = "0000000000000000f000000000000000";
/// Unity's `unity default resources` guid.
pub const DEFAULT_RESOURCES: &str = "0000000000000000e000000000000000";

/// The guid token of an object in the installation.
pub fn token(node: usize, pid: i64) -> String {
    format!("u:{node}:{pid}")
}

/// (bundle node, path id) of a guid token.
pub fn parse_token(s: &str) -> Option<(usize, i64)> {
    let (n, p) = s.strip_prefix("u:")?.split_once(':')?;
    Some((n.parse().ok()?, p.parse().ok()?))
}

/// Built-in id of a Unity built-in resource copied into the build (by class and name): the path id of the object of
/// that name in the installation's own `unity_builtin_extra` (shaders by their parsed name).
pub fn builtin_id(class: i32, name: &str) -> Option<i64> {
    // Unity's fixed ids of the built-ins copied into a build (no longer in its unity_builtin_extra)
    let fixed = match (class, name) {
        (21, "Default-Material") => Some(10303),
        (21, "Default-Line") => Some(10306),
        (21, "Default-ParticleSystem") => Some(10308),
        (21, "Default-Particle") => Some(10301),
        (21, "Sprites-Default") => Some(10754),
        (28, "Default-Particle") => Some(10300),
        (28, "Default-Checker") => Some(10305),
        (28, "Default-ParticleSystem") => Some(10307),
        (28, "Default-Checker-Gray") => Some(10309),
        (213, "Checkmark") => Some(10901),
        (213, "UISprite") => Some(10905),
        (213, "Background") => Some(10907),
        (213, "InputFieldBackground") => Some(10911),
        (213, "Knob") => Some(10913),
        (213, "DropdownArrow") => Some(10915),
        (213, "UIMask") => Some(10917),
        (48, "Standard") => Some(46),
        (48, "Skybox/Cubemap") => Some(103),
        (48, "Skybox/6 Sided") => Some(104),
        (48, "Legacy Shaders/Particles/Additive") => Some(200),
        (48, "Legacy Shaders/Particles/Alpha Blended") => Some(203),
        (48, "Legacy Shaders/Particles/VertexLit Blended") => Some(208),
        (48, "Particles/Standard Surface") => Some(210),
        (48, "Particles/Standard Unlit") => Some(211),
        (48, "Mobile/Skybox") => Some(10700),
        (48, "Mobile/Particles/Additive") => Some(10720),
        (48, "Mobile/Particles/Alpha Blended") => Some(10721),
        (48, "Mobile/Particles/Multiply") => Some(10723),
        (48, "Unlit/Texture") => Some(10752),
        (48, "Unlit/Color") => Some(10755),
        (48, "UI/Lit/Transparent") => Some(10764),
        (48, "UI/Default") => Some(10770),
        (48, "Nature/SpeedTree") => Some(14000),
        _ => None,
    };
    if fixed.is_some() {
        return fixed;
    }
    static T: std::sync::OnceLock<std::collections::HashMap<(i32, String), i64>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        let Some(o) = Owned::global() else { return m };
        let (Some(node), Some(asm)) = (o.builtin_extra_node(), o.assemblies()) else { return m };
        let b = &o.0.bundle;
        let Ok(sf) = crate::unity_fs::SerializedFile::open(b, &b.nodes[node]) else { return m };
        for obj in sf.objects.iter().filter(|x| matches!(x.class_id, 21 | 28 | 48 | 213)) {
            let name = if obj.class_id == 48 {
                crate::prefab_json::shader_name_at(o, asm, node, obj.path_id).ok()
            } else {
                sf.object_data(b, &b.nodes[node], obj).ok().and_then(|d| crate::unity_fs::object_name(&d, sf.big_endian))
            };
            if let Some(n) = name.filter(|n| !n.is_empty()) {
                m.entry((obj.class_id, n)).or_insert(obj.path_id);
            }
        }
        m
    })
    .get(&(class, name.to_string()))
    .copied()
}

fn to_mapping(m: serde_json::Map<String, serde_json::Value>) -> Mapping {
    match serde_yaml::to_value(serde_json::Value::Object(m)) {
        Ok(serde_yaml::Value::Mapping(m)) => m,
        _ => Mapping::new(),
    }
}

pub type Docs = Vec<(i64, u32, Arc<Mapping>)>;

fn hierarchy(o: &Owned, node: usize, roots: &[i64], extra: &[i64]) -> Option<Docs> {
    let docs = crate::prefab_json::raw_docs(o, o.assemblies()?, node, roots, extra).map_err(|e| bevy::log::warn!("documents: {e}")).ok()?;
    Some(docs.into_iter().map(|(id, class, m)| (id, class as u32, Arc::new(to_mapping(m)))).collect())
}

/// Every root of a scene (by scene name, "MainMenu") with its RenderSettings.
pub fn scene(name: &str) -> Option<Docs> {
    let o = Owned::global()?;
    let level = o.scene_level(name)?;
    let b = &o.0.bundle;
    let n = &b.nodes[level];
    let sf = crate::unity_fs::SerializedFile::open(b, n).ok()?;
    let mut roots = Vec::new();
    let mut extra = Vec::new();
    for obj in &sf.objects {
        match obj.class_id {
            4 | 224 => {
                let t = crate::unity_fs::Transform::read(&sf.object_data(b, n, obj).ok()?, sf.big_endian).ok()?;
                if t.father.1 == 0 {
                    roots.push(t.game_object.1);
                }
            }
            104 => extra.push(obj.path_id),
            _ => {}
        }
    }
    hierarchy(o, level, &roots, &extra)
}

/// A prefab (by root GameObject name, as its exported file stem).
pub fn prefab(name: &str) -> Option<Docs> {
    let o = Owned::global()?;
    let (node, go) = o.resolve_prefab(name, "sharedassets")?;
    hierarchy(o, node, &[go], &[])
}

/// An asset's document by guid token: (class, fields).
pub fn asset(token_str: &str) -> Option<(i32, Arc<Mapping>)> {
    let (node, pid) = parse_token(token_str)?;
    asset_at(node, pid)
}

/// An asset's document by (node, path id); cached.
pub fn asset_at(node: usize, pid: i64) -> Option<(i32, Arc<Mapping>)> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<(usize, i64), Option<(i32, Arc<Mapping>)>>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap().get(&(node, pid)) {
        return v.clone();
    }
    let o = Owned::global()?;
    let v = crate::prefab_json::raw_asset(o, o.assemblies()?, node, pid).map_err(|e| bevy::log::warn!("asset {node}:{pid}: {e}")).ok().map(|(c, m)| (c, Arc::new(to_mapping(m))));
    cache.lock().unwrap().insert((node, pid), v.clone());
    v
}

/// The asset path an old export gave a token's object ("Material/<name>.mat", "Shader/<name>.shader" ...), for code
/// that recognises assets by file name.
pub fn path(token_str: &str) -> Option<String> {
    let (node, pid) = parse_token(token_str)?;
    let o = Owned::global()?;
    let b = &o.0.bundle;
    let n = &b.nodes[node];
    let sf = crate::unity_fs::SerializedFile::open(b, n).ok()?;
    let obj = sf.objects.iter().find(|x| x.path_id == pid)?;
    let d = sf.object_data(b, n, obj).ok()?;
    let name = match obj.class_id {
        48 => crate::prefab_json::shader_name_at(o, o.assemblies()?, node, pid).ok()?.replace('/', "_"),
        114 => {
            let mut r = crate::unity_fs::ObjReader::new(&d, sf.big_endian);
            r.pptr().ok()?;
            r.bool().ok()?;
            r.align();
            r.pptr().ok()?;
            r.string().ok()?
        }
        _ => crate::unity_fs::object_name(&d, sf.big_endian)?,
    };
    // the old exports' duplicate suffix (2nd, 3rd ... object of a name: _0, _1 ...; case-insensitive)
    let name = match o.same_named_ci(obj.class_id, &name).and_then(|v| v.iter().position(|&x| x == (node, pid))) {
        Some(i) if i > 0 && obj.class_id != 48 => format!("{name}_{}", i - 1),
        _ => name,
    };
    // characters file names avoid became '_'
    let name = name.replace([',', ':', '/', '*', '?', '"', '<', '>', '|'], "_");
    let (dir, ext) = match obj.class_id {
        21 => ("Material", "mat"),
        28 => ("Texture2D", "png"),
        43 => ("Mesh", "asset"),
        48 => ("Shader", "shader"),
        74 => ("AnimationClip", "anim"),
        83 => ("AudioClip", "ogg"),
        84 => ("RenderTexture", "renderTexture"),
        89 => ("Cubemap", "png"),
        91 => ("AnimatorController", "controller"),
        114 => ("MonoBehaviour", "asset"),
        128 => ("Font", "ttf"),
        213 => ("Sprite", "asset"),
        _ => ("Object", "asset"),
    };
    Some(format!("{dir}/{name}.{ext}"))
}
