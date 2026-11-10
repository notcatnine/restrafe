//! Unity scene / prefab documents for UGUI subtrees, built from the installation (owned_docs.rs): GameObjects,
//! Transforms and components with their Unity field names, `{fileID}` links inside the scene / prefab and guid
//! tokens for assets. MonoBehaviours carry their class name ("Image", "TextMeshProUGUI", "PauseManager" ...).

use serde_yaml::{Mapping, Value};
use std::collections::HashMap;


pub struct UScene {
    /// id -> (class, document)
    docs: HashMap<i64, (u32, std::sync::Arc<Mapping>)>,
}

pub fn file_ref(v: Option<&Value>) -> i64 {
    v.and_then(|v| v.get("fileID")).and_then(Value::as_i64).unwrap_or(0)
}

pub fn guid_ref(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v.get("guid")).and_then(Value::as_str).map(str::to_string)
}

pub fn f(v: Option<&Value>) -> f32 {
    v.and_then(Value::as_f64).unwrap_or(0.0) as f32
}

pub fn v2(v: Option<&Value>) -> [f32; 2] {
    [f(v.and_then(|v| v.get("x"))), f(v.and_then(|v| v.get("y")))]
}

pub fn v3(v: Option<&Value>) -> [f32; 3] {
    [f(v.and_then(|v| v.get("x"))), f(v.and_then(|v| v.get("y"))), f(v.and_then(|v| v.get("z")))]
}

pub fn v4(v: Option<&Value>) -> [f32; 4] {
    [f(v.and_then(|v| v.get("x"))), f(v.and_then(|v| v.get("y"))), f(v.and_then(|v| v.get("z"))), f(v.and_then(|v| v.get("w")))]
}

pub fn rgba(v: Option<&Value>) -> [f32; 4] {
    match v {
        Some(v) if v.get("r").is_some() => [f(v.get("r")), f(v.get("g")), f(v.get("b")), f(v.get("a"))],
        // TMP stores Color32 as `rgba: 4294967295` (ABGR packed little-endian).
        Some(v) if v.get("rgba").is_some() => {
            let p = v.get("rgba").and_then(Value::as_u64).unwrap_or(0xffff_ffff) as u32;
            [(p & 0xff) as f32 / 255.0, ((p >> 8) & 0xff) as f32 / 255.0, ((p >> 16) & 0xff) as f32 / 255.0, (p >> 24) as f32 / 255.0]
        }
        _ => [1.0; 4],
    }
}

pub fn int(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_i64).unwrap_or(0)
}

impl UScene {
    /// `.../Scenes/<dir>/<Scene>.unity` (a scene) or `.../GameObject/<Prefab>.prefab` (a prefab), read from the
    /// installation.
    pub fn load(path: &str) -> Option<UScene> {
        let file = path.replace('\\', "/");
        let file = file.rsplit('/').next()?;
        let docs = if let Some(scene) = file.strip_suffix(".unity") {
            crate::owned_docs::scene(scene)?
        } else {
            crate::owned_docs::prefab(file.strip_suffix(".prefab")?)?
        };
        Some(UScene { docs: docs.into_iter().map(|(id, class, m)| (id, (class, m))).collect() })
    }

    pub fn class(&self, id: i64) -> u32 {
        self.docs.get(&id).map(|x| x.0).unwrap_or(0)
    }

    /// The document's fields.
    pub fn doc(&self, id: i64) -> Option<std::sync::Arc<Mapping>> {
        self.docs.get(&id).map(|x| x.1.clone())
    }

    /// MonoBehaviour class name ("Image", "TextMeshProUGUI", "PauseManager", ...).
    pub fn script_name(&self, m: &Mapping) -> String {
        m.get("__script").and_then(Value::as_str).unwrap_or("?").to_string()
    }

    /// Components of a GameObject: (id, class, script name or class name).
    pub fn components(&self, go: i64) -> Vec<(i64, u32, String)> {
        let Some(m) = self.doc(go) else { return vec![] };
        let list = m.get("m_Component").and_then(Value::as_sequence).cloned().unwrap_or_default();
        list.iter()
            .map(|c| {
                let id = file_ref(c.get("component"));
                let class = self.class(id);
                let name = match class {
                    114 => self.doc(id).map(|d| self.script_name(&d)).unwrap_or_default(),
                    224 => "RectTransform".into(),
                    4 => "Transform".into(),
                    222 => "CanvasRenderer".into(),
                    223 => "Canvas".into(),
                    225 => "CanvasGroup".into(),
                    82 => "AudioSource".into(),
                    95 => "Animator".into(),
                    c => format!("class{c}"),
                };
                (id, class, name)
            })
            .collect()
    }

    pub fn go_name(&self, go: i64) -> String {
        self.doc(go).and_then(|m| m.get("m_Name").and_then(Value::as_str).map(str::to_string)).unwrap_or_default()
    }

    pub fn go_active(&self, go: i64) -> bool {
        self.doc(go).map(|m| int(m.get("m_IsActive")) != 0).unwrap_or(false)
    }

    /// RectTransform / Transform of a GameObject.
    pub fn transform_of(&self, go: i64) -> Option<i64> {
        self.components(go).into_iter().find(|c| c.1 == 224 || c.1 == 4).map(|c| c.0)
    }

    pub fn go_of(&self, comp: i64) -> i64 {
        self.doc(comp).map(|m| file_ref(m.get("m_GameObject"))).unwrap_or(0)
    }

    /// Child GameObjects in sibling order.
    pub fn children(&self, go: i64) -> Vec<i64> {
        let Some(t) = self.transform_of(go) else { return vec![] };
        let Some(m) = self.doc(t) else { return vec![] };
        m.get("m_Children").and_then(Value::as_sequence).map(|s| s.iter().map(|c| self.go_of(file_ref(Some(c)))).collect()).unwrap_or_default()
    }

    pub fn parent(&self, go: i64) -> Option<i64> {
        let t = self.transform_of(go)?;
        let p = file_ref(self.doc(t)?.get("m_Father"));
        (p != 0).then(|| self.go_of(p))
    }

    pub fn path(&self, go: i64) -> String {
        let mut names = vec![self.go_name(go)];
        let mut g = go;
        while let Some(p) = self.parent(g) {
            names.push(self.go_name(p));
            g = p;
        }
        names.reverse();
        names.join("/")
    }

    /// GameObjects with this name (whole scene).
    pub fn find_by_name(&self, name: &str) -> Vec<i64> {
        let mut out: Vec<i64> = self.docs.iter().filter(|(_, v)| v.0 == 1).map(|(k, _)| *k).filter(|&id| self.go_name(id) == name).collect();
        out.sort();
        out
    }
}

impl UScene {
    /// All document ids of a Unity class id.
    pub fn ids_of_class(&self, class: u32) -> Vec<i64> {
        let mut v: Vec<i64> = self.docs.iter().filter(|(_, x)| x.0 == class).map(|(k, _)| *k).collect();
        v.sort();
        v
    }
}

/// One persistent UnityEvent listener (`m_PersistentCalls.m_Calls[i]`).
#[derive(Clone, Debug)]
pub struct PCall {
    pub target: i64,
    pub type_name: String,
    pub method: String,
    /// PersistentListenerMode: 0 EventDefined, 1 Void, 2 Object, 3 Int, 4 Float, 5 String, 6 Bool.
    pub mode: i64,
    pub object_arg: i64,
    // the call's arguments (none of the handled calls takes one yet)
    #[allow(dead_code)]
    pub int_arg: i64,
    #[allow(dead_code)]
    pub float_arg: f32,
    #[allow(dead_code)]
    pub string_arg: String,
    pub bool_arg: bool,
    /// m_CallState: 0 Off, 1 EditorAndRuntime, 2 RuntimeOnly.
    pub state: i64,
}

/// Persistent calls of a UnityEvent field (e.g. "m_OnClick", "m_OnValueChanged", "onValueChanged").
pub fn persistent_calls(m: &Mapping, key: &str) -> Vec<PCall> {
    let calls = m.get(key).and_then(|e| e.get("m_PersistentCalls")).and_then(|p| p.get("m_Calls")).and_then(Value::as_sequence).cloned().unwrap_or_default();
    calls
        .iter()
        .map(|c| {
            let a = c.get("m_Arguments");
            let g = |k: &str| a.and_then(|a| a.get(k));
            PCall {
                target: file_ref(c.get("m_Target")),
                type_name: c.get("m_TargetAssemblyTypeName").and_then(Value::as_str).unwrap_or("").split(',').next().unwrap_or("").to_string(),
                method: c.get("m_MethodName").and_then(Value::as_str).unwrap_or("").to_string(),
                mode: int(c.get("m_Mode")),
                object_arg: file_ref(g("m_ObjectArgument")),
                int_arg: int(g("m_IntArgument")),
                float_arg: f(g("m_FloatArgument")),
                string_arg: g("m_StringArgument").and_then(Value::as_str).unwrap_or("").to_string(),
                bool_arg: int(g("m_BoolArgument")) != 0,
                state: int(c.get("m_CallState")),
            }
        })
        .collect()
}
