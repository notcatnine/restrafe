//! Unity materials on glTF prefab scenes (weapons, projectiles, placed mines).
//!
//! AssetRipper's glTF export guesses PBR slots for custom (Amplify) shaders: for S_WeaponOutline_00 it often used the
//! `_MS` mask as base colour, never exported metallic/roughness factors (glTF default: metallic 1, roughness 1 -> dark,
//! noisy weapons), and dropped `_AO` (uv2), tint, emission and every special effect. It also exports inactive
//! GameObjects and disabled renderers as ordinary visible nodes.
//!
//! [`PrefabRender`] on a `WorldAssetRoot` entity names the extracted prefab JSON of that scene. When the scene instance
//! is ready, every node is matched to its prefab GameObject by path: inactive GameObjects are hidden, disabled
//! renderers hidden, and each primitive (= sub-mesh) gets the converted Unity material of its renderer slot
//! (`m_Materials[i]`, so name clashes such as M_ProximityMine_00 vs M_ProximityMine_00_0 cannot happen). Conversions
//! (recovered from the compiled shaders, see `UnityAssets::surface_material`) are a `StandardMaterial` plus the
//! [`UnityExt`] extension (`assets/shaders/unity_surface.wgsl`) for the parts Bevy's PBR cannot express. Materials of
//! shaders without a port keep AssetRipper's glTF material.

use crate::ugui_assets::UnityAssets;
use crate::weapons::data::Prefab;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::world_serialization::WorldInstanceReady;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// `UnityExtParams::kind` values (which shader-specific term `unity_surface.wgsl` adds).
pub mod kind {
    // 0: plain Standard lighting (everything is in the StandardMaterial)
    /// S_WeaponOutline_00 `_USEAO_ON`: albedo *= pow(_AO(uv2).r, _AO_Power).
    pub const AO: u32 = 1;
    /// S_WeaponOutline_01_Emissive: green emission = _Emissive.g * boost * sin(_timeglow * t) * 0.5 * u^6.88.
    pub const HANDCANON: u32 = 2;
    /// S_WeaponOutline_00_Special: scrolling object-space bands * fresnel^5 emission.
    pub const SPECIAL: u32 = 3;
    /// S_EmissiveColor: emission = saturate(pow(_DirtTex, _Power) + _NoiseValue) * _Color0 * _EmissiveBoost * albedo.
    pub const EMISSIVE_COLOR: u32 = 4;
    /// S_ProximityMine_00 (additive): glowing band just above the sphere's equator, pulsing with sin(_time * t).
    pub const PROX_RING: u32 = 6;
    /// S_ProximityMine_01 (alpha blend): _Color0, alpha = max(sin(_time t), 0)^70 * depth-intersection fade.
    pub const PROX_PULSE: u32 = 7;
    /// S_HandGrenadeRadius_00 (alpha blend): _Color0, alpha = _BaseOpacity * pulse + depth-intersection fade.
    pub const GRENADE_RADIUS: u32 = 8;
    /// S_BlackHole_00 (alpha blend): black Standard + fresnel^5 emission, alpha (1 - fresnel^5) * _Float0.
    pub const BLACK_HOLE: u32 = 9;
    /// Shader Graphs/DF_Cyst_Liquid_Shader: unlit liquid cut at a world-space fill height (front _SideColour, back _TopColour).
    pub const LIQUID: u32 = 10;
    /// S_Emissive_Mask_00: black surface, emission = lerp(_MaskedColor, _LightColor, pow(tex.r, _Power)) * _EmissiveBoost.
    pub const EMISSIVE_MASK: u32 = 11;
    /// S_FogCard02 (alpha blend, Cull Off, ZWrite Off; teleporter portals): unlit _Color0, alpha = min(|sceneEyeDepth
    /// - eyeDepth| / _DepthFade, 1) (* the optional uv border gradient).
    pub const FOG_CARD: u32 = 12;
}

/// Uniforms of `unity_surface.wgsl` (meaning of `color` / `p0` / `p1` depends on `kind`, see `surface_material`).
#[derive(ShaderType, Clone, Copy, Debug, Default, Reflect)]
pub struct UnityExtParams {
    pub kind: u32,
    pub ao_power: f32,
    pub boost: f32,
    pub pad: f32,
    pub color: Vec4,
    pub p0: Vec4,
    pub p1: Vec4,
    /// Unity `_ST` (scale xy, offset zw) of the extension texture, in Unity uv space.
    pub st: Vec4,
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct UnityExt {
    #[uniform(100)]
    pub p: UnityExtParams,
    /// _AO (kind AO), _Emissive (HANDCANON) or _DirtTex (EMISSIVE_COLOR).
    #[texture(101)]
    #[sampler(102)]
    pub tex: Option<Handle<Image>>,
}

impl MaterialExtension for UnityExt {
    fn fragment_shader() -> bevy::shader::ShaderRef {
        "shaders/unity_surface.wgsl".into()
    }
    // The PC graphics tier renders Deferred (GraphicsSettings tier 3 renderingPath 3): same surface code, G-buffer out.
    fn deferred_fragment_shader() -> bevy::shader::ShaderRef {
        "shaders/unity_surface.wgsl".into()
    }
}

pub type UnityPbr = ExtendedMaterial<StandardMaterial, UnityExt>;

/// On a `WorldAssetRoot` entity: path (relative to the game dir) of the extracted prefab JSON of that glTF scene,
/// e.g. `assets/data/weapons/Glock.json`.
#[derive(Component, Clone, Debug)]
pub struct PrefabRender(pub String);

/// Set on a [`PrefabRender`] root once its nodes were matched (materials / visibility applied).
#[derive(Component)]
pub struct PrefabRenderDone;

/// On a [`PrefabRender`] root spawned `Visibility::Hidden`: shown once the materials / inactive nodes are applied, so
/// the raw glTF (placeholder materials, inactive nodes) never renders for a frame (fast projectiles).
#[derive(Component)]
pub struct HiddenUntilReady;

#[derive(Resource, Default)]
pub struct UnityMaterials {
    prefabs: HashMap<String, Option<Arc<Prefab>>>,
    mats: HashMap<String, Option<Handle<UnityPbr>>>,
}

impl UnityMaterials {
    pub fn prefab(&mut self, path: &str) -> Option<Arc<Prefab>> {
        self.prefabs
            .entry(path.to_string())
            .or_insert_with(|| match Prefab::load(path) {
                Ok(p) => Some(Arc::new(p)),
                Err(e) => {
                    warn!("PrefabRender: {e}");
                    None
                }
            })
            .clone()
    }

    /// `Material/<name>.mat` converted (cached), or None when its shader has no port.
    pub fn material(&mut self, name: &str, ua: &mut UnityAssets, images: &mut Assets<Image>, mats: &mut Assets<UnityPbr>) -> Option<Handle<UnityPbr>> {
        if let Some(h) = self.mats.get(name) {
            return h.clone();
        }
        let h = ua.surface_material(name, images).map(|mut s| {
            // Lightmaps (scene_map.rs) hold Unity radiance; Bevy multiplies indirect light by the view exposure.
            s.std.lightmap_exposure = 1.0 / crate::unity_light::view_exposure();
            mats.add(UnityPbr { base: s.std, extension: UnityExt { p: s.ext, tex: s.ext_tex } })
        });
        self.mats.insert(name.to_string(), h.clone());
        h
    }
}

/// "asset:Material/M_X.mat" -> "M_X" (built-in materials such as Default-Material return None).
pub fn material_name(v: &Value) -> Option<String> {
    v.as_str()?.strip_prefix("asset:Material/")?.strip_suffix(".mat").map(str::to_string)
}

/// Primitive index of a glTF mesh handle ("...#Mesh0/Primitive1" -> 1).
fn primitive_index(m: &Mesh3d) -> usize {
    m.0.path().and_then(|p| p.label().map(|l| l.to_string())).and_then(|l| l.rsplit("Primitive").next().and_then(|n| n.parse().ok())).unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn on_ready(
    ev: On<WorldInstanceReady>,
    roots: Query<&PrefabRender>,
    reveal: Query<(), With<HiddenUntilReady>>,
    children: Query<&Children>,
    names: Query<&Name>,
    prims: Query<&Mesh3d>,
    mut commands: Commands,
    mut cache: ResMut<UnityMaterials>,
    mut ua: ResMut<UnityAssets>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<UnityPbr>>,
) {
    let root = ev.entity;
    let Ok(pr) = roots.get(root) else { return };
    let keep_gltf = std::env::var_os("RESTRAFE_GLTF_MATERIALS").is_some();
    commands.entity(root).try_insert(PrefabRenderDone);
    let Some(prefab) = cache.prefab(&pr.0) else { return };
    let dbg = std::env::var_os("RESTRAFE_DEBUG_PREFAB_RENDER").is_some();
    let (mut n_mat, mut n_hidden, mut n_nodes, mut missing) = (0, 0, 0, Vec::new());
    let mut stack: Vec<(Entity, String)> = children.get(root).map(|c| c.iter().map(|e| (e, String::new())).collect()).unwrap_or_default();
    while let Some((e, parent)) = stack.pop() {
        if prims.contains(e) {
            continue;
        }
        let Ok(name) = names.get(e) else { continue };
        let path = if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") };
        // The glTF scene entity ("Scene0") above the prefab root node is transparent.
        if parent.is_empty() && prefab.find(&path).is_none() {
            for k in children.get(e).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default() {
                stack.push((k, String::new()));
            }
            continue;
        }
        let obj = prefab.find(&path);
        n_nodes += 1;
        if obj.is_none() {
            missing.push(path.clone());
        }
        if obj.is_some_and(|o| !o.active) {
            n_hidden += 1;
            commands.entity(e).try_insert(Visibility::Hidden);
        }
        let renderer = obj.and_then(|o| o.comp("MeshRenderer").or_else(|| o.comp("SkinnedMeshRenderer")));
        // A duplicate of the parent's mesh with only Unity's built-in Default-Material (AR15's SM_AR153_00 child): same
        // transform and mesh as the textured parent renderer, which is what shows in the game; the glTF fallback
        // material on the copy covered it.
        let mesh_of = |o: &crate::weapons::data::PrefabObject| o.comp("MeshFilter").and_then(|f| f.get("m_Mesh")).and_then(Value::as_str).map(str::to_string);
        let builtin_only = renderer.and_then(|r| r.get("m_Materials")).and_then(Value::as_array).is_some_and(|m| !m.is_empty() && m.iter().all(|v| v.as_str().is_some_and(|s| s.contains(":10303"))));
        let duplicate = builtin_only
            && obj.is_some_and(|o| {
                let parent = o.path.rsplit_once('/').and_then(|(pp, _)| prefab.find(pp));
                mesh_of(o).is_some() && parent.is_some_and(|pa| mesh_of(pa) == mesh_of(o))
            });
        for k in children.get(e).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default() {
            let Ok(m3) = prims.get(k) else {
                stack.push((k, path.clone()));
                continue;
            };
            let Some(r) = renderer else { continue };
            if duplicate {
                commands.entity(k).try_insert(Visibility::Hidden);
                continue;
            }
            if r.get("m_Enabled").and_then(Value::as_i64) == Some(0) {
                commands.entity(k).try_insert(Visibility::Hidden);
                continue;
            }
            if r.get("m_CastShadows").and_then(Value::as_i64) == Some(0) {
                commands.entity(k).try_insert(bevy::light::NotShadowCaster);
            }
            if r.get("m_ReceiveShadows").and_then(Value::as_i64) == Some(0) {
                commands.entity(k).try_insert(bevy::light::NotShadowReceiver);
            }
            // AssetRipper (SharpGLTF MeshBuilder.UsePrimitive(material)) merges sub-meshes that share a material into
            // one primitive, so primitive i = the i-th DISTINCT slot in first-appearance order (Katana: slots
            // [SM_Katana_1, SM_Katana_1, SM_Katana_2] -> 2 primitives; the blade is primitive 1 = SM_Katana_2).
            let mut slots: Vec<&Value> = Vec::new();
            for v in r.get("m_Materials").and_then(Value::as_array).into_iter().flatten() {
                // RESTRAFE_RAW_SLOTS=1: old (wrong) mapping primitive i = m_Materials[i], for comparisons.
                if !slots.contains(&v) || std::env::var_os("RESTRAFE_RAW_SLOTS").is_some() {
                    slots.push(v);
                }
            }
            let i = primitive_index(m3);
            let Some(slot) = slots.get(i).copied() else {
                // More sub-meshes than materials: Unity does not draw the extra sub-meshes.
                commands.entity(k).try_insert(Visibility::Hidden);
                continue;
            };
            if keep_gltf {
                continue;
            }
            if let Some(h) = material_name(slot).and_then(|n| cache.material(&n, &mut ua, &mut images, &mut mats)) {
                n_mat += 1;
                // These shaders' ShadowCaster passes dither by the (mostly ~0) surface alpha: no visible shadow.
                if mats.get(&h).is_some_and(|m| matches!(m.extension.p.kind, kind::PROX_PULSE | kind::GRENADE_RADIUS | kind::BLACK_HOLE)) {
                    commands.entity(k).try_insert(bevy::light::NotShadowCaster);
                }
                commands.entity(k).try_remove::<MeshMaterial3d<StandardMaterial>>().try_insert(MeshMaterial3d(h));
            }
        }
    }
    if dbg {
        info!("PrefabRender {}: {n_nodes} nodes, {n_mat} materials, {n_hidden} inactive hidden, unmatched {missing:?}", pr.0);
    }
    if reveal.contains(root) {
        commands.entity(root).try_remove::<HiddenUntilReady>().try_insert(Visibility::Inherited);
    }
}

pub struct UnityPbrPlugin;

impl Plugin for UnityPbrPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<UnityPbr>::default()).init_resource::<UnityMaterials>().init_resource::<UnityAssets>().add_observer(on_ready);
    }
}

/// Material conversion access for gameplay systems (e.g. ProximityMine's `realMat` swap).
#[derive(bevy::ecs::system::SystemParam)]
pub struct UnityMatParams<'w> {
    pub cache: ResMut<'w, UnityMaterials>,
    pub ua: ResMut<'w, UnityAssets>,
    pub images: ResMut<'w, Assets<Image>>,
    pub mats: ResMut<'w, Assets<UnityPbr>>,
}

impl UnityMatParams<'_> {
    /// `Material/<name>.mat` converted (cached).
    pub fn material(&mut self, name: &str) -> Option<Handle<UnityPbr>> {
        self.cache.material(name, &mut self.ua, &mut self.images, &mut self.mats)
    }
}

/// First descendant of `root` named `name`.
pub fn find_named(root: Entity, name: &str, children: &Query<&Children>, names: &Query<&Name>) -> Option<Entity> {
    children.iter_descendants(root).find(|e| names.get(*e).is_ok_and(|n| n.as_str() == name))
}
