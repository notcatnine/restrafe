//! The player's own STRAFTAT installation, read in place (no export): the opened bundle plus lookups that turn
//! its objects into Bevy assets. Systems move from the old exported files onto these loaders one by one.

use crate::{install, unity_fs};
use bevy::asset::RenderAssetUsages;
use bevy::asset::io::{AssetReader, AssetReaderError, PathStream, Reader, VecReader};
use bevy::asset::meta::{AssetAction, AssetMeta, AssetMetaDyn};
use bevy::asset::{AssetLoader, LoadContext};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Reference data for the comparison tests: `p` ("assets/...") itself, else the same file in `../dev/moved-assets/`
/// where the old exports were moved when the game stopped needing them.
#[cfg(test)]
pub fn reference_file(p: &str) -> String {
    // (an emptied directory counts as missing)
    let present = |p: &str| std::fs::read_dir(p).map(|mut d| d.next().is_some()).unwrap_or_else(|_| std::path::Path::new(p).exists());
    if present(p) {
        return p.to_string();
    }
    match p.strip_prefix("assets/").map(|r| format!("../dev/moved-assets/{r}")) {
        Some(m) if present(&m) => m,
        _ => p.to_string(),
    }
}

/// Whether a slow comparison test runs: its own variable, or RESTRAFE_REF_CHECKS=1 for all of them.
#[cfg(test)]
pub fn ref_checks(var: &str) -> bool {
    std::env::var_os(var).is_some() || std::env::var_os("RESTRAFE_REF_CHECKS").is_some()
}

/// Resources path per object (see `Owned::resource_path`).
static RES_BY_OBJECT: OnceLock<std::sync::Mutex<HashMap<(usize, i64), String>>> = OnceLock::new();

/// The opened installation, shared by every loader.
#[derive(Resource, Clone)]
pub struct Owned(pub Arc<OwnedInner>);

pub struct OwnedInner {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub bundle: unity_fs::Bundle,
    /// name -> (bundle node index, path id), built on first use
    textures: OnceLock<HashMap<String, (usize, i64)>>,
    /// normalized name (`norm_name`) -> name, for file names the old exports sanitized
    textures_norm: OnceLock<HashMap<String, String>>,
    audio: OnceLock<HashMap<String, (usize, i64)>>,
    prefabs: OnceLock<HashMap<String, Vec<(usize, i64)>>>,
    resources: OnceLock<HashMap<String, (usize, i64)>>,
    meshes: OnceLock<HashMap<String, Vec<(usize, i64)>>>,
    cubemaps: OnceLock<HashMap<String, Vec<(usize, i64)>>>,
    assemblies: OnceLock<Option<crate::dotnet_meta::Assemblies>>,
    setups: OnceLock<crate::fsb5::SetupSource>,
    /// decoded clips (WAV bytes) under 4 MB, so repeated loads of a short sound do not decode again
    wav_cache: std::sync::Mutex<HashMap<String, Arc<Vec<u8>>>>,
}

impl Owned {
    /// Finds and opens the installation (same discovery as `setup`).
    pub fn open() -> Result<Owned, String> {
        let root = install::discover(None)?;
        let data_dir = root.join(install::DATA_DIR);
        let mut bundle = unity_fs::Bundle::open(&data_dir.join(install::BUNDLE))?;
        // Unity's built-in meshes / materials (Cube, Sphere, Default-Material ...) live next to the bundle.
        let _ = bundle.add_loose(&data_dir.join("Resources").join("unity default resources"));
        Ok(Owned(Arc::new(OwnedInner { root, data_dir, bundle, textures: OnceLock::new(), textures_norm: OnceLock::new(), audio: OnceLock::new(), prefabs: OnceLock::new(), resources: OnceLock::new(), meshes: OnceLock::new(), cubemaps: OnceLock::new(), assemblies: OnceLock::new(), setups: OnceLock::new(), wav_cache: Default::default() })))
    }

    fn cache_key(&self) -> String {
        let size = std::fs::metadata(self.0.data_dir.join(install::BUNDLE)).map(|m| m.len()).unwrap_or(0);
        format!("{}-{size}", install::installed_build(&self.0.root).unwrap_or_default())
    }

    /// name -> (bundle node, path id) of every object of `class` (first one wins), cached per game build in
    /// `<config>/<tag>-<build>-<bundle size>.tsv`.
    fn name_index(&self, class: i32, tag: &str) -> HashMap<String, (usize, i64)> {
        let b = &self.0.bundle;
        let cache = install::config_dir().map(|d| d.join(format!("{tag}-{}.tsv", self.cache_key())));
        if let Some(text) = cache.as_ref().and_then(|c| std::fs::read_to_string(c).ok()) {
            let map: HashMap<String, (usize, i64)> = text
                .lines()
                .filter_map(|l| {
                    let mut f = l.split('\t');
                    Some((f.next()?.to_string(), (f.next()?.parse().ok()?, f.next()?.parse().ok()?)))
                })
                .collect();
            if !map.is_empty() {
                return map;
            }
        }
        let mut map = HashMap::new();
        for (i, n) in b.nodes.iter().enumerate() {
            let Ok(sf) = unity_fs::SerializedFile::open(b, n) else { continue };
            for o in sf.objects.iter().filter(|o| o.class_id == class) {
                // the name is the first field: read just its head
                let Ok(head) = b.read_node(n, sf.data_offset + o.start, (o.size as u64).min(260)) else { continue };
                if let Some(name) = unity_fs::object_name(&head, sf.big_endian) {
                    map.entry(name).or_insert((i, o.path_id));
                }
            }
        }
        if let Some(c) = &cache {
            let _ = std::fs::create_dir_all(c.parent().unwrap());
            let _ = std::fs::write(c, map.iter().map(|(k, (n, p))| format!("{k}\t{n}\t{p}\n")).collect::<String>());
        }
        map
    }

    fn texture_index(&self) -> &HashMap<String, (usize, i64)> {
        self.0.textures.get_or_init(|| self.name_index(28, "textures"))
    }

    fn audio_index(&self) -> &HashMap<String, (usize, i64)> {
        self.0.audio.get_or_init(|| self.name_index(83, "audio"))
    }

    /// Root GameObjects of the prefab files (`sharedassets*.assets`): name -> (node, GameObject path id), cached
    /// per game build like the other indexes.
    fn prefab_index(&self) -> &HashMap<String, Vec<(usize, i64)>> {
        self.0.prefabs.get_or_init(|| {
            let cache = install::config_dir().map(|d| d.join(format!("prefabs2-{}.tsv", self.cache_key())));
            if let Some(text) = cache.as_ref().and_then(|c| std::fs::read_to_string(c).ok()) {
                let mut map: HashMap<String, Vec<(usize, i64)>> = HashMap::new();
                for l in text.lines() {
                    let mut f = l.split('\t');
                    if let (Some(k), Some(Ok(n)), Some(Ok(p))) = (f.next(), f.next().map(str::parse), f.next().map(str::parse)) {
                        map.entry(k.to_string()).or_default().push((n, p));
                    }
                }
                if !map.is_empty() {
                    return map;
                }
            }
            let b = &self.0.bundle;
            let mut map = HashMap::new();
            for (i, n) in b.nodes.iter().enumerate() {
                if !(n.path.starts_with("sharedassets") || n.path == "resources.assets") || !n.path.ends_with(".assets") {
                    continue;
                }
                let Ok(sf) = unity_fs::SerializedFile::open(b, n) else { continue };
                let by_id: HashMap<i64, &unity_fs::ObjectInfo> = sf.objects.iter().map(|o| (o.path_id, o)).collect();
                for o in sf.objects.iter().filter(|o| o.class_id == 4 || o.class_id == 224) {
                    let Ok(d) = sf.object_data(b, n, o) else { continue };
                    let Ok(t) = unity_fs::Transform::read(&d, sf.big_endian) else { continue };
                    if t.father.1 != 0 || t.game_object.0 != 0 {
                        continue;
                    }
                    let Some(go) = by_id.get(&t.game_object.1) else { continue };
                    let Ok(gd) = sf.object_data(b, n, go) else { continue };
                    if let Ok(g) = unity_fs::GameObject::read(&gd, sf.big_endian) {
                        map.entry(g.name).or_insert_with(Vec::new).push((i, go.path_id));
                    }
                }
            }
            if let Some(c) = &cache {
                let _ = std::fs::create_dir_all(c.parent().unwrap());
                let _ = std::fs::write(c, map.iter().flat_map(|(k, v)| v.iter().map(move |(n, p)| format!("{k}\t{n}\t{p}\n"))).collect::<String>());
            }
            map
        })
    }

    /// The prefab root for an exported .glb file stem (same matching rules as textures).
    /// Unity's Resources table (`ResourceManager.m_Container` in globalgamemanagers): lower-case path such as
    /// "randomweapons/claymore" -> (node, path id) of the object.
    /// The Resources path of an object (every entry of the table, also the 2nd, 3rd ... object under one path).
    pub fn resource_path(&self, obj: (usize, i64)) -> Option<String> {
        self.resources();
        RES_BY_OBJECT.get()?.lock().unwrap().get(&obj).cloned()
    }

    pub fn resources(&self) -> &HashMap<String, (usize, i64)> {
        self.0.resources.get_or_init(|| {
            let b = &self.0.bundle;
            let mut map = HashMap::new();
            let Some(gi) = b.nodes.iter().position(|n| n.path == "globalgamemanagers") else { return map };
            let Ok(sf) = unity_fs::SerializedFile::open(b, &b.nodes[gi]) else { return map };
            for o in sf.objects.iter().filter(|o| o.class_id == 147) {
                let Ok(d) = sf.object_data(b, &b.nodes[gi], o) else { continue };
                let mut r = unity_fs::ObjReader::new(&d, sf.big_endian);
                let Ok(n) = r.i32() else { continue };
                for _ in 0..n {
                    let (Ok(path), Ok((fid, pid))) = (r.string(), r.pptr()) else { break };
                    // file index into globalgamemanagers' externals
                    let node = if fid == 0 {
                        Some(gi)
                    } else {
                        sf.externals.get(fid as usize - 1).and_then(|e| {
                            let name = e.rsplit('/').next().unwrap_or(e);
                            b.nodes.iter().position(|x| x.path == name)
                        })
                    };
                    if let Some(node) = node {
                        RES_BY_OBJECT.get_or_init(Default::default).lock().unwrap().entry((node, pid)).or_insert_with(|| path.clone());
                        map.entry(path).or_insert((node, pid));
                    }
                }
            }
            map
        })
    }

    /// Every Mesh by name, all same-named ones in (file, path id) order (the old exports named the 2nd "<name>_0").
    fn mesh_index(&self) -> &HashMap<String, Vec<(usize, i64)>> {
        self.0.meshes.get_or_init(|| {
            let cache = install::config_dir().map(|d| d.join(format!("meshes-{}.tsv", self.cache_key())));
            let mut map: HashMap<String, Vec<(usize, i64)>> = HashMap::new();
            if let Some(text) = cache.as_ref().and_then(|c| std::fs::read_to_string(c).ok()) {
                for l in text.lines() {
                    let mut f = l.split('\t');
                    if let (Some(k), Some(Ok(n)), Some(Ok(p))) = (f.next(), f.next().map(str::parse), f.next().map(str::parse)) {
                        map.entry(k.to_string()).or_default().push((n, p));
                    }
                }
                if !map.is_empty() {
                    return map;
                }
            }
            let b = &self.0.bundle;
            for (i, n) in b.nodes.iter().enumerate() {
                let Ok(sf) = unity_fs::SerializedFile::open(b, n) else { continue };
                let mut objs: Vec<_> = sf.objects.iter().filter(|o| o.class_id == 43).collect();
                objs.sort_by_key(|o| o.path_id);
                for o in objs {
                    let Ok(head) = b.read_node(n, sf.data_offset + o.start, (o.size as u64).min(260)) else { continue };
                    if let Some(name) = unity_fs::object_name(&head, sf.big_endian) {
                        map.entry(name).or_default().push((i, o.path_id));
                    }
                }
            }
            if let Some(c) = &cache {
                let _ = std::fs::create_dir_all(c.parent().unwrap());
                let _ = std::fs::write(c, map.iter().flat_map(|(k, v)| v.iter().map(move |(n, p)| format!("{k}\t{n}\t{p}\n"))).collect::<String>());
            }
            map
        })
    }

    /// All objects of `class` named `name`, in (file, path id) order (materials, meshes ...; cached per class).
    pub fn same_named(&self, class: i32, name: &str) -> Option<Vec<(usize, i64)>> {
        self.same_named_impl(class, name, false)
    }

    /// Like `same_named`, but names that differ only in case count as the same (the old exports were written to a
    /// case-insensitive file system: "Pig" and "pig" became Pig / pig_0), merged in (file, path id) order.
    pub fn same_named_ci(&self, class: i32, name: &str) -> Option<Vec<(usize, i64)>> {
        self.same_named_impl(class, name, true)
    }

    fn same_named_impl(&self, class: i32, name: &str, any_case: bool) -> Option<Vec<(usize, i64)>> {
        static IDX: OnceLock<std::sync::Mutex<HashMap<i32, HashMap<String, Vec<(usize, i64)>>>>> = OnceLock::new();
        let mut all = IDX.get_or_init(Default::default).lock().unwrap();
        if !all.contains_key(&class) {
            // one pass over the bundle for the classes looked up by name (materials, textures, scripts, sprites)
            let classes: Vec<i32> = if [21, 28, 74, 90, 91, 114, 213, 241, 245].contains(&class) { vec![21, 28, 74, 90, 91, 114, 213, 241, 245] } else { vec![class] };
            for (c, m) in self.name_lists(&classes) {
                all.insert(c, m);
            }
        }
        let m = all.get(&class)?;
        if any_case {
            let lower = name.to_lowercase();
            let mut v: Vec<(usize, i64)> = m.iter().filter(|(k, _)| k.to_lowercase() == lower).flat_map(|(_, v)| v.iter().copied()).collect();
            v.sort();
            return (!v.is_empty()).then_some(v);
        }
        if let Some(v) = m.get(name) {
            return Some(v.clone());
        }
        // an exported file name: characters file names avoid became '_' ("wdth,wght" -> "wdth_wght")
        let file_form = |s: &str| s.replace([',', ':', '/', '\\', '*', '?', '"', '<', '>', '|'], "_");
        m.iter().find(|(k, _)| file_form(k) == name).map(|(_, v)| v.clone())
    }

    /// name -> every object of that name, in (file, path id) order, for each of `classes`; cached per game build in
    /// `<config>/names<class>-<build>-<bundle size>.tsv` (name, node, path id).
    fn name_lists(&self, classes: &[i32]) -> Vec<(i32, HashMap<String, Vec<(usize, i64)>>)> {
        let b = &self.0.bundle;
        let cache = |c: i32| install::config_dir().map(|d| d.join(format!("names{c}-{}.tsv", self.cache_key())));
        let parse = |text: &str| {
            let mut map: HashMap<String, Vec<(usize, i64)>> = HashMap::new();
            for l in text.lines() {
                let mut f = l.split('\t');
                if let (Some(k), Some(Ok(n)), Some(Ok(p))) = (f.next(), f.next().map(str::parse), f.next().map(str::parse)) {
                    map.entry(k.to_string()).or_default().push((n, p));
                }
            }
            map
        };
        let cached: Vec<Option<String>> = classes.iter().map(|&c| cache(c).and_then(|p| std::fs::read_to_string(p).ok())).collect();
        if cached.iter().all(|t| t.is_some()) {
            return classes.iter().zip(cached).map(|(&c, t)| (c, parse(&t.unwrap()))).collect();
        }
        let mut order: HashMap<i32, Vec<(String, usize, i64)>> = classes.iter().map(|&c| (c, Vec::new())).collect();
        for (i, n) in b.nodes.iter().enumerate() {
            let Ok(sf) = unity_fs::SerializedFile::open(b, n) else { continue };
            let mut objs: Vec<_> = sf.objects.iter().filter(|o| classes.contains(&o.class_id)).collect();
            objs.sort_by_key(|o| o.path_id);
            for o in objs {
                let Ok(head) = b.read_node(n, sf.data_offset + o.start, (o.size as u64).min(300)) else { continue };
                // MonoBehaviour: m_GameObject, m_Enabled, m_Script before m_Name
                let head = if o.class_id == 114 { head.get(28..).unwrap_or_default() } else { &head[..] };
                if let Some(nm) = unity_fs::object_name(head, sf.big_endian).filter(|n| !n.contains(['\t', '\n'])) {
                    order.get_mut(&o.class_id).unwrap().push((nm, i, o.path_id));
                }
            }
        }
        classes
            .iter()
            .map(|&c| {
                let text: String = order[&c].iter().map(|(k, n, p)| format!("{k}\t{n}\t{p}\n")).collect();
                if let Some(p) = cache(c) {
                    let _ = std::fs::create_dir_all(p.parent().unwrap());
                    let _ = std::fs::write(p, &text);
                }
                (c, parse(&text))
            })
            .collect()
    }

    /// The object of `class` an exported file stem names: the first of that name, or for "<name>_N" (the old
    /// exports' suffix for duplicates) the (N+2)th of `<name>`.
    pub fn find_named(&self, class: i32, name: &str) -> Option<(usize, i64)> {
        if let Some(v) = self.same_named(class, name) {
            return v.first().copied();
        }
        let (base, n) = name.rsplit_once('_')?;
        let i: usize = n.parse().ok()?;
        self.same_named_ci(class, base)?.get(i + 1).copied()
    }

    /// All meshes of that name in (file, path id) order.
    pub fn mesh_index_entries(&self, name: &str) -> Option<&Vec<(usize, i64)>> {
        self.mesh_index().get(name)
    }

    /// A Mesh for an exported mesh file stem: the exact name, or "<name>_<n>" = the (n + 2)-th mesh of that name.
    pub fn resolve_mesh(&self, name: &str) -> Option<(usize, i64)> {
        let idx = self.mesh_index();
        if let Some(v) = idx.get(name) {
            return v.first().copied();
        }
        if let Some((base, d)) = name.rsplit_once('_') {
            if let Ok(n) = d.parse::<usize>() {
                if let Some(v) = idx.get(base) {
                    return v.get(n + 1).copied();
                }
            }
        }
        // names the old exports sanitized ("laurel_wreath" for "laurel wreath")
        let n = norm_name(name);
        idx.iter().find(|(k, _)| norm_name(k) == n).and_then(|(_, v)| v.first().copied())
    }

    pub fn mesh_glb(&self, name: &str) -> Result<Vec<u8>, String> {
        let (node, pid) = self.resolve_mesh(name).ok_or_else(|| format!("mesh {name} not in the installation"))?;
        crate::owned_gltf::mesh_glb(self, node, pid)
    }

    /// The bundle node (`levelN`) of a scene by file name, from BuildSettings.m_Scenes in globalgamemanagers.
    pub fn scene_level(&self, name: &str) -> Option<usize> {
        let b = &self.0.bundle;
        let gi = b.nodes.iter().position(|n| n.path == "globalgamemanagers")?;
        let sf = unity_fs::SerializedFile::open(b, &b.nodes[gi]).ok()?;
        let o = sf.objects.iter().find(|o| o.class_id == 141)?;
        let d = sf.object_data(b, &b.nodes[gi], o).ok()?;
        let mut r = unity_fs::ObjReader::new(&d, sf.big_endian);
        let n = r.i32().ok()?;
        for i in 0..n {
            let path = r.string().ok()?;
            let file = path.rsplit('/').next().unwrap_or(&path).trim_end_matches(".unity");
            if file == name {
                return b.nodes.iter().position(|x| x.path == format!("level{i}"));
            }
        }
        None
    }

    /// A scene's baked light probes (the LightProbes object of its `sharedassetsN.assets`), in the exported JSON layout
    /// (`m_Data.m_Positions`, `m_Data.m_Tetrahedralization`, `m_BakedCoefficients`).
    pub fn light_probes_json(&self, scene: &str) -> Option<serde_json::Value> {
        let b = &self.0.bundle;
        let level = self.scene_level(scene)?;
        let shared = b.nodes[level].path.replace("level", "sharedassets") + ".assets";
        let n = b.nodes.iter().find(|n| n.path == shared)?;
        let sf = unity_fs::SerializedFile::open(b, n).ok()?;
        let o = sf.objects.iter().find(|o| o.class_id == 258)?;
        let d = sf.object_data(b, n, o).ok()?;
        let mut r = unity_fs::ObjReader::new(&d, sf.big_endian);
        let read = |r: &mut unity_fs::ObjReader| -> unity_fs::Result<serde_json::Value> {
            use serde_json::json;
            let v3 = |r: &mut unity_fs::ObjReader| -> unity_fs::Result<serde_json::Value> { Ok(json!({ "m_X": r.f32()?, "m_Y": r.f32()?, "m_Z": r.f32()? })) };
            r.string()?;
            let mut tets = Vec::new();
            for _ in 0..r.i32()? {
                let mut t = serde_json::Map::new();
                for k in 0..4 {
                    t.insert(format!("m_Indices_{k}_"), json!(r.i32()?));
                }
                for _ in 0..4 + 12 {
                    r.i32()?; // neighbours, matrix
                }
                tets.push(serde_json::Value::Object(t));
            }
            let rays = (0..r.i32()?).map(|_| v3(r)).collect::<unity_fs::Result<Vec<_>>>()?;
            for _ in 0..r.i32()? * 6 {
                r.i32()?; // m_ProbeSets (Hash128, offset, size)
            }
            let pos = (0..r.i32()?).map(|_| v3(r)).collect::<unity_fs::Result<Vec<_>>>()?;
            for _ in 0..r.i32()? * 5 {
                r.i32()?; // m_NonTetrahedralizedProbeSetIndexMap
            }
            let mut sh = Vec::new();
            for _ in 0..r.i32()? {
                let mut c = serde_json::Map::new();
                for k in 0..27 {
                    c.insert(format!("m_Sh{:_>3}_", k), json!(r.f32()?));
                }
                sh.push(serde_json::Value::Object(c));
            }
            Ok(json!({ "m_Data": { "m_Positions": pos, "m_Tetrahedralization": { "m_Tetrahedra": tets, "m_HullRays": rays } }, "m_BakedCoefficients": sh }))
        };
        read(&mut r).map_err(|e| warn!("{scene} light probes: {e}")).ok()
    }

    /// A PostProcessProfile asset as the YAML lines `postfx` reads (`m_Name` / `active` per effect, then
    /// `param:` / `overrideState:` / `value:`), built from the installation's MonoBehaviours.
    pub fn pp_profile_yaml(&self, name: &str) -> Option<String> {
        let asm = self.assemblies()?;
        let b = &self.0.bundle;
        let (node, pid) = self.same_named(114, name)?.into_iter().next()?;
        let n = &b.nodes[node];
        let sf = unity_fs::SerializedFile::open(b, n).ok()?;
        let o = sf.objects.iter().find(|o| o.path_id == pid)?;
        let d = sf.object_data(b, n, o).ok()?;
        let mut r = unity_fs::ObjReader::new(&d, sf.big_endian);
        r.pptr().ok()?;
        r.bool().ok()?;
        r.align();
        r.pptr().ok()?;
        r.string().ok()?;
        let count = r.i32().ok()?;
        let fmt = |v: &serde_json::Value| -> String {
            match v {
                serde_json::Value::Bool(x) => (*x as u8).to_string(),
                serde_json::Value::Object(m) => {
                    let keys: &[&str] = if m.contains_key("r") { &["r", "g", "b", "a"] } else { &["x", "y", "z", "w"] };
                    let parts: Vec<String> = keys.iter().filter_map(|k| m.get(*k).map(|x| format!("{k}: {x}"))).collect();
                    format!("{{{}}}", parts.join(", "))
                }
                x => x.to_string(),
            }
        };
        let mut out = String::new();
        for _ in 0..count {
            let (fid, ep) = r.pptr().ok()?;
            if fid != 0 {
                continue;
            }
            let (class, fields) = crate::prefab_json::script_object(self, asm, node, ep).map_err(|e| warn!("{name}: {e}")).ok()?;
            out += &format!("  m_Name: {}
  active: {}
", fields.get("m_Name").and_then(|v| v.as_str()).unwrap_or(&class), fmt(fields.get("active").unwrap_or(&serde_json::Value::Null)));
            for (k, v) in &fields {
                let (Some(st), Some(val)) = (v.get("overrideState"), v.get("value")) else { continue };
                out += &format!("  {k}:
    overrideState: {}
    value: {}
", fmt(st), fmt(val));
            }
        }
        Some(out)
    }

    /// An effect material (`data/vfx/mat/<name>.json` layout) built from the installation's Material of that name.
    pub fn material_json(&self, name: &str) -> Option<serde_json::Value> {
        let (node, pid) = match self.same_named(21, name) {
            Some(v) => v[0],
            // "<name>_N": the (N+2)th material of that name (the old exports' duplicate suffix)
            None => {
                let (base, d) = name.rsplit_once('_')?;
                *self.same_named_ci(21, base)?.get(d.parse::<usize>().ok()? + 1)?
            }
        };
        crate::prefab_json::material_json(self, self.assemblies()?, node, pid).map_err(|e| warn!("material {name}: {e}")).ok()
    }

    pub fn scene_glb(&self, name: &str) -> Result<Vec<u8>, String> {
        let level = self.scene_level(name).ok_or_else(|| format!("scene {name} not in BuildSettings"))?;
        crate::owned_gltf::scene_glb(self, level)
    }

    /// Every Cubemap (class 89) by name, all same-named ones in (file, path id) order.
    fn cubemap_index(&self) -> &HashMap<String, Vec<(usize, i64)>> {
        self.0.cubemaps.get_or_init(|| {
            let b = &self.0.bundle;
            let mut map: HashMap<String, Vec<(usize, i64)>> = HashMap::new();
            for (i, n) in b.nodes.iter().enumerate() {
                let Ok(sf) = unity_fs::SerializedFile::open(b, n) else { continue };
                let mut objs: Vec<_> = sf.objects.iter().filter(|o| o.class_id == 89).collect();
                objs.sort_by_key(|o| o.path_id);
                for o in objs {
                    let Ok(head) = b.read_node(n, sf.data_offset + o.start, (o.size as u64).min(260)) else { continue };
                    if let Some(name) = unity_fs::object_name(&head, sf.big_endian) {
                        map.entry(name).or_default().push((i, o.path_id));
                    }
                }
            }
            map
        })
    }

    /// A Cubemap as the old exports' image: the 6 faces (level 0, top row first) stacked vertically, RGBA8.
    /// `file`: prefer the cubemap stored in that bundle file (several scenes have a "ReflectionProbe-0").
    pub fn cubemap_strip(&self, name: &str, file: Option<&str>) -> Result<(u32, Vec<u8>, i32), String> {
        let idx = self.cubemap_index();
        let cands = idx.get(name).ok_or_else(|| format!("cubemap {name} not in the installation"))?;
        let b = &self.0.bundle;
        let &(ni, pid) = file.and_then(|f| cands.iter().find(|(n, _)| b.nodes[*n].path == f)).or(cands.first()).ok_or("no cubemap")?;
        let n = &b.nodes[ni];
        let sf = unity_fs::SerializedFile::open(b, n)?;
        let o = sf.objects.iter().find(|o| o.path_id == pid).ok_or("object vanished")?;
        let t = unity_fs::Texture2D::read(&sf.object_data(b, n, o)?, sf.big_endian)?;
        let data = t.pixels(b, &self.0.data_dir)?;
        let gf = unity_fs::GpuFormat::from_unity(t.format).ok_or_else(|| format!("{name}: cubemap format {} not supported yet", t.format))?;
        let w = t.width as u32;
        let chain: usize = (0..t.mip_count.max(1) as u32).map(|m| gf.level_size((w >> m).max(1), (w >> m).max(1))).sum();
        let level0 = gf.level_size(w, w);
        let mut strip = Vec::with_capacity((w * w * 4 * 6) as usize);
        for f in 0..6 {
            let face = data.get(f * chain..f * chain + level0).ok_or("cubemap data too short")?;
            let top_first = decode_level_rgba(gf, w, w, face)?;
            strip.extend(top_first.chunks_exact(w as usize * 4).rev().flatten());
        }
        Ok((w, strip, t.color_space))
    }

    /// The game's managed assemblies (STRAFTAT_Data/Managed), parsed once.
    pub fn assemblies(&self) -> Option<&crate::dotnet_meta::Assemblies> {
        self.0.assemblies.get_or_init(|| crate::dotnet_meta::Assemblies::load(&self.0.data_dir.join("Managed")).map_err(|e| error!("Managed assemblies: {e}")).ok()).as_ref()
    }

    /// Prefab data (the extractor's JSON layout) of the prefab with that name, built in place.
    pub fn prefab_json(&self, name: &str, prefer: &str) -> Result<serde_json::Value, String> {
        let res = (prefer == "resources.assets").then(|| self.resources().get(&format!("randomweapons/{}", name.to_lowercase())).copied()).flatten();
        let (node, pid) = res.or_else(|| self.resolve_prefab(name, prefer)).ok_or_else(|| format!("prefab {name} not in the installation"))?;
        crate::prefab_json::prefab_json(self, self.assemblies().ok_or("no managed assemblies")?, node, pid)
    }

    /// `prefer`: bundle file to take a prefab from when several share the name (the game's random weapons are in
    /// `resources.assets`, projectiles of the same name in `sharedassets*`).
    pub fn resolve_prefab(&self, name: &str, prefer: &str) -> Option<(usize, i64)> {
        let idx = self.prefab_index();
        let pick = |v: &Vec<(usize, i64)>| v.iter().find(|(n, _)| self.0.bundle.nodes[*n].path.starts_with(prefer)).or(v.first()).copied();
        if let Some(v) = idx.get(name) {
            return pick(v);
        }
        let n = norm_name(name);
        if let Some((_, v)) = idx.iter().find(|(k, _)| norm_name(k) == n) {
            return pick(v);
        }
        let (base, d) = name.rsplit_once('_')?;
        if !d.chars().all(|c| c.is_ascii_digit()) || d.is_empty() {
            return None;
        }
        // "<name>_N": the (N+2)th prefab of that name (the old exports' duplicate suffix)
        if let Some(v) = idx.get(base).filter(|v| v.len() > 1) {
            return d.parse::<usize>().ok().and_then(|i| v.get(i + 1)).copied().or_else(|| pick(v));
        }
        self.resolve_prefab(base, prefer)
    }

    /// All prefabs (root GameObjects) of that name, in index order.
    pub fn prefab_entries(&self, name: &str) -> Option<&Vec<(usize, i64)>> {
        self.prefab_index().get(name)
    }

    /// The prefab as an in-memory GLB (`owned_gltf.rs`).
    pub fn prefab_glb(&self, name: &str, prefer: &str) -> Result<Vec<u8>, String> {
        // The game's weapons are Resources prefabs ("randomweapons/<name>"); projectiles etc. share some names.
        let res = (prefer == "resources.assets").then(|| self.resources().get(&format!("randomweapons/{}", name.to_lowercase())).copied()).flatten();
        let (node, pid) = res.or_else(|| self.resolve_prefab(name, prefer)).ok_or_else(|| format!("prefab {name} not in the installation"))?;
        crate::owned_gltf::prefab_glb(self, node, pid)
    }

    /// The AudioClip name for an exported file stem (same matching rules as textures).
    pub fn resolve_audio(&self, name: &str) -> Option<String> {
        let idx = self.audio_index();
        if idx.contains_key(name) {
            return Some(name.to_string());
        }
        let n = norm_name(name);
        if let Some(k) = idx.keys().find(|k| norm_name(k) == n) {
            return Some(k.clone());
        }
        let (base, d) = name.rsplit_once('_')?;
        (!d.is_empty() && d.chars().all(|c| c.is_ascii_digit())).then(|| self.resolve_audio(base)).flatten()
    }

    fn setups(&self) -> &crate::fsb5::SetupSource {
        self.0.setups.get_or_init(|| {
            let dll = std::fs::read(self.0.root.join("UnityPlayer.dll")).ok().and_then(|d| crate::fsb5::fmod_vorbis_setups(&d).ok()).unwrap_or_default();
            let qualities = install::config_dir()
                .and_then(|d| std::fs::read_to_string(d.join(format!("vorbis-{}.tsv", self.cache_key()))).ok())
                .map(|t| t.lines().filter_map(|l| l.split_once('\t')).filter_map(|(c, q)| Some((u32::from_str_radix(c, 16).ok()?, q.parse().ok()?))).collect())
                .unwrap_or_default();
            crate::fsb5::SetupSource { dll, qualities: std::sync::Mutex::new(qualities) }
        })
    }

    /// An AudioClip decoded in place (FSB5 -> PCM16), as a WAV file.
    pub fn audio_wav(&self, name: &str) -> Result<Vec<u8>, String> {
        let name = self.resolve_audio(name).ok_or_else(|| format!("audio clip {name} not in the installation"))?;
        if let Some(w) = self.0.wav_cache.lock().unwrap().get(&name) {
            return Ok(w.as_ref().clone());
        }
        let &(ni, pid) = self.audio_index().get(&name).ok_or("audio index")?;
        let b = &self.0.bundle;
        let n = &b.nodes[ni];
        let sf = unity_fs::SerializedFile::open(b, n)?;
        let o = sf.objects.iter().find(|o| o.path_id == pid).ok_or("object vanished")?;
        let clip = unity_fs::AudioClip::read(&sf.object_data(b, n, o)?, sf.big_endian)?;
        let fsb = unity_fs::read_resource(b, &self.0.data_dir, &clip.resource)?;
        let setups = self.setups();
        let before = setups.qualities.lock().unwrap().len();
        let pcm = crate::fsb5::decode(&fsb, &|c, ch, r| setups.get(c, ch, r))?;
        let q = setups.qualities.lock().unwrap();
        if q.len() != before {
            if let Some(d) = install::config_dir() {
                let _ = std::fs::create_dir_all(&d);
                let _ = std::fs::write(d.join(format!("vorbis-{}.tsv", self.cache_key())), q.iter().map(|(c, q)| format!("{c:08x}\t{q}\n")).collect::<String>());
            }
        }
        let wav = pcm.wav();
        if wav.len() < 4 << 20 {
            self.0.wav_cache.lock().unwrap().insert(name, Arc::new(wav.clone()));
        }
        Ok(wav)
    }

    /// The Texture2D name for `name`, also when it was written in the old exports' sanitized form
    /// ("tenue_blanche" for "tenue blanche", "eboueur" for "Éboueur").
    /// The Texture2D object for an exported file stem: `resolve_texture`, except that "<name>_N" picks the (N+2)th
    /// texture of that name (the old exports' duplicate suffix) when there are several.
    fn texture_ref(&self, name: &str) -> Option<(usize, i64)> {
        if !self.texture_index().contains_key(name) {
            if let Some((base, n)) = name.rsplit_once('_').filter(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
                if let Some(v) = self.same_named_ci(28, base).filter(|v| v.len() > 1) {
                    if let Some(&r) = n.parse::<usize>().ok().and_then(|i| v.get(i + 1)) {
                        return Some(r);
                    }
                }
            }
        }
        let name = self.resolve_texture(name)?;
        self.texture_index().get(&name).copied()
    }

    pub fn resolve_texture(&self, name: &str) -> Option<String> {
        let idx = self.texture_index();
        if idx.contains_key(name) {
            return Some(name.to_string());
        }
        let norm = self.0.textures_norm.get_or_init(|| idx.keys().map(|k| (norm_name(k), k.clone())).collect());
        if let Some(n) = norm.get(&norm_name(name)) {
            return Some(n.clone());
        }
        // "<name>_<n>": the exports' suffix for a second texture of the same name (the copies checked so far are
        // identical images, e.g. T_MuzzleFlash_Blast_00_M path ids 829 / 1096).
        let (base, n) = name.rsplit_once('_')?;
        (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then(|| self.resolve_texture(base)).flatten()
    }

    /// Pixel size of a texture (mip 0).
    pub fn texture_size(&self, name: &str) -> Option<(u32, u32)> {
        let n = self.resolve_texture(name)?;
        let (t, _) = self.texture_object(&n).ok()?;
        Some((t.width as u32, t.height as u32))
    }

    /// A Texture2D by name: its header and stored pixel data (all mips).
    /// The master volume (dB) of an AudioMixer in its start snapshot. Players keep it in the baked mixer constant:
    /// the group without a parent, its `volumeIndex` into the start snapshot's `values`.
    pub fn mixer_master_db(&self, name: &str) -> Option<f32> {
        let asm = self.assemblies()?;
        let (ni, pid) = *self.same_named(241, name)?.first()?;
        let (_, f) = crate::prefab_json::script_object(self, asm, ni, pid).map_err(|e| warn!("mixer {name}: {e}")).ok()?;
        let c = f.get("m_MixerConstant")?;
        let master = c["groups"].as_array()?.iter().find(|g| g["parentConstantIndex"].as_i64() == Some(-1))?;
        let vi = master["volumeIndex"].as_u64()? as usize;
        // the start snapshot (by GUID; the first one otherwise)
        let snaps = c["snapshots"].as_array()?;
        let start = (|| {
            let sp = f.get("m_StartSnapshot")?;
            let target = self.mixer_snapshot_guid(ni, sp)?;
            c["snapshotGUIDs"].as_array()?.iter().position(|g| *g == target)
        })()
        .unwrap_or(0);
        snaps.get(start)?["values"].as_array()?.get(vi)?.as_f64().map(|v| v as f32)
    }

    /// m_SnapshotID of the snapshot an AudioMixerController's `m_StartSnapshot` field names.
    fn mixer_snapshot_guid(&self, node: usize, start: &serde_json::Value) -> Option<serde_json::Value> {
        let name = start.as_str()?.rsplit('/').next()?.rsplit_once('.')?.0.to_string();
        let asm = self.assemblies()?;
        let v = self.same_named(245, &name)?;
        let &(sn, sp) = v.iter().find(|(n, _)| *n == node).or(v.first())?;
        let (_, f) = crate::prefab_json::script_object(self, asm, sn, sp).ok()?;
        f.get("m_SnapshotID").cloned()
    }

    /// A Sprite's packed rect in its texture (`m_RD.textureRect`: x, y, width, height; Unity's bottom-left origin).
    pub fn sprite_rect(&self, name: &str) -> Option<[f32; 4]> {
        let (ni, pid) = *self.same_named(213, name)?.first()?;
        let (_, f) = crate::prefab_json::script_object(self, self.assemblies()?, ni, pid).map_err(|e| warn!("sprite {name}: {e}")).ok()?;
        let r = &f.get("m_RD")?["textureRect"];
        let g = |k: &str| r[k].as_f64().map(|v| v as f32);
        Some([g("x")?, g("y")?, g("width")?, g("height")?])
    }

    /// A texture's header (size, format, mips, colour space) without its pixels; `name` may be a file name.
    pub fn texture_header(&self, name: &str) -> Option<unity_fs::Texture2D> {
        let stem = name.rsplit('/').next().unwrap_or(name);
        let stem = stem.strip_suffix(".png").unwrap_or(stem);
        let (ni, pid) = self.texture_ref(stem)?;
        let b = &self.0.bundle;
        let n = &b.nodes[ni];
        let sf = unity_fs::SerializedFile::open(b, n).ok()?;
        let o = sf.objects.iter().find(|o| o.path_id == pid)?;
        unity_fs::Texture2D::read(&sf.object_data(b, n, o).ok()?, sf.big_endian).ok()
    }

    /// A Texture2D by (node, path id): header and level data.
    pub fn texture_object_at(&self, ni: usize, pid: i64) -> Result<(unity_fs::Texture2D, Vec<u8>), String> {
        let b = &self.0.bundle;
        let n = &b.nodes[ni];
        let sf = unity_fs::SerializedFile::open(b, n)?;
        let o = sf.objects.iter().find(|o| o.path_id == pid).ok_or("texture missing")?;
        let t = unity_fs::Texture2D::read(&sf.object_data(b, n, o)?, sf.big_endian)?;
        let data = t.pixels(b, &self.0.data_dir)?;
        Ok((t, data))
    }

    /// A Texture2D by (node, path id) decoded to RGBA8 (mip 0, top row first).
    pub fn rgba_texture_at(&self, ni: usize, pid: i64) -> Result<(u32, u32, Vec<u8>, unity_fs::Texture2D), String> {
        let (t, data) = self.texture_object_at(ni, pid)?;
        let gf = unity_fs::GpuFormat::from_unity(t.format).ok_or_else(|| format!("{}: texture format {} not supported yet", t.name, t.format))?;
        let (w, h) = (t.width as u32, t.height as u32);
        let len = gf.level_size(w, h);
        if data.len() < len {
            return Err(format!("{}: {} bytes, level 0 needs {len}", t.name, data.len()));
        }
        let rgba = decode_level_rgba(gf, w, h, &data[..len])?;
        Ok((w, h, rgba, t))
    }

    /// A Texture2D's header by (node, path id).
    pub fn texture_header_at(&self, ni: usize, pid: i64) -> Option<unity_fs::Texture2D> {
        let b = &self.0.bundle;
        let n = &b.nodes[ni];
        let sf = unity_fs::SerializedFile::open(b, n).ok()?;
        let o = sf.objects.iter().find(|o| o.path_id == pid)?;
        unity_fs::Texture2D::read(&sf.object_data(b, n, o).ok()?, sf.big_endian).ok()
    }

    /// A Mesh by (node, path id).
    pub fn mesh_at(&self, ni: usize, pid: i64) -> Option<unity_fs::Mesh> {
        let b = &self.0.bundle;
        let n = &b.nodes[ni];
        let sf = unity_fs::SerializedFile::open(b, n).ok()?;
        let o = sf.objects.iter().find(|o| o.path_id == pid)?;
        unity_fs::Mesh::read(&sf.object_data(b, n, o).ok()?, sf.big_endian, b, &self.0.data_dir).ok()
    }

    /// The bundle node of `unity_builtin_extra`.
    pub fn builtin_extra_node(&self) -> Option<usize> {
        self.0.bundle.nodes.iter().position(|n| n.path.ends_with("unity_builtin_extra"))
    }

    pub fn texture_object(&self, name: &str) -> Result<(unity_fs::Texture2D, Vec<u8>), String> {
        let (ni, pid) = self.texture_ref(name).ok_or_else(|| format!("texture {name} not in the installation"))?;
        let b = &self.0.bundle;
        let n = &b.nodes[ni];
        let sf = unity_fs::SerializedFile::open(b, n)?;
        let o = sf.objects.iter().find(|o| o.path_id == pid).ok_or("object vanished")?;
        let t = unity_fs::Texture2D::read(&sf.object_data(b, n, o)?, sf.big_endian)?;
        let data = t.pixels(b, &self.0.data_dir)?;
        Ok((t, data))
    }

    /// A Texture2D by name decoded to RGBA8 (mip 0, top row first), for code that edits pixels on the CPU.
    pub fn rgba_texture(&self, name: &str) -> Result<(u32, u32, Vec<u8>, unity_fs::Texture2D), String> {
        let (t, data) = self.texture_object(name)?;
        let gf = unity_fs::GpuFormat::from_unity(t.format).ok_or_else(|| format!("{name}: texture format {} not supported yet", t.format))?;
        let (w, h) = (t.width as u32, t.height as u32);
        let len = gf.level_size(w, h);
        if data.len() < len {
            return Err(format!("{name}: {} bytes, level 0 needs {len}", data.len()));
        }
        let rgba = decode_level_rgba(gf, w, h, &data[..len])?;
        Ok((w, h, rgba, t))
    }

    /// A Texture2D by name as a Bevy image, uploaded in its stored GPU format with all mips.
    pub fn texture(&self, name: &str) -> Result<Image, String> {
        let (t, data) = self.texture_object(name)?;
        build_image(&Utex::from_texture(&t), &data, &ImageLoaderSettings::default())
    }

    /// Process-wide installation, opened on first use (asset readers run before any resource exists).
    pub fn global() -> Option<&'static Owned> {
        static G: OnceLock<Option<Owned>> = OnceLock::new();
        G.get_or_init(|| match Owned::open() {
            Ok(o) => Some(o),
            Err(e) => {
                error!("STRAFTAT installation not available: {e} (run `cargo run --release --bin setup`)");
                None
            }
        })
        .as_ref()
    }
}

/// `assets/data/<dir>/<name>.json`: the file when it exists, else the same prefab data built in place.
pub fn data_text(path: &str) -> Option<String> {
    if let Ok(t) = std::fs::read_to_string(path) {
        return Some(t);
    }
    static CACHE: OnceLock<std::sync::Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap().get(path) {
        return v.clone();
    }
    let v = data_text_build(path);
    cache.lock().unwrap().insert(path.to_string(), v.clone());
    v
}

/// Whether prefab data for `path` exists on disk or can be built in place.
pub fn data_exists(path: &str) -> bool {
    std::path::Path::new(path).exists() || data_text(path).is_some()
}

fn data_text_build(path: &str) -> Option<String> {
    let p = path.replace('\\', "/");
    let rest = p.split("data/").nth(1)?;
    let (dir, file) = rest.split_once('/')?;
    let stem = file.strip_suffix(".json")?;
    if dir == "maps" {
        // a whole scene (TrainingRange_00): every root of its levelN
        let o = Owned::global()?;
        let level = o.scene_level(stem)?;
        return match crate::prefab_json::scene_json(o, o.assemblies()?, level) {
            Ok(mut v) => {
                v["name"] = serde_json::json!(stem);
                serde_json::to_string(&v).ok()
            }
            Err(e) => {
                warn!("{path}: {e}");
                None
            }
        };
    }
    if !matches!(dir, "weapons" | "projectiles" | "vfx" | "props" | "map" | "hats" | "player") {
        return None;
    }
    let prefer = if dir == "weapons" { "resources.assets" } else { "sharedassets" };
    match Owned::global()?.prefab_json(stem, prefer) {
        Ok(v) => serde_json::to_string(&v).ok(),
        Err(e) => {
            warn!("{path}: {e}");
            None
        }
    }
}

/// The game's weapons: the Resources table's "randomweapons/<name>" prefabs (root GameObject names).
pub fn weapon_names() -> Vec<String> {
    let Some(owned) = Owned::global() else { return vec![] };
    let b = &owned.0.bundle;
    let mut v: Vec<String> = owned
        .resources()
        .iter()
        .filter(|(k, _)| k.starts_with("randomweapons/"))
        .filter_map(|(_, &(n, p))| {
            let sf = unity_fs::SerializedFile::open(b, &b.nodes[n]).ok()?;
            let o = sf.objects.iter().find(|o| o.path_id == p && o.class_id == 1)?;
            unity_fs::GameObject::read(&sf.object_data(b, &b.nodes[n], o).ok()?, sf.big_endian).ok().map(|g| g.name)
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Texture header passed from the asset reader to the loader (`.utex` stream: "UTEX", 9 little-endian i32, pixels).
#[derive(Clone, Copy, Debug)]
pub struct Utex {
    pub width: i32,
    pub height: i32,
    pub format: i32,
    pub mips: i32,
    pub color_space: i32,
    pub wrap_u: i32,
    pub wrap_v: i32,
    pub filter: i32,
    pub aniso: i32,
}

impl Utex {
    const MAGIC: &'static [u8; 4] = b"UTEX";
    pub fn from_texture(t: &unity_fs::Texture2D) -> Utex {
        Utex { width: t.width, height: t.height, format: t.format, mips: t.mip_count, color_space: t.color_space, wrap_u: t.wrap.0, wrap_v: t.wrap.1, filter: t.filter, aniso: t.aniso }
    }
    fn encode(&self, data: &[u8]) -> Vec<u8> {
        let mut v = Self::MAGIC.to_vec();
        for x in [self.width, self.height, self.format, self.mips, self.color_space, self.wrap_u, self.wrap_v, self.filter, self.aniso] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(data);
        v
    }
    fn decode(b: &[u8]) -> Result<(Utex, &[u8]), String> {
        if b.len() < 40 || &b[..4] != Self::MAGIC {
            return Err("not a utex stream".into());
        }
        let i = |k: usize| i32::from_le_bytes(b[4 + 4 * k..8 + 4 * k].try_into().unwrap());
        Ok((Utex { width: i(0), height: i(1), format: i(2), mips: i(3), color_space: i(4), wrap_u: i(5), wrap_v: i(6), filter: i(7), aniso: i(8) }, &b[40..]))
    }
}

/// Bevy image in the stored GPU format. `settings.is_srgb = false` forces linear sampling; a non-default
/// `settings.sampler` replaces Unity's sampler settings (same meaning as for the PNG loader).
/// Utex format code for RGBA8 rows already top row first (cubemap strips), loaded as is.
pub const UTEX_RGBA8_TOP_FIRST: i32 = 1004;

pub fn build_image(t: &Utex, data: &[u8], settings: &ImageLoaderSettings) -> Result<Image, String> {
    if t.format == UTEX_RGBA8_TOP_FIRST {
        let fmt = if t.color_space == 1 && settings.is_srgb { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm };
        let mut img = Image::new(Extent3d { width: t.width as u32, height: t.height as u32, depth_or_array_layers: 1 }, TextureDimension::D2, data.to_vec(), fmt, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD);
        img.sampler = settings.sampler.clone();
        return Ok(img);
    }
    let srgb = t.color_space == 1 && settings.is_srgb;
    let gf = unity_fs::GpuFormat::from_unity(t.format).ok_or_else(|| format!("texture format {} not supported yet", t.format))?;
    use unity_fs::GpuFormat as G;
    let format = match (gf, srgb) {
        (G::Bc1, true) => TextureFormat::Bc1RgbaUnormSrgb,
        (G::Bc1, false) => TextureFormat::Bc1RgbaUnorm,
        (G::Bc3, true) => TextureFormat::Bc3RgbaUnormSrgb,
        (G::Bc3, false) => TextureFormat::Bc3RgbaUnorm,
        (G::Bc7, true) => TextureFormat::Bc7RgbaUnormSrgb,
        (G::Bc7, false) => TextureFormat::Bc7RgbaUnorm,
        (G::Bc4, _) => TextureFormat::Bc4RUnorm,
        (G::Bc5, _) => TextureFormat::Bc5RgUnorm,
        (G::Bc6h, _) => TextureFormat::Bc6hRgbUfloat,
        (G::Rgba8 | G::Rgb8, true) => TextureFormat::Rgba8UnormSrgb,
        (G::Rgba8 | G::Rgb8, false) => TextureFormat::Rgba8Unorm,
        (G::R8 | G::Alpha8, _) => TextureFormat::R8Unorm,
        (G::Rgba16f, _) => TextureFormat::Rgba16Float,
    };
    let (w, h, mips) = (t.width.max(1) as u32, t.height.max(1) as u32, t.mips.max(1) as u32);
    let dims = |m: u32| ((w >> m).max(1), (h >> m).max(1));
    let need: usize = (0..mips).map(|m| gf.level_size(dims(m).0, dims(m).1)).sum();
    if data.len() < need {
        return Err(format!("{} bytes, mip chain needs {need}", data.len()));
    }
    // Unity rows are bottom-up: flip every level (in the stored format when possible, else decoded to RGBA8).
    let mut pixels = data[..need].to_vec();
    let mut format = format;
    let mut ok = gf != G::Rgb8;
    if ok {
        let mut off = 0;
        for m in 0..mips {
            let (lw, lh) = dims(m);
            let len = gf.level_size(lw, lh);
            if !flip_level(gf, lw, lh, &mut pixels[off..off + len]) {
                ok = false;
                break;
            }
            off += len;
        }
    }
    if !ok {
        let mut rgba = Vec::new();
        let mut off = 0;
        for m in 0..mips {
            let (lw, lh) = dims(m);
            let len = gf.level_size(lw, lh);
            rgba.extend(decode_level_rgba(gf, lw, lh, &data[off..off + len])?);
            off += len;
        }
        pixels = rgba;
        format = if srgb { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm };
    }
    let mut img = Image::new(Extent3d { width: w, height: h, depth_or_array_layers: 1 }, TextureDimension::D2, pixels, format, RenderAssetUsages::RENDER_WORLD);
    img.texture_descriptor.mip_level_count = mips;
    img.sampler = if matches!(settings.sampler, ImageSampler::Default) {
        let wrap = |w: i32| match w {
            1 => ImageAddressMode::ClampToEdge,
            2 | 3 => ImageAddressMode::MirrorRepeat,
            _ => ImageAddressMode::Repeat,
        };
        let filt = if t.filter == 0 { ImageFilterMode::Nearest } else { ImageFilterMode::Linear };
        ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: wrap(t.wrap_u),
            address_mode_v: wrap(t.wrap_v),
            mag_filter: filt,
            min_filter: filt,
            mipmap_filter: if t.filter == 2 { ImageFilterMode::Linear } else { ImageFilterMode::Nearest },
            anisotropy_clamp: if t.filter == 0 { 1 } else { (t.aniso.clamp(1, 16) as u16).next_power_of_two() },
            ..default()
        })
    } else {
        settings.sampler.clone()
    };
    Ok(img)
}

/// Unity stores texture rows bottom-up; Bevy / wgpu expect the top row first. Flips one mip level in place where the
/// format allows it without decoding (row formats; BC1/3/4/5 by reversing block rows and the index rows inside each
/// block). Returns false when the level has to be decoded instead (BC6H / BC7, or block heights not multiple of 4).
pub fn flip_level(gf: unity_fs::GpuFormat, w: u32, h: u32, level: &mut [u8]) -> bool {
    use unity_fs::GpuFormat as G;
    let bpp = match gf {
        G::Rgba8 => Some(4),
        G::Rgb8 => Some(3),
        G::R8 | G::Alpha8 => Some(1),
        G::Rgba16f => Some(8),
        _ => None,
    };
    if let Some(bpp) = bpp {
        let row = w as usize * bpp;
        let rows: Vec<Vec<u8>> = level.chunks_exact(row).map(<[u8]>::to_vec).collect();
        for (i, r) in rows.iter().rev().enumerate() {
            level[i * row..(i + 1) * row].copy_from_slice(r);
        }
        return true;
    }
    let block = match gf {
        G::Bc1 | G::Bc4 => 8,
        G::Bc3 | G::Bc5 => 16,
        _ => return false,
    };
    if h > 4 && h % 4 != 0 {
        return false;
    }
    let (bw, bh) = (w.div_ceil(4) as usize, h.div_ceil(4) as usize);
    let n = h.min(4) as usize; // valid pixel rows inside a block
    // BC1 colour indices: one byte per row.
    let flip_color = |b: &mut [u8]| b[4..4 + n].reverse();
    // BC3/BC4 alpha indices: 48 bits, 12 bits per row.
    let flip_alpha = |b: &mut [u8]| {
        let mut bits = 0u64;
        for (i, &x) in b[2..8].iter().enumerate() {
            bits |= (x as u64) << (8 * i);
        }
        let rows: Vec<u64> = (0..4).map(|r| (bits >> (12 * r)) & 0xfff).collect();
        let mut out = bits;
        for r in 0..n {
            out &= !(0xfffu64 << (12 * r));
            out |= rows[n - 1 - r] << (12 * r);
        }
        for i in 0..6 {
            b[2 + i] = (out >> (8 * i)) as u8;
        }
    };
    let row_bytes = bw * block;
    let src = level.to_vec();
    for by in 0..bh {
        let dst = bh - 1 - by;
        level[dst * row_bytes..(dst + 1) * row_bytes].copy_from_slice(&src[by * row_bytes..(by + 1) * row_bytes]);
    }
    for b in level.chunks_exact_mut(block) {
        match gf {
            G::Bc1 => flip_color(b),
            G::Bc4 => flip_alpha(b),
            G::Bc3 => {
                flip_alpha(&mut b[..8]);
                flip_color(&mut b[8..]);
            }
            G::Bc5 => {
                flip_alpha(&mut b[..8]);
                flip_alpha(&mut b[8..]);
            }
            _ => {}
        }
    }
    true
}

/// Decodes one level to RGBA8 rows, top row first.
pub fn decode_level_rgba(gf: unity_fs::GpuFormat, w: u32, h: u32, level: &[u8]) -> Result<Vec<u8>, String> {
    use unity_fs::GpuFormat as G;
    let (wu, hu) = (w as usize, h as usize);
    let mut px = vec![0u32; wu * hu];
    let r = match gf {
        G::Bc1 => texture2ddecoder::decode_bc1a(level, wu, hu, &mut px),
        G::Bc3 => texture2ddecoder::decode_bc3(level, wu, hu, &mut px),
        G::Bc4 => texture2ddecoder::decode_bc4(level, wu, hu, &mut px),
        G::Bc5 => texture2ddecoder::decode_bc5(level, wu, hu, &mut px),
        G::Bc6h => texture2ddecoder::decode_bc6_unsigned(level, wu, hu, &mut px),
        G::Bc7 => texture2ddecoder::decode_bc7(level, wu, hu, &mut px),
        G::Rgba8 => {
            return Ok(level.chunks_exact(wu * 4).rev().flatten().copied().collect());
        }
        G::Rgb8 => {
            return Ok(level.chunks_exact(wu * 3).rev().flat_map(|r| r.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255])).collect());
        }
        G::R8 => {
            return Ok(level.chunks_exact(wu).rev().flat_map(|r| r.iter().flat_map(|&v| [v, v, v, 255])).collect());
        }
        // alpha-only: the value is alpha (as the old exports and the TMP / particle shaders read it)
        G::Alpha8 => {
            return Ok(level.chunks_exact(wu).rev().flat_map(|r| r.iter().flat_map(|&v| [0, 0, 0, v])).collect());
        }
        G::Rgba16f => return Err("half-float textures are not decoded".into()),
    };
    r.map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(wu * hu * 4);
    for row in px.chunks_exact(wu).rev() {
        for p in row {
            let [b, g, r, a] = p.to_le_bytes();
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    Ok(out)
}

/// Loads `.utex` streams produced by [`OwnedFallbackReader`].
#[derive(Default, TypePath)]
pub struct UtexLoader;

impl AssetLoader for UtexLoader {
    type Asset = Image;
    type Settings = ImageLoaderSettings;
    type Error = std::io::Error;
    async fn load(&self, reader: &mut dyn Reader, settings: &ImageLoaderSettings, _: &mut LoadContext<'_>) -> Result<Image, Self::Error> {
        let mut b = Vec::new();
        reader.read_to_end(&mut b).await?;
        let (t, data) = Utex::decode(&b).map_err(std::io::Error::other)?;
        build_image(&t, data, settings).map_err(std::io::Error::other)
    }
    fn extensions(&self) -> &[&str] {
        &["utex"]
    }
}

/// Case-, accent- and space/underscore-insensitive key.
pub fn norm_name(s: &str) -> String {
    s.chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| match c {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'à' | 'â' | 'ä' => 'a',
            'ç' => 'c',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            ' ' | '-' | '(' | ')' => '_',
            c => c,
        })
        .fold(String::new(), |mut acc, c| {
            if !(c == '_' && acc.ends_with('_')) {
                acc.push(c);
            }
            acc
        })
}

/// Image file names the exports used (`T_Foo_00.png`, `Foo_0.png`, `foo.tga`) -> Texture2D names to try.
fn texture_names(path: &Path) -> Vec<String> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    if !matches!(ext.as_deref(), Some("png" | "tga" | "jpg" | "jpeg" | "exr" | "utex")) {
        return Vec::new();
    }
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { return Vec::new() };
    let mut v = vec![stem.to_string()];
    // AssetRipper appended _0, _1 ... to duplicate names
    if let Some((base, n)) = stem.rsplit_once('_') {
        if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
            v.push(base.to_string());
        }
    }
    v
}

/// The default asset source: files under game/assets as usual; an image that is not there is read in place from
/// the STRAFTAT installation (matched by texture name) and loaded by [`UtexLoader`], with a generated `.meta`.
pub struct OwnedFallbackReader {
    pub inner: bevy::asset::io::file::FileAssetReader,
    pub root: PathBuf,
}

impl OwnedFallbackReader {
    /// A font file that is not under game/assets: the Font object of that name, its embedded TTF / OTF bytes.
    fn owned_font(&self, path: &Path) -> Option<Vec<u8>> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if !matches!(ext.as_str(), "ttf" | "otf") || self.root.join(path).exists() {
            return None;
        }
        let stem = path.file_stem()?.to_str()?;
        let owned = Owned::global()?;
        let b = &owned.0.bundle;
        for n in &b.nodes {
            let Ok(sf) = unity_fs::SerializedFile::open(b, n) else { continue };
            for o in sf.objects.iter().filter(|o| o.class_id == 128) {
                let Ok(d) = sf.object_data(b, n, o) else { continue };
                if unity_fs::object_name(&d, sf.big_endian).is_some_and(|nm| norm_name(&nm) == norm_name(stem)) {
                    if let Ok(f) = unity_fs::font_data(&d, sf.big_endian) {
                        return Some(f);
                    }
                }
            }
        }
        None
    }

    /// A Cubemap of that name (no Texture2D of the name): the old exports' strip image.
    fn owned_cubemap(&self, path: &Path) -> Option<Vec<u8>> {
        if self.root.join(path).exists() || texture_names(path).is_empty() {
            return None;
        }
        let stem = path.file_stem()?.to_str()?;
        let owned = Owned::global()?;
        if owned.resolve_texture(stem).is_some() {
            return None;
        }
        let (w, strip, cs) = owned.cubemap_strip(stem, None).ok()?;
        let u = Utex { width: w as i32, height: w as i32 * 6, format: UTEX_RGBA8_TOP_FIRST, mips: 1, color_space: cs, wrap_u: 1, wrap_v: 1, filter: 1, aniso: 1 };
        Some(u.encode(&strip))
    }

    fn owned_name(&self, path: &Path) -> Option<String> {
        if self.root.join(path).exists() {
            return None;
        }
        let names = texture_names(path);
        if names.is_empty() {
            return None;
        }
        let owned = Owned::global()?;
        names.into_iter().find_map(|n| owned.resolve_texture(&n))
    }
}

async fn slurp<'a>(r: impl Reader + 'a) -> Result<Vec<u8>, AssetReaderError> {
    let mut r = r;
    let mut b = Vec::new();
    r.read_to_end(&mut b).await.map_err(|e| AssetReaderError::Io(std::sync::Arc::new(e)))?;
    Ok(b)
}

impl OwnedFallbackReader {
    /// A sound file that is not under game/assets: the AudioClip of that name, decoded in place, as WAV bytes
    /// (Bevy's audio loader keeps the bytes; rodio detects the format, so the `.ogg` path can stay).
    fn owned_audio(&self, path: &Path) -> Option<Vec<u8>> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if !matches!(ext.as_str(), "ogg" | "wav" | "mp3") || self.root.join(path).exists() {
            return None;
        }
        let stem = path.file_stem()?.to_str()?;
        match Owned::global()?.audio_wav(stem) {
            Ok(w) => {
                if std::env::var_os("RESTRAFE_DEBUG_OWNED").is_some() {
                    println!("OWNED in place: {} -> AudioClip {stem} ({} bytes WAV)", path.display(), w.len());
                }
                Some(w)
            }
            Err(e) => {
                warn!("{}: {e}", path.display());
                None
            }
        }
    }
}

impl OwnedFallbackReader {
    /// A .glb that is not under game/assets: the prefab of that name built in place.
    fn owned_glb(&self, path: &Path) -> Option<Vec<u8>> {
        if path.extension()?.to_str()? != "glb" || self.root.join(path).exists() {
            return None;
        }
        let stem = path.file_stem()?.to_str()?;
        let prefer = if path.to_string_lossy().replace('\\', "/").starts_with("models/weapons") { "resources.assets" } else { "sharedassets" };
        if std::env::var_os("RESTRAFE_DEBUG_OWNED").is_some() {
            println!("OWNED glb request {:?} prefer {prefer}", path);
        }
        let p = path.to_string_lossy().replace('\\', "/");
        let mesh_dir = ["cosmetics/body/", "cosmetics/hats/mesh/", "vfx/mesh/", "models/vfx/", "models/player/SM_", "models/player/SK_FPArm"].iter().any(|d| p.starts_with(d));
        let owned = Owned::global()?;
        let built = if p.starts_with("maps/") {
            owned.scene_glb(stem)
        } else if mesh_dir {
            owned.mesh_glb(stem)
        } else {
            owned.prefab_glb(stem, prefer).or_else(|e| owned.mesh_glb(stem).map_err(|_| e))
        };
        match built {
            Ok(g) => {
                if std::env::var_os("RESTRAFE_DEBUG_OWNED").is_some() {
                    println!("OWNED in place: {} -> prefab {stem} ({} bytes GLB)", path.display(), g.len());
                }
                Some(g)
            }
            Err(e) => {
                warn!("{}: {e}", path.display());
                None
            }
        }
    }
}

impl AssetReader for OwnedFallbackReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        if let Some(w) = self.owned_audio(path) {
            return Ok(VecReader::new(w));
        }
        if let Some(g) = self.owned_glb(path) {
            return Ok(VecReader::new(g));
        }
        if let Some(c) = self.owned_cubemap(path) {
            return Ok(VecReader::new(c));
        }
        if let Some(f) = self.owned_font(path) {
            return Ok(VecReader::new(f));
        }
        if let Some(name) = self.owned_name(path) {
            let owned = Owned::global().unwrap();
            if std::env::var_os("RESTRAFE_DEBUG_OWNED").is_some() {
                println!("OWNED in place: {} -> Texture2D {name}", path.display());
            }
            let (t, data) = owned.texture_object(&name).map_err(|e| AssetReaderError::Io(std::sync::Arc::new(std::io::Error::other(e))))?;
            return Ok(VecReader::new(Utex::from_texture(&t).encode(&data)));
        }
        Ok(VecReader::new(slurp(self.inner.read(path).await?).await?))
    }
    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        if self.owned_name(path).is_some() || (!self.root.join(path).exists() && !texture_names(path).is_empty() && path.file_stem().and_then(|s| s.to_str()).and_then(|s| Owned::global()?.cubemap_index().get(s).map(|_| ())).is_some()) {
            let meta = AssetMeta::<UtexLoader, ()>::new(AssetAction::Load { loader: std::any::type_name::<UtexLoader>().to_string(), settings: ImageLoaderSettings::default() });
            return Ok(VecReader::new(meta.serialize()));
        }
        Ok(VecReader::new(slurp(self.inner.read_meta(path).await?).await?))
    }
    async fn read_directory<'a>(&'a self, path: &'a Path) -> Result<Box<PathStream>, AssetReaderError> {
        self.inner.read_directory(path).await
    }
    async fn is_directory<'a>(&'a self, path: &'a Path) -> Result<bool, AssetReaderError> {
        self.inner.is_directory(path).await
    }
}

/// Installs the in-place fallback as the default asset source (call before adding DefaultPlugins).
pub fn register_source(app: &mut App, root: &str) {
    if std::env::var_os("RESTRAFE_NO_OWNED_SOURCE").is_some() {
        return;
    }
    let root = PathBuf::from(root);
    app.register_asset_source(
        bevy::asset::io::AssetSourceId::Default,
        bevy::asset::io::AssetSourceBuilder::new(move || Box::new(OwnedFallbackReader { inner: bevy::asset::io::file::FileAssetReader::new(&root), root: root.clone() })),
    );
}

/// `RESTRAFE_OWNED_CHECK=<texture,...>`: opens the installation at startup and loads the named textures in place
/// (logged as OWNED lines), to check the in-place path inside the running game.
pub fn startup_check(mut images: ResMut<Assets<Image>>) {
    let Ok(list) = std::env::var("RESTRAFE_OWNED_CHECK") else { return };
    let t = std::time::Instant::now();
    match Owned::open() {
        Ok(owned) => {
            for name in list.split(',') {
                match owned.texture(name) {
                    Ok(img) => {
                        let (w, h, f, m) = (img.width(), img.height(), img.texture_descriptor.format, img.texture_descriptor.mip_level_count);
                        images.add(img);
                        println!("OWNED texture {name}: {w}x{h} {f:?} mips {m} ({:?})", t.elapsed());
                    }
                    Err(e) => println!("OWNED texture {name}: {e}"),
                }
            }
        }
        Err(e) => println!("OWNED open failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the installation and the old exported PNGs (pass their paths in RESTRAFE_FLIP_PNGS=name=path,...).
    #[test]
    fn block_flip_matches_exported_png() {
        let Ok(list) = std::env::var("RESTRAFE_FLIP_PNGS") else { return };
        let owned = Owned::open().expect("installation");
        for item in list.split(',') {
            let (name, png) = item.split_once('=').unwrap();
            let (t, data) = owned.texture_object(name).unwrap();
            let gf = unity_fs::GpuFormat::from_unity(t.format).unwrap();
            let (w, h) = (t.width as u32, t.height as u32);
            let mut level = data[..gf.level_size(w, h)].to_vec();
            assert!(flip_level(gf, w, h, &mut level), "{name}: not flippable in place");
            // decode the flipped blocks as stored (no further flip) and compare with the exported PNG
            let mut px = vec![0u32; (w * h) as usize];
            match gf {
                unity_fs::GpuFormat::Bc1 => texture2ddecoder::decode_bc1a(&level, w as usize, h as usize, &mut px),
                unity_fs::GpuFormat::Bc3 => texture2ddecoder::decode_bc3(&level, w as usize, h as usize, &mut px),
                _ => panic!("format"),
            }
            .unwrap();
            let img = Image::from_buffer(&std::fs::read(crate::owned::reference_file(png)).unwrap(), bevy::image::ImageType::Extension("png"), bevy::image::CompressedImageFormats::NONE, true, ImageSampler::Default, RenderAssetUsages::default()).unwrap();
            let rgba = img.try_into_dynamic().unwrap().to_rgba8();
            let mut diff = 0u64;
            for (i, p) in px.iter().enumerate() {
                let [b, g, r, a] = p.to_le_bytes();
                let q = rgba.as_raw()[i * 4..i * 4 + 4].to_vec();
                diff += [r, g, b, a].iter().zip(&q).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64).sum::<u64>();
            }
            let mean = diff as f64 / (w * h * 4) as f64;
            println!("{name}: mean abs diff {mean:.3}");
            assert!(mean < 1.0, "{name}: flipped data differs from the exported PNG ({mean})");
        }
    }
}

#[cfg(test)]
mod glb_tests {
    use super::*;

    /// RESTRAFE_GLB_DIRS=models/weapons,cosmetics/hats: writes the in-place GLB of every exported .glb found there to
    /// target/glbgen/<dir>/<name>.glb for comparison with the export (scratch compare script).
    #[test]
    fn generate_glbs_for_compare() {
        let Ok(dirs) = std::env::var("RESTRAFE_GLB_DIRS") else { return };
        let owned = Owned::open().expect("installation");
        let (mut ok, mut bad) = (0, 0);
        for dir in dirs.split(',') {
            let out = format!("target/glbgen/{dir}");
            std::fs::create_dir_all(&out).unwrap();
            for e in std::fs::read_dir(format!("assets/{dir}")).unwrap().flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("glb") {
                    continue;
                }
                let stem = p.file_stem().unwrap().to_str().unwrap().to_string();
                let prefer = if dir.starts_with("models/weapons") { "resources.assets" } else { "sharedassets" };
                let mesh_dir = ["cosmetics/body", "cosmetics/hats/mesh", "vfx/mesh", "models/vfx"].contains(&dir);
                let built = if dir == "maps" {
                    owned.scene_glb(&stem)
                } else if mesh_dir {
                    owned.mesh_glb(&stem)
                } else {
                    owned.prefab_glb(&stem, prefer).or_else(|e| owned.mesh_glb(&stem).map_err(|_| e))
                };
                match built {
                    Ok(g) => {
                        std::fs::write(format!("{out}/{stem}.glb"), g).unwrap();
                        ok += 1;
                    }
                    Err(e) => {
                        println!("FAIL {dir}/{stem}: {e}");
                        bad += 1;
                    }
                }
            }
        }
        println!("generated {ok}, failed {bad}");
    }
}

#[cfg(test)]
mod cube_tests {
    use super::*;
    /// With the old exports present: the in-place cubemap strips equal the exported PNGs (RESTRAFE_CUBE_CHECK=1).
    #[test]
    fn cubemap_strip_matches_export() {
        if !crate::owned::ref_checks("RESTRAFE_CUBE_CHECK") {
            return;
        }
        let owned = Owned::open().unwrap();
        let level = owned.scene_level("TrainingRange_00").map(|l| owned.0.bundle.nodes[l].path.replace("level", "sharedassets") + ".assets");
        for (name, file, png) in [
            ("ReflectionProbe-0", level.clone(), "assets/maps/reflection/TrainingRange_00_ReflectionProbe-0.png"),
            ("ReflectionProbe-1", level.clone(), "assets/maps/reflection/TrainingRange_00_ReflectionProbe-1.png"),
            ("Haze_WarmRainCloudsOnHorizon_EquiRect_flat", None, "assets/maps/sky/Haze_WarmRainCloudsOnHorizon_EquiRect_flat.png"),
        ] {
            let (w, strip, cs) = owned.cubemap_strip(name, file.as_deref()).unwrap();
            let img = Image::from_buffer(&std::fs::read(crate::owned::reference_file(png)).unwrap(), bevy::image::ImageType::Extension("png"), bevy::image::CompressedImageFormats::NONE, true, ImageSampler::Default, RenderAssetUsages::default()).unwrap();
            let rgba = img.try_into_dynamic().unwrap().to_rgba8();
            let raw = rgba.as_raw();
            let diff = |flip: bool| -> f64 {
                let mut d = 0u64;
                for f in 0..6usize {
                    for y in 0..w as usize {
                        let sy = if flip { w as usize - 1 - y } else { y };
                        for x in 0..w as usize * 4 {
                            let a = strip[((f * w as usize + sy) * w as usize) * 4 + x] as i64;
                            let b = raw[((f * w as usize + y) * w as usize) * 4 + x] as i64;
                            d += (a - b).unsigned_abs();
                        }
                    }
                }
                d as f64 / strip.len() as f64
            };
            println!("{name}: {w}px colorspace {cs}, mean abs diff {:.3} (faces as decoded) / {:.3} (faces flipped)", diff(false), diff(true));
        }
    }
}

#[cfg(test)]
mod font_tests {
    /// With the old export present: the in-place font bytes equal the exported .ttf (RESTRAFE_FONT_CHECK=1).
    #[test]
    fn owned_font_matches_export() {
        if !crate::owned::ref_checks("RESTRAFE_FONT_CHECK") {
            return;
        }
        let r = super::OwnedFallbackReader { inner: bevy::asset::io::file::FileAssetReader::new("assets"), root: std::path::PathBuf::from("assets-none") };
        for f in ["centurygothic.ttf", "GOTHICBI.ttf"] {
            let ours = r.owned_font(std::path::Path::new(&format!("fonts/{f}"))).expect("font in place");
            let theirs = std::fs::read(crate::owned::reference_file(&format!("assets/fonts/{f}"))).unwrap();
            println!("{f}: {} vs {} bytes, equal {}", ours.len(), theirs.len(), ours == theirs);
            assert_eq!(ours, theirs);
        }
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;
    /// Texture headers read in place agree with the old importer .meta files (sRGB, mipmaps, wrap U).
    #[test]
    fn texture_headers_match_meta() {
        let Ok(owned) = Owned::open() else { return };
        let dir = "../ripped/ExportedProject/Assets/Texture2D";
        let (mut n, mut bad, mut missing) = (0, 0, 0);
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(file) = name.strip_suffix(".meta") else { continue };
            let meta = std::fs::read_to_string(e.path()).unwrap();
            let Some(t) = owned.texture_header(file) else {
                missing += 1;
                continue;
            };
            let want = (!meta.contains("sRGBTexture: 0"), !meta.contains("enableMipMap: 0"), meta.contains("wrapU: 0"));
            let got = (t.color_space != 0, t.mip_count > 1, t.wrap.0 == 0);
            n += 1;
            if want != got {
                bad += 1;
                if bad <= 10 {
                    println!("TEX {file}: meta (srgb, mips, repeat) {want:?} vs install {got:?}");
                }
            }
        }
        println!("texture headers: {n} compared, {bad} differ, {missing} not found");
    }

    /// Mixer master volumes read in place equal the old .mixer files (Master group's m_Volume in the snapshot).
    #[test]
    fn mixer_volumes_match_export() {
        let Ok(owned) = Owned::open() else { return };
        let dir = "../ripped/ExportedProject/Assets/AudioMixerController";
        let mut n = 0;
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            let Some(stem) = file.strip_suffix(".mixer") else { continue };
            let txt = std::fs::read_to_string(e.path()).unwrap();
            let mut guid = None;
            let mut in_master = false;
            for l in txt.lines() {
                let t = l.trim();
                if let Some(nm) = t.strip_prefix("m_Name: ") {
                    in_master = nm == "Master";
                } else if let (true, Some(g)) = (in_master, t.strip_prefix("m_Volume: ")) {
                    guid = Some(g.to_string());
                }
            }
            let want = guid.and_then(|g| txt.lines().find_map(|l| l.trim().strip_prefix(&format!("{g}: ")).and_then(|v| v.parse::<f32>().ok()))).unwrap_or(0.0);
            let got = owned.mixer_master_db(stem);
            println!("{stem}: {want} dB, in place {got:?}");
            assert!(got.is_some_and(|g| (g - want).abs() < 1e-4), "{stem}");
            n += 1;
        }
        println!("{n} mixers match");
    }

    /// Sprite rects read in place equal the old sprite assets' m_Rect (the crosshair / hit marker sprites).
    #[test]
    fn sprite_rects_match_export() {
        let Ok(owned) = Owned::open() else { return };
        let dir = "../ripped/ExportedProject/Assets/Sprite";
        let mut n = 0;
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix(".asset").filter(|s| s.contains("Crosshair") || s.contains("Hitmarker")) else { continue };
            let txt = std::fs::read_to_string(e.path()).unwrap();
            let block = txt.split("  m_Rect:").nth(1).unwrap();
            let grab = |k: &str| -> f32 { block.lines().find(|l| l.trim_start().starts_with(k)).and_then(|l| l.split(':').nth(1)).and_then(|v| v.trim().parse().ok()).unwrap_or(0.0) };
            let want = [grab("x:"), grab("y:"), grab("width:"), grab("height:")];
            assert_eq!(owned.sprite_rect(stem), Some(want), "{stem}");
            n += 1;
        }
        println!("{n} sprite rects match");
    }

    /// The in-place light probes equal the exported ones (positions, SH, tetrahedra, hull rays).
    #[test]
    fn light_probes_match_export() {
        let Ok(owned) = Owned::open() else { return };
        for scene in ["TrainingRange_00", "MainMenu"] {
            let t = std::fs::read_to_string(crate::owned::reference_file(&format!("assets/maps/{scene}_LightProbes.json"))).expect("light probe reference");
            let r: serde_json::Value = serde_json::from_str(&t).unwrap();
            let g = owned.light_probes_json(scene).expect(scene);
            fn close(a: &serde_json::Value, b: &serde_json::Value) -> bool {
                match (a, b) {
                    (serde_json::Value::Object(a), serde_json::Value::Object(b)) => a.iter().all(|(k, v)| close(v, &b[k])),
                    (serde_json::Value::Array(a), serde_json::Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| close(x, y)),
                    _ => match (a.as_f64(), b.as_f64()) {
                        (Some(x), Some(y)) => (x - y).abs() <= 1e-5 * x.abs().max(1.0),
                        _ => a == b,
                    },
                }
            }
            for k in ["m_Positions", "m_Tetrahedralization"] {
                assert!(close(&g["m_Data"][k], &r["m_Data"][k]), "{scene} {k}");
            }
            assert!(close(&g["m_BakedCoefficients"], &r["m_BakedCoefficients"]), "{scene} SH");
            println!("{scene}: {} probes match", g["m_Data"]["m_Positions"].as_array().unwrap().len());
        }
    }

    /// The in-place effect materials equal the exported JSON (RESTRAFE_MAT_DIR, default assets/data/vfx/mat).
    #[test]
    fn vfx_materials_match_export() {
        let Ok(owned) = Owned::open() else { return };
        let dir = std::env::var("RESTRAFE_MAT_DIR").unwrap_or(crate::owned::reference_file("assets/data/vfx/mat"));
        let (mut ok, mut bad) = (0, 0);
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let stem = e.path().file_stem().unwrap().to_string_lossy().to_string();
            let r: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(e.path()).unwrap()).unwrap();
            let Some(g) = owned.material_json(&stem) else {
                println!("MAT {stem}: not built");
                bad += 1;
                continue;
            };
            let close = |a: &serde_json::Value, b: &serde_json::Value| -> bool {
                fn c(a: &serde_json::Value, b: &serde_json::Value) -> bool {
                    match (a, b) {
                        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| c(v, w))),
                        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| c(x, y)),
                        _ => match (a.as_f64(), b.as_f64()) {
                            (Some(x), Some(y)) => (x - y).abs() <= 1e-5 * x.abs().max(1.0),
                            _ => a == b,
                        },
                    }
                }
                c(a, b)
            };
            let diff: Vec<&str> = ["name", "shader", "queue", "floats", "colors", "tex"].into_iter().filter(|k| !close(&r[*k], &g[*k])).collect();
            if diff.is_empty() {
                ok += 1;
            } else {
                bad += 1;
                for k in diff {
                    println!("MAT {stem}.{k}: {} | {}", r[k].to_string().chars().take(160).collect::<String>(), g[k].to_string().chars().take(160).collect::<String>());
                }
            }
        }
        println!("materials: {ok} match, {bad} differ");
    }

    /// Every old prefab / scene export (dev/moved-assets/data/<dir>) equals the data built in place. Allowed: the
    /// component part of "ref:<path>::<Type>" (scripts outside Assembly-CSharp were labelled by GUID), components the
    /// player build does not keep (editor-only LightProbeGroup: "Unknown" with no counterpart), scene roots' order, an
    /// empty list written as null.
    #[test]
    fn prefab_json_matches_export() {
        if !crate::owned::ref_checks("RESTRAFE_JSON_CHECK") {
            return;
        }
        use serde_json::Value;
        let owned = Owned::open().unwrap();
        const SKIP: &[&str] = &["m_LocalEulerAnglesHint", "m_ConstrainProportionsScale", "serializedVersion", "m_EditorHideFlags", "m_EditorClassIdentifier", "m_AutoUVMaxAngle", "m_AutoUVMaxDistance", "m_IgnoreNormalsForChartDetection", "m_ImportantGI", "m_LightmapParameters", "m_MinimumChartSize", "m_PreserveUVs", "m_ReceiveGI", "m_ScaleInLightmap", "m_SelectedEditorRenderState", "m_StitchLightmapSeams", "_networkObjectCache", "m_WarningMessage", "m_FOVAxisMode", "tangentMode", "colorLabel0", "colorLabel1", "vectorLabel0_0", "vectorLabel0_1", "vectorLabel0_2", "vectorLabel0_3", "vectorLabel1_0", "vectorLabel1_1", "vectorLabel1_2", "vectorLabel1_3"];
        fn norm_ref(s: &str) -> String {
            if s.starts_with("ref:") {
                return s.split("::").next().unwrap_or(s).to_string();
            }
            // scene lightmaps: the old exports kept the scene folder ("Scenes/.../X.texture2D")
            match s.strip_suffix(".texture2D").and_then(|t| t.rsplit_once('/')) {
                Some((_, stem)) => format!("asset:Texture2D/{stem}.png"),
                None => s.to_string(),
            }
        }
        fn close(a: &Value, b: &Value) -> bool {
            match (a, b) {
                (Value::Null, Value::Array(v)) | (Value::Array(v), Value::Null) => v.is_empty(),
                (Value::Object(a), Value::Object(b)) => a.iter().all(|(k, v)| SKIP.contains(&k.as_str()) || b.get(k).is_some_and(|w| close(v, w))),
                (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| close(x, y)),
                (Value::String(x), Value::String(y)) => norm_ref(x) == norm_ref(y),
                // a TMP text that is a number in YAML
                (Value::Number(n), Value::String(t)) | (Value::String(t), Value::Number(n)) => t.parse::<f64>().ok() == n.as_f64(),
                (Value::Bool(x), y) | (y, Value::Bool(x)) if y.is_number() => y.as_f64() == Some(*x as u8 as f64),
                _ => match (a.as_f64(), b.as_f64()) {
                    (Some(x), Some(y)) => (x - y).abs() <= 1e-4 * x.abs().max(1.0),
                    _ => a == b,
                },
            }
        }
        let (mut objects, mut fields, mut bad) = (0, 0, 0);
        for dir in ["weapons", "projectiles", "vfx", "props", "map", "hats", "player", "maps"] {
            let root = crate::owned::reference_file(&format!("assets/data/{dir}"));
            let mut files: Vec<_> = std::fs::read_dir(&root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
            files.sort();
            for p in files {
                let stem = p.file_stem().unwrap().to_string_lossy().to_string();
                let r: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
                let g = if dir == "maps" {
                    crate::prefab_json::scene_json(&owned, owned.assemblies().unwrap(), owned.scene_level(&stem).unwrap())
                } else {
                    owned.prefab_json(&stem, if dir == "weapons" { "resources.assets" } else { "sharedassets" })
                };
                let g = g.unwrap_or_else(|e| panic!("{dir}/{stem}: {e}"));
                let by_path: HashMap<&str, &Value> = g["objects"].as_array().unwrap().iter().map(|o| (o["path"].as_str().unwrap(), o)).collect();
                let mut report = |what: String| {
                    bad += 1;
                    if std::env::var_os("RESTRAFE_JSON_ALL").is_some() || bad <= 25 {
                        println!("JSON {dir}/{stem}: {what}");
                    }
                };
                for o in r["objects"].as_array().unwrap() {
                    objects += 1;
                    let path = o["path"].as_str().unwrap();
                    let Some(q) = by_path.get(path) else {
                        report(format!("{path}: missing object"));
                        continue;
                    };
                    for k in ["layer", "tag", "active"] {
                        if !close(&o[k], &q[k]) {
                            report(format!("{path}: {k} {} vs {}", o[k], q[k]));
                        }
                    }
                    let theirs = o["components"].as_array().unwrap();
                    let ours: Vec<&Value> = q["components"].as_array().unwrap().iter().collect();
                    let mut used = vec![false; ours.len()];
                    for c in theirs {
                        let ty = c["type"].as_str().unwrap();
                        // the next component of ours that can be this one (same type, or a renamed script)
                        let keys: Vec<&String> = c["fields"].as_object().unwrap().keys().filter(|k| !SKIP.contains(&k.as_str())).collect();
                        let Some(k) = (0..ours.len()).filter(|&k| !used[k]).find(|&k| {
                            let t = ours[k]["type"].as_str().unwrap();
                            // a renamed script: the same field names
                            t == ty || (ty.starts_with("MonoBehaviour:") && keys.iter().all(|key| ours[k]["fields"].get(key.as_str()).is_some()))
                        }) else {
                            if ty != "Unknown" {
                                report(format!("{path}: missing component {ty}"));
                            }
                            continue;
                        };
                        used[k] = true;
                        for (key, v) in c["fields"].as_object().unwrap() {
                            if SKIP.contains(&key.as_str()) || (key == "m_RootOrder" && !path.contains('/')) {
                                continue;
                            }
                            fields += 1;
                            match ours[k]["fields"].get(key) {
                                Some(w) if close(v, w) => {}
                                w => report(format!("{path}: {ty}.{key}: {} vs {}", v.to_string().chars().take(100).collect::<String>(), w.map(|w| w.to_string()).unwrap_or("missing".into()).chars().take(100).collect::<String>())),
                            }
                        }
                    }
                }
            }
        }
        println!("prefab data: {objects} objects, {fields} fields compared, {bad} differences");
        assert_eq!(bad, 0);
    }

    /// RESTRAFE_JSON_DIRS=props,map: writes the in-place prefab JSON for every exported data/<dir>/*.json to
    /// target/jsongen/<dir>/ (compare with the scratch script).
    #[test]
    fn generate_prefab_json_for_compare() {
        let Ok(dirs) = std::env::var("RESTRAFE_JSON_DIRS") else { return };
        let owned = Owned::open().unwrap();
        for dir in dirs.split(',') {
            let out = format!("target/jsongen/{dir}");
            std::fs::create_dir_all(&out).unwrap();
            let prefer = if dir == "weapons" { "resources.assets" } else { "sharedassets" };
            let root = std::env::var("RESTRAFE_JSON_REF").unwrap_or("assets/data".into());
            for e in std::fs::read_dir(format!("{root}/{dir}")).into_iter().flatten().flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                let stem = p.file_stem().unwrap().to_str().unwrap().to_string();
                let built = if dir == "maps" {
                    owned.scene_level(&stem).ok_or("no scene".to_string()).and_then(|l| crate::prefab_json::scene_json(&owned, owned.assemblies().unwrap(), l))
                } else {
                    owned.prefab_json(&stem, prefer)
                };
                match built {
                    Ok(v) => std::fs::write(format!("{out}/{stem}.json"), serde_json::to_string_pretty(&v).unwrap()).unwrap(),
                    Err(e) => println!("FAIL {dir}/{stem}: {e}"),
                }
            }
        }
    }
}
