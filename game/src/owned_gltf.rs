//! Prefab hierarchies of the installation as glTF (GLB) built in memory, in the layout the game's loaders expect
//! (the layout AssetRipper's exporter used): one node per GameObject (Unity names, child order), Unity's left-handed
//! space mirrored on X (translation (-x, y, z), rotation (x, -y, -z, w)), one mesh per MeshFilter + MeshRenderer
//! with one primitive per submesh (positions / normals / tangents mirrored, UV v flipped, winding reversed,
//! vertices packed per primitive in first-use order), one glTF material per Unity material (name + base colour
//! texture referenced by name, which the in-place texture loader resolves).

use crate::owned::Owned;
use crate::unity_fs::{self, ObjectInfo, SerializedFile};
use bevy::math::{Mat3, Mat4, Quat, Vec3};
use serde_json::{Value, json};
use std::collections::HashMap;

/// One serialized file of the bundle with an id -> object map.
struct File {
    node: usize,
    sf: SerializedFile,
    by_id: HashMap<i64, ObjectInfo>,
}

pub struct Files<'a> {
    owned: &'a Owned,
    files: HashMap<usize, File>,
}

impl<'a> Files<'a> {
    pub fn new(owned: &'a Owned) -> Self {
        Files { owned, files: HashMap::new() }
    }
    fn open(&mut self, node: usize) -> Result<&File, String> {
        if !self.files.contains_key(&node) {
            let b = &self.owned.0.bundle;
            let sf = SerializedFile::open(b, &b.nodes[node])?;
            let by_id = sf.objects.iter().map(|o| (o.path_id, o.clone())).collect();
            self.files.insert(node, File { node, sf, by_id });
        }
        Ok(&self.files[&node])
    }
    /// The bundle node a PPtr of `file` points into (0 = the same file, else its externals[i - 1]).
    pub fn target_node(&mut self, file: usize, fid: i32) -> Option<usize> {
        if fid == 0 {
            return Some(file);
        }
        let ext = self.open(file).ok()?.sf.externals.get(fid as usize - 1)?.clone();
        let name = ext.rsplit('/').next()?.to_string();
        self.owned.0.bundle.nodes.iter().position(|n| n.path == name || n.path.ends_with(&format!("/{name}")))
    }
    /// Raw data, class and owning file of the object a PPtr points to.
    pub fn read(&mut self, file: usize, p: (i32, i64)) -> Option<(usize, i32, Vec<u8>)> {
        if p.1 == 0 {
            return None;
        }
        let node = self.target_node(file, p.0)?;
        let owned = self.owned;
        let f = self.open(node).ok()?;
        let o = f.by_id.get(&p.1)?.clone();
        let b = &owned.0.bundle;
        let d = f.sf.object_data(b, &b.nodes[f.node], &o).ok()?;
        Some((node, o.class_id, d))
    }
    fn big_endian(&mut self, node: usize) -> bool {
        self.open(node).map(|f| f.sf.big_endian).unwrap_or(false)
    }
}

#[derive(Default)]
struct Glb {
    json_nodes: Vec<Value>,
    meshes: Vec<Value>,
    materials: Vec<Value>,
    material_ids: HashMap<(usize, i64), usize>,
    textures: Vec<Value>,
    images: Vec<Value>,
    image_ids: HashMap<String, usize>,
    /// (mesh file, mesh path id, materials) -> glTF mesh, so a Mesh drawn by several renderers is stored once
    mesh_ids: HashMap<(usize, i64, Vec<Option<usize>>), usize>,
    accessors: Vec<Value>,
    views: Vec<Value>,
    bin: Vec<u8>,
}

impl Glb {
    fn push_view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
        let mut v = json!({ "buffer": 0, "byteOffset": self.bin.len(), "byteLength": bytes.len() });
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.bin.extend_from_slice(bytes);
        self.views.push(v);
        self.views.len() - 1
    }
    fn accessor_f32(&mut self, data: &[f32], dim: usize, minmax: bool) -> usize {
        let bytes: Vec<u8> = data.iter().flat_map(|f| f.to_le_bytes()).collect();
        let view = self.push_view(&bytes, Some(34962));
        let ty = ["SCALAR", "VEC2", "VEC3", "VEC4"][dim - 1];
        let mut a = json!({ "bufferView": view, "componentType": 5126, "count": data.len() / dim, "type": ty });
        if minmax {
            let mut mn = vec![f32::MAX; dim];
            let mut mx = vec![f32::MIN; dim];
            for v in data.chunks_exact(dim) {
                for k in 0..dim {
                    mn[k] = mn[k].min(v[k]);
                    mx[k] = mx[k].max(v[k]);
                }
            }
            a["min"] = json!(mn);
            a["max"] = json!(mx);
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }
    fn accessor_indices(&mut self, idx: &[u32]) -> usize {
        let bytes: Vec<u8> = idx.iter().flat_map(|i| i.to_le_bytes()).collect();
        let view = self.push_view(&bytes, Some(34963));
        self.accessors.push(json!({ "bufferView": view, "componentType": 5125, "count": idx.len(), "type": "SCALAR" }));
        self.accessors.len() - 1
    }

    fn material(&mut self, files: &mut Files, file: usize, p: (i32, i64)) -> Option<usize> {
        let node = files.target_node(file, p.0)?;
        if let Some(&m) = self.material_ids.get(&(node, p.1)) {
            return Some(m);
        }
        let (mnode, class, d) = files.read(file, p)?;
        if class != 21 {
            return None;
        }
        let be = files.big_endian(mnode);
        let m = unity_fs::Material::read(&d, be).ok()?;
        let mut mat = json!({ "name": m.name, "pbrMetallicRoughness": {} });
        let main = ["_MainTex", "_BaseMap", "_BC", "_BaseColorMap", "_Albedo"]
            .iter()
            .find_map(|k| m.textures.iter().find(|(n, t)| n == k && t.texture.1 != 0))
            .or_else(|| m.textures.iter().find(|(_, t)| t.texture.1 != 0));
        if let Some((_, t)) = main {
            if let Some((tnode, 28, td)) = files.read(mnode, t.texture) {
                let be = files.big_endian(tnode);
                if let Some(name) = unity_fs::object_name(&td, be) {
                    let img = match self.image_ids.get(&name) {
                        Some(&i) => i,
                        None => {
                            // resolved by the in-place texture loader (relative to the .glb path)
                            let uri: String = format!("{name}.png").chars().map(|c| if c == ' ' { "%20".to_string() } else { c.to_string() }).collect();
                            self.images.push(json!({ "uri": uri }));
                            self.textures.push(json!({ "source": self.images.len() - 1 }));
                            self.image_ids.insert(name, self.textures.len() - 1);
                            self.textures.len() - 1
                        }
                    };
                    mat["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": img });
                }
            }
        }
        self.materials.push(mat);
        self.material_ids.insert((node, p.1), self.materials.len() - 1);
        Some(self.materials.len() - 1)
    }

    fn mesh(&mut self, m: &unity_fs::Mesh, mats: &[Option<usize>]) -> usize {
        self.mesh_ex(m, mats, None, false, None)
    }

    /// The submeshes `first .. first + count` of a static batch's combined mesh (world space), material i for
    /// submesh first + i.
    /// `to_local`: inverse of the renderer's world matrix (Unity space): the combined mesh is in world space, the
    /// old exports stored each batched renderer's part in its own local space.
    fn mesh_batched(&mut self, m: &unity_fs::Mesh, mats: &[Option<usize>], first: usize, count: usize, to_local: Mat4) -> usize {
        let mut sub = m.clone();
        sub.submeshes = m.submeshes.iter().skip(first).take(count).cloned().collect();
        self.mesh_ex(&sub, mats, None, false, Some(to_local))
    }

    /// `only`: just that submesh; `skin`: also JOINTS_0 / WEIGHTS_0 (Unity blend indices / weights).
    fn mesh_ex(&mut self, m: &unity_fs::Mesh, mats: &[Option<usize>], only: Option<usize>, skin: bool, xf: Option<Mat4>) -> usize {
        let nxf = xf.map(|x| Mat3::from_mat4(x).inverse().transpose());
        let mut prims = Vec::new();
        let pos = m.attr(0);
        let nrm = m.attr(1);
        let tan = m.attr(2);
        let col = m.attr(3);
        let uvs: Vec<_> = (4..12).map(|c| m.attr(c)).collect();
        let idx_size = if m.index_format == 1 { 4 } else { 2 };
        // Submeshes drawn with the same material become one primitive (in order of first appearance).
        let mut groups: Vec<(Option<usize>, Vec<u32>)> = Vec::new();
        for (si, s) in m.submeshes.iter().enumerate() {
            if only.is_some_and(|o| o != si) {
                continue;
            }
            if s.topology != 0 {
                continue; // triangles only
            }
            let first = s.first_byte as usize / idx_size;
            let tri = &m.indices[first..(first + s.index_count as usize).min(m.indices.len())];
            let mat = mats.get(si).or(mats.last()).copied().flatten();
            let g = match groups.iter().position(|(gm, _)| *gm == mat) {
                Some(g) => g,
                None => {
                    groups.push((mat, Vec::new()));
                    groups.len() - 1
                }
            };
            groups[g].1.extend(tri.iter().map(|&v| v + s.base_vertex));
        }
        // Identical vertices (all attribute values equal) are merged.
        let key = |v: u32| -> Vec<u32> {
            let mut k = Vec::new();
            for a in [pos, nrm, tan, col].into_iter().chain(uvs.iter().copied()) {
                if let Some((dim, data)) = a {
                    let d = dim as usize;
                    if let Some(x) = data.get(v as usize * d..v as usize * d + d) {
                        k.extend(x.iter().map(|f| f.to_bits()));
                    }
                }
            }
            k
        };
        for (mat, tri) in groups {
            // reversed winding, then vertices in first-use order
            let mut order: Vec<u32> = Vec::new();
            let mut remap: HashMap<Vec<u32>, u32> = HashMap::new();
            let mut out_idx = Vec::with_capacity(tri.len());
            for t in tri.chunks_exact(3) {
                for &v in [t[2], t[1], t[0]].iter() {
                    let n = *remap.entry(key(v)).or_insert_with(|| {
                        order.push(v);
                        order.len() as u32 - 1
                    });
                    out_idx.push(n);
                }
            }
            // `want`: components to keep (half-float positions / normals are stored with 4)
            let gather = |a: Option<(u8, &[f32])>, want: usize, f: &dyn Fn(&mut [f32])| -> Option<(usize, Vec<f32>)> {
                let (dim, data) = a?;
                let dim = dim as usize;
                let keep = want.min(dim);
                let mut out = Vec::with_capacity(order.len() * keep);
                for &v in &order {
                    let mut x = data.get(v as usize * dim..v as usize * dim + keep)?.to_vec();
                    f(&mut x);
                    out.extend(x);
                }
                Some((keep, out))
            };
            let mut attrs = serde_json::Map::new();
            if let Some((3, p)) = gather(pos, 3, &|x| {
                if let Some(t) = xf {
                    let v = t.transform_point3(Vec3::new(x[0], x[1], x[2]));
                    x.copy_from_slice(&v.to_array());
                }
                x[0] = -x[0];
            }) {
                attrs.insert("POSITION".into(), json!(self.accessor_f32(&p, 3, true)));
            }
            if let Some((3, n)) = gather(nrm, 3, &|x| {
                if let Some(t) = nxf {
                    let v = (t * Vec3::new(x[0], x[1], x[2])).normalize_or_zero();
                    x.copy_from_slice(&v.to_array());
                }
                x[0] = -x[0];
            }) {
                attrs.insert("NORMAL".into(), json!(self.accessor_f32(&n, 3, false)));
            }
            if let Some((4, t)) = gather(tan, 4, &|x| {
                if let Some(m) = xf {
                    let v = m.transform_vector3(Vec3::new(x[0], x[1], x[2])).normalize_or_zero();
                    x[..3].copy_from_slice(&v.to_array());
                }
                x[0] = -x[0];
                x[3] = -x[3];
            }) {
                attrs.insert("TANGENT".into(), json!(self.accessor_f32(&t, 4, false)));
            }
            if let Some((d, c)) = gather(col, 4, &|_| {}) {
                if d == 4 {
                    attrs.insert("COLOR_0".into(), json!(self.accessor_f32(&c, 4, false)));
                }
            }
            for (k, uv) in uvs.iter().enumerate() {
                if let Some((2, u)) = gather(*uv, 2, &|x| x[1] = 1.0 - x[1]) {
                    attrs.insert(format!("TEXCOORD_{k}"), json!(self.accessor_f32(&u, 2, false)));
                }
            }
            if skin {
                if let (Some((wd, w)), Some((jd, j))) = (gather(m.attr(12), 4, &|_| {}), gather(m.attr(13), 4, &|_| {})) {
                    let pad = |v: Vec<f32>, d: usize| -> Vec<f32> { v.chunks(d).flat_map(|c| (0..4).map(move |k| c.get(k).copied().unwrap_or(0.0))).collect() };
                    let (w, j) = (pad(w, wd), pad(j, jd));
                    let jb: Vec<u8> = j.iter().flat_map(|x| (*x as u16).to_le_bytes()).collect();
                    let view = self.push_view(&jb, Some(34962));
                    self.accessors.push(json!({ "bufferView": view, "componentType": 5123, "count": j.len() / 4, "type": "VEC4" }));
                    attrs.insert("JOINTS_0".into(), json!(self.accessors.len() - 1));
                    attrs.insert("WEIGHTS_0".into(), json!(self.accessor_f32(&w, 4, false)));
                }
            }
            let ind = self.accessor_indices(&out_idx);
            let mut prim = json!({ "attributes": attrs, "indices": ind, "mode": 4 });
            if let Some(mi) = mat {
                prim["material"] = json!(mi);
            }
            prims.push(prim);
        }
        self.meshes.push(json!({ "primitives": prims }));
        self.meshes.len() - 1
    }

    fn finish(self) -> Vec<u8> {
        self.finish_with_roots(&[0])
    }

    fn finish_with_roots(self, roots: &[usize]) -> Vec<u8> {
        let mut j = json!({
            "asset": { "version": "2.0", "generator": "restrafe owned_gltf" },
            "scene": 0,
            "scenes": [{ "nodes": roots }],
            "nodes": self.json_nodes,
            "buffers": [{ "byteLength": self.bin.len() }],
            "bufferViews": self.views,
            "accessors": self.accessors,
        });
        if !self.meshes.is_empty() {
            j["meshes"] = json!(self.meshes);
        }
        if !self.materials.is_empty() {
            j["materials"] = json!(self.materials);
        }
        if !self.textures.is_empty() {
            j["textures"] = json!(self.textures);
            j["images"] = json!(self.images);
        }
        let mut js = serde_json::to_vec(&j).unwrap();
        while js.len() % 4 != 0 {
            js.push(b' ');
        }
        let mut bin = self.bin;
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let total = 12 + 8 + js.len() + 8 + bin.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(js.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&js);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
        out
    }
}

/// Builds the GLB of the prefab whose root GameObject is (`node`, `path_id`).
pub fn prefab_glb(owned: &Owned, node: usize, path_id: i64) -> Result<Vec<u8>, String> {
    let mut files = Files::new(owned);
    let mut g = Glb::default();
    let data_dir = owned.0.data_dir.clone();
    // depth-first, parents before children (node 0 = root)
    walk(&mut files, &mut g, &data_dir, node, path_id, true, Mat4::IDENTITY)?;
    Ok(g.finish())
}

/// Depth-first, parents before children. `reset`: this node is a prefab root (exported at the origin).
fn walk(files: &mut Files, g: &mut Glb, data_dir: &std::path::Path, file: usize, go_id: i64, reset: bool, parent_world: Mat4) -> Result<usize, String> {
        let (gnode, _, gd) = files.read(file, (0, go_id)).ok_or("GameObject missing")?;
        let be = files.big_endian(gnode);
        let go = unity_fs::GameObject::read(&gd, be)?;
        let me = g.json_nodes.len();
        g.json_nodes.push(json!({ "name": go.name }));
        let mut tf = None;
        let (mut mesh_ptr, mut renderer) = (None, None);
        for c in &go.components {
            let Some((cnode, class, d)) = files.read(gnode, *c) else { continue };
            match class {
                4 | 224 => tf = Some((cnode, unity_fs::Transform::read(&d, be)?)),
                33 => mesh_ptr = unity_fs::mesh_filter(&d, be).ok().map(|(_, m)| (cnode, m)),
                23 | 137 => renderer = unity_fs::Renderer::read(&d, be).ok().map(|r| (cnode, r)),
                _ => {}
            }
        }
        let (tnode, t) = tf.ok_or_else(|| format!("{}: no Transform", go.name))?;
        let world = parent_world * Mat4::from_scale_rotation_translation(Vec3::from(t.scale), Quat::from_array(t.rotation), Vec3::from(t.position));
        let n = &mut g.json_nodes[me];
        let is_root = reset;
        if !is_root && t.position != [0.0; 3] {
            n["translation"] = json!([-t.position[0], t.position[1], t.position[2]]);
        }
        if !is_root && t.rotation != [0.0, 0.0, 0.0, 1.0] {
            n["rotation"] = json!([t.rotation[0], -t.rotation[1], -t.rotation[2], t.rotation[3]]);
        }
        if !is_root && t.scale != [1.0; 3] {
            n["scale"] = json!(t.scale);
        }
        if let (Some((mn, mp)), Some((rn, r))) = (mesh_ptr, renderer) {
            let mats: Vec<Option<usize>> = r.materials.iter().map(|&p| g.material(files, rn, p)).collect();
            let batch = r.static_batch;
            let mut key = files.target_node(mn, mp.0).map(|n| (n, mp.1, mats.clone()));
            if batch.1 > 0 {
                // static batch: this renderer's submeshes of the combined mesh, already in world space
                key = None;

            }
            if let Some(&mi) = key.as_ref().and_then(|k| g.mesh_ids.get(k)) {
                g.json_nodes[me]["mesh"] = json!(mi);
            } else if let Some((meshnode, 43, md)) = files.read(mn, mp) {
                let be = files.big_endian(meshnode);
                match unity_fs::Mesh::read(&md, be, &files.owned.0.bundle, data_dir) {
                    Ok(mesh) if mesh.vertex_count > 0 => {
                        let mi = if batch.1 > 0 { g.mesh_batched(&mesh, &mats, batch.0 as usize, batch.1 as usize, world.inverse()) } else { g.mesh(&mesh, &mats) };
                        if let Some(k) = key {
                            g.mesh_ids.insert(k, mi);
                        }
                        g.json_nodes[me]["mesh"] = json!(mi);
                    }
                    Ok(_) => {} // empty in the build (e.g. stripped by static batching)
                    Err(e) => bevy::log::warn!("{}: mesh {e}", go.name),
                }
            }
        }
        let mut kids = Vec::new();
        for ch in &t.children {
            let Some((cn, _, cd)) = files.read(tnode, *ch) else { continue };
            let ct = unity_fs::Transform::read(&cd, files.big_endian(cn))?;
            let gid = files.target_node(cn, ct.game_object.0).map(|_| ct.game_object.1).unwrap_or(0);
            if gid != 0 {
                kids.push(walk(files, g, data_dir, cn, gid, false, world)?);
            }
        }
        if !kids.is_empty() {
            g.json_nodes[me]["children"] = json!(kids);
        }
        Ok(me)
}

/// A Mesh asset on its own (the old exports' mesh files): root node named after the mesh, one `SubMesh_<i>` child per
/// submesh, `DefaultMaterial`, with JOINTS_0 / WEIGHTS_0 when the mesh has skin data.
pub fn mesh_glb(owned: &Owned, node: usize, path_id: i64) -> Result<Vec<u8>, String> {
    let mut files = Files::new(owned);
    let (mnode, class, d) = files.read(node, (0, path_id)).ok_or("mesh missing")?;
    if class != 43 {
        return Err("not a Mesh".into());
    }
    let be = files.big_endian(mnode);
    let m = unity_fs::Mesh::read(&d, be, &owned.0.bundle, &owned.0.data_dir)?;
    let mut g = Glb::default();
    g.materials.push(json!({ "name": "DefaultMaterial", "pbrMetallicRoughness": {} }));
    g.json_nodes.push(json!({ "name": m.name }));
    let mut kids = Vec::new();
    for si in 0..m.submeshes.len() {
        let mi = g.mesh_ex(&m, &[Some(0)], Some(si), true, None);
        g.json_nodes.push(json!({ "name": format!("SubMesh_{si}"), "mesh": mi }));
        kids.push(g.json_nodes.len() - 1);
    }
    g.json_nodes[0]["children"] = json!(kids);
    Ok(g.finish())
}

/// A scene (bundle node `levelN`) as GLB: its root GameObjects (Transform without a father) in path-id order as the
/// glTF scene's nodes, each with its hierarchy (no root reset).
pub fn scene_glb(owned: &Owned, level: usize) -> Result<Vec<u8>, String> {
    let b = &owned.0.bundle;
    let sf = SerializedFile::open(b, &b.nodes[level])?;
    let mut roots: Vec<(i64, i64)> = Vec::new(); // (GameObject path id, GameObject path id)
    for o in sf.objects.iter().filter(|o| o.class_id == 4 || o.class_id == 224) {
        let Ok(d) = sf.object_data(b, &b.nodes[level], o) else { continue };
        let Ok(t) = unity_fs::Transform::read(&d, sf.big_endian) else { continue };
        if t.father.1 == 0 && t.game_object.0 == 0 {
            roots.push((t.game_object.1, t.game_object.1));
        }
    }
    // the order of the GameObjects in the file's object table
    let pos: HashMap<i64, usize> = sf.objects.iter().enumerate().map(|(i, o)| (o.path_id, i)).collect();
    roots.sort_by_key(|(go, _)| pos.get(go).copied().unwrap_or(usize::MAX));
    let mut files = Files::new(owned);
    let mut g = Glb::default();
    let data_dir = owned.0.data_dir.clone();
    let mut scene_nodes = Vec::new();
    for (_, go) in roots {
        scene_nodes.push(walk(&mut files, &mut g, &data_dir, level, go, true, Mat4::IDENTITY)?);
    }
    Ok(g.finish_with_roots(&scene_nodes))
}
