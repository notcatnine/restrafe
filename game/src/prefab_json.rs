//! Prefab / scene data in the JSON layout the game's loaders read (`{ name, objects: [ { path, layer, tag, active,
//! components: [ { type, id, fields } ] } ] }`, the layout of the old extractor), built from the installation:
//! engine components from their Unity 2021.3 player layouts, scripts from the managed metadata
//! (`dotnet_meta::Assemblies`). References: in the same hierarchy "ref:<path>::<Type>", other assets
//! "asset:<Dir>/<name>.<ext>", Unity's built-in resources "asset-guid:<guid>:<path id>", none `null`.
//! A component class without a layout here makes the whole build fail, so callers can fall back.

use bevy::log::warn;
use crate::dotnet_meta::{Assemblies, TypeSig};
use crate::owned::Owned;
use crate::owned_gltf::Files;
use crate::unity_fs::{self, ObjReader};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

type R<T> = Result<T, String>;

/// Hierarchy context: (file node, path id) -> "ref:" string for objects inside the built hierarchy.
struct Ctx<'a> {
    owned: &'a Owned,
    asm: &'a Assemblies,
    local: HashMap<(usize, i64), String>,
    tags: Vec<String>,
    /// Raw (YAML-style) references: objects of this set are `{fileID: <path id>}`, everything else
    /// `{fileID, guid: "u:<node>:<path id>"}` (Unity's built-ins keep their built-in guid and id).
    raw: Option<std::collections::HashSet<(usize, i64)>>,
}

/// A float as the YAML exports wrote it (non-finite values as strings).
fn fval(x: f32) -> Value {
    if x.is_finite() {
        json!(x)
    } else if x.is_nan() {
        json!("NaN")
    } else if x > 0.0 {
        json!("Infinity")
    } else {
        json!("-Infinity")
    }
}

fn v3(r: &mut ObjReader) -> R<Value> {
    Ok(json!({ "x": r.f32()?, "y": r.f32()?, "z": r.f32()? }))
}
fn v2(r: &mut ObjReader) -> R<Value> {
    Ok(json!({ "x": r.f32()?, "y": r.f32()? }))
}
fn quat(r: &mut ObjReader) -> R<Value> {
    let (x, y, z, w) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?);
    Ok(json!({ "x": x, "y": y, "z": z, "w": w }))
}
fn color(r: &mut ObjReader) -> R<Value> {
    Ok(json!({ "r": r.f32()?, "g": r.f32()?, "b": r.f32()?, "a": r.f32()? }))
}
fn b(r: &mut ObjReader) -> R<Value> {
    Ok(json!(r.bool()? as u8))
}
fn curve(r: &mut ObjReader) -> R<Value> {
    let n = r.i32()? as usize;
    let mut keys = Vec::with_capacity(n);
    for _ in 0..n {
        let (time, value, in_slope, out_slope) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?);
        let (wm, iw, ow) = (r.i32()?, r.f32()?, r.f32()?);
        keys.push(json!({ "time": time, "value": value, "inSlope": in_slope, "outSlope": out_slope, "weightedMode": wm, "inWeight": iw, "outWeight": ow, "tangentMode": 0, "serializedVersion": 3 }));
    }
    let (pre, post, rot) = (r.i32()?, r.i32()?, r.i32()?);
    Ok(json!({ "m_Curve": keys, "m_PreInfinity": pre, "m_PostInfinity": post, "m_RotationOrder": rot, "serializedVersion": 2 }))
}

/// Directory + extension the old exports used for an asset of `class`.
fn asset_dir(class: i32) -> Option<(&'static str, &'static str)> {
    Some(match class {
        1 => ("GameObject", "prefab"),
        21 => ("Material", "mat"),
        28 => ("Texture2D", "png"),
        43 => ("Mesh", "asset"),
        48 => ("Shader", "shader"),
        74 => ("AnimationClip", "anim"),
        83 => ("AudioClip", "ogg"),
        89 => ("Cubemap", "png"),
        90 => ("Avatar", "asset"),
        91 => ("AnimatorController", "controller"),
        128 => ("Font", "ttf"),
        134 => ("PhysicMaterial", "physicMaterial"),
        213 => ("Sprite", "asset"),
        241 => ("AudioMixerController", "mixer"),
        243 => ("AudioMixerGroupController", "asset"),
        114 => ("MonoBehaviour", "asset"),
        _ => return None,
    })
}

impl Ctx<'_> {
    /// A PPtr of an object in `file` as the extractor's reference string.
    fn reference(&self, files: &mut Files, file: usize, p: (i32, i64)) -> Value {
        if let Some(set) = &self.raw {
            return self.raw_reference(set, files, file, p);
        }
        if p.1 == 0 {
            return Value::Null;
        }
        let Some(node) = files.target_node(file, p.0) else {
            return json!(format!("ref-missing:{}", p.1));
        };
        if let Some(s) = self.local.get(&(node, p.1)) {
            return json!(s);
        }
        let b = &self.owned.0.bundle;
        let path = &b.nodes[node].path;
        if path == "unity default resources" {
            // the old exports could not resolve Unity's default resources either
            return json!(format!("ref-missing:{}", p.1));
        }
        if path.ends_with("unity_builtin_extra") {
            return json!(format!("asset-guid:0000000000000000f000000000000000:{}", p.1));
        }
        let Some((gnode, class, d)) = files.read(file, p) else { return json!(format!("ref-missing:{}", p.1)) };
        let be = false;
        if class == 243 {
            // a mixer group: the old exports referenced the mixer (sub-asset of the controller file)
            let mut r = ObjReader::new(&d, be);
            if let (Ok(_), Ok(m)) = (r.string(), r.pptr()) {
                // (the mixer pointer is relative to the group's own file)
                if let Some((_, 241, md)) = files.read(gnode, m) {
                    if let Some(n) = unity_fs::object_name(&md, be) {
                        return json!(format!("asset:AudioMixerController/{n}.mixer"));
                    }
                }
            }
        }
        let name = match class {
            1 => unity_fs::GameObject::read(&d, be).map(|g| g.name).ok(),
            114 => {
                // MonoBehaviour assets: m_GameObject, m_Enabled, m_Script, then m_Name
                let mut r = ObjReader::new(&d, be);
                (|| -> R<String> {
                    r.pptr()?;
                    r.bool()?;
                    r.align();
                    r.pptr()?;
                    r.string()
                })()
                .ok()
                .filter(|n| !n.is_empty())
            }
            // shaders keep their name in the parsed form only (m_Name is empty in players)
            48 => self.shader_name(files, gnode, &d).ok().filter(|n| !n.is_empty()),
            _ => unity_fs::object_name(&d, be),
        };
        if let Some((dir, ext)) = asset_dir(class).filter(|_| class != 114 || name.as_deref().is_some_and(|n| !n.is_empty())) {
            if class != 1 {
                if let Some(n) = name {
                    // Unity's built-in resources copied into the build: the old exports kept their built-in ids
                    let builtin = match (class, n.as_str()) {
                        (21, "Default-Material") => Some(10303),
                        (21, "Default-Line") => Some(10306),
                        (21, "Default-ParticleSystem") => Some(10308),
                        (21, "Default-Particle") => Some(10301),
                        (21, "Sprites-Default") => Some(10754),
                        // UnityEngine.UI's default sprites
                        (213, "Checkmark") => Some(10901),
                        (213, "UISprite") => Some(10905),
                        (213, "Background") => Some(10907),
                        (213, "InputFieldBackground") => Some(10911),
                        (213, "Knob") => Some(10913),
                        (213, "DropdownArrow") => Some(10915),
                        (213, "UIMask") => Some(10917),
                        _ => None,
                    };
                    if let Some(id) = builtin {
                        return json!(format!("asset-guid:0000000000000000f000000000000000:{id}"));
                    }
                    // file names as exported (characters Windows rejects become '_'); same-named meshes / materials /
                    // textures: the old exports suffixed the 2nd, 3rd ... with _0, _1 ...
                    let mut fname = n.replace([':', '/', '\\', '*', '?', '"', '<', '>', '|'], "_");
                    if matches!(class, 21 | 28 | 43 | 74 | 90 | 91 | 114 | 213 | 241 | 245) {
                        let v = if class == 43 { self.owned.mesh_index_entries(&n).cloned() } else { self.owned.same_named_ci(class, &n) };
                        if let Some(i) = v.and_then(|v| v.iter().position(|&(nn, pp)| nn == node && pp == p.1)).filter(|&i| i > 0) {
                            fname = format!("{fname}_{}", i - 1);
                        }
                    }
                    // assets listed in the Resources table keep their Resources path
                    if let Some(k) = self.owned.resource_path((node, p.1)) {
                        let d = k.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
                        return json!(format!("asset:Resources/{d}{fname}.{ext}"));
                    }
                    return json!(format!("asset:{dir}/{fname}.{ext}"));
                }
            }
        }
        // a GameObject or component of another prefab: the old exports referenced that prefab's file
        let go = if class == 1 { Some((file, p)) } else { files.read(file, p).and_then(|(cn, _, cd)| ObjReader::new(&cd, be).pptr().ok().map(|g| (cn, g))) };
        if let Some((gfile, g)) = go {
            if let Some(root) = root_name(files, gfile, g) {
                // prefabs in the Resources folder (the game's random weapons) kept their Resources path
                if let Some(&(rn, rp)) = self.owned.resources().get(&format!("randomweapons/{}", root.to_lowercase())) {
                    if root_go(files, gfile, g) == Some((rn, rp)) {
                        return json!(format!("asset:Resources/randomweapons/{root}.prefab"));
                    }
                }
                // same-named prefabs: the old exports suffixed the 2nd, 3rd ... with _0, _1 ...
                if let (Some(v), Some(rg)) = (self.owned.prefab_entries(&root), root_go(files, gfile, g)) {
                    if let Some(i) = v.iter().position(|&x| x == rg).filter(|&i| i > 0) {
                        return json!(format!("asset:GameObject/{root}_{}.prefab", i - 1));
                    }
                }
                return json!(format!("asset:GameObject/{root}.prefab"));
            }
        }
        json!(format!("ref-missing:{}", p.1))
    }

    fn script_value(&self, r: &mut ObjReader, ty: &TypeSig, files: &mut Files, file: usize) -> R<Value> {
        use TypeSig as T;
        Ok(match ty {
            T::Prim(p) => {
                let v = match *p {
                    "bool" => json!(r.bool()? as u8),
                    "byte" | "sbyte" => json!(ObjReaderExt::u8(r)?),
                    "char" | "ushort" | "short" => json!(ObjReaderExt::u16(r)?),
                    "int" => json!(r.i32()?),
                    "uint" => json!(r.u32()?),
                    "long" => json!(r.i64()?),
                    "ulong" => json!(r.u64()? as f64),
                    "float" => json!(r.f32()?),
                    "double" => json!(f64::from_bits(r.u64()?)),
                    _ => return Err(format!("primitive {p}")),
                };
                r.align();
                v
            }
            T::String => {
                // the old extractor wrote empty YAML scalars as null
                let t = r.string()?;
                if t.is_empty() { Value::Null } else { json!(t) }
            }
            T::Named { name, .. } if name == "Vector3" => v3(r)?,
            T::Named { name, .. } if name == "Vector2" => v2(r)?,
            T::Named { name, .. } if name == "Vector4" => json!({ "x": r.f32()?, "y": r.f32()?, "z": r.f32()?, "w": r.f32()? }),
            T::Named { name, .. } if name == "Quaternion" => quat(r)?,
            T::Named { name, .. } if name == "Color" => color(r)?,
            T::Named { name, .. } if name == "Color32" => json!({ "rgba": r.u32()? }),
            T::Named { name, .. } if name == "Rect" => json!({ "x": r.f32()?, "y": r.f32()?, "width": r.f32()?, "height": r.f32()? }),
            // Unity serializes these engine classes / structs although they are not [Serializable]
            T::Named { name, .. } if name == "RectOffset" => json!({ "m_Left": r.i32()?, "m_Right": r.i32()?, "m_Top": r.i32()?, "m_Bottom": r.i32()? }),
            T::Named { name, .. } if name == "Vector2Int" => json!({ "x": r.i32()?, "y": r.i32()? }),
            T::Named { name, .. } if name == "Vector3Int" => json!({ "x": r.i32()?, "y": r.i32()?, "z": r.i32()? }),
            T::Named { name, .. } if name == "LayerMask" => json!({ "m_Bits": r.u32()?, "serializedVersion": 2 }),
            T::Named { name, .. } if name == "AnimationCurve" => curve(r)?,
            T::Named { name, .. } if name == "Gradient" => {
                // key0..key7 (RGBA), ctime0..7, atime0..7 (u16), m_Mode, m_NumColorKeys, m_NumAlphaKeys
                let mut m = Map::new();
                for i in 0..8 {
                    m.insert(format!("key{i}"), color(r)?);
                }
                for p in ["ctime", "atime"] {
                    for i in 0..8 {
                        m.insert(format!("{p}{i}"), json!(r.short()?));
                    }
                }
                m.insert("m_Mode".into(), json!(r.i32()?));
                m.insert("m_NumColorKeys".into(), json!(r.byte()?));
                m.insert("m_NumAlphaKeys".into(), json!(r.byte()?));
                r.align();
                Value::Object(m)
            }
            T::Named { namespace, name, .. } if self.asm.is_enum(namespace, name) => {
                // enums are stored with their underlying type's size
                let u = self.asm.enum_underlying(namespace, name);
                return self.script_value(r, &T::Prim(u), files, file);
            }
            T::Named { namespace, name, .. } if self.asm.is_unity_object(namespace, name) => {
                let p = r.pptr()?;
                self.reference(files, file, p)
            }
            T::Named { namespace, name, .. } if self.asm.find(namespace, name).is_some_and(|t| t.flags & 0x2000 != 0) => {
                let t = self.asm.find(namespace, name).unwrap();
                let mut m = Map::new();
                let dbg = std::env::var("RESTRAFE_DEBUG_MB").is_ok_and(|c| c == *name);
                for f in self.asm.serialized_fields(t) {
                    let at = r.pos();
                    let v = self.script_value(r, &f.ty, files, file).map_err(|e| format!("{name}.{}: {e}", f.name))?;
                    if dbg {
                        println!("MB {name}.{} @{at} {} = {}", f.name, f.ty, v.to_string().chars().take(120).collect::<String>());
                    }
                    m.insert(f.name.clone(), v);
                }
                Value::Object(m)
            }
            T::Array(e) => self.script_list(r, e, files, file)?,
            T::Generic { base, args } if matches!(&**base, T::Named { name, .. } if name == "List`1") && args.len() == 1 => self.script_list(r, &args[0], files, file)?,
            t => return Err(format!("field type {t} not handled yet")),
        })
    }

    /// A script array / List: numbers as the YAML wrote them (little-endian hex text, empty = null), else a list.
    fn script_list(&self, r: &mut ObjReader, e: &TypeSig, files: &mut Files, file: usize) -> R<Value> {
        let n = r.i32()? as usize;
        let size = match e {
            TypeSig::Prim("bool" | "byte" | "sbyte") => 1,
            TypeSig::Prim("short" | "ushort" | "char") => 2,
            TypeSig::Prim("int" | "uint" | "float") => 4,
            TypeSig::Prim("long" | "ulong" | "double") => 8,
            _ => 0,
        };
        let v = if size > 0 {
            if n == 0 {
                Value::Null
            } else {
                let mut hex = String::with_capacity(n * size * 2);
                for _ in 0..n * size {
                    hex.push_str(&format!("{:02x}", r.byte()?));
                }
                json!(hex)
            }
        } else {
            json!((0..n).map(|_| self.script_value(r, e, files, file)).collect::<R<Vec<_>>>()?)
        };
        r.align();
        Ok(v)
    }

    /// One component: (type name, fields). Err for classes without a layout here.
    fn component(&self, files: &mut Files, file: usize, class: i32, d: &[u8]) -> R<(String, Map<String, Value>)> {
        let mut r = ObjReader::new(d, false);
        let mut f = Map::new();
        macro_rules! put {
            ($k:expr, $v:expr) => {
                f.insert($k.to_string(), $v)
            };
        }
        let collider_base = |r: &mut ObjReader, f: &mut Map<String, Value>, files: &mut Files| -> R<()> {
            r.pptr()?; // m_GameObject
            let m = r.pptr()?;
            f.insert("m_Material".into(), self.reference(files, file, m));
            f.insert("m_IsTrigger".into(), json!(r.bool()? as u8));
            f.insert("m_Enabled".into(), json!(r.bool()? as u8));
            r.align();
            Ok(())
        };
        let name = match class {
            4 | 224 => {
                let t = unity_fs::Transform::read(d, false)?;
                put!("m_LocalRotation", json!({ "x": t.rotation[0], "y": t.rotation[1], "z": t.rotation[2], "w": t.rotation[3] }));
                put!("m_LocalPosition", json!({ "x": t.position[0], "y": t.position[1], "z": t.position[2] }));
                put!("m_LocalScale", json!({ "x": t.scale[0], "y": t.scale[1], "z": t.scale[2] }));
                put!("m_Children", json!(t.children.iter().map(|&c| self.reference(files, file, c)).collect::<Vec<_>>()));
                put!("m_Father", self.reference(files, file, t.father));
                if class == 224 {
                    // RectTransform: anchors, position, size, pivot after the Transform fields
                    let mut r = ObjReader::new(d, false);
                    r.pptr()?;
                    for _ in 0..10 {
                        r.f32()?;
                    }
                    let n = r.i32()? as usize;
                    for _ in 0..n {
                        r.pptr()?;
                    }
                    r.pptr()?;
                    put!("m_AnchorMin", v2(&mut r)?);
                    put!("m_AnchorMax", v2(&mut r)?);
                    put!("m_AnchoredPosition", v2(&mut r)?);
                    put!("m_SizeDelta", v2(&mut r)?);
                    put!("m_Pivot", v2(&mut r)?);
                    "RectTransform"
                } else {
                    "Transform"
                }
            }
            33 => {
                r.pptr()?;
                let m = r.pptr()?;
                put!("m_Mesh", self.reference(files, file, m));
                "MeshFilter"
            }
            23 | 137 => {
                r.pptr()?;
                let bytes: Vec<u8> = (0..10).map(|_| ObjReaderExt::u8(&mut r).map(|x| x as u8)).collect::<R<_>>()?;
                r.align();
                for (k, i) in [("m_Enabled", 0), ("m_CastShadows", 1), ("m_ReceiveShadows", 2), ("m_DynamicOccludee", 3), ("m_StaticShadowCaster", 4), ("m_MotionVectors", 5), ("m_LightProbeUsage", 6), ("m_ReflectionProbeUsage", 7), ("m_RayTracingMode", 8), ("m_RayTraceProcedural", 9)] {
                    put!(k, json!(bytes[i]));
                }
                put!("m_RenderingLayerMask", json!(r.u32()?));
                put!("m_RendererPriority", json!(r.i32()?));
                put!("m_LightmapIndex", json!(ObjReaderExt::u16(&mut r)?));
                put!("m_LightmapIndexDynamic", json!(ObjReaderExt::u16(&mut r)?));
                put!("m_LightmapTilingOffset", json!({ "x": r.f32()?, "y": r.f32()?, "z": r.f32()?, "w": r.f32()? }));
                put!("m_LightmapTilingOffsetDynamic", json!({ "x": r.f32()?, "y": r.f32()?, "z": r.f32()?, "w": r.f32()? }));
                let n = r.i32()? as usize;
                let mats: Vec<Value> = (0..n).map(|_| r.pptr().map(|p| self.reference(files, file, p))).collect::<R<_>>()?;
                put!("m_Materials", json!(mats));
                let (first, count) = (ObjReaderExt::u16(&mut r)?, ObjReaderExt::u16(&mut r)?);
                put!("m_StaticBatchInfo", json!({ "firstSubMesh": first, "subMeshCount": count }));
                let sbr = r.pptr()?;
                put!("m_StaticBatchRoot", self.reference(files, file, sbr));
                let pa = r.pptr()?;
                put!("m_ProbeAnchor", self.reference(files, file, pa));
                let lpv = r.pptr()?;
                put!("m_LightProbeVolumeOverride", self.reference(files, file, lpv));
                put!("m_SortingLayerID", json!(r.i32()?));
                put!("m_SortingLayer", json!(ObjReaderExt::u16(&mut r)? as i16));
                put!("m_SortingOrder", json!(ObjReaderExt::u16(&mut r)? as i16));
                r.align();
                if class == 23 {
                    let avs = r.pptr()?;
                    put!("m_AdditionalVertexStreams", self.reference(files, file, avs));
                    "MeshRenderer"
                } else {
                    put!("m_Quality", json!(r.i32()?));
                    put!("m_UpdateWhenOffscreen", b(&mut r)?);
                    put!("m_SkinnedMotionVectors", b(&mut r)?);
                    r.align();
                    let m = r.pptr()?;
                    put!("m_Mesh", self.reference(files, file, m));
                    let n = r.i32()? as usize;
                    let bones: Vec<Value> = (0..n).map(|_| r.pptr().map(|p| self.reference(files, file, p))).collect::<R<_>>()?;
                    put!("m_Bones", json!(bones));
                    let nw = r.i32()? as usize;
                    put!("m_BlendShapeWeights", json!((0..nw).map(|_| r.f32()).collect::<R<Vec<_>>>()?));
                    let root = r.pptr()?;
                    put!("m_RootBone", self.reference(files, file, root));
                    put!("m_AABB", json!({ "m_Center": v3(&mut r)?, "m_Extent": v3(&mut r)? }));
                    put!("m_DirtyAABB", b(&mut r)?);
                    "SkinnedMeshRenderer"
                }
            }
            65 => {
                collider_base(&mut r, &mut f, files)?;
                put!("m_Size", v3(&mut r)?);
                put!("m_Center", v3(&mut r)?);
                "BoxCollider"
            }
            135 => {
                collider_base(&mut r, &mut f, files)?;
                put!("m_Radius", json!(r.f32()?));
                put!("m_Center", v3(&mut r)?);
                "SphereCollider"
            }
            136 => {
                collider_base(&mut r, &mut f, files)?;
                put!("m_Radius", json!(r.f32()?));
                put!("m_Height", json!(r.f32()?));
                put!("m_Direction", json!(r.i32()?));
                put!("m_Center", v3(&mut r)?);
                "CapsuleCollider"
            }
            64 => {
                collider_base(&mut r, &mut f, files)?;
                put!("m_Convex", b(&mut r)?);
                r.align();
                put!("m_CookingOptions", json!(r.i32()?));
                let m = r.pptr()?;
                put!("m_Mesh", self.reference(files, file, m));
                "MeshCollider"
            }
            143 => {
                collider_base(&mut r, &mut f, files)?;
                for k in ["m_Height", "m_Radius", "m_SlopeLimit", "m_StepOffset", "m_SkinWidth", "m_MinMoveDistance"] {
                    put!(k, json!(r.f32()?));
                }
                put!("m_Center", v3(&mut r)?);
                "CharacterController"
            }
            54 => {
                r.pptr()?;
                put!("m_Mass", json!(r.f32()?));
                put!("m_Drag", json!(r.f32()?));
                put!("m_AngularDrag", json!(r.f32()?));
                put!("m_UseGravity", b(&mut r)?);
                put!("m_IsKinematic", b(&mut r)?);
                put!("m_Interpolate", json!(ObjReaderExt::u8(&mut r)?));
                r.align();
                put!("m_Constraints", json!(r.i32()?));
                put!("m_CollisionDetection", json!(r.i32()?));
                "Rigidbody"
            }
            82 => {
                r.pptr()?;
                put!("m_Enabled", b(&mut r)?);
                r.align();
                let g = r.pptr()?;
                put!("OutputAudioMixerGroup", self.reference(files, file, g));
                let c = r.pptr()?;
                put!("m_audioClip", self.reference(files, file, c));
                put!("m_PlayOnAwake", b(&mut r)?);
                r.align();
                put!("m_Volume", json!(r.f32()?));
                put!("m_Pitch", json!(r.f32()?));
                for k in ["Loop", "Mute", "Spatialize", "SpatializePostEffects"] {
                    put!(k, b(&mut r)?);
                }
                r.align();
                put!("Priority", json!(r.i32()?));
                for k in ["DopplerLevel", "MinDistance", "MaxDistance", "Pan2D"] {
                    put!(k, json!(r.f32()?));
                }
                put!("rolloffMode", json!(r.i32()?));
                for k in ["BypassEffects", "BypassListenerEffects", "BypassReverbZones"] {
                    put!(k, b(&mut r)?);
                }
                r.align();
                for k in ["rolloffCustomCurve", "panLevelCustomCurve", "spreadCustomCurve", "reverbZoneMixCustomCurve"] {
                    put!(k, curve(&mut r)?);
                }
                "AudioSource"
            }
            114 => {
                r.pptr()?;
                put!("m_Enabled", b(&mut r)?);
                r.align();
                let script = r.pptr()?;
                let nm = r.string()?;
                put!("m_Name", if nm.is_empty() { Value::Null } else { json!(nm) });
                let (snode, _, sd) = files.read(file, script).ok_or("MonoScript missing")?;
                let _ = snode;
                let mut sr = ObjReader::new(&sd, false);
                sr.string()?; // m_Name
                sr.i32()?; // m_ExecutionOrder
                for _ in 0..4 {
                    sr.u32()?; // m_PropertiesHash
                }
                let class_name = sr.string()?;
                let namespace = sr.string()?;
                let t = self.asm.find(&namespace, &class_name).ok_or_else(|| format!("script {namespace}.{class_name} not in the managed assemblies"))?;
                let dbg = std::env::var("RESTRAFE_DEBUG_MB").is_ok_and(|c| c == class_name);
                for fd in self.asm.serialized_fields(t) {
                    let at = r.pos();
                    let v = self.script_value(&mut r, &fd.ty, files, file).map_err(|e| format!("{class_name}.{}: {e}", fd.name))?;
                    if dbg {
                        println!("MB {class_name}.{} @{at} {} = {}", fd.name, fd.ty, v.to_string().chars().take(120).collect::<String>());
                    }
                    f.insert(fd.name.clone(), v);
                }
                return Ok((class_name, f));
            }
            95 => {
                r.pptr()?;
                put!("m_Enabled", b(&mut r)?);
                r.align();
                let a = r.pptr()?;
                put!("m_Avatar", self.reference(files, file, a));
                let c = r.pptr()?;
                put!("m_Controller", self.reference(files, file, c));
                put!("m_CullingMode", json!(r.i32()?));
                put!("m_UpdateMode", json!(r.i32()?));
                for k in ["m_ApplyRootMotion", "m_LinearVelocityBlending", "m_StabilizeFeet"] {
                    put!(k, b(&mut r)?);
                }
                r.align();
                for k in ["m_HasTransformHierarchy", "m_AllowConstantClipSamplingOptimization", "m_KeepAnimatorStateOnDisable", "m_WriteDefaultValuesOnDisable"] {
                    put!(k, b(&mut r)?);
                }
                r.align();
                "Animator"
            }
            108 => {
                r.pptr()?;
                put!("m_Enabled", b(&mut r)?);
                r.align();
                put!("m_Type", json!(r.i32()?));
                put!("m_Shape", json!(r.i32()?));
                put!("m_Color", color(&mut r)?);
                for k in ["m_Intensity", "m_Range", "m_SpotAngle", "m_InnerSpotAngle", "m_CookieSize"] {
                    put!(k, json!(r.f32()?));
                }
                let mut sh = Map::new();
                sh.insert("m_Type".into(), json!(r.i32()?));
                sh.insert("m_Resolution".into(), json!(r.i32()?));
                sh.insert("m_CustomResolution".into(), json!(r.i32()?));
                for k in ["m_Strength", "m_Bias", "m_NormalBias", "m_NearPlane"] {
                    sh.insert(k.into(), json!(r.f32()?));
                }
                let mut m = Map::new();
                for i in 0..16 {
                    m.insert(format!("e{}{}", i / 4, i % 4), json!(r.f32()?));
                }
                sh.insert("m_CullingMatrixOverride".into(), Value::Object(m));
                sh.insert("m_UseCullingMatrixOverride".into(), b(&mut r)?);
                r.align();
                put!("m_Shadows", Value::Object(sh));
                let ck = r.pptr()?;
                put!("m_Cookie", self.reference(files, file, ck));
                put!("m_DrawHalo", b(&mut r)?);
                r.align();
                // m_BakingOutput (player data)
                let (pi, oc, bt, ml) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
                let baked = r.bool()?;
                r.align();
                put!("m_BakingOutput", json!({ "probeOcclusionLightIndex": pi, "occlusionMaskChannel": oc, "lightmapBakeMode": { "lightmapBakeType": bt, "mixedLightingMode": ml }, "isBaked": baked as u8 }));
                let fl = r.pptr()?;
                put!("m_Flare", self.reference(files, file, fl));
                put!("m_RenderMode", json!(r.i32()?));
                put!("m_CullingMask", json!({ "m_Bits": r.u32()?, "serializedVersion": 2 }));
                put!("m_RenderingLayerMask", json!(r.i32()?));
                put!("m_Lightmapping", json!(r.i32()?));
                put!("m_LightShadowCasterMode", json!(r.i32()?));
                put!("m_AreaSize", v2(&mut r)?);
                put!("m_BounceIntensity", json!(r.f32()?));
                put!("m_ColorTemperature", json!(r.f32()?));
                put!("m_UseColorTemperature", b(&mut r)?);
                r.align();
                put!("m_BoundingSphereOverride", json!({ "x": fval(r.f32()?), "y": fval(r.f32()?), "z": fval(r.f32()?), "w": fval(r.f32()?) }));
                put!("m_UseBoundingSphereOverride", b(&mut r)?);
                put!("m_UseViewFrustumForShadowCasterCull", b(&mut r)?);
                r.align();
                // baking-only in the editor; player data usually ends here
                put!("m_ShadowRadius", json!(r.f32().unwrap_or(0.0)));
                put!("m_ShadowAngle", json!(r.f32().unwrap_or(0.0)));
                "Light"
            }
            215 => {
                // ReflectionProbe (the old extractor named it "Unknown")
                r.pptr()?;
                put!("m_Enabled", b(&mut r)?);
                r.align();
                for k in ["m_Type", "m_Mode", "m_RefreshMode", "m_TimeSlicingMode", "m_Resolution", "m_UpdateFrequency"] {
                    put!(k, json!(r.i32()?));
                }
                put!("m_BoxSize", v3(&mut r)?);
                put!("m_BoxOffset", v3(&mut r)?);
                for k in ["m_NearClip", "m_FarClip", "m_ShadowDistance"] {
                    put!(k, json!(r.f32()?));
                }
                put!("m_ClearFlags", json!(r.u32()?));
                put!("m_BackGroundColor", color(&mut r)?);
                put!("m_CullingMask", json!({ "m_Bits": r.u32()?, "serializedVersion": 2 }));
                put!("m_IntensityMultiplier", json!(r.f32()?));
                put!("m_BlendDistance", json!(r.f32()?));
                for k in ["m_HDR", "m_BoxProjection", "m_RenderDynamicObjects", "m_UseOcclusionCulling"] {
                    put!(k, b(&mut r)?);
                }
                put!("m_Importance", json!(ObjReaderExt::u16(&mut r)? as i16));
                r.align();
                let t = r.pptr()?;
                put!("m_CustomBakedTexture", self.reference(files, file, t));
                "Unknown"
            }
            81 => {
                r.pptr()?;
                put!("m_Enabled", b(&mut r)?);
                r.align();
                "Unknown" // AudioListener (the old extractor had no name for it)
            }
            c if engine_types().contains_key(&c) => {
                let nodes = &engine_types()[&c];
                let (v, _) = self.tree_value(nodes, 0, &mut r, files, file)?;
                let mut m = v.as_object().cloned().unwrap_or_default();
                m.remove("m_GameObject");
                f = m;
                // type names the old extractor knew; every other class was "Unknown"
                let known = ["Camera", "TrailRenderer", "LineRenderer", "ParticleSystem", "ParticleSystemRenderer", "SpriteRenderer", "CanvasRenderer", "Canvas"];
                let t = if known.contains(&nodes[0].ty.as_str()) { nodes[0].ty.clone() } else { "Unknown".to_string() };
                return Ok((t, f));
            }
            c => return Err(format!("component class {c} has no layout yet")),
        };
        Ok((name.to_string(), f))
    }
}

/// Name of the root GameObject above the GameObject `g` (PPtr seen from `file`).
fn root_name(files: &mut Files, file: usize, g: (i32, i64)) -> Option<String> {
    let (mut gn, _, mut gd) = files.read(file, g)?;
    for _ in 0..64 {
        let go = unity_fs::GameObject::read(&gd, false).ok()?;
        let tf = go.components.iter().find_map(|c| files.read(gn, *c).filter(|(_, cl, _)| *cl == 4 || *cl == 224))?;
        let t = unity_fs::Transform::read(&tf.2, false).ok()?;
        if t.father.1 == 0 {
            return Some(go.name);
        }
        let (fnode, _, fd) = files.read(tf.0, t.father)?;
        let ft = unity_fs::Transform::read(&fd, false).ok()?;
        let (n2, _, d2) = files.read(fnode, ft.game_object)?;
        gn = n2;
        gd = d2;
    }
    None
}

/// (file, path id) of the root GameObject above `g`.
fn root_go(files: &mut Files, file: usize, g: (i32, i64)) -> Option<(usize, i64)> {
    let mut cur_file = files.target_node(file, g.0)?;
    let mut cur = g.1;
    for _ in 0..64 {
        let (gn, _, gd) = files.read(cur_file, (0, cur))?;
        let go = unity_fs::GameObject::read(&gd, false).ok()?;
        let tf = go.components.iter().find_map(|c| files.read(gn, *c).filter(|(_, cl, _)| *cl == 4 || *cl == 224))?;
        let t = unity_fs::Transform::read(&tf.2, false).ok()?;
        if t.father.1 == 0 {
            return Some((gn, cur));
        }
        let (fnode, _, fd) = files.read(tf.0, t.father)?;
        let ft = unity_fs::Transform::read(&fd, false).ok()?;
        cur_file = files.target_node(fnode, ft.game_object.0)?;
        cur = ft.game_object.1;
    }
    None
}

/// One node of an engine type tree (`engine_types.txt`, generated from UnityPy's type tree package).
#[derive(Clone)]
struct TNode {
    level: u32,
    ty: String,
    name: String,
    flags: u32,
}

/// class id -> nodes (pre-order), for the engine classes read generically.
fn engine_types() -> &'static HashMap<i32, Vec<TNode>> {
    static T: std::sync::OnceLock<HashMap<i32, Vec<TNode>>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut map: HashMap<i32, Vec<TNode>> = HashMap::new();
        let mut cur = None;
        for l in include_str!("engine_types.txt").lines() {
            if l.starts_with('#') || l.is_empty() {
                continue;
            }
            if let Some(c) = l.strip_prefix("class ") {
                cur = c.trim().parse::<i32>().ok();
                continue;
            }
            let f: Vec<&str> = l.split('\t').collect();
            if let (Some(c), 5) = (cur, f.len()) {
                map.entry(c).or_default().push(TNode { level: f[0].parse().unwrap_or(0), ty: f[1].into(), name: f[2].into(), flags: f[4].parse().unwrap_or(0) });
            }
        }
        map
    })
}

impl Ctx<'_> {
    /// See `Ctx::raw`.
    fn raw_reference(&self, set: &std::collections::HashSet<(usize, i64)>, files: &mut Files, file: usize, p: (i32, i64)) -> Value {
        if p.1 == 0 {
            return json!({ "fileID": 0 });
        }
        let Some(node) = files.target_node(file, p.0) else { return json!({ "fileID": p.1 }) };
        if set.contains(&(node, p.1)) {
            return json!({ "fileID": p.1 });
        }
        let path = self.owned.0.bundle.nodes[node].path.clone();
        if path.ends_with("unity_builtin_extra") {
            return json!({ "fileID": p.1, "guid": crate::owned_docs::BUILTIN_EXTRA, "type": 0 });
        }
        if path == "unity default resources" {
            return json!({ "fileID": p.1, "guid": crate::owned_docs::DEFAULT_RESOURCES, "type": 0 });
        }
        // Unity's built-in resources copied into the build keep their built-in ids (as the old exports did)
        if let Some((_, class, d)) = files.read(file, p) {
            let name = match class {
                48 => self.shader_name(files, node, &d).ok(),
                21 | 213 | 28 => unity_fs::object_name(&d, false),
                _ => None,
            };
            if let Some(id) = name.and_then(|n| crate::owned_docs::builtin_id(class, &n)) {
                return json!({ "fileID": id, "guid": crate::owned_docs::BUILTIN_EXTRA, "type": 0 });
            }
        }
        json!({ "fileID": p.1, "guid": crate::owned_docs::token(node, p.1), "type": 2 })
    }

    /// A Shader's name (`m_ParsedForm.m_Name`; the object's own m_Name is empty in players).
    fn shader_name(&self, files: &mut Files, node: usize, d: &[u8]) -> R<String> {
        let nodes = engine_types().get(&48).ok_or("no Shader layout")?;
        let mut r = ObjReader::new(d, false);
        let (_, i) = self.tree_value(nodes, 1, &mut r, files, node)?; // m_Name
        let (pf, _) = self.tree_value(nodes, i, &mut r, files, node)?; // m_ParsedForm
        Ok(pf["m_Name"].as_str().unwrap_or("").to_string())
    }

    /// Reads the node at `i` (and its subtree); returns the value and the index after the subtree.
    fn tree_value(&self, nodes: &[TNode], i: usize, r: &mut ObjReader, files: &mut Files, file: usize) -> R<(Value, usize)> {
        let n = &nodes[i];
        let mut end = i + 1;
        while end < nodes.len() && nodes[end].level > n.level {
            end += 1;
        }
        let kids: Vec<usize> = (i + 1..end).filter(|&k| nodes[k].level == n.level + 1).collect();
        let v = match n.ty.as_str() {
            "bool" => json!(r.bool()? as u8),
            "SInt8" | "char" => json!(r.byte()? as i8),
            "UInt8" => json!(r.byte()?),
            "SInt16" | "short" => json!(r.short()? as i16),
            "UInt16" | "unsigned short" => json!(r.short()?),
            "int" | "SInt32" | "Type*" => json!(r.i32()?),
            "UInt32" | "unsigned int" => json!(r.u32()?),
            "SInt64" | "long long" => json!(r.i64()?),
            "UInt64" | "unsigned long long" | "FileSize" => json!(r.u64()? as f64),
            "float" => {
                fval(r.f32()?)
            }
            "double" => json!(f64::from_bits(r.u64()?)),
            "string" => {
                let t = r.string()?;
                return Ok((if t.is_empty() { Value::Null } else { json!(t) }, end));
            }
            t if t.starts_with("PPtr<") => {
                let p = r.pptr()?;
                self.reference(files, file, p)
            }
            "TypelessData" => {
                let len = r.i32()? as usize;
                for _ in 0..len {
                    r.byte()?;
                }
                json!(len)
            }
            _ if kids.len() == 1 && nodes[kids[0]].ty == "Array" => {
                // vector / map / staticvector: Array { size, data }
                let arr = kids[0];
                let akids: Vec<usize> = (arr + 1..end).filter(|&k| nodes[k].level == nodes[arr].level + 1).collect();
                let count = r.i32()? as usize;
                let data = *akids.get(1).ok_or("array without data node")?;
                if matches!(nodes[data].ty.as_str(), "UInt8" | "SInt8" | "char") && end == data + 1 + (end - data - 1) && nodes[data].level == nodes[arr].level + 1 && (data + 1 == end || nodes[data + 1].level <= nodes[arr].level) {
                    // byte arrays: hex text, as in the YAML the old extractor read
                    let mut hex = String::with_capacity(count * 2);
                    for _ in 0..count {
                        hex.push_str(&format!("{:02x}", r.byte()?));
                    }
                    if nodes[arr].flags & 0x4000 != 0 {
                        r.align();
                    }
                    if n.flags & 0x4000 != 0 {
                        r.align();
                    }
                    return Ok((json!(hex), end));
                }
                let mut items = Vec::with_capacity(count.min(1 << 16));
                for _ in 0..count {
                    items.push(self.tree_value(nodes, data, r, files, file)?.0);
                }
                if nodes[arr].flags & 0x4000 != 0 {
                    r.align();
                }
                json!(items)
            }
            _ => {
                let mut m = Map::new();
                for &k in &kids {
                    let (v, _) = self.tree_value(nodes, k, r, files, file)?;
                    m.insert(nodes[k].name.clone(), v);
                }
                Value::Object(m)
            }
        };
        if n.flags & 0x4000 != 0 {
            r.align();
        }
        Ok((v, end))
    }
}

trait ObjReaderExt {
    fn u8(&mut self) -> R<u32>;
    fn u16(&mut self) -> R<u32>;
}
impl ObjReaderExt for ObjReader<'_> {
    fn u8(&mut self) -> R<u32> {
        self.byte().map(|b| b as u32)
    }
    fn u16(&mut self) -> R<u32> {
        self.short().map(|s| s as u32)
    }
}

/// Tag names from the TagManager (built-in ids 0..7, custom ones from 20000).
fn tag_names(owned: &Owned) -> Vec<String> {
    let b = &owned.0.bundle;
    let Some(gi) = b.nodes.iter().position(|n| n.path == "globalgamemanagers") else { return vec![] };
    let Ok(sf) = unity_fs::SerializedFile::open(b, &b.nodes[gi]) else { return vec![] };
    let Some(o) = sf.objects.iter().find(|o| o.class_id == 78) else { return vec![] };
    let Ok(d) = sf.object_data(b, &b.nodes[gi], o) else { return vec![] };
    let mut r = ObjReader::new(&d, sf.big_endian);
    let Ok(n) = r.i32() else { return vec![] };
    (0..n).filter_map(|_| r.string().ok()).collect()
}

fn tag_name(tags: &[String], t: u16) -> String {
    match t {
        0 => "Untagged".into(),
        1 => "Respawn".into(),
        2 => "Finish".into(),
        3 => "EditorOnly".into(),
        5 => "MainCamera".into(),
        6 => "Player".into(),
        7 => "GameController".into(),
        t if t >= 20000 => tags.get((t - 20000) as usize).cloned().unwrap_or_default(),
        t => format!("tag{t}"),
    }
}

/// The prefab whose root GameObject is (`node`, `go`) in the extractor's JSON layout.
pub fn prefab_json(owned: &Owned, asm: &Assemblies, node: usize, go: i64) -> R<Value> {
    objects_json(owned, asm, node, &[go], false)
}

/// A scene (`levelN`) in the same layout: every root GameObject's hierarchy. Components without a layout here are
/// left out (logged) instead of failing the scene.
pub fn scene_json(owned: &Owned, asm: &Assemblies, level: usize) -> R<Value> {
    let b = &owned.0.bundle;
    let n = &b.nodes[level];
    let sf = unity_fs::SerializedFile::open(b, n)?;
    let mut roots = Vec::new();
    for o in sf.objects.iter().filter(|o| o.class_id == 4 || o.class_id == 224) {
        let t = unity_fs::Transform::read(&sf.object_data(b, n, o)?, sf.big_endian)?;
        if t.father.1 == 0 {
            roots.push(t.game_object.1);
        }
    }
    let mut v = objects_json(owned, asm, level, &roots, true)?;
    let name = n.path.clone();
    v["name"] = json!(name);
    Ok(v)
}

fn objects_json(owned: &Owned, asm: &Assemblies, node: usize, roots: &[i64], tolerant: bool) -> R<Value> {
    let prof = std::env::var("RESTRAFE_PROFILE_JSON").is_ok();
    let t0 = std::time::Instant::now();
    let mut files = Files::new(owned);
    // pass 1: walk the hierarchy, give every GameObject / component its "ref:" name
    struct Obj {
        path: String,
        file: usize,
        go: unity_fs::GameObject,
        comps: Vec<(usize, i64, i32, Vec<u8>)>,
    }
    let mut objs: Vec<Obj> = Vec::new();
    let mut local = HashMap::new();
    fn walk(files: &mut Files, objs: &mut Vec<Obj>, local: &mut HashMap<(usize, i64), String>, file: usize, go_id: i64, parent: &str) -> R<()> {
        let (gnode, _, gd) = files.read(file, (0, go_id)).ok_or("GameObject missing")?;
        let g = unity_fs::GameObject::read(&gd, false)?;
        let path = if parent.is_empty() { g.name.clone() } else { format!("{parent}/{}", g.name) };
        local.insert((gnode, go_id), format!("ref:{path}::GameObject"));
        let mut comps = Vec::new();
        let mut tf = None;
        for c in &g.components {
            let Some((cn, class, d)) = files.read(gnode, *c) else { continue };
            if class == 4 || class == 224 {
                tf = Some((cn, unity_fs::Transform::read(&d, false)?));
            }
            comps.push((cn, c.1, class, d));
        }
        let me = objs.len();
        objs.push(Obj { path: path.clone(), file: gnode, go: g, comps });
        let _ = me;
        if let Some((tn, t)) = tf {
            for ch in &t.children {
                let Some((cn, _, cd)) = files.read(tn, *ch) else { continue };
                let ct = unity_fs::Transform::read(&cd, false)?;
                walk(files, objs, local, cn, ct.game_object.1, &path)?;
            }
        }
        Ok(())
    }
    for &go in roots {
        let keep = objs.len();
        if let Err(e) = walk(&mut files, &mut objs, &mut local, node, go, "") {
            if !tolerant {
                return Err(e);
            }
            warn!("scene root {go}: {e}");
            objs.truncate(keep);
        }
    }
    let ctx = Ctx { owned, asm, local: HashMap::new(), tags: tag_names(owned), raw: None };
    // component type names for "ref:<path>::<Type>" (scripts by class name)
    let mut ctx = ctx;
    let mut types: Vec<Vec<String>> = Vec::new();
    for o in &objs {
        let mut tv = Vec::new();
        for (cn, pid, class, d) in &o.comps {
            let t = match ctx.component(&mut files, *cn, *class, d) {
                Ok((t, _)) => t,
                Err(e) if tolerant => {
                    warn!("{}: {e}", o.path);
                    String::new()
                }
                Err(e) => return Err(format!("{}: {e}", o.path)),
            };
            local.insert((*cn, *pid), format!("ref:{}::{t}", o.path));
            tv.push(t);
        }
        types.push(tv);
    }
    ctx.local = local;
    if prof {
        println!("objects_json: pass 1 ({} objects) {:?}", objs.len(), t0.elapsed());
    }
    // sibling index of every path (Unity's m_RootOrder; the first object of a duplicated path counts)
    let mut sibling: HashMap<&str, usize> = HashMap::new();
    let mut per_parent: HashMap<Option<&str>, usize> = HashMap::new();
    for o in &objs {
        let c = per_parent.entry(o.path.rsplit_once('/').map(|p| p.0)).or_insert(0);
        sibling.entry(o.path.as_str()).or_insert(*c);
        *c += 1;
    }
    // pass 2: fields with references resolved
    let mut out = Vec::new();
    for o in &objs {
        let mut comps = Vec::new();
        for (cn, pid, class, d) in &o.comps {
            let (t, mut fields) = match ctx.component(&mut files, *cn, *class, d) {
                Ok(x) => x,
                Err(_) if tolerant => continue,
                Err(e) => return Err(format!("{}: {e}", o.path)),
            };
            if *class == 4 || *class == 224 {
                // sibling index (Unity's m_RootOrder)
                fields.insert("m_RootOrder".into(), json!(sibling.get(o.path.as_str()).copied().unwrap_or(0)));
            }
            comps.push(json!({ "type": t, "id": pid, "fields": fields }));
        }
        out.push(json!({ "path": o.path, "layer": o.go.layer, "tag": tag_name(&ctx.tags, o.go.tag), "active": o.go.active, "components": comps }));
        let _ = o.file;
    }
    if prof {
        println!("objects_json: pass 2 done {:?}", t0.elapsed());
    }
    Ok(json!({ "name": objs.first().map(|o| o.go.name.clone()).unwrap_or_default(), "objects": out }))
}

/// One standalone object (a ScriptableObject-like MonoBehaviour ...): (type name, fields).
pub fn script_object(owned: &Owned, asm: &Assemblies, node: usize, pid: i64) -> R<(String, Map<String, Value>)> {
    let mut files = Files::new(owned);
    let (cn, class, d) = files.read(node, (0, pid)).ok_or("object missing")?;
    let ctx = Ctx { owned, asm, local: HashMap::new(), tags: Vec::new(), raw: None };
    ctx.component(&mut files, cn, class, &d)
}

/// Raw (YAML-style) documents of a hierarchy: every GameObject under `roots` (in `node`), its components, and the
/// `extra` objects (e.g. a scene's RenderSettings), as (path id, class, fields). References inside the set are
/// `{fileID}`, others `{fileID, guid}` (see `Ctx::raw`). MonoBehaviours carry their class name as "__script".
pub fn raw_docs(owned: &Owned, asm: &Assemblies, node: usize, roots: &[i64], extra: &[i64]) -> R<Vec<(i64, i32, Map<String, Value>)>> {
    let mut files = Files::new(owned);
    // the objects of the set: GameObjects, their components, children
    let mut gos: Vec<i64> = Vec::new();
    let mut comps: Vec<i64> = Vec::new();
    let mut stack: Vec<i64> = roots.to_vec();
    while let Some(go) = stack.pop() {
        let Some((gn, 1, gd)) = files.read(node, (0, go)) else { continue };
        if gn != node {
            continue;
        }
        let g = unity_fs::GameObject::read(&gd, false)?;
        gos.push(go);
        for c in &g.components {
            comps.push(c.1);
            if let Some((_, cls, cd)) = files.read(node, *c) {
                if cls == 4 || cls == 224 {
                    let t = unity_fs::Transform::read(&cd, false)?;
                    for ch in t.children.iter().rev() {
                        if let Some((_, _, chd)) = files.read(node, *ch) {
                            stack.push(ObjReader::new(&chd, false).pptr()?.1);
                        }
                    }
                }
            }
        }
    }
    let set: std::collections::HashSet<(usize, i64)> = gos.iter().chain(&comps).chain(extra).map(|&p| (node, p)).collect();
    let ctx = Ctx { owned, asm, local: HashMap::new(), tags: tag_names(owned), raw: Some(set) };
    let mut out = Vec::new();
    for &go in &gos {
        let Some((_, _, gd)) = files.read(node, (0, go)) else { continue };
        let g = unity_fs::GameObject::read(&gd, false)?;
        let mut m = Map::new();
        m.insert("m_Component".into(), json!(g.components.iter().map(|c| json!({ "component": { "fileID": c.1 } })).collect::<Vec<_>>()));
        m.insert("m_Layer".into(), json!(g.layer));
        m.insert("m_Name".into(), json!(g.name));
        m.insert("m_TagString".into(), json!(tag_name(&ctx.tags, g.tag)));
        m.insert("m_IsActive".into(), json!(g.active as u8));
        out.push((go, 1, m));
    }
    for &c in comps.iter().chain(extra) {
        let Some((cn, class, d)) = files.read(node, (0, c)) else { continue };
        let (t, mut f) = match ctx.component(&mut files, cn, class, &d) {
            Ok(x) => x,
            Err(e) => {
                warn!("raw docs: object {c} (class {class}): {e}");
                continue;
            }
        };
        if !extra.contains(&c) {
            let g = ObjReader::new(&d, false).pptr()?;
            f.insert("m_GameObject".into(), ctx.reference(&mut files, cn, g));
        }
        if class == 114 {
            f.insert("__script".into(), json!(t));
        }
        out.push((c, class, f));
    }
    Ok(out)
}

/// A Shader's name (`m_ParsedForm.m_Name`) without parsing the rest of it.
pub fn shader_name_at(owned: &Owned, asm: &Assemblies, node: usize, pid: i64) -> R<String> {
    let mut files = Files::new(owned);
    let (cn, _, d) = files.read(node, (0, pid)).ok_or("shader missing")?;
    let ctx = Ctx { owned, asm, local: HashMap::new(), tags: Vec::new(), raw: None };
    ctx.shader_name(&mut files, cn, &d)
}

/// One asset object's raw document (Material, Sprite, RenderTexture, MonoBehaviour asset ...), see [`raw_docs`].
pub fn raw_asset(owned: &Owned, asm: &Assemblies, node: usize, pid: i64) -> R<(i32, Map<String, Value>)> {
    let mut files = Files::new(owned);
    let set: std::collections::HashSet<(usize, i64)> = [(node, pid)].into_iter().collect();
    let ctx = Ctx { owned, asm, local: HashMap::new(), tags: Vec::new(), raw: Some(set) };
    let (cn, class, d) = files.read(node, (0, pid)).ok_or("object missing")?;
    if class == 21 {
        let m = unity_fs::Material::read(&d, false)?;
        let shader = ctx.reference(&mut files, cn, m.shader);
        let tex: Map<String, Value> = m
            .textures
            .iter()
            .map(|(k, t)| (k.clone(), json!({ "m_Texture": ctx.reference(&mut files, cn, t.texture), "m_Scale": { "x": t.scale[0], "y": t.scale[1] }, "m_Offset": { "x": t.offset[0], "y": t.offset[1] } })))
            .collect();
        let ints: Map<String, Value> = m.ints.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        let floats: Map<String, Value> = m.floats.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        let colors: Map<String, Value> = m.colors.iter().map(|(k, c)| (k.clone(), json!({ "r": c[0], "g": c[1], "b": c[2], "a": c[3] }))).collect();
        let mut f = Map::new();
        f.insert("m_Name".into(), json!(m.name));
        f.insert("m_Shader".into(), shader);
        f.insert("m_ValidKeywords".into(), json!(m.keywords));
        f.insert("m_CustomRenderQueue".into(), json!(m.render_queue));
        f.insert("stringTagMap".into(), Value::Object(m.tags.iter().map(|(k, v)| (k.clone(), json!(v))).collect()));
        f.insert("m_SavedProperties".into(), json!({ "m_TexEnvs": tex, "m_Ints": ints, "m_Floats": floats, "m_Colors": colors }));
        return Ok((21, f));
    }
    let (t, mut f) = ctx.component(&mut files, cn, class, &d)?;
    if class == 114 {
        f.insert("__script".into(), json!(t));
    }
    Ok((class, f))
}

/// A Material in the effect-material layout (`data/vfx/mat/<name>.json`): name, shader ("builtin:<id>" or
/// "Shader/<name with / as _>.shader"), render queue, float / colour properties and textures with their paths.
pub fn material_json(owned: &Owned, asm: &Assemblies, node: usize, pid: i64) -> R<Value> {
    let mut files = Files::new(owned);
    let (mn, class, d) = files.read(node, (0, pid)).ok_or("material missing")?;
    if class != 21 {
        return Err(format!("class {class} is not a Material"));
    }
    let m = unity_fs::Material::read(&d, false)?;
    let ctx = Ctx { owned, asm, local: HashMap::new(), tags: Vec::new(), raw: None };
    let shader = match files.read(mn, m.shader) {
        Some((sn, 48, sd)) if owned.0.bundle.nodes[sn].path.ends_with("unity_builtin_extra") => {
            let _ = sd;
            // built-in shaders by their path id in unity_builtin_extra
            let p = files_path_id(&mut files, mn, m.shader).unwrap_or(m.shader.1);
            json!(format!("builtin:{p}"))
        }
        Some((sn, 48, sd)) => {
            let name = ctx.shader_name(&mut files, sn, &sd)?;
            let name = name.as_str();
            // Unity's built-in shaders copied into the build keep their built-in file ids
            let builtin = match name {
                "Standard" => Some(46),
                "Legacy Shaders/Particles/Additive" => Some(200),
                "Legacy Shaders/Particles/Alpha Blended" => Some(203),
                "Legacy Shaders/Particles/VertexLit Blended" => Some(208),
                "Particles/Standard Unlit" => Some(211),
                "Mobile/Particles/Additive" => Some(10720),
                "Mobile/Particles/Alpha Blended" => Some(10721),
                "Mobile/Particles/Multiply" => Some(10723),
                _ => None,
            };
            match builtin {
                Some(id) => json!(format!("builtin:{id}")),
                None => json!(format!("Shader/{}.shader", name.replace('/', "_"))),
            }
        }
        _ => Value::Null,
    };
    let mut tex = Map::new();
    for (k, t) in &m.textures {
        if t.texture.1 == 0 {
            continue;
        }
        let path = match ctx.reference(&mut files, mn, t.texture) {
            // Unity's default particle texture copied into the build
            Value::String(s) if s == "asset:Texture2D/Default-Particle.png" => "guid:0000000000000000f000000000000000".to_string(),
            Value::String(s) if s.starts_with("asset-guid:") => s.trim_start_matches("asset-").rsplit_once(':').map(|x| x.0.to_string()).unwrap_or(s),
            Value::String(s) => s.trim_start_matches("asset:").to_string(),
            v => v.to_string(),
        };
        tex.insert(k.clone(), json!({ "offset": t.offset, "path": path, "scale": t.scale }));
    }
    let floats: Map<String, Value> = m.floats.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    // (non-finite components were written as 0)
    let colors: Map<String, Value> = m.colors.iter().map(|(k, v)| (k.clone(), json!(v.map(|x| if x.is_finite() { x } else { 0.0 })))).collect();
    Ok(json!({ "name": m.name, "shader": shader, "queue": m.render_queue, "floats": floats, "colors": colors, "tex": tex, "keywords": m.keywords }))
}

/// The path id a PPtr points at in its target file.
fn files_path_id(_files: &mut Files, _file: usize, p: (i32, i64)) -> Option<i64> {
    Some(p.1)
}

