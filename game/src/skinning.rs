//! Skinning for the dummy's body meshes. AssetRipper exported the `SkinnedMeshRenderer` meshes as glbs that carry
//! JOINTS_0/WEIGHTS_0 but no glTF skin, so we rebuild the skin here:
//!  - joint order = the Unity mesh asset's `m_BoneNameHashes` (crc32 of bone name, same order as `m_BindPose`
//!    and `SkinnedMeshRenderer.m_Bones`); bones are looked up by name under the dummy's `PF_Aboubi_04`.
//!  - inverse bind pose (Unity: mesh-local -> bone-local) converted to the X-mirrored glb space: `S * B * S`, S = diag(-1,1,1,1).
//!  - joint indices / weights are read straight from the glb's BIN chunk and put on a clone of the loaded Mesh.

use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::VertexAttributeValues;
use bevy::prelude::*;
use std::collections::HashMap;

const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");
/// `SkinnedMeshRenderer.m_Bones` order of every body renderer (identical for all 8; matches `m_BindPose` order).
const BONES: [&str; 38] = ["Hips", "Torso", "Neck", "Head", "Shoulder.R", "Arm.R", "ForeArm.R", "Wrist.R", "RingFinger_1.R", "RingFinger_2.R", "Index_1.R", "Index_2.R", "Thumb_1.R", "Thumb_2.R", "Pinkie_1.R", "Pinkie_2.R", "MiddleFinger_1.R", "MiddleFinger_2.R", "Shoulder.L", "Arm.L", "ForeArm.L", "Wrist.L", "RingFinger_1.L", "RingFinger_2.L", "Index_1.L", "Index_2.L", "Thumb_1.L", "Thumb_2.L", "Pinkie_1.L", "Pinkie_2.L", "MiddleFinger_1.L", "MiddleFinger_2.L", "Leg.R", "Shin.R", "Foot.R", "Leg.L", "Shin.L", "Foot.L", ];
#[cfg(test)]
const MESH_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../ripped/ExportedProject/Assets/Mesh");

/// Put on a body mesh entity (already carrying the static `Mesh3d`); replaced by a real skin once the mesh is loaded.
#[derive(Component)]
pub struct PendingSkin {
    /// glb path relative to `assets/`.
    pub model: String,
    /// The dummy root (to find the bones below).
    pub dummy: Entity,
}

pub fn read_glb_skin(path: &str) -> Option<(Vec<[u16; 4]>, Vec<[f32; 4]>)> {
    // the file under assets/, else the same mesh file built in place from the installation
    let data = std::fs::read(format!("{ASSETS}/{path}")).ok().or_else(|| {
        let stem = path.rsplit('/').next()?.trim_end_matches(".glb");
        crate::owned::Owned::global()?.mesh_glb(stem).ok()
    })?;
    let u32at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
    let jlen = u32at(12);
    let json: serde_json::Value = serde_json::from_slice(&data[20..20 + jlen]).ok()?;
    let bo = 20 + jlen;
    let bin = &data[bo + 8..bo + 8 + u32at(bo)];
    let prim = &json["meshes"][0]["primitives"][0]["attributes"];
    let read = |acc: usize| -> Option<(Vec<f32>, u64, bool)> {
        let a = &json["accessors"][acc];
        let bv = &json["bufferViews"][a["bufferView"].as_u64()? as usize];
        let off = bv["byteOffset"].as_u64().unwrap_or(0) as usize + a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let count = a["count"].as_u64()? as usize;
        let ct = a["componentType"].as_u64()?;
        let size = match ct {
            5121 => 1,
            5123 => 2,
            _ => 4,
        };
        let stride = bv["byteStride"].as_u64().map(|s| s as usize).unwrap_or(size * 4);
        let norm = a["normalized"].as_bool().unwrap_or(false);
        let mut v = Vec::with_capacity(count * 4);
        for i in 0..count {
            for k in 0..4 {
                let p = off + i * stride + k * size;
                v.push(match ct {
                    5121 => bin[p] as f32,
                    5123 => u16::from_le_bytes([bin[p], bin[p + 1]]) as f32,
                    _ => f32::from_le_bytes(bin[p..p + 4].try_into().unwrap()),
                });
            }
        }
        Some((v, ct, norm))
    };
    let (j, _, _) = read(prim["JOINTS_0"].as_u64()? as usize)?;
    let (w, ct, norm) = read(prim["WEIGHTS_0"].as_u64()? as usize)?;
    let scale = if norm {
        match ct {
            5121 => 1.0 / 255.0,
            5123 => 1.0 / 65535.0,
            _ => 1.0,
        }
    } else {
        1.0
    };
    let joints = j.chunks(4).map(|c| [c[0] as u16, c[1] as u16, c[2] as u16, c[3] as u16]).collect();
    let weights = w.chunks(4).map(|c| [c[0] * scale, c[1] * scale, c[2] * scale, c[3] * scale]).collect();
    Some((joints, weights))
}

/// (bone name hashes, inverse bind poses converted to glb space) from `Mesh/<stem>.asset`.
/// Read from the installation's Mesh (the old asset file only when that fails).
pub fn read_mesh_asset(stem: &str) -> Option<(Vec<u32>, Vec<Mat4>)> {
    owned_mesh_skin(stem)
}

/// The same from the old exported `Mesh/<stem>.asset` YAML (the reference of the comparison test).
#[cfg(test)]
fn read_mesh_yaml(stem: &str) -> Option<(Vec<u32>, Vec<Mat4>)> {
    let txt = std::fs::read_to_string(format!("{MESH_DIR}/{stem}.asset")).ok()?;
    let mut hashes = Vec::new();
    let mut e: Vec<f32> = Vec::new();
    let mut in_bind = false;
    for line in txt.lines() {
        let t = line.trim_start_matches([' ', '-']).trim();
        if t.starts_with("m_BindPose:") {
            in_bind = true;
            continue;
        }
        if in_bind {
            if let Some((k, v)) = t.split_once(':') {
                if k.len() == 3 && k.starts_with('e') {
                    e.push(v.trim().parse().ok()?);
                    continue;
                }
            }
            in_bind = false;
        }
        if let Some(h) = t.strip_prefix("m_BoneNameHashes:") {
            let h = h.trim();
            hashes = (0..h.len() / 8)
                .map(|i| {
                    let b: Vec<u8> = (0..4).map(|k| u8::from_str_radix(&h[i * 8 + k * 2..i * 8 + k * 2 + 2], 16).unwrap_or(0)).collect();
                    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
                })
                .collect();
        }
    }
    let s = Mat4::from_diagonal(Vec4::new(-1.0, 1.0, 1.0, 1.0));
    let binds = e
        .chunks(16)
        .map(|m| {
            // e_ij = row i, column j; the list is row-major, from_cols_array wants column-major.
            let cols: Vec<f32> = (0..16).map(|n| m[(n % 4) * 4 + n / 4]).collect();
            s * Mat4::from_cols_array(&cols.try_into().unwrap()) * s
        })
        .collect();
    Some((hashes, binds))
}

/// Bone name hashes + bind poses (port space) of a Mesh read in place from the installation.
pub fn owned_mesh_skin(stem: &str) -> Option<(Vec<u32>, Vec<Mat4>)> {
    let owned = crate::owned::Owned::global()?;
    let (node, pid) = owned.resolve_mesh(stem)?;
    let b = &owned.0.bundle;
    let n = &b.nodes[node];
    let sf = crate::unity_fs::SerializedFile::open(b, n).ok()?;
    let o = sf.objects.iter().find(|o| o.path_id == pid)?;
    let m = crate::unity_fs::Mesh::read(&sf.object_data(b, n, o).ok()?, sf.big_endian, b, &owned.0.data_dir).ok()?;
    Some((m.bone_name_hashes.clone(), bind_poses_to_port(m.bind_poses.iter().flatten().copied().collect())))
}

/// Unity bind poses (e00 e01 e02 e03 e10 ... per matrix, row-major) -> port space (X mirror on both sides).
fn bind_poses_to_port(e: Vec<f32>) -> Vec<Mat4> {
    let s = Mat4::from_diagonal(Vec4::new(-1.0, 1.0, 1.0, 1.0));
    e.chunks(16)
        .map(|m| {
            let cols: Vec<f32> = (0..16).map(|n| m[(n % 4) * 4 + n / 4]).collect();
            s * Mat4::from_cols_array(&cols.try_into().unwrap()) * s
        })
        .collect()
}

#[cfg(test)]
mod tests {
    /// With the old export present: the in-place bind poses / hashes equal the YAML ones (RESTRAFE_SKIN_CHECK=1).
    #[test]
    fn owned_skin_matches_export() {
        if !crate::owned::ref_checks("RESTRAFE_SKIN_CHECK") {
            return;
        }
        let mut stems: Vec<String> = std::fs::read_dir(super::MESH_DIR).into_iter().flatten().flatten().filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".asset").map(str::to_string)).collect();
        stems.sort();
        let mut n = 0;
        for stem in &stems {
            let stem = stem.as_str();
            let Some((ha, ba)) = super::read_mesh_yaml(stem).filter(|(h, _)| !h.is_empty()) else { continue };
            n += 1;
            let Some((hb, bb)) = super::owned_mesh_skin(stem) else { panic!("{stem}: not read from the installation") };
            assert_eq!(ha, hb, "{stem} hashes");
            let d = ba.iter().zip(&bb).map(|(a, b)| (*a - *b).to_cols_array().iter().map(|x| x.abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
            println!("{stem}: {} bones, max bind pose diff {d}", hb.len());
            assert!(d < 1e-5, "{stem}: bind pose diff {d}");
        }
        println!("{n} skinned meshes match");
    }
}

fn skin_meshes(
    mut commands: Commands,
    pending: Query<(Entity, &PendingSkin, &Mesh3d)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut ibps: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    children: Query<&Children>,
    names: Query<&Name>,
) {
    for (e, p, m) in &pending {
        let Some(src) = meshes.get(&m.0) else { continue };
        let mut mesh = src.clone();
        let stem = p.model.rsplit('/').next().unwrap().trim_end_matches(".glb");
        let (Some((_hashes, binds)), Some((joints, weights))) = (read_mesh_asset(stem), read_glb_skin(&p.model)) else {
            warn!("skinning: cannot read skin data for {stem}");
            commands.entity(e).remove::<PendingSkin>();
            continue;
        };
        if joints.len() != mesh.count_vertices() {
            warn!("skinning: {stem}: {} joint verts vs {} mesh verts", joints.len(), mesh.count_vertices());
            commands.entity(e).remove::<PendingSkin>();
            continue;
        }
        // Bone entities by name hash under PF_Aboubi_04 (the FPArms rig has same-named bones).
        let mut stack = vec![p.dummy];
        let mut pf = None;
        while let Some(n) = stack.pop() {
            if names.get(n).is_ok_and(|x| x.as_str() == "PF_Aboubi_04") {
                pf = Some(n);
                break;
            }
            if let Ok(k) = children.get(n) {
                stack.extend(k.iter());
            }
        }
        let Some(pf) = pf else { continue };
        let mut by_name: HashMap<String, Entity> = HashMap::new();
        let mut stack = vec![pf];
        while let Some(n) = stack.pop() {
            if let Ok(x) = names.get(n) {
                by_name.insert(x.as_str().to_string(), n);
            }
            if let Ok(k) = children.get(n) {
                stack.extend(k.iter());
            }
        }
        let fallback = by_name["Hips"];
        let ents: Vec<Entity> = BONES.iter().map(|b| by_name.get(*b).copied().unwrap_or(fallback)).collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(joints));
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, weights);
        let h = meshes.add(mesh);
        commands.entity(e).remove::<PendingSkin>().insert((
            Mesh3d(h),
            SkinnedMesh { inverse_bindposes: ibps.add(SkinnedMeshInverseBindposes::from(binds)), joints: ents },
            NoFrustumCulling,
        ));
    }
}

pub struct SkinningPlugin;

impl Plugin for SkinningPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, skin_meshes);
    }
}
