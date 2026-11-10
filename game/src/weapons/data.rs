//! Weapon definitions loaded from extracted prefab JSON (`assets/data/weapons/*.json`,
//! produced by `src/bin/extract.rs`) and the PlayerIK hand anchors.

use bevy::prelude::*;
use serde_json::{Map, Value};
use std::sync::Arc;

pub type Fields = Map<String, Value>;

/// Unity serialized-field accessors (bools are stored as 0/1, vectors as {x,y,z}).
pub trait UField {
    fn f(&self, k: &str) -> f32;
    fn i(&self, k: &str) -> i32;
    fn b(&self, k: &str) -> bool;
    fn v3(&self, k: &str) -> Vec3;
    fn mask(&self, k: &str) -> u32;
    fn s(&self, k: &str) -> String;
    /// Asset reference -> path relative to Assets (e.g. "AudioClip/R_Glock.ogg").
    fn asset(&self, k: &str) -> Option<String>;
    /// GameObject field instantiated as an effect: an asset path, or an object inside the weapon prefab as
    /// "ref:<object path>" (DualLauncher / RocketLauncher muzzleFlash).
    fn prefab_ref(&self, k: &str) -> Option<String> {
        self.asset(k).or_else(|| {
            let s = self.s(k);
            s.strip_prefix("ref:").map(|r| format!("ref:{}", r.split("::").next().unwrap_or(r)))
        })
    }
}

impl UField for Fields {
    fn f(&self, k: &str) -> f32 {
        self.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32
    }
    fn i(&self, k: &str) -> i32 {
        self.get(k).and_then(|v| v.as_i64()).unwrap_or(0) as i32
    }
    fn b(&self, k: &str) -> bool {
        match self.get(k) {
            Some(Value::Bool(b)) => *b,
            Some(v) => v.as_i64().unwrap_or(0) != 0,
            None => false,
        }
    }
    fn v3(&self, k: &str) -> Vec3 {
        self.get(k).map(v3).unwrap_or(Vec3::ZERO)
    }
    fn mask(&self, k: &str) -> u32 {
        self.get(k).and_then(|v| v.get("m_Bits")).and_then(|v| v.as_u64()).unwrap_or(0) as u32
    }
    fn s(&self, k: &str) -> String {
        self.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
    }
    fn asset(&self, k: &str) -> Option<String> {
        self.get(k).and_then(|v| v.as_str()).and_then(|s| s.strip_prefix("asset:")).map(str::to_string)
    }
}

pub fn v3(v: &Value) -> Vec3 {
    let g = |c: &str| v.get(c).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
    Vec3::new(g("x"), g("y"), g("z"))
}

/// Unity local position -> Bevy.
pub fn u_pos(v: &Value) -> Vec3 {
    let p = v3(v);
    Vec3::new(p.x, p.y, -p.z)
}

/// Unity local rotation quaternion -> Bevy.
pub fn u_quat(v: &Value) -> Quat {
    let g = |c: &str| v.get(c).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
    Quat::from_xyzw(-g("x"), -g("y"), g("z"), g("w")).normalize()
}

pub struct PrefabObject {
    pub path: String,
    pub layer: u32,
    pub tag: String,
    /// GameObject.m_IsActive (own flag, not the hierarchy).
    pub active: bool,
    pub components: Vec<(String, Fields)>,
}

impl PrefabObject {
    pub fn comp(&self, ty: &str) -> Option<&Fields> {
        self.components.iter().find(|(t, _)| t == ty).map(|(_, f)| f)
    }
    pub fn local_transform(&self) -> Transform {
        match self.comp("Transform") {
            Some(t) => Transform {
                translation: t.get("m_LocalPosition").map(u_pos).unwrap_or_default(),
                rotation: t.get("m_LocalRotation").map(u_quat).unwrap_or_default(),
                scale: t.get("m_LocalScale").map(v3).unwrap_or(Vec3::ONE),
            },
            None => Transform::IDENTITY,
        }
    }
}

pub struct Prefab {
    pub name: String,
    pub objects: Vec<PrefabObject>,
}

impl Prefab {
    pub fn load(path: &str) -> Result<Prefab, String> {
        let v: Value = match std::fs::read_to_string(path) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| e.to_string())?,
            // Not exported: built in place from the installation (prefab_json.rs).
            Err(_) => serde_json::from_str(&crate::owned::data_text(path).ok_or_else(|| format!("{path}: not in the installation"))?).map_err(|e| e.to_string())?,
        };
        let objects = v["objects"]
            .as_array()
            .ok_or("no objects")?
            .iter()
            .map(|o| PrefabObject {
                path: o["path"].as_str().unwrap_or("").to_string(),
                layer: o["layer"].as_u64().unwrap_or(0) as u32,
                tag: o["tag"].as_str().unwrap_or("").to_string(),
                active: o["active"].as_bool().unwrap_or(true),
                components: o["components"]
                    .as_array()
                    .map(|cs| cs.iter().map(|c| (c["type"].as_str().unwrap_or("").to_string(), c["fields"].as_object().cloned().unwrap_or_default())).collect())
                    .unwrap_or_default(),
            })
            .collect();
        Ok(Prefab { name: v["name"].as_str().unwrap_or("").to_string(), objects })
    }

    pub fn find(&self, path: &str) -> Option<&PrefabObject> {
        self.objects.iter().find(|o| o.path == path)
    }

    /// Object path from a "ref:<path>::<Type>" reference.
    pub fn deref(&self, r: &str) -> Option<&PrefabObject> {
        let p = r.strip_prefix("ref:")?.split("::").next()?;
        self.find(p)
    }

    /// First object having component `ty`, plus that component.
    pub fn first_with(&self, ty: &str) -> Option<(&PrefabObject, &Fields)> {
        self.objects.iter().find_map(|o| o.comp(ty).map(|c| (o, c)))
    }

    /// Children of `path` in sibling order (Transform.m_Children).
    pub fn children(&self, path: &str) -> Vec<&PrefabObject> {
        let Some(o) = self.find(path) else { return vec![] };
        let tf = o.comp("Transform").or_else(|| o.comp("RectTransform"));
        tf.and_then(|t| t.get("m_Children"))
            .and_then(|c| c.as_array())
            .map(|a| a.iter().filter_map(|r| r.as_str()).filter_map(|r| self.deref(r)).collect())
            .unwrap_or_default()
    }

    /// Transform of `path` relative to `ancestor` (both prefab paths).
    pub fn relative_transform(&self, path: &str, ancestor: &str) -> Transform {
        let mut chain = Vec::new();
        let mut p = path.to_string();
        while p != ancestor && !p.is_empty() {
            if let Some(o) = self.find(&p) {
                chain.push(o.local_transform());
            }
            p = p.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap_or_default();
        }
        chain.iter().rev().fold(Transform::IDENTITY, |acc, t| acc.mul_transform(*t))
    }
}

/// The 14 weapon behaviour classes found in the game's prefabs.
pub const WEAPON_CLASSES: [&str; 14] = [
    "Gun", "Shotgun", "LargeRaycastGun", "Minigun", "ChargeGun", "BeamGun", "RepulsiveGun", "MeleeWeapon",
    "DualLauncher", "Taser", "BumpGun", "Propeller", "FlashLight", "WeaponHandSpawner",
];

pub struct WeaponDef {
    /// Prefab name (e.g. "Glock").
    pub name: String,
    /// Behaviour class (one of [`WEAPON_CLASSES`]).
    pub class: String,
    /// Fields of the weapon behaviour component (includes inherited `Weapon` fields).
    pub w: Fields,
    /// ItemBehaviour fields.
    pub item: Fields,
    pub prefab: Prefab,
}

impl WeaponDef {
    pub fn weapon_name(&self) -> String {
        self.item.s("weaponName")
    }
    pub fn require_both_hands(&self) -> bool {
        self.w.b("requireBothHands")
    }
}

#[derive(Resource, Default)]
pub struct WeaponDb {
    pub defs: Vec<Arc<WeaponDef>>,
}

impl WeaponDb {
    pub fn load(dir: &str) -> WeaponDb {
        let mut defs = Vec::new();
        let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        // every weapon of the installation (built in place by Prefab::load when its file is absent)
        for n in crate::owned::weapon_names() {
            let p = std::path::PathBuf::from(format!("{dir}/{n}.json"));
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
        paths.sort();
        for path in paths {
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let prefab = match Prefab::load(path.to_str().unwrap()) {
                Ok(p) => p,
                Err(err) => {
                    error!("weapon {path:?}: {err}");
                    continue;
                }
            };
            let root = prefab.objects.first().map(|o| o.path.clone()).unwrap_or_default();
            let Some(root_obj) = prefab.find(&root) else { continue };
            let Some((class, w)) = root_obj.components.iter().find(|(t, _)| WEAPON_CLASSES.contains(&t.as_str())).map(|(t, f)| (t.clone(), f.clone())) else {
                continue;
            };
            let item = root_obj.comp("ItemBehaviour").cloned().unwrap_or_default();
            defs.push(Arc::new(WeaponDef { name: prefab.name.clone(), class, w, item, prefab }));
        }
        info!("loaded {} weapon definitions", defs.len());
        WeaponDb { defs }
    }

    pub fn get(&self, name: &str) -> Option<Arc<WeaponDef>> {
        self.defs.iter().find(|d| d.name == name).cloned()
    }
}

/// PlayerIK hand anchor transforms (relative to FPArms), in `GetComponentsInChildren` order.
#[derive(Resource, Default)]
pub struct HandAnchors {
    pub right: Vec<Transform>,
    pub left: Vec<Transform>,
    pub both: Vec<Transform>,
    pub aim_right: Vec<Transform>,
    pub aim_both: Vec<Transform>,
    pub player: Option<Arc<Prefab>>,
}

impl HandAnchors {
    pub fn load(path: &str) -> HandAnchors {
        let Ok(p) = Prefab::load(path) else {
            error!("cannot load {path}");
            return HandAnchors::default();
        };
        let fparms = "PlayerIK/CameraLeanPivot/LandBobPivot/CameraHolder/Camera/BobPosition/FPArms";
        let group = |name: &str| -> Vec<Transform> {
            let gpath = format!("{fparms}/{name}");
            // GetComponentsInChildren<ItemPosition>: depth-first, sibling order.
            let mut out = Vec::new();
            let mut stack = vec![gpath.clone()];
            while let Some(cur) = stack.pop() {
                if let Some(o) = p.find(&cur) {
                    if o.comp("ItemPosition").is_some() {
                        out.push(p.relative_transform(&cur, fparms));
                    }
                }
                let kids: Vec<String> = p.children(&cur).iter().map(|o| o.path.clone()).collect();
                for k in kids.into_iter().rev() {
                    stack.push(k);
                }
            }
            out
        };
        let a = HandAnchors {
            right: group("RightHandPositions"),
            left: group("LeftHandPositions"),
            both: group("BothHandPositions"),
            aim_right: group("RightHandAimPositions"),
            aim_both: group("BothHandAimPositions"),
            player: None,
        };
        info!("hand anchors: right {} left {} both {} aimR {} aimB {}", a.right.len(), a.left.len(), a.both.len(), a.aim_right.len(), a.aim_both.len());
        HandAnchors { player: Some(Arc::new(p)), ..a }
    }
}
