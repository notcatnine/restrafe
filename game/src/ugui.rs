//! uGUI port: imports a subtree of a Unity scene canvas (RectTransform hierarchy + Graphic / Selectable / layout
//! components + the 3D objects, cameras and lights some menus render into RenderTextures) and runs it with Unity's
//! semantics:
//!
//! * Layout: RectTransform anchors / anchoredPosition / sizeDelta / pivot / localScale / localRotation, recomputed
//!   every frame from the CanvasScaler (ScaleWithScreenSize 1920x1080; `CanvasScalerFixer` matches width when the
//!   window is at most 16:9 and height otherwise, i.e. scale = min(w / 1920, h / 1080)).
//! * Rendering: like the Canvas batcher, every Graphic is turned into canvas-space vertices (Image Simple/Sliced/Tiled/Filled with
//!   sprite borders, RawImage incl. camera RenderTextures, TextMeshProUGUI through the TMP SDF shader of
//!   `tmp_text.rs`) drawn in hierarchy order by an orthographic overlay camera whose world unit is one canvas unit.
//!   Vertex colour = Graphic colour x CanvasRenderer colour (Selectable tint / CrossFade) x CanvasGroup alpha,
//!   linearised as the Canvas does in a Linear project. Mask / RectMask2D clip on the CPU (axis-aligned rects).
//! * 3D: MeshFilter + MeshRenderer, Camera (-> Bevy camera rendering into the RenderTexture image) and Light nodes
//!   become Bevy entities placed from the same transforms (Unity left-handed -> Bevy: z mirrored).
//! * Events (StandaloneInputModule + GraphicRaycaster): raycast against raycastTarget graphics (CanvasGroup
//!   blocksRaycasts), enter/exit along the hovered hierarchy, press / click / drag handlers found up the hierarchy,
//!   EventSystem selection. Widgets: Button, Toggle, Slider, Scrollbar, ScrollRect (wheel), TMP_Dropdown (template
//!   instantiation, blocker), TMP_InputField (typing, caret). Persistent UnityEvent calls `GameObject.SetActive` are
//!   executed here; all other calls are forwarded as [`UiCall`] messages to the game scripts.
//!
//! Not implemented (unused by the imported menus at runtime): layout groups / ContentSizeFitter (their results are
//! serialized in the scene), SpriteSwap/Animation transitions, navigation, rich text tags.

use crate::tmp_text::{layout_ugui, TmpFont, TmpMatProps, TmpSdfMaterial, TmpUiLayout};
use crate::ugui_assets::{SpriteInfo, UnityAssets};
use crate::ugui_scene::*;
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::{RenderTarget, ScalingMode};
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError, TextureFormat};
use bevy::shader::ShaderRef;
use bevy::window::PrimaryWindow;
use serde_yaml::{Mapping, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Render layer of the uGUI canvas camera.
pub const CANVAS_LAYER: usize = 4;
pub const REF_RES: Vec2 = Vec2::new(1920.0, 1080.0);

/// CanvasScaler scale factor for a window size (ScaleWithScreenSize 1920x1080 + CanvasScalerFixer).
pub fn canvas_scale(window: Vec2) -> f32 {
    (window.x / REF_RES.x).min(window.y / REF_RES.y).max(1e-4)
}

// ------------------------------------------------------------------------------------------------------------------
// Tree

/// Vertical / HorizontalLayoutGroup (runtime child placement; only the childControl = false case is run).
#[derive(Clone, Copy, Debug)]
pub struct LayoutGroupC {
    pub vertical: bool,
    /// left, right, top, bottom
    pub padding: [f32; 4],
    pub spacing: f32,
    pub child_alignment: i64,
    pub force_expand: [bool; 2],
    pub control: [bool; 2],
}

#[derive(Clone, Debug)]
pub struct RectT {
    pub amin: Vec2,
    pub amax: Vec2,
    pub apos: Vec2,
    pub size_delta: Vec2,
    pub pivot: Vec2,
}

#[derive(Clone)]
pub struct TextG {
    pub text: String,
    pub font: Arc<TmpFont>,
    pub mat_key: String,
    pub props: Arc<TmpMatProps>,
    pub size: f32,
    pub h_align: u32,
    pub v_align: u32,
    pub margin: Vec4,
    pub char_spacing: f32,
    pub word_spacing: f32,
    pub line_spacing: f32,
    pub wrap: bool,
    /// m_fontStyle (FontStyles flags).
    pub style: u32,
    /// (layout, text) -> quads (pos BL TL TR BR, uv, scale)
    cache: Option<(TmpUiLayout, String, Arc<Vec<([Vec2; 4], [Vec2; 4], f32)>>)>,
}

#[derive(Clone)]
pub enum GKind {
    Image { sprite: Option<SpriteInfo>, ty: i64, fill_center: bool, preserve_aspect: bool, ppu_mult: f32, fill: ImgFill },
    Raw { tex: Option<Handle<Image>>, uv: Rect },
    Text(TextG),
}

#[derive(Clone)]
pub struct Graphic {
    pub kind: GKind,
    /// Graphic colour (gamma).
    pub color: [f32; 4],
    pub raycast: bool,
    pub enabled: bool,
    /// CanvasRenderer colour (CrossFadeColor / CrossFadeAlpha).
    pub tint: Vec4,
    tint_tween: Option<(Vec4, Vec4, f32, f32, bool)>,
    pub(crate) entity: Option<Entity>,
    last: u64,
    mesh: Option<Handle<Mesh>>,
}

impl Graphic {
    /// `Graphic.CrossFadeColor` (useAlpha true) / `CrossFadeAlpha` (alpha_only).
    pub fn cross_fade(&mut self, to: Vec4, duration: f32, alpha_only: bool) {
        let to = if alpha_only { self.tint.truncate().extend(to.w) } else { to };
        if duration <= 0.0 {
            self.tint = to;
            self.tint_tween = None;
        } else {
            self.tint_tween = Some((self.tint, to, 0.0, duration, alpha_only));
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ColorBlock {
    pub normal: Vec4,
    pub highlighted: Vec4,
    pub pressed: Vec4,
    pub selected: Vec4,
    pub disabled: Vec4,
    pub multiplier: f32,
    pub fade: f32,
}

#[derive(Clone, Debug)]
pub struct Selectable {
    pub interactable: bool,
    pub transition: i64,
    pub colors: ColorBlock,
    pub target: Option<usize>,
    pub hover: bool,
    pub down: bool,
    pub selected: bool,
    state: i32,
    /// Navigation.mode: 0 None, 1 Horizontal, 2 Vertical, 3 Automatic, 4 Explicit.
    pub nav_mode: i64,
    pub nav_wrap: bool,
    /// Explicit selectOnUp / Down / Left / Right (nodes).
    pub nav: [Option<usize>; 4],
    /// Button.OnSubmit: Pressed state until OnFinishSubmit (colors.fadeDuration later).
    pub submit_t: f32,
}

/// UI actions of the InputSystemUIInputModule (CustomUiInput asset, UI map), gamepad part: set by input.rs.
#[derive(Clone, Copy, Debug, Default)]
pub struct NavInput {
    /// Navigate value (gamepad 2DVector of both sticks, DigitalNormalized, or the dpad).
    pub mv: Vec2,
    /// Submit (<Gamepad>/buttonSouth) performed this frame.
    pub submit: bool,
    /// Cancel (*/{Cancel}: gamepad buttonEast) performed this frame.
    pub cancel: bool,
}

/// MoveDirection (Left, Up, Right, Down) as the unit vector of the move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveDir {
    Left,
    Up,
    Right,
    Down,
}

#[derive(Clone, Debug)]
pub struct DropList {
    pub root: usize,
    pub blocker: usize,
    pub items: Vec<usize>,
    pub alpha_t: f32,
}

#[derive(Clone, Debug)]
pub enum Widget {
    None,
    Button { calls: Vec<PCall> },
    Toggle { is_on: bool, graphic: Option<usize>, transition: i64, calls: Vec<PCall> },
    Slider { min: f32, max: f32, whole: bool, value: f32, dir: i64, fill: Option<usize>, handle: Option<usize>, calls: Vec<PCall>, offset: Vec2 },
    Scrollbar { value: f32, size: f32, dir: i64, handle: Option<usize> },
    Dropdown { template: Option<usize>, caption: Option<usize>, item_text: Option<usize>, options: Vec<String>, value: i32, calls: Vec<PCall>, fade: f32, list: Option<DropList> },
    Input { text_comp: Option<usize>, placeholder: Option<usize>, text: String, end_calls: Vec<PCall>, change_calls: Vec<PCall>, focused: bool, caret_t: f32, caret_rate: f32, caret_color: [f32; 4], caret_width: f32, original: String, caret: Option<usize> },
}

#[derive(Clone, Debug)]
pub struct ScrollRectC {
    pub content: Option<usize>,
    pub viewport: Option<usize>,
    pub vbar: Option<usize>,
    pub sensitivity: f32,
}

#[derive(Clone)]
pub enum Obj3d {
    Mesh { meshes: Vec<Handle<Mesh>>, mats: Vec<Option<crate::ugui_assets::UMat>>, layer: usize, enabled: bool, light_probes: bool },
    Camera { target: Option<Handle<Image>>, fov: f32, ortho: Option<f32>, near: f32, far: f32, clear: [f32; 4], mask: u32, depth: f32, enabled: bool },
    Light { ty: i64, color: [f32; 4], intensity: f32, range: f32, mask: u32, enabled: bool },
}

#[derive(Clone)]
pub struct UNode {
    pub go: i64,
    pub name: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub active: bool,
    pub rect_t: Option<RectT>,
    pub pos: Vec3,
    pub rot: Quat,
    pub scale: Vec3,
    pub layer: u32,
    // Computed by layout.
    pub rect: Rect,
    pub world: Mat4,
    pub on: bool,
    pub alpha: f32,
    pub interactable: bool,
    pub blocks: bool,
    pub clip: Option<Rect>,
    pub order: u32,
    pub graphic: Option<Graphic>,
    /// CanvasGroup (alpha, interactable, blocksRaycasts, ignoreParentGroups)
    pub group: Option<(f32, bool, bool, bool)>,
    pub sel: Option<Selectable>,
    pub widget: Widget,
    pub scroll: Option<ScrollRectC>,
    /// Mask (showMaskGraphic) / RectMask2D
    pub mask: Option<bool>,
    pub rect_mask: bool,
    pub layout_group: Option<LayoutGroupC>,
    /// Game scripts: (component id, class name, serialized fields, enabled)
    pub scripts: Vec<(i64, String, Arc<Mapping>, bool)>,
    pub obj3d: Option<Obj3d>,
    pub(crate) entity: Option<Entity>,
    pub override_sort: bool,
}

/// A persistent call forwarded to the game scripts.
#[derive(Message, Clone, Debug)]
pub struct UiCall {
    pub node: usize,
    pub call: PCall,
    pub value: CallValue,
}

#[derive(Clone, Debug)]
pub enum CallValue {
    None,
    Bool(bool),
    Float(f32),
    Int(i32),
    Str(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Enter,
    Exit,
    Down,
    Up,
    Click,
    Select,
    Deselect,
}

/// Pointer events delivered to a GameObject (for the game scripts implementing IPointer*Handler).
#[derive(Message, Clone, Copy, Debug)]
pub struct UiPointer {
    pub node: usize,
    pub kind: PointerKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TweenProp {
    Pos,
    Scale,
}

#[derive(Clone, Debug)]
pub struct NodeTween {
    pub node: usize,
    pub prop: TweenProp,
    pub from: Vec3,
    pub to: Vec3,
    pub t: f32,
    pub dur: f32,
    pub ease: crate::tween::Ease,
}

#[derive(Resource, Default)]
pub struct UiTree {
    pub nodes: Vec<UNode>,
    pub by_go: HashMap<i64, usize>,
    /// Component / RectTransform file id -> node.
    pub by_comp: HashMap<i64, usize>,
    pub roots: Vec<usize>,
    pub tweens: Vec<NodeTween>,
    pub canvas_size: Vec2,
    pub scale: f32,
    // Event system state.
    pub hovered: Vec<usize>,
    pub pressed: Option<usize>,
    pub dragging: Option<usize>,
    pub selected: Option<usize>,
    pub pointer: Option<Vec2>,
    pub raycast_hit: Option<usize>,
    /// When false nothing is processed (no visible root).
    pub any_on: bool,
    /// `PauseManager.gamepad` (copied from PlayerInput each frame): gates the gamepad-only selection behaviour.
    pub gamepad: bool,
    /// UI navigation input of this frame and the module's NavigationModel (last direction, count, time).
    pub nav_in: NavInput,
    /// Navigation suspended (interactive rebinding running).
    pub nav_block: bool,
    nav_state: (Option<MoveDir>, u32, f32),
    calls: Vec<(usize, PCall, CallValue)>,
    pointer_events: Vec<(usize, PointerKind)>,
    rt_images: HashMap<String, Handle<Image>>,
}

fn col4(c: [f32; 4]) -> Vec4 {
    Vec4::from_array(c)
}

fn color_block(m: &Mapping) -> ColorBlock {
    let c = m.get("m_Colors");
    let g = |k: &str| col4(rgba(c.and_then(|c| c.get(k))));
    ColorBlock {
        normal: g("m_NormalColor"),
        highlighted: g("m_HighlightedColor"),
        pressed: g("m_PressedColor"),
        selected: if c.and_then(|c| c.get("m_SelectedColor")).is_some() { g("m_SelectedColor") } else { g("m_HighlightedColor") },
        disabled: g("m_DisabledColor"),
        multiplier: c.and_then(|c| c.get("m_ColorMultiplier")).and_then(Value::as_f64).unwrap_or(1.0) as f32,
        fade: c.and_then(|c| c.get("m_FadeDuration")).and_then(Value::as_f64).unwrap_or(0.1) as f32,
    }
}

/// Context passed to the importer.
pub struct ImportCtx<'a> {
    pub assets: &'a mut UnityAssets,
    pub images: &'a mut Assets<Image>,
    pub meshes: &'a mut Assets<Mesh>,
    pub materials: &'a mut Assets<StandardMaterial>,
    pub fx_materials: &'a mut Assets<crate::ugui_assets::UnityFxMaterial>,
    pub fonts: &'a mut HashMap<String, Option<Arc<TmpFont>>>,
    pub tmp_mats: &'a mut HashMap<String, Option<Arc<TmpMatProps>>>,
}

impl UiTree {
    pub fn node(&self, go: i64) -> Option<usize> {
        self.by_go.get(&go).copied()
    }

    pub fn comp(&self, id: i64) -> Option<usize> {
        self.by_comp.get(&id).copied()
    }

    pub fn find(&self, from: usize, path: &str) -> Option<usize> {
        let mut n = from;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            n = *self.nodes[n].children.iter().find(|&&c| self.nodes[c].name == part)?;
        }
        Some(n)
    }

    pub fn set_active(&mut self, n: usize, v: bool) {
        self.nodes[n].active = v;
    }

    pub fn script(&self, n: usize, name: &str) -> Option<&Arc<Mapping>> {
        self.nodes[n].scripts.iter().find(|s| s.1 == name).map(|s| &s.2)
    }

    /// The node owning a script component id.
    pub fn script_node(&self, comp: i64) -> Option<usize> {
        self.comp(comp)
    }

    pub fn text(&self, n: usize) -> Option<&str> {
        match &self.nodes[n].graphic.as_ref()?.kind {
            GKind::Text(t) => Some(&t.text),
            _ => None,
        }
    }

    pub fn set_text(&mut self, n: usize, s: &str) {
        if let Some(Graphic { kind: GKind::Text(t), .. }) = self.nodes[n].graphic.as_mut() {
            if t.text != s {
                t.text = s.to_string();
            }
        }
    }

    /// First TextMeshProUGUI in the node or its descendants (GetComponentInChildren, active only when `active_only`).
    pub fn text_in_children(&self, n: usize, active_only: bool) -> Option<usize> {
        if active_only && !self.nodes[n].active {
            return None;
        }
        if matches!(self.nodes[n].graphic.as_ref().map(|g| &g.kind), Some(GKind::Text(_))) {
            return Some(n);
        }
        self.nodes[n].children.iter().find_map(|&c| self.text_in_children(c, active_only))
    }

    pub fn widget_in_children(&self, n: usize, pred: &dyn Fn(&Widget) -> bool) -> Option<usize> {
        if pred(&self.nodes[n].widget) {
            return Some(n);
        }
        self.nodes[n].children.iter().find_map(|&c| self.widget_in_children(c, pred))
    }

    pub fn ancestors(&self, n: usize) -> Vec<usize> {
        let mut v = vec![n];
        let mut c = n;
        while let Some(p) = self.nodes[c].parent {
            v.push(p);
            c = p;
        }
        v
    }

    fn queue_calls(&mut self, n: usize, calls: &[PCall], value: CallValue) {
        for c in calls {
            if c.state == 0 {
                continue;
            }
            self.calls.push((n, c.clone(), value.clone()));
        }
    }

    pub fn tween(&mut self, node: usize, prop: TweenProp, to: Vec3, dur: f32, ease: crate::tween::Ease) {
        self.tweens.retain(|t| !(t.node == node && t.prop == prop));
        let from = match prop {
            TweenProp::Pos => self.local_position(node),
            TweenProp::Scale => self.nodes[node].scale,
        };
        if dur <= 0.0 {
            self.apply_prop(node, prop, to);
        } else {
            self.tweens.push(NodeTween { node, prop, from, to, t: 0.0, dur, ease });
        }
    }

    pub fn kill_tweens(&mut self, node: usize) {
        self.tweens.retain(|t| t.node != node);
    }

    /// Transform.localPosition (for RectTransforms: derived from the anchors as Unity does).
    pub fn local_position(&self, n: usize) -> Vec3 {
        let node = &self.nodes[n];
        match &node.rect_t {
            None => node.pos,
            Some(r) => {
                let pr = node.parent.map(|p| self.nodes[p].rect).unwrap_or(Rect::from_center_size(Vec2::ZERO, self.canvas_size));
                let a0 = pr.min + pr.size() * r.amin;
                let a1 = pr.min + pr.size() * r.amax;
                (a0 + (a1 - a0) * r.pivot + r.apos).extend(node.pos.z)
            }
        }
    }

    pub fn apply_prop(&mut self, n: usize, prop: TweenProp, v: Vec3) {
        match prop {
            TweenProp::Scale => self.nodes[n].scale = v,
            TweenProp::Pos => {
                let cur = self.local_position(n);
                let node = &mut self.nodes[n];
                match &mut node.rect_t {
                    None => node.pos = v,
                    Some(r) => {
                        r.apos += v.truncate() - cur.truncate();
                        node.pos.z = v.z;
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Import

    /// Imports `go` (and its children) under `parent`.
    pub fn import(&mut self, sc: &UScene, go: i64, parent: Option<usize>, cx: &mut ImportCtx) -> usize {
        let idx = self.nodes.len();
        let mut node = UNode {
            go,
            name: sc.go_name(go),
            parent,
            children: vec![],
            active: sc.go_active(go),
            rect_t: None,
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            scale: Vec3::ONE,
            layer: sc.doc(go).map(|m| int(m.get("m_Layer")) as u32).unwrap_or(0),
            rect: Rect::default(),
            world: Mat4::IDENTITY,
            on: false,
            alpha: 1.0,
            interactable: true,
            blocks: true,
            clip: None,
            order: 0,
            graphic: None,
            group: None,
            sel: None,
            widget: Widget::None,
            scroll: None,
            mask: None,
            rect_mask: false,
            layout_group: None,
            scripts: vec![],
            obj3d: None,
            entity: None,
            override_sort: false,
        };
        self.nodes.push(node.clone());
        self.by_go.insert(go, idx);
        let comps = sc.components(go);
        let mut pending: Vec<(i64, String, Arc<Mapping>)> = vec![];
        for (id, class, name) in &comps {
            self.by_comp.insert(*id, idx);
            let Some(m) = sc.doc(*id) else { continue };
            match *class {
                224 | 4 => {
                    node.pos = Vec3::from_array(v3(m.get("m_LocalPosition")));
                    let q = v4(m.get("m_LocalRotation"));
                    node.rot = Quat::from_xyzw(q[0], q[1], q[2], q[3]);
                    node.scale = Vec3::from_array(v3(m.get("m_LocalScale")));
                    if *class == 224 {
                        node.rect_t = Some(RectT {
                            amin: Vec2::from_array(v2(m.get("m_AnchorMin"))),
                            amax: Vec2::from_array(v2(m.get("m_AnchorMax"))),
                            apos: Vec2::from_array(v2(m.get("m_AnchoredPosition"))),
                            size_delta: Vec2::from_array(v2(m.get("m_SizeDelta"))),
                            pivot: Vec2::from_array(v2(m.get("m_Pivot"))),
                        });
                    }
                }
                225 => {
                    node.group = Some((f(m.get("m_Alpha")), int(m.get("m_Interactable")) != 0, int(m.get("m_BlocksRaycasts")) != 0, int(m.get("m_IgnoreParentGroups")) != 0));
                }
                223 => {
                    node.override_sort = int(m.get("m_OverrideSorting")) != 0;
                }
                _ => pending.push((*id, name.clone(), m.clone())),
            }
        }
        self.nodes[idx] = node;
        for (id, name, m) in pending {
            self.import_component(sc, idx, id, &name, &m, cx);
        }
        let kids = sc.children(go);
        for c in kids {
            let ci = self.import(sc, c, Some(idx), cx);
            self.nodes[idx].children.push(ci);
        }
        idx
    }

    fn import_component(&mut self, sc: &UScene, idx: usize, id: i64, name: &str, m: &Arc<Mapping>, cx: &mut ImportCtx) {
        let enabled = int(m.get("m_Enabled")) != 0 || m.get("m_Enabled").is_none();
        let graphic = |kind: GKind, color: [f32; 4]| Graphic {
            kind,
            color,
            raycast: int(m.get("m_RaycastTarget")) != 0,
            enabled,
            tint: Vec4::ONE,
            tint_tween: None,
            entity: None,
            last: 0,
            mesh: None,
        };
        match name {
            "Image" => {
                let sp = m.get("m_Sprite");
                let sprite = match (guid_ref(sp), file_ref(sp)) {
                    (Some(g), fid) if fid != 0 => cx.assets.sprite(&g, fid, cx.images),
                    _ => None,
                };
                // Image.pixelsPerUnitMultiplier getter: Mathf.Max(0.01, m_PixelsPerUnitMultiplier).
                let ppu_mult = (m.get("m_PixelsPerUnitMultiplier").and_then(Value::as_f64).unwrap_or(1.0) as f32).max(0.01);
                let ty = int(m.get("m_Type"));
                let fill = ImgFill {
                    method: int(m.get("m_FillMethod")),
                    amount: m.get("m_FillAmount").and_then(Value::as_f64).unwrap_or(1.0) as f32,
                    clockwise: int(m.get("m_FillClockwise")) != 0,
                    origin: int(m.get("m_FillOrigin")),
                };
                let k = GKind::Image { sprite, ty, fill_center: int(m.get("m_FillCenter")) != 0, preserve_aspect: int(m.get("m_PreserveAspect")) != 0, ppu_mult, fill };
                self.nodes[idx].graphic = Some(graphic(k, rgba(m.get("m_Color"))));
            }
            "RawImage" => {
                let tex = guid_ref(m.get("m_Texture")).and_then(|g| {
                    let p = cx.assets.path(&g)?;
                    if p.ends_with(".renderTexture") {
                        Some(self.render_texture(&g, &p, cx))
                    } else {
                        cx.assets.texture(&g, cx.images).map(|t| t.0)
                    }
                });
                let r = m.get("m_UVRect");
                let g = |k: &str| f(r.and_then(|r| r.get(k)));
                let uv = Rect::new(g("x"), g("y"), g("x") + g("width"), g("y") + g("height"));
                self.nodes[idx].graphic = Some(graphic(GKind::Raw { tex, uv }, rgba(m.get("m_Color"))));
            }
            "TextMeshProUGUI" => {
                let font = guid_ref(m.get("m_fontAsset")).and_then(|g| {
                    let (n, p) = crate::owned_docs::parse_token(&g)?;
                    let images = &mut *cx.images;
                    cx.fonts.entry(g.clone()).or_insert_with(|| TmpFont::load_at(n, p, images).map(Arc::new)).clone()
                });
                let mat_tok = guid_ref(m.get("m_sharedMaterial"));
                let props = mat_tok.as_ref().and_then(|g| {
                    let (n, p) = crate::owned_docs::parse_token(g)?;
                    cx.tmp_mats.entry(g.clone()).or_insert_with(|| TmpMatProps::load_at(n, p).map(Arc::new)).clone()
                });
                let (Some(font), Some(props)) = (font, props) else {
                    warn!("ugui: {} TMP font/material missing", self.nodes[idx].name);
                    return;
                };
                let mg = v4(m.get("m_margin"));
                let t = TextG {
                    text: m.get("m_text").and_then(Value::as_str).unwrap_or("").to_string(),
                    font,
                    mat_key: mat_tok.unwrap_or_default(),
                    props,
                    size: f(m.get("m_fontSize")),
                    h_align: int(m.get("m_HorizontalAlignment")) as u32,
                    v_align: int(m.get("m_VerticalAlignment")) as u32,
                    margin: Vec4::from_array(mg),
                    char_spacing: f(m.get("m_characterSpacing")),
                    word_spacing: f(m.get("m_wordSpacing")),
                    line_spacing: f(m.get("m_lineSpacing")),
                    wrap: int(m.get("m_enableWordWrapping")) != 0,
                    style: int(m.get("m_fontStyle")) as u32,
                    cache: None,
                };
                if int(m.get("m_enableAutoSizing")) != 0 {
                    warn!("ugui: {} TMP auto-size not supported", self.nodes[idx].name);
                }
                self.nodes[idx].graphic = Some(graphic(GKind::Text(t), rgba(m.get("m_fontColor"))));
            }
            "Button" | "Toggle" | "Slider" | "Scrollbar" | "TMP_Dropdown" | "TMP_InputField" => {
                let target = file_ref(m.get("m_TargetGraphic"));
                let nav = m.get("m_Navigation");
                self.nodes[idx].sel = Some(Selectable {
                    interactable: int(m.get("m_Interactable")) != 0,
                    transition: int(m.get("m_Transition")),
                    colors: color_block(m),
                    target: if target != 0 { Some(-target as usize) } else { None },
                    hover: false,
                    down: false,
                    selected: false,
                    state: -1,
                    nav_mode: nav.and_then(|n| n.get("m_Mode")).map(|v| int(Some(v))).unwrap_or(3),
                    nav_wrap: nav.map(|n| int(n.get("m_WrapAround")) != 0).unwrap_or(false),
                    nav: ["m_SelectOnUp", "m_SelectOnDown", "m_SelectOnLeft", "m_SelectOnRight"].map(|k| {
                        let v = nav.map(|n| file_ref(n.get(k))).unwrap_or(0);
                        if v != 0 { Some(-v as usize) } else { None }
                    }),
                    submit_t: 0.0,
                });
                // References are resolved after import (ids stored negated, see `resolve`).
                let rf = |k: &str| {
                    let v = file_ref(m.get(k));
                    if v != 0 { Some(-v as usize) } else { None }
                };
                self.nodes[idx].widget = match name {
                    "Button" => Widget::Button { calls: persistent_calls(m, "m_OnClick") },
                    "Toggle" => Widget::Toggle { is_on: int(m.get("m_IsOn")) != 0, graphic: rf("graphic"), transition: int(m.get("toggleTransition")), calls: persistent_calls(m, "onValueChanged") },
                    "Slider" => Widget::Slider {
                        min: f(m.get("m_MinValue")),
                        max: f(m.get("m_MaxValue")),
                        whole: int(m.get("m_WholeNumbers")) != 0,
                        value: f(m.get("m_Value")),
                        dir: int(m.get("m_Direction")),
                        fill: rf("m_FillRect"),
                        handle: rf("m_HandleRect"),
                        calls: persistent_calls(m, "m_OnValueChanged"),
                        offset: Vec2::ZERO,
                    },
                    "Scrollbar" => Widget::Scrollbar { value: f(m.get("m_Value")), size: f(m.get("m_Size")), dir: int(m.get("m_Direction")), handle: rf("m_HandleRect") },
                    "TMP_Dropdown" => Widget::Dropdown {
                        template: rf("m_Template"),
                        caption: rf("m_CaptionText"),
                        item_text: rf("m_ItemText"),
                        options: m.get("m_Options").and_then(|o| o.get("m_Options")).and_then(Value::as_sequence).map(|s| s.iter().map(|o| o.get("m_Text").and_then(Value::as_str).unwrap_or("").to_string()).collect()).unwrap_or_default(),
                        value: int(m.get("m_Value")) as i32,
                        calls: persistent_calls(m, "m_OnValueChanged"),
                        fade: m.get("m_AlphaFadeSpeed").and_then(Value::as_f64).unwrap_or(0.15) as f32,
                        list: None,
                    },
                    _ => Widget::Input {
                        text_comp: rf("m_TextComponent"),
                        placeholder: rf("m_Placeholder"),
                        text: m.get("m_Text").and_then(Value::as_str).unwrap_or("").to_string(),
                        end_calls: persistent_calls(m, "m_OnEndEdit"),
                        change_calls: persistent_calls(m, "m_OnValueChanged"),
                        focused: false,
                        caret_t: 0.0,
                        caret_rate: m.get("m_CaretBlinkRate").and_then(Value::as_f64).unwrap_or(0.85) as f32,
                        caret_color: if int(m.get("m_CustomCaretColor")) != 0 { rgba(m.get("m_CaretColor")) } else { [0.196, 0.196, 0.196, 1.0] },
                        caret_width: m.get("m_CaretWidth").and_then(Value::as_f64).unwrap_or(1.0) as f32,
                        original: String::new(),
                        caret: None,
                    },
                };
                if !enabled {
                    if let Some(s) = self.nodes[idx].sel.as_mut() {
                        s.interactable = false;
                    }
                }
            }
            "ScrollRect" => {
                let rf = |k: &str| {
                    let v = file_ref(m.get(k));
                    if v != 0 { Some(-v as usize) } else { None }
                };
                self.nodes[idx].scroll = Some(ScrollRectC { content: rf("m_Content"), viewport: rf("m_Viewport"), vbar: rf("m_VerticalScrollbar"), sensitivity: m.get("m_ScrollSensitivity").and_then(Value::as_f64).unwrap_or(1.0) as f32 });
            }
            "Mask" => self.nodes[idx].mask = Some(int(m.get("m_ShowMaskGraphic")) != 0),
            "RectMask2D" => self.nodes[idx].rect_mask = enabled,
            "Shadow" | "Outline" => warn!("ugui: {} {name} effect not supported", self.nodes[idx].name),
            "VerticalLayoutGroup" | "HorizontalLayoutGroup" if enabled => {
                let pad = m.get("m_Padding");
                let pi = |k: &str| pad.and_then(|p| p.get(k)).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let fl = |k: &str| m.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                self.nodes[idx].layout_group = Some(LayoutGroupC {
                    vertical: name == "VerticalLayoutGroup",
                    padding: [pi("m_Left"), pi("m_Right"), pi("m_Top"), pi("m_Bottom")],
                    spacing: fl("m_Spacing"),
                    child_alignment: int(m.get("m_ChildAlignment")),
                    force_expand: [int(m.get("m_ChildForceExpandWidth")) != 0, int(m.get("m_ChildForceExpandHeight")) != 0],
                    control: [int(m.get("m_ChildControlWidth")) != 0, int(m.get("m_ChildControlHeight")) != 0],
                });
            }
            "GraphicRaycaster" | "CanvasScaler" | "LayoutElement" | "VerticalLayoutGroup" | "HorizontalLayoutGroup" | "GridLayoutGroup" | "ContentSizeFitter" | "ToggleGroup" => {}
            _ => {
                let cls = sc.class(id);
                match cls {
                    33 => {
                        let mesh = guid_ref(m.get("m_Mesh")).and_then(|g| cx.assets.mesh(&g, cx.meshes)).unwrap_or_default();
                        match self.nodes[idx].obj3d.as_mut() {
                            Some(Obj3d::Mesh { meshes, .. }) => *meshes = mesh,
                            _ => self.nodes[idx].obj3d = Some(Obj3d::Mesh { meshes: mesh, mats: vec![], layer: 0, enabled: true, light_probes: false }),
                        }
                    }
                    23 => {
                        // m_LightProbeUsage (0 Off, 1 Blend Probes, ...): lit by the scene's light probes, not its ambient.
                        let probes = int(m.get("m_LightProbeUsage")) != 0;
                        // Built-in Default-Material (fileID 10303 of the built-in resources: the build's sharedassets0
                        // Default-Material, Standard, white, _Glossiness 0.5, _Metallic 0; read with UnityPy) is a real
                        // material (SM_MainMenu_Cosmetics_00 / SM_OptionsMenu_MainFrame_00 render with it).
                        let mats: Vec<Option<crate::ugui_assets::UMat>> = m
                            .get("m_Materials")
                            .and_then(Value::as_sequence)
                            .map(|s| {
                                s.iter()
                                    .map(|v| match guid_ref(Some(v)) {
                                        Some(g) if g == crate::ugui_assets::BUILTIN_EXTRA && file_ref(Some(v)) == 10303 => Some(cx.assets.default_material(cx.materials)),
                                        Some(g) => cx.assets.material(&g, cx.images, cx.materials, cx.fx_materials),
                                        None => None,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        match self.nodes[idx].obj3d.as_mut() {
                            Some(Obj3d::Mesh { mats: mm, enabled: en, light_probes: lp, .. }) => {
                                *mm = mats;
                                *en = enabled;
                                *lp = probes;
                            }
                            _ => self.nodes[idx].obj3d = Some(Obj3d::Mesh { meshes: vec![], mats, layer: 0, enabled, light_probes: probes }),
                        }
                    }
                    20 => {
                        let target = guid_ref(m.get("m_TargetTexture")).and_then(|g| {
                            let p = cx.assets.path(&g)?;
                            Some(self.render_texture(&g, &p, cx))
                        });
                        self.nodes[idx].obj3d = Some(Obj3d::Camera {
                            target,
                            fov: f(m.get("field of view")),
                            ortho: (int(m.get("orthographic")) != 0).then(|| f(m.get("orthographic size"))),
                            near: f(m.get("near clip plane")),
                            far: f(m.get("far clip plane")),
                            clear: rgba(m.get("m_BackGroundColor")),
                            mask: m.get("m_CullingMask").and_then(|c| c.get("m_Bits")).and_then(Value::as_u64).unwrap_or(0) as u32,
                            depth: f(m.get("m_Depth")),
                            enabled,
                        });
                    }
                    108 => {
                        self.nodes[idx].obj3d = Some(Obj3d::Light {
                            ty: int(m.get("m_Type")),
                            color: rgba(m.get("m_Color")),
                            intensity: f(m.get("m_Intensity")),
                            range: f(m.get("m_Range")),
                            mask: m.get("m_CullingMask").and_then(|c| c.get("m_Bits")).and_then(Value::as_u64).unwrap_or(0) as u32,
                            enabled,
                        });
                    }
                    114 => self.nodes[idx].scripts.push((id, name.to_string(), m.clone(), enabled)),
                    _ => {}
                }
            }
        }
    }

    fn render_texture(&mut self, guid: &str, path: &str, cx: &mut ImportCtx) -> Handle<Image> {
        if let Some(h) = self.rt_images.get(guid) {
            return h.clone();
        }
        let _ = path;
        let doc = cx.assets.doc(guid);
        let num = |k: &str| doc.as_ref().and_then(|d| d.get(k)).and_then(Value::as_u64).map(|v| v as u32);
        let (w, h) = (num("m_Width").unwrap_or(1920), num("m_Height").unwrap_or(1080));
        let img = Image::new_target_texture(w, h, TextureFormat::Rgba8UnormSrgb, None);
        let handle = cx.images.add(img);
        self.rt_images.insert(guid.to_string(), handle.clone());
        handle
    }

    /// Resolves the negated file-id references stored during import to node indices.
    pub fn resolve(&mut self) {
        let map = self.by_comp.clone();
        let fix = |r: &mut Option<usize>| {
            if let Some(v) = *r {
                let id = -(v as i64);
                *r = map.get(&id).copied();
            }
        };
        for n in &mut self.nodes {
            if let Some(s) = n.sel.as_mut() {
                if s.target.map(|v| (v as i64) < 0).unwrap_or(false) {
                    fix(&mut s.target);
                }
                for r in s.nav.iter_mut() {
                    if r.map(|v| (v as i64) < 0).unwrap_or(false) {
                        fix(r);
                    }
                }
            }
            match &mut n.widget {
                Widget::Toggle { graphic, .. } => fix(graphic),
                Widget::Slider { fill, handle, .. } => {
                    fix(fill);
                    fix(handle);
                }
                Widget::Scrollbar { handle, .. } => fix(handle),
                Widget::Dropdown { template, caption, item_text, .. } => {
                    fix(template);
                    fix(caption);
                    fix(item_text);
                }
                Widget::Input { text_comp, placeholder, .. } => {
                    fix(text_comp);
                    fix(placeholder);
                }
                _ => {}
            }
            if let Some(s) = n.scroll.as_mut() {
                fix(&mut s.content);
                fix(&mut s.viewport);
                fix(&mut s.vbar);
            }
        }
        for i in 0..self.nodes.len() {
            let layer = self.nodes[i].layer as usize;
            if let Some(Obj3d::Mesh { layer: l, .. }) = self.nodes[i].obj3d.as_mut() {
                *l = layer;
            }
        }
    }

    /// Deep copy of a subtree (Object.Instantiate) under `parent`; returns the new root.
    pub fn clone_subtree(&mut self, src: usize, parent: Option<usize>) -> usize {
        let mut map = HashMap::new();
        let root = self.clone_rec(src, parent, &mut map);
        let remap = |r: &mut Option<usize>| {
            if let Some(v) = *r {
                if let Some(n) = map.get(&v) {
                    *r = Some(*n);
                }
            }
        };
        for &new in map.values() {
            let n = &mut self.nodes[new];
            if let Some(s) = n.sel.as_mut() {
                remap(&mut s.target);
                for r in s.nav.iter_mut() {
                    remap(r);
                }
            }
            match &mut n.widget {
                Widget::Toggle { graphic, .. } => remap(graphic),
                Widget::Slider { fill, handle, .. } => {
                    remap(fill);
                    remap(handle);
                }
                Widget::Scrollbar { handle, .. } => remap(handle),
                _ => {}
            }
            if let Some(s) = n.scroll.as_mut() {
                remap(&mut s.content);
                remap(&mut s.viewport);
                remap(&mut s.vbar);
            }
        }
        root
    }

    fn clone_rec(&mut self, src: usize, parent: Option<usize>, map: &mut HashMap<usize, usize>) -> usize {
        let mut n = self.nodes[src].clone();
        n.parent = parent;
        n.children = vec![];
        n.entity = None;
        n.obj3d = None;
        if let Some(g) = n.graphic.as_mut() {
            g.entity = None;
            g.mesh = None;
            g.last = 0;
        }
        let idx = self.nodes.len();
        self.nodes.push(n);
        map.insert(src, idx);
        if let Some(p) = parent {
            self.nodes[p].children.push(idx);
        }
        let kids = self.nodes[src].children.clone();
        for c in kids {
            self.clone_rec(c, Some(idx), map);
        }
        idx
    }

    /// Removes a subtree created with `clone_subtree` (its render entities are despawned by the renderer).
    pub fn destroy(&mut self, n: usize, despawn: &mut Vec<Entity>) {
        if let Some(p) = self.nodes[n].parent {
            self.nodes[p].children.retain(|&c| c != n);
        }
        let mut stack = vec![n];
        while let Some(i) = stack.pop() {
            stack.extend(self.nodes[i].children.clone());
            self.nodes[i].active = false;
            self.nodes[i].parent = None;
            if let Some(g) = self.nodes[i].graphic.as_mut() {
                if let Some(e) = g.entity.take() {
                    despawn.push(e);
                }
            }
            self.nodes[i].graphic = None;
            self.nodes[i].sel = None;
            self.nodes[i].widget = Widget::None;
        }
        self.roots.retain(|&r| r != n);
        if self.selected == Some(n) {
            self.selected = None;
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Layout

    pub fn layout(&mut self) {
        let mut order = 0u32;
        let roots = self.roots.clone();
        let canvas = Rect::from_center_size(Vec2::ZERO, self.canvas_size);
        self.any_on = false;
        let mut overlays = vec![];
        for r in roots {
            self.layout_rec(r, canvas, Mat4::IDENTITY, true, 1.0, true, true, None, &mut order, &mut overlays);
        }
        // Override-sorting canvases (dropdown lists / blockers) draw over everything else (blocker 29999 < list 30000).
        overlays.sort_by_key(|(n, _)| self.nodes[*n].name != "Blocker");
        for (n, base) in overlays {
            let mut o = order + base;
            self.reorder(n, &mut o);
            order = o;
        }
    }

    fn reorder(&mut self, n: usize, order: &mut u32) {
        if self.nodes[n].graphic.is_some() {
            self.nodes[n].order = *order;
            *order += 1;
        }
        for c in self.nodes[n].children.clone() {
            self.reorder(c, order);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_rec(&mut self, n: usize, prect: Rect, pworld: Mat4, pon: bool, palpha: f32, pinter: bool, pblock: bool, pclip: Option<Rect>, order: &mut u32, overlays: &mut Vec<(usize, u32)>) {
        let node = &self.nodes[n];
        let (local_pos, rect) = match &node.rect_t {
            None => (node.pos, Rect::default()),
            Some(r) => {
                let a0 = prect.min + prect.size() * r.amin;
                let a1 = prect.min + prect.size() * r.amax;
                let size = (a1 - a0) + r.size_delta;
                let p = a0 + (a1 - a0) * r.pivot + r.apos;
                (p.extend(node.pos.z), Rect::from_corners(-r.pivot * size, -r.pivot * size + size))
            }
        };
        let world = pworld * Mat4::from_scale_rotation_translation(node.scale, node.rot, local_pos);
        let on = pon && node.active;
        let (mut alpha, mut inter, mut block) = (palpha, pinter, pblock);
        if let Some((a, i, b, ignore)) = node.group {
            if ignore {
                alpha = a;
                inter = i;
                block = b;
            } else {
                alpha *= a;
                inter &= i;
                block &= b;
            }
        }
        let mut clip = pclip;
        if node.mask.is_some() || node.rect_mask {
            let r = world_rect(&world, rect);
            clip = Some(match clip {
                Some(c) => c.intersect(r),
                None => r,
            });
        }
        let has_g = node.graphic.is_some();
        let over = node.override_sort;
        let nn = &mut self.nodes[n];
        nn.rect = rect;
        nn.world = world;
        nn.on = on;
        nn.alpha = alpha;
        nn.interactable = inter;
        nn.blocks = block;
        // A Mask clips its children, not its own graphic.
        nn.clip = pclip;
        if has_g {
            nn.order = *order;
            *order += 1;
        }
        if on {
            self.any_on = true;
        }
        if over && on {
            overlays.push((n, 0));
        }
        if let Some(g) = self.nodes[n].layout_group {
            self.apply_layout_group(n, g, rect.size());
        }
        for c in self.nodes[n].children.clone() {
            self.layout_rec(c, rect, world, on, alpha, inter, block, clip, order, overlays);
        }
    }

    /// HorizontalOrVerticalLayoutGroup.SetChildrenAlongAxis for both axes, childControl = false (children keep their
    /// sizeDelta; force-expand children share the surplus along the layout axis; anchors forced to top-left).
    fn apply_layout_group(&mut self, n: usize, g: LayoutGroupC, size: Vec2) {
        if g.control[0] || g.control[1] {
            return;
        }
        let kids: Vec<usize> = self.nodes[n].children.iter().copied().filter(|&c| self.nodes[c].active && self.nodes[c].rect_t.is_some()).collect();
        if kids.is_empty() {
            return;
        }
        // ChildAlignment: 0 UpperLeft .. 8 LowerRight -> alignment on x (column) and y (row, 0 = top).
        let align = [(g.child_alignment % 3) as f32 * 0.5, (g.child_alignment / 3) as f32 * 0.5];
        let main = if g.vertical { 1 } else { 0 };
        let pad_start = [g.padding[0], g.padding[2]];
        let pad_total = [g.padding[0] + g.padding[1], g.padding[2] + g.padding[3]];
        for axis in 0..2 {
            let sizes: Vec<f32> = kids.iter().map(|&c| self.nodes[c].rect_t.as_ref().unwrap().size_delta[axis]).collect();
            let set = |s: &mut Self, c: usize, pos: f32, sz: f32| {
                let r = s.nodes[c].rect_t.as_mut().unwrap();
                r.amin = Vec2::new(0.0, 1.0);
                r.amax = Vec2::new(0.0, 1.0);
                r.apos[axis] = if axis == 0 { pos + sz * r.pivot.x } else { -pos - sz * (1.0 - r.pivot.y) };
            };
            if axis == main {
                let total_pref = pad_total[axis] + sizes.iter().sum::<f32>() + g.spacing * (kids.len() as f32 - 1.0);
                let surplus = size[axis] - total_pref;
                let flex_total = if g.force_expand[axis] { kids.len() as f32 } else { 0.0 };
                let mut pos = pad_start[axis];
                let mut mult = 0.0;
                if surplus > 0.0 {
                    if flex_total == 0.0 {
                        pos = pad_start[axis] + (size[axis] - total_pref) * align[axis];
                    } else {
                        mult = surplus / flex_total;
                    }
                }
                for (k, &c) in kids.iter().enumerate() {
                    let flex = if g.force_expand[axis] { 1.0 } else { 0.0 };
                    let child = sizes[k] + flex * mult;
                    let off = (child - sizes[k]) * align[axis];
                    set(self, c, pos + off, sizes[k]);
                    pos += child + g.spacing;
                }
            } else {
                let inner = size[axis] - pad_total[axis];
                for (k, &c) in kids.iter().enumerate() {
                    let req = if g.force_expand[axis] { inner.clamp(sizes[k], size[axis].max(sizes[k])) } else { sizes[k] };
                    let start = pad_start[axis] + (size[axis] - req - pad_total[axis]) * align[axis];
                    let off = (req - sizes[k]) * align[axis];
                    set(self, c, start + off, sizes[k]);
                }
            }
        }
    }
}

fn world_rect(m: &Mat4, r: Rect) -> Rect {
    let pts = [r.min, Vec2::new(r.min.x, r.max.y), r.max, Vec2::new(r.max.x, r.min.y)].map(|p| m.transform_point3(p.extend(0.0)).truncate());
    let mut out = Rect::from_corners(pts[0], pts[0]);
    for p in pts {
        out = out.union_point(p);
    }
    out
}

// ------------------------------------------------------------------------------------------------------------------
// Rendering

/// UI/Default: texture x vertex colour, alpha blended, no depth write, no culling.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct UiImageMaterial {
    #[texture(0)]
    #[sampler(1)]
    pub texture: Handle<Image>,
}

impl Material for UiImageMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/ugui_image.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "shaders/ugui_image.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(_: &MaterialPipeline, descriptor: &mut RenderPipelineDescriptor, layout: &MeshVertexBufferLayoutRef, _: MaterialPipelineKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        let vertex_layout = layout.0.get_layout(&[Mesh::ATTRIBUTE_POSITION.at_shader_location(0), Mesh::ATTRIBUTE_UV_0.at_shader_location(1), Mesh::ATTRIBUTE_COLOR.at_shader_location(2)])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        descriptor.primitive.cull_mode = None;
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_write_enabled = Some(false).into();
        }
        Ok(())
    }
}

#[derive(Resource, Default)]
pub struct UguiRender {
    img_mats: HashMap<AssetId<Image>, Handle<UiImageMaterial>>,
    tmp_mats: HashMap<String, Handle<TmpSdfMaterial>>,
    pub fonts: HashMap<String, Option<Arc<TmpFont>>>,
    pub tmp_props: HashMap<String, Option<Arc<TmpMatProps>>>,
    pub despawn: Vec<Entity>,
}

#[derive(Component)]
pub struct UguiCanvasCamera;

fn lin(c: f32) -> f32 {
    crate::tmp_text::gamma_to_linear(c)
}

/// Canvas vertex colour (Color32 quantised, then linearised by the Canvas in a Linear project).
fn vcolor(c: Vec4) -> [f32; 4] {
    let q = c.to_array().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() / 255.0);
    [lin(q[0]), lin(q[1]), lin(q[2]), q[3]]
}

struct Quad {
    pos: [Vec2; 4],
    uv: [Vec2; 4],
}

/// Clips an axis-aligned quad (BL TL TR BR) against a rect, interpolating the UVs.
fn clip_quad(q: &Quad, c: Rect) -> Option<Quad> {
    let (mn, mx) = (q.pos[0].min(q.pos[2]), q.pos[0].max(q.pos[2]));
    let r = Rect::from_corners(mn, mx).intersect(c);
    if r.is_empty() || r.width() <= 0.0 || r.height() <= 0.0 {
        return None;
    }
    let size = (mx - mn).max(Vec2::splat(1e-6));
    let t0 = (r.min - mn) / size;
    let t1 = (r.max - mn) / size;
    let uv_at = |t: Vec2| {
        let b = q.uv[0].lerp(q.uv[3], t.x);
        let tp = q.uv[1].lerp(q.uv[2], t.x);
        b.lerp(tp, t.y)
    };
    Some(Quad {
        pos: [r.min, Vec2::new(r.min.x, r.max.y), r.max, Vec2::new(r.max.x, r.min.y)],
        uv: [uv_at(t0), uv_at(Vec2::new(t0.x, t1.y)), uv_at(t1), uv_at(Vec2::new(t1.x, t0.y))],
    })
}

fn quad(r: Rect, uv: Rect) -> Quad {
    Quad {
        pos: [r.min, Vec2::new(r.min.x, r.max.y), r.max, Vec2::new(r.max.x, r.min.y)],
        uv: [uv.min, Vec2::new(uv.min.x, uv.max.y), uv.max, Vec2::new(uv.max.x, uv.min.y)],
    }
}

/// Image fill settings (`m_FillMethod` / `m_FillAmount` / `m_FillClockwise` / `m_FillOrigin`), used by Type Filled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImgFill {
    /// 0 Horizontal, 1 Vertical, 2 Radial90, 3 Radial180, 4 Radial360.
    pub method: i64,
    pub amount: f32,
    pub clockwise: bool,
    pub origin: i64,
}

impl Default for ImgFill {
    fn default() -> Self {
        Self { method: 4, amount: 1.0, clockwise: true, origin: 0 }
    }
}

/// `Image.RadialCut(Vector3[], float cos, float sin, bool invert, int corner)`.
fn radial_cut_pts(xy: &mut [Vec2; 4], mut cos: f32, mut sin: f32, invert: bool, corner: usize) {
    let (i0, i1, i2, i3) = (corner, (corner + 1) % 4, (corner + 2) % 4, (corner + 3) % 4);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t.clamp(0.0, 1.0);
    if corner & 1 == 1 {
        if sin > cos {
            cos /= sin;
            sin = 1.0;
            if invert {
                xy[i1].x = lerp(xy[i0].x, xy[i2].x, cos);
                xy[i2].x = xy[i1].x;
            }
        } else if cos > sin {
            sin /= cos;
            cos = 1.0;
            if !invert {
                xy[i2].y = lerp(xy[i0].y, xy[i2].y, sin);
                xy[i3].y = xy[i2].y;
            }
        } else {
            cos = 1.0;
            sin = 1.0;
        }
        if !invert {
            xy[i3].x = lerp(xy[i0].x, xy[i2].x, cos);
        } else {
            xy[i1].y = lerp(xy[i0].y, xy[i2].y, sin);
        }
    } else {
        if cos > sin {
            sin /= cos;
            cos = 1.0;
            if !invert {
                xy[i1].y = lerp(xy[i0].y, xy[i2].y, sin);
                xy[i2].y = xy[i1].y;
            }
        } else if sin > cos {
            cos /= sin;
            sin = 1.0;
            if invert {
                xy[i2].x = lerp(xy[i0].x, xy[i2].x, cos);
                xy[i3].x = xy[i2].x;
            }
        } else {
            cos = 1.0;
            sin = 1.0;
        }
        if invert {
            xy[i3].y = lerp(xy[i0].y, xy[i2].y, sin);
        } else {
            xy[i1].x = lerp(xy[i0].x, xy[i2].x, cos);
        }
    }
}

/// `Image.RadialCut(Vector3[] xy, Vector3[] uv, float fill, bool invert, int corner)`: false = nothing drawn.
fn radial_cut(xy: &mut [Vec2; 4], uv: &mut [Vec2; 4], fill: f32, mut invert: bool, corner: usize) -> bool {
    if fill < 0.001 {
        return false;
    }
    if corner & 1 == 1 {
        invert = !invert;
    }
    if !invert && fill > 0.999 {
        return true;
    }
    let mut angle = fill.clamp(0.0, 1.0);
    if invert {
        angle = 1.0 - angle;
    }
    angle *= std::f32::consts::FRAC_PI_2;
    let (sin, cos) = angle.sin_cos();
    radial_cut_pts(xy, cos, sin, invert, corner);
    radial_cut_pts(uv, cos, sin, invert, corner);
    true
}

/// `Image.GetDrawingDimensions` (sprite padding 0: no atlas / tight packing in the port) incl.
/// `PreserveSpriteAspectRatio`, which keeps the pivot point fixed.
/// `Image.GetDrawingDimensions`: the sprite's padding (transparent margin around its tight texture rect) as fractions
/// of the rounded sprite size, applied to the (aspect-preserved) rect.
fn padded_rect(r: Rect, s: &SpriteInfo) -> Rect {
    if s.padding == Vec4::ZERO {
        return r;
    }
    let size = s.full_size();
    let (w, h) = (size.x.round().max(1.0), size.y.round().max(1.0));
    let v = Vec4::new(s.padding.x / w, s.padding.y / h, (w - s.padding.z) / w, (h - s.padding.w) / h);
    Rect::new(r.min.x + r.width() * v.x, r.min.y + r.height() * v.y, r.min.x + r.width() * v.z, r.min.y + r.height() * v.w)
}

fn drawing_rect(rect: Rect, sprite_size: Vec2, preserve: bool) -> Rect {
    let mut r = rect;
    if preserve && sprite_size.length_squared() > 0.0 {
        let pivot = if rect.width() > 0.0 && rect.height() > 0.0 { -rect.min / rect.size() } else { Vec2::splat(0.5) };
        let sa = sprite_size.x / sprite_size.y;
        let ra = r.width() / r.height();
        if sa > ra {
            let h = r.width() / sa;
            r.min.y += (rect.height() - h) * pivot.y;
            r.max.y = r.min.y + h;
        } else {
            let w = r.height() * sa;
            r.min.x += (rect.width() - w) * pivot.x;
            r.max.x = r.min.x + w;
        }
    }
    r
}

/// `Image.GetAdjustedBorders` (pixel-adjusted rect = rect): shrink when the rect is smaller than the borders.
fn adjusted_borders(mut b: Vec4, size: Vec2) -> Vec4 {
    for axis in 0..2 {
        let (lo, hi) = if axis == 0 { (b.x, b.z) } else { (b.y, b.w) };
        let comb = lo + hi;
        let sz = size[axis];
        if sz < comb && comb != 0.0 {
            let k = sz / comb;
            if axis == 0 {
                b.x *= k;
                b.z *= k;
            } else {
                b.y *= k;
                b.w *= k;
            }
        }
    }
    b
}

fn corners(r: Rect) -> [Vec2; 4] {
    [r.min, Vec2::new(r.min.x, r.max.y), r.max, Vec2::new(r.max.x, r.min.y)]
}

/// Image vertices in RectTransform-local space, Unity `Image.OnPopulateMesh` (Simple / Sliced / Tiled / Filled).
fn image_quads(rect: Rect, kind: &GKind) -> Vec<Quad> {
    match kind {
        GKind::Raw { uv, .. } => vec![quad(rect, *uv)],
        // No sprite: Graphic.OnPopulateMesh (one quad, UV 0..1) whatever the type.
        GKind::Image { sprite: None, .. } => vec![quad(rect, Rect::new(0.0, 0.0, 1.0, 1.0))],
        GKind::Image { sprite: Some(s), ty, fill_center, preserve_aspect, ppu_mult, fill } => {
            let outer = s.outer_uv();
            let has_border = s.border != Vec4::ZERO;
            // Inner UV: the border measured from the sprite rect (= texture rect + padding).
            let inner = Rect::new(
                (s.rect.min.x - s.padding.x + s.border.x) / s.tex_size.x,
                (s.rect.min.y - s.padding.y + s.border.y) / s.tex_size.y,
                (s.rect.max.x + s.padding.z - s.border.z) / s.tex_size.x,
                (s.rect.max.y + s.padding.w - s.border.w) / s.tex_size.y,
            );
            // multipliedPixelsPerUnit = sprite ppu / canvas referencePixelsPerUnit (100 on every canvas) x multiplier.
            let mppu = s.ppu / 100.0 * ppu_mult;
            match *ty {
                1 if has_border => {
                    let b = adjusted_borders(s.border / mppu, rect.size());
                    // GenerateSlicedSprite: the outer vertices are inset by the padding.
                    let pd = s.padding / mppu;
                    let xs = [rect.min.x + pd.x, rect.min.x + b.x, rect.max.x - b.z, rect.max.x - pd.z];
                    let ys = [rect.min.y + pd.y, rect.min.y + b.y, rect.max.y - b.w, rect.max.y - pd.w];
                    let us = [outer.min.x, inner.min.x, inner.max.x, outer.max.x];
                    let vs = [outer.min.y, inner.min.y, inner.max.y, outer.max.y];
                    let mut out = vec![];
                    for x in 0..3 {
                        for y in 0..3 {
                            if !fill_center && x == 1 && y == 1 {
                                continue;
                            }
                            let r = Rect::new(xs[x], ys[y], xs[x + 1], ys[y + 1]);
                            if r.width() <= 0.0 || r.height() <= 0.0 {
                                continue;
                            }
                            out.push(quad(r, Rect::new(us[x], vs[y], us[x + 1], vs[y + 1])));
                        }
                    }
                    out
                }
                2 => tiled_quads(rect, s, outer, inner, has_border, mppu, *fill_center),
                3 => filled_quads(padded_rect(drawing_rect(rect, s.full_size(), *preserve_aspect), s), outer, fill),
                // Simple; Sliced without border = GenerateSimpleSprite(vh, false).
                t => vec![quad(padded_rect(drawing_rect(rect, s.full_size(), t == 0 && *preserve_aspect), s), outer)],
            }
        }
        GKind::Text(_) => vec![],
    }
}

/// `Image.GenerateTiledSprite` (sprites are never packed in the port; wrapMode from the texture importer).
fn tiled_quads(rect: Rect, s: &SpriteInfo, outer: Rect, inner: Rect, has_border: bool, mppu: f32, fill_center: bool) -> Vec<Quad> {
    let sz = s.rect.size();
    let mut tw = (sz.x - s.border.x - s.border.z) / mppu;
    let mut th = (sz.y - s.border.y - s.border.w) / mppu;
    let b = adjusted_borders(s.border / mppu, rect.size());
    let (uv_min, uv_max) = (inner.min, inner.max);
    let (x_min, x_max, y_min, y_max) = (b.x, rect.width() - b.z, b.y, rect.height() - b.w);
    let (w, h, o) = (rect.width(), rect.height(), rect.min);
    let mut out = vec![];
    let mut add = |p0: Vec2, p1: Vec2, u0: Vec2, u1: Vec2| out.push(quad(Rect { min: p0 + o, max: p1 + o }, Rect { min: u0, max: u1 }));
    if tw <= 0.0 {
        tw = x_max - x_min;
    }
    if th <= 0.0 {
        th = y_max - y_min;
    }
    if !(has_border || !s.repeat) {
        // No border, Repeat, not packed: one quad with texture tiling (UVs scaled past 1).
        if fill_center {
            let k = Vec2::new((x_max - x_min) / tw, (y_max - y_min) / th);
            add(Vec2::new(x_min, y_min), Vec2::new(x_max, y_max), uv_min * k, uv_max * k);
        }
        return out;
    }
    let (mut nw, mut nh) = (0i64, 0i64);
    if fill_center || has_border {
        nw = ((x_max - x_min) / tw).ceil() as i64;
        nh = ((y_max - y_min) / th).ceil() as i64;
        let (nwf, nhf) = (nw as f64, nh as f64);
        let (nv, ratio, bd) = match (fill_center, has_border) {
            (true, true) => ((nwf + 2.0) * (nhf + 2.0) * 4.0, (nwf + 2.0) / (nhf + 2.0), 2.0),
            (true, false) => (nwf * nhf * 4.0, nwf / nhf, 0.0),
            _ => ((nhf + nwf + 2.0) * 2.0 * 4.0, (nwf + 2.0) / (nhf + 2.0), 2.0),
        };
        if nv > 65000.0 {
            warn!("ugui: Tiled Image has too many tiles, reduced like Unity");
            let tw_t = (65000.0 / 4.0 / ratio).sqrt();
            nw = (tw_t - bd).floor() as i64;
            nh = (tw_t * ratio - bd).floor() as i64;
            tw = (x_max - x_min) / nw as f32;
            th = (y_max - y_min) / nh as f32;
        }
    }
    if fill_center {
        let mut clipped = uv_max;
        for j in 0..nh {
            let y1 = y_min + j as f32 * th;
            let mut y2 = y_min + (j + 1) as f32 * th;
            if y2 > y_max {
                clipped.y = uv_min.y + (uv_max.y - uv_min.y) * (y_max - y1) / (y2 - y1);
                y2 = y_max;
            }
            clipped.x = uv_max.x;
            for i in 0..nw {
                let x1 = x_min + i as f32 * tw;
                let mut x2 = x_min + (i + 1) as f32 * tw;
                if x2 > x_max {
                    clipped.x = uv_min.x + (uv_max.x - uv_min.x) * (x_max - x1) / (x2 - x1);
                    x2 = x_max;
                }
                add(Vec2::new(x1, y1), Vec2::new(x2, y2), uv_min, clipped);
            }
        }
    }
    if has_border {
        let mut clipped = uv_max;
        for j in 0..nh {
            let y1 = y_min + j as f32 * th;
            let mut y2 = y_min + (j + 1) as f32 * th;
            if y2 > y_max {
                clipped.y = uv_min.y + (uv_max.y - uv_min.y) * (y_max - y1) / (y2 - y1);
                y2 = y_max;
            }
            add(Vec2::new(0.0, y1), Vec2::new(x_min, y2), Vec2::new(outer.min.x, uv_min.y), Vec2::new(uv_min.x, clipped.y));
            add(Vec2::new(x_max, y1), Vec2::new(w, y2), Vec2::new(uv_max.x, uv_min.y), Vec2::new(outer.max.x, clipped.y));
        }
        let mut clipped = uv_max;
        for i in 0..nw {
            let x1 = x_min + i as f32 * tw;
            let mut x2 = x_min + (i + 1) as f32 * tw;
            if x2 > x_max {
                clipped.x = uv_min.x + (uv_max.x - uv_min.x) * (x_max - x1) / (x2 - x1);
                x2 = x_max;
            }
            add(Vec2::new(x1, 0.0), Vec2::new(x2, y_min), Vec2::new(uv_min.x, outer.min.y), Vec2::new(clipped.x, uv_min.y));
            add(Vec2::new(x1, y_max), Vec2::new(x2, h), Vec2::new(uv_min.x, uv_max.y), Vec2::new(clipped.x, outer.max.y));
        }
        add(Vec2::ZERO, Vec2::new(x_min, y_min), outer.min, uv_min);
        add(Vec2::new(x_max, 0.0), Vec2::new(w, y_min), Vec2::new(uv_max.x, outer.min.y), Vec2::new(outer.max.x, uv_min.y));
        add(Vec2::new(0.0, y_max), Vec2::new(x_min, h), Vec2::new(outer.min.x, uv_max.y), Vec2::new(uv_min.x, outer.max.y));
        add(Vec2::new(x_max, y_max), Vec2::new(w, h), uv_max, outer.max);
    }
    out
}

/// `Image.GenerateFilledSprite`.
fn filled_quads(rect: Rect, outer: Rect, f: &ImgFill) -> Vec<Quad> {
    let amount = f.amount.clamp(0.0, 1.0);
    if amount < 0.001 {
        return vec![];
    }
    let (mut v, mut t) = (rect, outer);
    match f.method {
        0 => {
            let fill = (t.max.x - t.min.x) * amount;
            if f.origin == 1 {
                v.min.x = v.max.x - (v.max.x - v.min.x) * amount;
                t.min.x = t.max.x - fill;
            } else {
                v.max.x = v.min.x + (v.max.x - v.min.x) * amount;
                t.max.x = t.min.x + fill;
            }
        }
        1 => {
            let fill = (t.max.y - t.min.y) * amount;
            if f.origin == 1 {
                v.min.y = v.max.y - (v.max.y - v.min.y) * amount;
                t.min.y = t.max.y - fill;
            } else {
                v.max.y = v.min.y + (v.max.y - v.min.y) * amount;
                t.max.y = t.min.y + fill;
            }
        }
        _ => {}
    }
    if amount >= 1.0 || f.method < 2 {
        return vec![Quad { pos: corners(v), uv: corners(t) }];
    }
    // Sub-rect (fractions of the drawing rect / outer UV).
    let sub = |fx: (f32, f32), fy: (f32, f32)| {
        let part = |r: Rect| Rect {
            min: Vec2::new(r.min.x + (r.max.x - r.min.x) * fx.0, r.min.y + (r.max.y - r.min.y) * fy.0),
            max: Vec2::new(r.min.x + (r.max.x - r.min.x) * fx.1, r.min.y + (r.max.y - r.min.y) * fy.1),
        };
        (corners(part(v)), corners(part(t)))
    };
    let mut out = vec![];
    let origin = f.origin.rem_euclid(4) as usize;
    match f.method {
        2 => {
            let (mut xy, mut uv) = (corners(v), corners(t));
            if radial_cut(&mut xy, &mut uv, amount, f.clockwise, origin) {
                out.push(Quad { pos: xy, uv });
            }
        }
        3 => {
            let even = usize::from(origin > 1);
            for side in 0..2usize {
                let (fx, fy) = if origin == 0 || origin == 2 {
                    (if side == even { (0.0, 0.5) } else { (0.5, 1.0) }, (0.0, 1.0))
                } else {
                    ((0.0, 1.0), if side == even { (0.5, 1.0) } else { (0.0, 0.5) })
                };
                let (mut xy, mut uv) = sub(fx, fy);
                let val = if f.clockwise { amount * 2.0 - side as f32 } else { amount * 2.0 - (1 - side) as f32 };
                if radial_cut(&mut xy, &mut uv, val.clamp(0.0, 1.0), f.clockwise, (side + origin + 3) % 4) {
                    out.push(Quad { pos: xy, uv });
                }
            }
        }
        _ => {
            for corner in 0..4usize {
                let fx = if corner < 2 { (0.0, 0.5) } else { (0.5, 1.0) };
                let fy = if corner == 0 || corner == 3 { (0.0, 0.5) } else { (0.5, 1.0) };
                let (mut xy, mut uv) = sub(fx, fy);
                let k = ((corner + origin) % 4) as f32;
                let val = if f.clockwise { amount * 4.0 - k } else { amount * 4.0 - (3.0 - k) };
                if radial_cut(&mut xy, &mut uv, val.clamp(0.0, 1.0), f.clockwise, (corner + 2) % 4) {
                    out.push(Quad { pos: xy, uv });
                }
            }
        }
    }
    out
}

fn hash_f32s(h: &mut u64, v: &[f32]) {
    for x in v {
        *h = h.wrapping_mul(0x100000001b3) ^ (x.to_bits() as u64);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ugui_render(
    mut commands: Commands,
    mut tree: ResMut<UiTree>,
    mut rend: ResMut<UguiRender>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut img_mats: ResMut<Assets<UiImageMaterial>>,
    mut tmp_mats: ResMut<Assets<TmpSdfMaterial>>,
    mut uassets: ResMut<UnityAssets>,
    mut images: ResMut<Assets<Image>>,
    mut vis_q: Query<(&mut Visibility, &mut Transform)>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    for e in rend.despawn.drain(..) {
        commands.entity(e).try_despawn();
    }
    if let Ok(w) = windows.single() {
        let size = Vec2::new(w.width(), w.height());
        let s = canvas_scale(size);
        tree.scale = s;
        tree.canvas_size = size / s;
    }
    tree.layout();
    let white = uassets.white(&mut images);
    let n = tree.nodes.len();
    for i in 0..n {
        let Some(g) = tree.nodes[i].graphic.as_ref() else { continue };
        let node = &tree.nodes[i];
        let draw_self = node.mask != Some(false);
        let visible = node.on && g.enabled && draw_self;
        let entity = g.entity;
        if !visible {
            if let Some(e) = entity {
                if let Ok((mut v, _)) = vis_q.get_mut(e) {
                    if *v != Visibility::Hidden {
                        *v = Visibility::Hidden;
                    }
                }
            }
            continue;
        }
        let world = node.world;
        let rect = node.rect;
        let clip = node.clip;
        let order = node.order;
        let color = col4(g.color) * g.tint * Vec4::new(1.0, 1.0, 1.0, node.alpha);
        let lossy_y = world.y_axis.truncate().length();
        // Build vertices.
        let (mut pos, mut uv0, mut uv1, mut cols, mut idx): (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[f32; 2]>, Vec<[f32; 4]>, Vec<u32>) = (vec![], vec![], vec![], vec![], vec![]);
        let vc = vcolor(color);
        let mut push = |q: &Quad, scale: f32| {
            let mut q2 = Quad { pos: q.pos.map(|p| world.transform_point3(p.extend(0.0)).truncate()), uv: q.uv };
            // Radial-filled quads are not axis-aligned: kept whole when inside the clip (clip_quad assumes a rect).
            let inside = clip.is_none_or(|c| q2.pos.iter().all(|p| c.contains(*p)));
            if let (Some(c), false) = (clip, inside) {
                match clip_quad(&q2, c) {
                    Some(c) => q2 = c,
                    None => return,
                }
            }
            let b = pos.len() as u32;
            for k in 0..4 {
                pos.push([q2.pos[k].x, q2.pos[k].y, 0.0]);
                uv0.push([q2.uv[k].x, 1.0 - q2.uv[k].y]);
                uv1.push([0.0, scale]);
                cols.push(vc);
            }
            idx.extend_from_slice(&[b, b + 1, b + 2, b + 2, b + 3, b]);
        };
        let mut is_text = false;
        let g = tree.nodes[i].graphic.as_mut().unwrap();
        match &mut g.kind {
            GKind::Text(t) => {
                is_text = true;
                let l = TmpUiLayout {
                    font_size: t.size,
                    rect_size: rect.size(),
                    pivot: if rect.width() > 0.0 { -rect.min / rect.size() } else { Vec2::splat(0.5) },
                    margin: t.margin,
                    h_align: t.h_align,
                    v_align: t.v_align,
                    char_spacing: t.char_spacing,
                    word_spacing: t.word_spacing,
                    line_spacing: t.line_spacing,
                    paragraph_spacing: 0.0,
                    wrap: t.wrap,
                    style: t.style,
                    grad_ratio: t.props.grad_ratio(),
                };
                let fresh = match &t.cache {
                    Some((cl, ct, _)) => *cl != l || *ct != t.text,
                    None => true,
                };
                if fresh {
                    let q = layout_ugui(&t.font, t.props.padding(), &l, &t.text).into_iter().map(|q| (q.pos, q.uv, q.scale)).collect();
                    t.cache = Some((l, t.text.clone(), Arc::new(q)));
                }
                let quads = t.cache.as_ref().unwrap().2.clone();
                for (p, uv, s) in quads.iter() {
                    push(&Quad { pos: *p, uv: *uv }, *s * lossy_y);
                }
            }
            kind => {
                for q in image_quads(rect, kind) {
                    push(&q, 1.0);
                }
            }
        }
        let mut h = 0xcbf29ce484222325u64;
        for p in &pos {
            hash_f32s(&mut h, p);
        }
        for p in &uv0 {
            hash_f32s(&mut h, p);
        }
        hash_f32s(&mut h, &vc);
        hash_f32s(&mut h, &[order as f32]);
        let g = tree.nodes[i].graphic.as_mut().unwrap();
        let z = 1.0 + order as f32 * 0.01;
        if let Some(e) = g.entity {
            if let Ok((mut v, mut tf)) = vis_q.get_mut(e) {
                let want = if pos.is_empty() { Visibility::Hidden } else { Visibility::Inherited };
                if *v != want {
                    *v = want;
                }
                if tf.translation.z != z {
                    tf.translation.z = z;
                }
            }
            if g.last == h {
                continue;
            }
        }
        if pos.is_empty() {
            // Nothing to draw (empty text / fully clipped): never upload a zero-sized mesh.
            continue;
        }
        g.last = h;
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos.clone())
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv0)
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, cols)
            .with_inserted_indices(Indices::U32(idx));
        if is_text {
            let nrm = vec![[0.0, 0.0, -1.0]; pos.len()];
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nrm);
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv1);
        }
        match (g.entity, g.mesh.clone()) {
            (Some(_), Some(h)) => {
                // Replace the mesh data in place (NoFrustumCulling: no stale Aabb).
                let _ = meshes.insert(h.id(), mesh);
            }
            _ => {
                let mesh = meshes.add(mesh);
                g.mesh = Some(mesh.clone());
                let base = (Name::new(format!("ugui:{}", tree.nodes[i].name)), Mesh3d(mesh), Transform::from_xyz(0.0, 0.0, z), Visibility::Inherited, RenderLayers::layer(CANVAS_LAYER), NoFrustumCulling);
                let e = match &tree.nodes[i].graphic.as_ref().unwrap().kind {
                    GKind::Text(t) => {
                        let key = format!("{}|{}", t.mat_key, t.font.atlas.id());
                        let m = rend.tmp_mats.entry(key).or_insert_with(|| tmp_mats.add(TmpSdfMaterial { params: t.props.params(), atlas: t.font.atlas.clone() })).clone();
                        commands.spawn((base, MeshMaterial3d(m))).id()
                    }
                    GKind::Image { sprite, .. } => {
                        let tex = sprite.as_ref().map(|s| s.texture.clone()).unwrap_or(white.clone());
                        let m = rend.img_mats.entry(tex.id()).or_insert_with(|| img_mats.add(UiImageMaterial { texture: tex.clone() })).clone();
                        commands.spawn((base, MeshMaterial3d(m))).id()
                    }
                    GKind::Raw { tex, .. } => {
                        let tex = tex.clone().unwrap_or(white.clone());
                        let m = rend.img_mats.entry(tex.id()).or_insert_with(|| img_mats.add(UiImageMaterial { texture: tex.clone() })).clone();
                        commands.spawn((base, MeshMaterial3d(m))).id()
                    }
                };
                tree.nodes[i].graphic.as_mut().unwrap().entity = Some(e);
            }
        }
    }
}

/// Places the 3D objects (meshes, cameras, lights) of the imported menus.
#[allow(clippy::too_many_arguments)]
pub fn ugui_sync_3d(mut commands: Commands, mut tree: ResMut<UiTree>, mut q: Query<(&mut Transform, &mut Visibility)>, mut cams: Query<&mut Camera>, ambient: Res<UguiAmbient>, mut std_mats: ResMut<Assets<StandardMaterial>>, mut plights: Query<&mut PointLight>) {
    // Unity's canvas root is scaled by the CanvasScaler factor, which scales every 3D object below it but not a
    // Light's range (world units). The port lays the canvas out unscaled, so a light covers range / scale here.
    let range_scale = 1.0 / tree.scale.max(1e-4);
    let s = Mat4::from_scale(Vec3::new(1.0, 1.0, -1.0));
    for i in 0..tree.nodes.len() {
        let Some(obj) = tree.nodes[i].obj3d.clone() else { continue };
        let node = &tree.nodes[i];
        let m = s * node.world * s;
        let (sc, rot, tr) = m.to_scale_rotation_translation();
        let tf = Transform { translation: tr, rotation: rot.normalize(), scale: sc };
        let on = node.on;
        match node.entity {
            None => {
                let name = Name::new(format!("ugui3d:{}", node.name));
                let e = match &obj {
                    Obj3d::Mesh { meshes, mats, layer, .. } => {
                        if std::env::var_os("RESTRAFE_DEBUG_UGUI_LIGHTS").is_some() {
                            println!("UGUI_MESH {} at={:?} scale={:?} layer={layer}", node.name, tf.translation, tf.scale);
                        }
                        let e = commands.spawn((name, tf, Visibility::Hidden)).id();
                        for (k, mh) in meshes.iter().enumerate() {
                            let mat = mats.get(k).or(mats.last()).cloned().flatten();
                            let base = (Mesh3d(mh.clone()), Transform::default(), RenderLayers::layer(*layer), bevy::light::NotShadowCaster, ChildOf(e));
                            match mat {
                                Some(crate::ugui_assets::UMat::Std(m)) => {
                                    // The menu cameras render forward (their lights / ambient as in the original); opaque
                                    // materials default to deferred, which those cameras would skip.
                                    if let Some(mut sm) = std_mats.get_mut(&m) {
                                        if sm.opaque_render_method != bevy::material::OpaqueRendererMethod::Forward {
                                            sm.opaque_render_method = bevy::material::OpaqueRendererMethod::Forward;
                                        }
                                    }
                                    commands.spawn((base, MeshMaterial3d(m)));
                                }
                                Some(crate::ugui_assets::UMat::Fx(m)) => {
                                    commands.spawn((base, MeshMaterial3d(m)));
                                }
                                None => {}
                            }
                        }
                        e
                    }
                    Obj3d::Camera { target, fov, ortho, near, far, clear, mask, depth, .. } => {
                        let proj = match ortho {
                            Some(size) => Projection::Orthographic(OrthographicProjection { scaling_mode: ScalingMode::FixedVertical { viewport_height: size * 2.0 }, near: *near, far: *far, ..OrthographicProjection::default_3d() }),
                            None => Projection::Perspective(PerspectiveProjection { fov: fov.to_radians(), near: *near, far: *far, ..default() }),
                        };
                        let layers = RenderLayers::from_layers(&(0..32).filter(|b| mask & (1 << b) != 0).map(|b| b as usize).collect::<Vec<_>>());
                        let mut ec = commands.spawn((
                            name,
                            Camera3d::default(),
                            Camera { order: -20 + *depth as isize, clear_color: ClearColorConfig::Custom(Color::srgba(clear[0], clear[1], clear[2], clear[3])), is_active: false, ..default() },
                            proj,
                            tf,
                            layers,
                            Msaa::Off,
                            bevy::core_pipeline::tonemapping::Tonemapping::None,
                            bevy::camera::Exposure { ev100: 0.0 },
                            AmbientLight { color: ambient.color, brightness: ambient.brightness, affects_lightmapped_meshes: true },
                        ));
                        if let Some(t) = target {
                            ec.insert(RenderTarget::Image(t.clone().into()));
                        }
                        ec.id()
                    }
                    Obj3d::Light { ty, color, intensity, range, mask, .. } => {
                        if std::env::var_os("RESTRAFE_DEBUG_UGUI_LIGHTS").is_some() {
                            println!("UGUI_LIGHT {} ty={ty} at={:?} range={} (x{range_scale}) I={intensity} on={}", node.name, tf.translation, range, node.on);
                        }
                        let layers = RenderLayers::from_layers(&(0..32).filter(|b| mask & (1 << b) != 0).map(|b| b as usize).collect::<Vec<_>>());
                        let c = Color::srgb(color[0], color[1], color[2]);
                        // Unity intensity I (diffuse = albedo * I * NdotL) -> lux with the menu cameras' ev100 = 0.
                        let lux = intensity * std::f32::consts::PI * 1.2;
                        match ty {
                            1 => commands.spawn((name, DirectionalLight { color: c, illuminance: lux, shadow_maps_enabled: false, ..default() }, tf, layers)).id(),
                            _ => commands.spawn((name, PointLight { color: c, intensity: lux * 4.0 * std::f32::consts::PI, range: *range * range_scale, radius: crate::unity_light::UNITY_LIGHT_RADIUS, shadow_maps_enabled: false, ..default() }, tf, layers)).id(),
                        }
                    }
                };
                tree.nodes[i].entity = Some(e);
            }
            Some(e) => {
                let enabled = match &obj {
                    Obj3d::Mesh { enabled, .. } | Obj3d::Camera { enabled, .. } | Obj3d::Light { enabled, .. } => *enabled,
                };
                if let Ok((mut t, mut v)) = q.get_mut(e) {
                    if *t != tf {
                        *t = tf;
                    }
                    let want = if on && enabled { Visibility::Inherited } else { Visibility::Hidden };
                    if *v != want {
                        *v = want;
                    }
                }
                if let (Obj3d::Light { range, .. }, Ok(mut pl)) = (&obj, plights.get_mut(e)) {
                    let r = *range * range_scale;
                    if (pl.range - r).abs() > 1e-4 {
                        pl.range = r;
                    }
                }
                if let Ok(mut c) = cams.get_mut(e) {
                    let act = on && enabled;
                    if c.is_active != act {
                        c.is_active = act;
                    }
                }
            }
        }
    }
}

/// Unity lights a renderer with `m_LightProbeUsage` Blend Probes from the scene's light probes interpolated at its
/// bounds centre, instead of the scene ambient. The canvas is Screen Space - Overlay: its RectTransform sits at
/// (Screen.width / 2, Screen.height / 2, 0) with localScale = the CanvasScaler factor, so the menus' 3D objects (options
/// menu frame, cosmetics frames) lie hundreds of units from the map and take the outer probe cells' light. Each menu
/// camera gets that probe light (diffuse environment map) at the bounds centre of the probe-lit meshes it renders, and
/// no ambient when all of them use probes. Debug: RESTRAFE_DEBUG_UGUI_PROBES=1 prints the SH per camera.
#[allow(clippy::too_many_arguments)]
pub fn ugui_probe_light(
    tree: Res<UiTree>,
    probes: Option<Res<crate::scene_map::MapProbes>>,
    tets: Option<Res<crate::scene_map::MapProbeTets>>,
    meshes: Res<Assets<Mesh>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
    mut cams: Query<(&mut AmbientLight, Option<&mut bevy::light::EnvironmentMapLight>)>,
    mut commands: Commands,
    mut cache: Local<HashMap<usize, [Vec3; 4]>>,
) {
    let (Some(probes), Some(tets)) = (probes, tets) else { return };
    let Ok(win) = windows.single() else { return };
    let screen = Vec2::new(win.physical_width() as f32, win.physical_height() as f32);
    let sf = canvas_scale(screen);
    for (i, n) in tree.nodes.iter().enumerate() {
        let (Some(Obj3d::Camera { mask, .. }), Some(e), true) = (&n.obj3d, n.entity, n.on) else { continue };
        let (mut lo, mut hi, mut any_probe, mut all_probe) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN), false, true);
        for m in &tree.nodes {
            let Some(Obj3d::Mesh { meshes: hs, layer, enabled: true, light_probes, .. }) = &m.obj3d else { continue };
            if !m.on || mask & (1 << *layer) == 0 {
                continue;
            }
            all_probe &= *light_probes;
            if !*light_probes {
                continue;
            }
            for h in hs {
                let Some(aabb) = meshes.get(h).and_then(|m| bevy::camera::primitives::MeshAabb::compute_aabb(m)) else { continue };
                let (c, he) = (Vec3::from(aabb.center), Vec3::from(aabb.half_extents));
                for k in 0..8 {
                    let corner = c + he * Vec3::new(if k & 1 == 0 { -1.0 } else { 1.0 }, if k & 2 == 0 { -1.0 } else { 1.0 }, if k & 4 == 0 { -1.0 } else { 1.0 });
                    let w = m.world.transform_point3(corner);
                    lo = lo.min(w);
                    hi = hi.max(w);
                    any_probe = true;
                }
            }
        }
        if !any_probe {
            continue;
        }
        let unity = (screen / 2.0).extend(0.0) + sf * (lo + hi) * 0.5;
        let port = Vec3::new(unity.x, unity.y, -unity.z) + crate::scene_map::MAP_OFFSET;
        let Some(sh) = crate::scene_map::light_probe_sh(&probes, &tets, port) else { continue };
        let Ok((mut amb, env)) = cams.get_mut(e) else { continue };
        if all_probe && amb.brightness != 0.0 {
            amb.brightness = 0.0;
        }
        let changed = cache.get(&i).is_none_or(|old| old.iter().zip(sh.iter()).any(|(a, b)| (*a - *b).abs().max_element() > 1e-4));
        if !changed && env.is_some() {
            continue;
        }
        if std::env::var_os("RESTRAFE_DEBUG_UGUI_PROBES").is_some() {
            println!("UGUI_PROBES {} unity={unity:?} sh={sh:?} all_probe={all_probe}", n.name);
        }
        cache.insert(i, sh);
        // Menu cameras: ev100 0 (view exposure 1 / 1.2).
        commands.entity(e).insert(crate::scene_map::sh_environment(&mut images, sh, 1.0 / 1.2));
    }
}

/// Scene ambient light applied to the menu cameras (Unity RenderSettings of the active scene).
#[derive(Resource, Clone, Copy)]
pub struct UguiAmbient {
    pub color: Color,
    pub brightness: f32,
}

impl Default for UguiAmbient {
    fn default() -> Self {
        UguiAmbient { color: Color::WHITE, brightness: 0.0 }
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Widgets

fn slider_norm(min: f32, max: f32, v: f32) -> f32 {
    if (max - min).abs() < 1e-6 { 0.0 } else { ((v - min) / (max - min)).clamp(0.0, 1.0) }
}

impl UiTree {
    /// `Slider.Set` (clamped, whole numbers) + onValueChanged when changed and `notify`.
    pub fn slider_set(&mut self, n: usize, v: f32, notify: bool) {
        let mut fire = None;
        if let Widget::Slider { min, max, whole, value, calls, .. } = &mut self.nodes[n].widget {
            let mut nv = v.clamp(min.min(*max), max.max(*min));
            if *whole {
                nv = nv.round();
            }
            if *value != nv {
                *value = nv;
                if notify {
                    fire = Some((calls.clone(), nv));
                }
            }
        }
        self.slider_visuals(n);
        if let Some((calls, v)) = fire {
            self.queue_calls(n, &calls, CallValue::Float(v));
        }
    }

    pub fn slider_value(&self, n: usize) -> Option<f32> {
        match &self.nodes[n].widget {
            Widget::Slider { value, .. } => Some(*value),
            _ => None,
        }
    }

    pub fn slider_visuals(&mut self, n: usize) {
        let Widget::Slider { min, max, value, dir, fill, handle, .. } = self.nodes[n].widget.clone() else { return };
        let nv = slider_norm(min, max, value);
        let axis = if dir >= 2 { 1 } else { 0 };
        let reverse = dir == 1 || dir == 3;
        if let Some(f) = fill {
            if let Some(r) = self.nodes[f].rect_t.as_mut() {
                let (mut a0, mut a1) = (Vec2::ZERO, Vec2::ONE);
                if reverse {
                    a0[axis] = 1.0 - nv;
                } else {
                    a1[axis] = nv;
                }
                r.amin = a0;
                r.amax = a1;
            }
        }
        if let Some(h) = handle {
            if let Some(r) = self.nodes[h].rect_t.as_mut() {
                let (mut a0, mut a1) = (Vec2::ZERO, Vec2::ONE);
                let v = if reverse { 1.0 - nv } else { nv };
                a0[axis] = v;
                a1[axis] = v;
                r.amin = a0;
                r.amax = a1;
            }
        }
    }

    /// `Toggle.Set`.
    pub fn toggle_set(&mut self, n: usize, on: bool, notify: bool, instant: bool) {
        let mut fire = None;
        let mut gfx = None;
        if let Widget::Toggle { is_on, graphic, transition, calls } = &mut self.nodes[n].widget {
            if *is_on != on || instant {
                if *is_on != on && notify {
                    fire = Some(calls.clone());
                }
                *is_on = on;
                gfx = graphic.map(|g| (g, if instant || *transition == 0 { 0.0 } else { 0.1 }));
            }
        }
        if let Some((g, d)) = gfx {
            if let Some(gr) = self.nodes[g].graphic.as_mut() {
                gr.cross_fade(Vec4::new(1.0, 1.0, 1.0, if on { 1.0 } else { 0.0 }), d, true);
            }
        }
        if let Some(calls) = fire {
            self.queue_calls(n, &calls, CallValue::Bool(on));
        }
    }

    pub fn toggle_value(&self, n: usize) -> Option<bool> {
        match &self.nodes[n].widget {
            Widget::Toggle { is_on, .. } => Some(*is_on),
            _ => None,
        }
    }

    pub fn dropdown_value(&self, n: usize) -> Option<i32> {
        match &self.nodes[n].widget {
            Widget::Dropdown { value, .. } => Some(*value),
            _ => None,
        }
    }

    /// `TMP_Dropdown.SetValue` + RefreshShownValue.
    pub fn dropdown_set(&mut self, n: usize, v: i32, notify: bool) {
        let mut fire = None;
        let mut caption = None;
        if let Widget::Dropdown { value, options, calls, caption: cap, .. } = &mut self.nodes[n].widget {
            let v = v.clamp(0, (options.len() as i32 - 1).max(0));
            if *value != v && notify {
                fire = Some(calls.clone());
            }
            *value = v;
            caption = cap.map(|c| (c, options.get(v as usize).cloned().unwrap_or_default()));
        }
        if let Some((c, s)) = caption {
            self.set_text(c, &s);
        }
        if let Some(calls) = fire {
            self.queue_calls(n, &calls, CallValue::Int(v));
        }
    }

    pub fn dropdown_set_options(&mut self, n: usize, opts: Vec<String>) {
        if let Widget::Dropdown { options, .. } = &mut self.nodes[n].widget {
            *options = opts;
        }
        let v = self.dropdown_value(n).unwrap_or(0);
        self.dropdown_set(n, v, false);
    }

    pub fn input_text(&self, n: usize) -> Option<&str> {
        match &self.nodes[n].widget {
            Widget::Input { text, .. } => Some(text),
            _ => None,
        }
    }

    /// `TMP_InputField.text` setter (SetTextWithoutNotify when !notify).
    pub fn input_set(&mut self, n: usize, s: &str, notify: bool) {
        let mut fire = None;
        if let Widget::Input { text, change_calls, .. } = &mut self.nodes[n].widget {
            if text != s {
                *text = s.to_string();
                if notify {
                    fire = Some(change_calls.clone());
                }
            }
        }
        self.input_refresh(n);
        if let Some(calls) = fire {
            self.queue_calls(n, &calls, CallValue::Str(s.to_string()));
        }
    }

    fn input_refresh(&mut self, n: usize) {
        let Widget::Input { text_comp, placeholder, text, focused, .. } = self.nodes[n].widget.clone() else { return };
        if let Some(t) = text_comp {
            // TMP_InputField appends a zero-width space to the displayed text.
            self.set_text(t, &format!("{text}\u{200b}"));
        }
        if let Some(p) = placeholder {
            if let Some(g) = self.nodes[p].graphic.as_mut() {
                g.enabled = text.is_empty();
                let _ = focused;
            }
        }
    }

    /// `EventSystem.SetSelectedGameObject` (OnDeselect on the old object, OnSelect on the new one).
    pub fn set_selected(&mut self, n: Option<usize>) {
        self.select(n);
    }

    fn select(&mut self, n: Option<usize>) {
        if self.selected == n {
            return;
        }
        if let Some(old) = self.selected {
            if let Some(s) = self.nodes[old].sel.as_mut() {
                s.selected = false;
            }
            self.pointer_events.push((old, PointerKind::Deselect));
            self.input_deactivate(old);
        }
        self.selected = n;
        if let Some(new) = n {
            if let Some(s) = self.nodes[new].sel.as_mut() {
                s.selected = true;
            }
            self.pointer_events.push((new, PointerKind::Select));
        }
    }

    fn input_deactivate(&mut self, n: usize) {
        let mut fire = None;
        if let Widget::Input { focused, text, end_calls, .. } = &mut self.nodes[n].widget {
            if *focused {
                *focused = false;
                fire = Some((end_calls.clone(), text.clone()));
            }
        }
        self.input_refresh(n);
        if let Some((calls, t)) = fire {
            self.queue_calls(n, &calls, CallValue::Str(t));
        }
    }

    /// Selectable.DoStateTransition (ColorTint).
    fn update_selectables(&mut self) {
        for i in 0..self.nodes.len() {
            let Some(s) = self.nodes[i].sel.clone() else { continue };
            let inter = s.interactable && self.nodes[i].interactable;
            let state = if !inter {
                4
            } else if s.down || s.submit_t > 0.0 {
                2
            } else if s.selected {
                3
            } else if s.hover {
                1
            } else {
                0
            };
            if state == s.state {
                continue;
            }
            let instant = s.state < 0;
            self.nodes[i].sel.as_mut().unwrap().state = state;
            if s.transition != 1 {
                continue;
            }
            let c = &s.colors;
            let col = match state {
                1 => c.highlighted,
                2 => c.pressed,
                3 => c.selected,
                4 => c.disabled,
                _ => c.normal,
            } * c.multiplier;
            if let Some(t) = s.target {
                if let Some(g) = self.nodes[t].graphic.as_mut() {
                    g.cross_fade(col, if instant { 0.0 } else { c.fade }, false);
                }
            }
        }
    }

    /// Resets the transition state of selectables that became inactive (OnDisable -> InstantClearState).
    fn clear_inactive(&mut self) {
        for i in 0..self.nodes.len() {
            if self.nodes[i].on {
                continue;
            }
            if let Some(s) = self.nodes[i].sel.as_mut() {
                if s.state >= 0 || s.hover || s.down {
                    s.hover = false;
                    s.down = false;
                    s.state = -1;
                }
            }
        }
        if let Some(sel) = self.selected {
            if !self.nodes[sel].on {
                self.select(None);
            }
        }
        self.hovered.retain(|&h| self.nodes[h].on);
    }

    fn has_click(&self, n: usize) -> bool {
        matches!(self.nodes[n].widget, Widget::Button { .. } | Widget::Toggle { .. } | Widget::Dropdown { .. })
    }

    fn has_down(&self, n: usize) -> bool {
        self.nodes[n].sel.is_some() || self.nodes[n].scripts.iter().any(|s| s.3 && matches!(s.1.as_str(), "ButtonSizeTween" | "ModelButtonHandler"))
    }

    fn has_drag(&self, n: usize) -> bool {
        matches!(self.nodes[n].widget, Widget::Slider { .. } | Widget::Scrollbar { .. }) || self.nodes[n].scroll.is_some()
    }

    fn handler(&self, from: usize, pred: impl Fn(usize) -> bool) -> Option<usize> {
        self.ancestors(from).into_iter().find(|&n| pred(n))
    }

    /// GraphicRaycaster: topmost raycastTarget graphic under `p` (canvas units).
    pub fn raycast(&self, p: Vec2) -> Option<usize> {
        let mut best: Option<(u32, usize)> = None;
        for (i, node) in self.nodes.iter().enumerate() {
            let Some(g) = &node.graphic else { continue };
            if !node.on || !g.enabled || !g.raycast || !node.blocks {
                continue;
            }
            if let Some(c) = node.clip {
                if !c.contains(p) {
                    continue;
                }
            }
            let inv = node.world.inverse();
            let lp = inv.transform_point3(p.extend(0.0)).truncate();
            if !node.rect.contains(lp) {
                continue;
            }
            if best.map(|b| node.order > b.0).unwrap_or(true) {
                best = Some((node.order, i));
            }
        }
        best.map(|b| b.1)
    }

    fn local_point(&self, n: usize, p: Vec2) -> Vec2 {
        self.nodes[n].world.inverse().transform_point3(p.extend(0.0)).truncate()
    }

    /// Slider.UpdateDrag.
    fn slider_drag(&mut self, n: usize, p: Vec2) {
        let Widget::Slider { dir, fill, handle, offset, min, max, .. } = self.nodes[n].widget.clone() else { return };
        let container = handle.and_then(|h| self.nodes[h].parent).or(fill.and_then(|f| self.nodes[f].parent));
        let Some(c) = container else { return };
        let axis = if dir >= 2 { 1 } else { 0 };
        let r = self.nodes[c].rect;
        if r.size()[axis] <= 0.0 {
            return;
        }
        let lp = self.local_point(c, p) - r.min;
        let mut v = ((lp - offset)[axis] / r.size()[axis]).clamp(0.0, 1.0);
        if dir == 1 || dir == 3 {
            v = 1.0 - v;
        }
        self.slider_set(n, min + (max - min) * v, true);
    }

    fn scrollbar_visuals(&mut self, n: usize) {
        let Widget::Scrollbar { value, size, dir, handle, .. } = self.nodes[n].widget.clone() else { return };
        let Some(h) = handle else { return };
        let axis = if dir >= 2 { 1 } else { 0 };
        let mut a0 = Vec2::ZERO;
        let mut a1 = Vec2::ONE;
        let move_ = value * (1.0 - size);
        if dir == 1 || dir == 3 {
            a0[axis] = 1.0 - move_ - size;
            a1[axis] = 1.0 - move_;
        } else {
            a0[axis] = move_;
            a1[axis] = move_ + size;
        }
        if let Some(r) = self.nodes[h].rect_t.as_mut() {
            r.amin = a0;
            r.amax = a1;
        }
    }

    /// ScrollRect vertical normalized position (content anchored to the top of the viewport).
    fn scroll_by(&mut self, n: usize, delta: f32) {
        let Some(s) = self.nodes[n].scroll.clone() else { return };
        let (Some(c), Some(v)) = (s.content, s.viewport.or(Some(n))) else { return };
        let ch = self.nodes[c].rect.height();
        let vh = self.nodes[v].rect.height();
        let range = (ch - vh).max(0.0);
        if let Some(r) = self.nodes[c].rect_t.as_mut() {
            r.apos.y = (r.apos.y - delta * s.sensitivity).clamp(0.0, range);
        }
        if let Some(b) = s.vbar {
            let pos = if range > 0.0 { 1.0 - self.nodes[c].rect_t.as_ref().map(|r| r.apos.y).unwrap_or(0.0) / range } else { 1.0 };
            if let Widget::Scrollbar { value, size, .. } = &mut self.nodes[b].widget {
                *value = pos;
                *size = if ch > 0.0 { (vh / ch).clamp(0.0, 1.0) } else { 1.0 };
            }
            self.scrollbar_visuals(b);
        }
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Dropdown list (TMP_Dropdown.Show / Hide)

impl UiTree {
    pub fn dropdown_show(&mut self, n: usize) {
        let Widget::Dropdown { template: Some(tpl), item_text, options, value, list: None, .. } = self.nodes[n].widget.clone() else { return };
        // Template children: Viewport/Content/Item.
        let tparent = self.nodes[tpl].parent.unwrap_or(n);
        let list = self.clone_subtree(tpl, Some(tparent));
        self.nodes[list].name = "Dropdown List".into();
        self.nodes[list].active = true;
        self.nodes[list].override_sort = true;
        self.nodes[list].group = Some((0.0, true, true, false));
        // Find the item template inside the clone (the Toggle whose label is the cloned item text).
        let item_tpl = self.widget_in_children(list, &|w| matches!(w, Widget::Toggle { .. }));
        let Some(item_tpl) = item_tpl else { return };
        let content = self.nodes[item_tpl].parent.unwrap_or(list);
        let label_name = item_text.map(|t| self.nodes[t].name.clone());
        self.nodes[item_tpl].active = false;
        let mut items = vec![];
        for (i, opt) in options.iter().enumerate() {
            let it = self.clone_subtree(item_tpl, Some(content));
            self.nodes[it].active = true;
            self.nodes[it].name = format!("Item {i}: {opt}");
            if let Some(ln) = &label_name {
                if let Some(l) = self.nodes[it].children.clone().into_iter().find(|&c| &self.nodes[c].name == ln) {
                    self.set_text(l, opt);
                }
            }
            self.toggle_set(it, i as i32 == value, false, true);
            // Item toggles are not wired to Settings: clicking them selects the option (handled in on_click).
            if let Widget::Toggle { calls, .. } = &mut self.nodes[it].widget {
                calls.clear();
            }
            items.push(it);
        }
        // Reposition the items now that all of them have been added.
        self.layout();
        let item_rect = self.nodes[item_tpl].rect;
        let content_rect = self.nodes[content].rect;
        let ilp = self.local_position(item_tpl).truncate();
        let offset_min = item_rect.min - content_rect.min + ilp;
        let offset_max = item_rect.max - content_rect.max + ilp;
        let item_size = item_rect.size();
        if let Some(r) = self.nodes[content].rect_t.as_mut() {
            r.size_delta.y = item_size.y * items.len() as f32 + offset_min.y - offset_max.y;
        }
        self.layout();
        let extra = self.nodes[list].rect.height() - self.nodes[content].rect.height();
        if extra > 0.0 {
            if let Some(r) = self.nodes[list].rect_t.as_mut() {
                r.size_delta.y -= extra;
            }
        }
        let count = items.len();
        for (i, &it) in items.iter().enumerate() {
            if let Some(r) = self.nodes[it].rect_t.as_mut() {
                r.amin.y = 0.0;
                r.amax.y = 0.0;
                r.apos.y = offset_min.y + item_size.y * (count - 1 - i) as f32 + item_size.y * r.pivot.y;
                r.size_delta.y = item_size.y;
            }
        }
        // Blocker: full-screen transparent image just below the list (sorting order 29999), closes on click.
        let blocker = self.nodes.len();
        let mut b = self.nodes[list].clone();
        b.name = "Blocker".into();
        b.children = vec![];
        b.parent = None;
        b.rect_t = Some(RectT { amin: Vec2::ZERO, amax: Vec2::ONE, apos: Vec2::ZERO, size_delta: Vec2::ZERO, pivot: Vec2::splat(0.5) });
        b.pos = Vec3::ZERO;
        b.rot = Quat::IDENTITY;
        b.scale = Vec3::ONE;
        b.group = None;
        b.sel = None;
        b.widget = Widget::Button { calls: vec![] };
        b.scroll = None;
        b.mask = None;
        b.rect_mask = false;
        b.scripts = vec![];
        b.override_sort = true;
        b.graphic = Some(Graphic { kind: GKind::Image { sprite: None, ty: 0, fill_center: true, preserve_aspect: false, ppu_mult: 1.0, fill: ImgFill::default() }, color: [0.0; 4], raycast: true, enabled: true, tint: Vec4::ONE, tint_tween: None, entity: None, last: 0, mesh: None });
        self.nodes.push(b);
        // The blocker is a root so it covers the whole canvas; it is laid out before the list (lower order).
        let pos = self.roots.len();
        self.roots.insert(pos, blocker);
        // Explicit navigation between consecutive items (prev: down / right = item; item: up / left = prev), then
        // "Select current option": item.toggle.Select() for the item that is on.
        for k in 1..items.len() {
            let (prev, it) = (items[k - 1], items[k]);
            if let Some(s) = self.nodes[prev].sel.as_mut() {
                s.nav_mode = 4;
                s.nav[1] = Some(it);
                s.nav[3] = Some(it);
            }
            if let Some(s) = self.nodes[it].sel.as_mut() {
                s.nav_mode = 4;
                s.nav[0] = Some(prev);
                s.nav[2] = Some(prev);
            }
        }
        if let Some(&cur) = items.get(value.max(0) as usize) {
            self.select(Some(cur));
        }
        if let Widget::Dropdown { list: l, .. } = &mut self.nodes[n].widget {
            *l = Some(DropList { root: list, blocker, items, alpha_t: 0.0 });
        }
    }

    pub fn dropdown_hide(&mut self, n: usize, despawn: &mut Vec<Entity>) {
        let Widget::Dropdown { list: Some(l), .. } = self.nodes[n].widget.clone() else { return };
        self.destroy(l.root, despawn);
        self.destroy(l.blocker, despawn);
        if let Widget::Dropdown { list, .. } = &mut self.nodes[n].widget {
            *list = None;
        }
        // TMP_Dropdown.Hide ends with Select().
        if self.nodes[n].on {
            self.select(Some(n));
        }
    }

    fn dropdown_of_item(&self, item: usize) -> Option<(usize, usize)> {
        for (i, n) in self.nodes.iter().enumerate() {
            if let Widget::Dropdown { list: Some(l), .. } = &n.widget {
                if let Some(k) = l.items.iter().position(|&x| x == item) {
                    return Some((i, k));
                }
                if l.blocker == item {
                    return Some((i, usize::MAX));
                }
            }
        }
        None
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Navigation (Selectable.FindSelectable / FindSelectableOn* / OnMove / OnSubmit / OnCancel and
// InputSystemUIInputModule.ProcessNavigation: moveRepeatDelay 0.5, moveRepeatRate 0.1 from the scene's module)

const MOVE_REPEAT_DELAY: f32 = 0.5;
const MOVE_REPEAT_RATE: f32 = 0.1;

impl MoveDir {
    fn vec(self) -> Vec3 {
        match self {
            MoveDir::Left => Vec3::NEG_X,
            MoveDir::Up => Vec3::Y,
            MoveDir::Right => Vec3::X,
            MoveDir::Down => Vec3::NEG_Y,
        }
    }

    fn horizontal(self) -> bool {
        matches!(self, MoveDir::Left | MoveDir::Right)
    }

    /// Index into `Selectable::nav` (up, down, left, right).
    fn slot(self) -> usize {
        match self {
            MoveDir::Up => 0,
            MoveDir::Down => 1,
            MoveDir::Left => 2,
            MoveDir::Right => 3,
        }
    }
}

impl UiTree {
    /// Selectable.IsInteractable (own flag + CanvasGroups).
    fn sel_interactable(&self, i: usize) -> bool {
        self.nodes[i].sel.as_ref().map(|s| s.interactable).unwrap_or(false) && self.nodes[i].interactable
    }

    fn world_rot(&self, i: usize) -> Quat {
        self.nodes[i].world.to_scale_rotation_translation().1
    }

    /// Selectable.FindSelectable(dir): over s_Selectables (enabled, active in hierarchy), interactable and
    /// navigation != None; score dot / sqrMagnitude from the rect edge point towards `dir`.
    pub fn find_selectable(&self, me: usize, dir: Vec3) -> Option<usize> {
        let dir = dir.normalize_or_zero();
        let s = self.nodes[me].sel.as_ref()?;
        let local = (self.world_rot(me).inverse() * dir).truncate();
        // GetPointOnRectEdge
        let r = self.nodes[me].rect;
        let mut d = local;
        if d != Vec2::ZERO {
            d /= d.x.abs().max(d.y.abs());
        }
        let pos = self.nodes[me].world.transform_point3((r.center() + r.size() * d * 0.5).extend(0.0));
        let wrap = s.nav_wrap && (s.nav_mode == 1 || s.nav_mode == 2);
        let (mut best, mut best_s, mut far, mut far_s) = (None, f32::NEG_INFINITY, None, f32::NEG_INFINITY);
        for i in 0..self.nodes.len() {
            if i == me || !self.nodes[i].on || !self.sel_interactable(i) || self.nodes[i].sel.as_ref().map(|s| s.nav_mode == 0).unwrap_or(true) {
                continue;
            }
            let c = self.nodes[i].world.transform_point3(self.nodes[i].rect.center().extend(0.0));
            let v = c - pos;
            let dot = dir.dot(v);
            if wrap && dot < 0.0 {
                let sc = -dot * v.length_squared();
                if sc > far_s {
                    far_s = sc;
                    far = Some(i);
                }
                continue;
            }
            if dot <= 0.0 {
                continue;
            }
            let sc = dot / v.length_squared();
            if sc > best_s {
                best_s = sc;
                best = Some(i);
            }
        }
        if wrap && best.is_none() { far } else { best }
    }

    /// Selectable.FindSelectableOnLeft / Right / Up / Down (+ the Slider / Scrollbar overrides: Automatic
    /// navigation along their own axis returns null so the move changes the value).
    pub fn find_on(&self, me: usize, d: MoveDir) -> Option<usize> {
        let s = self.nodes[me].sel.as_ref()?;
        let own_axis = match &self.nodes[me].widget {
            Widget::Slider { dir, .. } | Widget::Scrollbar { dir, .. } => (*dir < 2) == d.horizontal(),
            _ => false,
        };
        if s.nav_mode == 3 && own_axis {
            return None;
        }
        if s.nav_mode == 4 {
            return s.nav[d.slot()];
        }
        let bit = if d.horizontal() { 1 } else { 2 };
        if s.nav_mode & bit != 0 {
            self.find_selectable(me, self.world_rot(me) * d.vec())
        } else {
            None
        }
    }

    /// IMoveHandler.OnMove of the selected object.
    fn on_move(&mut self, me: usize, d: MoveDir) {
        if self.nodes[me].sel.is_none() {
            return;
        }
        // Slider.OnMove: along its axis with no neighbour -> Set(value -/+ stepSize).
        if let Widget::Slider { dir, whole, min, max, value, .. } = self.nodes[me].widget.clone() {
            if self.nodes[me].on && self.sel_interactable(me) && (dir < 2) == d.horizontal() && self.find_on(me, d).is_none() {
                let step = if whole { 1.0 } else { (max - min) * 0.1 };
                let reverse = dir == 1 || dir == 3;
                let up = matches!(d, MoveDir::Right | MoveDir::Up);
                self.slider_set(me, if up != reverse { value + step } else { value - step }, true);
                return;
            }
        }
        // Scrollbar.OnMove changes its value along its axis (no ScrollRect link is driven by it here).
        if let Widget::Scrollbar { dir, .. } = self.nodes[me].widget {
            if (dir < 2) == d.horizontal() && self.find_on(me, d).is_none() {
                return;
            }
        }
        // Selectable.OnMove -> Navigate (target must be active).
        if let Some(t) = self.find_on(me, d) {
            if self.nodes[t].on {
                self.select(Some(t));
            }
        }
    }

    /// ISubmitHandler.OnSubmit of the selected object.
    fn on_submit(&mut self, me: usize, despawn: &mut Vec<Entity>) {
        if !self.nodes[me].on || !self.sel_interactable(me) {
            return;
        }
        match self.nodes[me].widget.clone() {
            Widget::Button { .. } => {
                // Press() (onClick) + Pressed state until OnFinishSubmit (fadeDuration, unscaled).
                on_click(self, me, despawn);
                if let Some(s) = self.nodes[me].sel.as_mut() {
                    s.submit_t = s.colors.fade.max(1e-4);
                }
            }
            // Toggle.OnSubmit -> InternalToggle (dropdown items: OnSelectItem, value + Hide).
            Widget::Toggle { .. } => on_click(self, me, despawn),
            // TMP_Dropdown.OnSubmit -> Show.
            Widget::Dropdown { list: None, .. } => {
                self.pointer_events.push((me, PointerKind::Click));
                self.dropdown_show(me);
            }
            // TMP_InputField.OnSubmit -> ActivateInputField.
            Widget::Input { .. } => {
                if let Widget::Input { focused, original, text, caret_t, .. } = &mut self.nodes[me].widget {
                    if !*focused {
                        *focused = true;
                        *original = text.clone();
                        *caret_t = 0.0;
                    }
                }
            }
            _ => {}
        }
    }

    /// ICancelHandler.OnCancel: TMP_Dropdown and its DropdownItems hide the list.
    fn on_cancel(&mut self, me: usize, despawn: &mut Vec<Entity>) {
        if matches!(self.nodes[me].widget, Widget::Dropdown { .. }) {
            self.dropdown_hide(me, despawn);
        } else if let Some((dd, _)) = self.dropdown_of_item(me) {
            self.dropdown_hide(dd, despawn);
        }
    }

    /// InputSystemUIInputModule.ProcessNavigation (move with repeat, then cancel / submit).
    fn process_navigation(&mut self, now: f32, despawn: &mut Vec<Entity>) {
        let input = self.nav_in;
        let Some(sel) = self.selected.filter(|_| !self.nav_block) else {
            self.nav_state.1 = 0;
            return;
        };
        // OnUpdateSelected: a focused input field uses the event (no navigation / submit).
        if matches!(self.nodes[sel].widget, Widget::Input { focused: true, .. }) {
            self.nav_state.1 = 0;
            return;
        }
        let mv = input.mv;
        if mv.x.abs() > 1e-6 || mv.y.abs() > 1e-6 {
            let d = if mv.x.abs() > mv.y.abs() {
                if mv.x > 0.0 { MoveDir::Right } else { MoveDir::Left }
            } else if mv.y > 0.0 {
                MoveDir::Up
            } else {
                MoveDir::Down
            };
            if Some(d) != self.nav_state.0 {
                self.nav_state.1 = 0;
            }
            let (count, last) = (self.nav_state.1, self.nav_state.2);
            let allow = match count {
                0 => true,
                1 => now > last + MOVE_REPEAT_DELAY,
                _ => now > last + MOVE_REPEAT_RATE,
            };
            if allow {
                self.on_move(sel, d);
                self.nav_state = (Some(d), count + 1, now);
            }
        } else {
            self.nav_state.1 = 0;
        }
        if let Some(cur) = self.selected {
            if input.cancel {
                self.on_cancel(cur, despawn);
            }
        }
        if let Some(cur) = self.selected {
            if input.submit {
                self.on_submit(cur, despawn);
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Event system

#[allow(clippy::too_many_arguments)]
pub fn ugui_input(
    mut tree: ResMut<UiTree>,
    mut rend: ResMut<UguiRender>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    mut keys: MessageReader<KeyboardInput>,
    time: Res<Time>,
    mut calls_out: MessageWriter<UiCall>,
    mut ptr_out: MessageWriter<UiPointer>,
    sim: Option<Res<SimPointer>>,
    pad: Option<Res<crate::input::PlayerInput>>,
    kb: Option<Res<ButtonInput<KeyCode>>>,
) {
    let dt = time.delta_secs();
    let tree = &mut *tree;
    if let Some(pad) = pad {
        tree.gamepad = pad.gamepad;
        tree.nav_in = NavInput { mv: pad.pad_nav, submit: pad.pad_submit, cancel: pad.pad_cancel };
    }
    // Keyboard part of the UI map: Navigate "Keyboard" 2DVector (up w|upArrow, down s|downArrow, left a|leftArrow,
    // right d|rightArrow, DigitalNormalized; pass-through: taken over the gamepad value while actuated), Cancel
    // */{Cancel} = escape. (Submit is bound to <Gamepad>/buttonSouth only.)
    if let Some(k) = kb {
        let h = |a: KeyCode, b: KeyCode| (k.pressed(a) || k.pressed(b)) as i32 as f32;
        let v = Vec2::new(h(KeyCode::KeyD, KeyCode::ArrowRight) - h(KeyCode::KeyA, KeyCode::ArrowLeft), h(KeyCode::KeyW, KeyCode::ArrowUp) - h(KeyCode::KeyS, KeyCode::ArrowDown));
        if v != Vec2::ZERO {
            tree.nav_in.mv = v.normalize();
        }
        tree.nav_in.cancel |= k.just_pressed(KeyCode::Escape);
    }
    // Tweens (DOTween, scaled time).
    let mut done = vec![];
    for (k, t) in tree.tweens.iter_mut().enumerate() {
        t.t += dt;
        if t.t >= t.dur {
            done.push(k);
        }
    }
    for k in 0..tree.tweens.len() {
        let t = tree.tweens[k].clone();
        let e = t.ease.eval((t.t / t.dur).clamp(0.0, 1.0));
        tree.apply_prop(t.node, t.prop, t.from.lerp(t.to, e));
    }
    for k in done.into_iter().rev() {
        tree.tweens.remove(k);
    }
    // CanvasRenderer colour tweens (unscaled).
    for n in &mut tree.nodes {
        if let Some(g) = n.graphic.as_mut() {
            if let Some((from, to, t, d, _)) = g.tint_tween.as_mut() {
                *t += dt;
                let k = (*t / *d).clamp(0.0, 1.0);
                g.tint = from.lerp(*to, k);
                if k >= 1.0 {
                    g.tint_tween = None;
                }
            }
        }
    }
    // Dropdown list fade-in.
    for i in 0..tree.nodes.len() {
        if let Widget::Dropdown { list: Some(l), fade, .. } = &mut tree.nodes[i].widget {
            l.alpha_t += dt;
            let a = if *fade > 0.0 { (l.alpha_t / *fade).min(1.0) } else { 1.0 };
            let root = l.root;
            if let Some(gr) = tree.nodes[root].group.as_mut() {
                gr.0 = a;
            }
        }
    }
    tree.layout();
    tree.clear_inactive();
    if !tree.any_on {
        tree.update_selectables();
        flush(tree, &mut calls_out, &mut ptr_out);
        return;
    }
    // Pointer position in canvas units.
    let (cursor, down, up, wheel) = match sim.as_deref() {
        Some(s) if s.active => (s.pos, s.down, s.up, s.wheel),
        _ => {
            let w = windows.single().ok();
            let c = w.and_then(|w| w.cursor_position().map(|p| (p, Vec2::new(w.width(), w.height()))));
            let pos = c.map(|(p, size)| {
                let s = canvas_scale(size);
                Vec2::new(p.x - size.x * 0.5, size.y * 0.5 - p.y) / s
            });
            (pos, mouse.just_pressed(MouseButton::Left), mouse.just_released(MouseButton::Left), scroll.delta.y)
        }
    };
    tree.pointer = cursor;
    let hit = cursor.and_then(|p| tree.raycast(p));
    tree.raycast_hit = hit;
    // Enter / exit along the hierarchy.
    let chain = hit.map(|h| tree.ancestors(h)).unwrap_or_default();
    let old = std::mem::take(&mut tree.hovered);
    for &o in old.iter().filter(|o| !chain.contains(o)) {
        if let Some(s) = tree.nodes[o].sel.as_mut() {
            s.hover = false;
        }
        tree.pointer_events.push((o, PointerKind::Exit));
    }
    for &c in chain.iter().filter(|c| !old.contains(c)) {
        if let Some(s) = tree.nodes[c].sel.as_mut() {
            s.hover = true;
        }
        tree.pointer_events.push((c, PointerKind::Enter));
    }
    tree.hovered = chain.clone();
    // Press.
    if down {
        if let Some(h) = hit {
            let press = tree.handler(h, |n| tree.has_down(n)).or_else(|| tree.handler(h, |n| tree.has_click(n)));
            // DeselectIfSelectionChanged + Selectable.OnPointerDown (SetSelectedGameObject).
            let sel = tree.handler(h, |n| tree.nodes[n].sel.is_some());
            let new_sel = sel.filter(|&s| tree.nodes[s].sel.as_ref().map(|x| x.interactable).unwrap_or(false) && tree.nodes[s].interactable);
            if new_sel != tree.selected {
                tree.select(None);
            }
            if let Some(p) = press {
                tree.pressed = Some(p);
                if let Some(s) = tree.nodes[p].sel.as_mut() {
                    if s.interactable {
                        s.down = true;
                    }
                }
                if new_sel == Some(p) {
                    tree.select(Some(p));
                }
                tree.pointer_events.push((p, PointerKind::Down));
                // Slider.OnPointerDown: offset when clicking the handle, else jump.
                if let Widget::Slider { handle, .. } = tree.nodes[p].widget.clone() {
                    if tree.nodes[p].sel.as_ref().map(|s| s.interactable).unwrap_or(false) && tree.nodes[p].interactable {
                        let pt = cursor.unwrap_or_default();
                        let on_handle = handle.map(|hd| tree.nodes[hd].rect.contains(tree.local_point(hd, pt))).unwrap_or(false);
                        let off = if on_handle { tree.local_point(handle.unwrap(), pt) - tree.nodes[handle.unwrap()].rect.center() } else { Vec2::ZERO };
                        if let Widget::Slider { offset, .. } = &mut tree.nodes[p].widget {
                            *offset = off;
                        }
                        if !on_handle {
                            tree.slider_drag(p, pt);
                        }
                    }
                }
                // TMP_InputField: activate on press.
                if let Widget::Input { focused, original, text, caret_t, .. } = &mut tree.nodes[p].widget {
                    if !*focused {
                        *focused = true;
                        *original = text.clone();
                        *caret_t = 0.0;
                    }
                }
            } else if tree.selected.is_some() && new_sel.is_none() {
                tree.select(None);
            }
            tree.dragging = tree.handler(h, |n| tree.has_drag(n));
        } else {
            tree.select(None);
        }
    }
    // Drag.
    if let (Some(d), Some(p)) = (tree.dragging, cursor) {
        if mouse.pressed(MouseButton::Left) || sim.as_deref().map(|s| s.active && s.held).unwrap_or(false) {
            if matches!(tree.nodes[d].widget, Widget::Slider { .. }) && tree.pressed == Some(d) {
                tree.slider_drag(d, p);
            }
        }
    }
    // Release / click.
    if up {
        if let Some(p) = tree.pressed.take() {
            if let Some(s) = tree.nodes[p].sel.as_mut() {
                s.down = false;
            }
            tree.pointer_events.push((p, PointerKind::Up));
            let click = hit.and_then(|h| tree.handler(h, |n| tree.has_click(n)).or_else(|| tree.handler(h, |n| tree.has_down(n))));
            if click == Some(p) {
                on_click(tree, p, &mut rend.despawn);
            }
        }
        tree.dragging = None;
    }
    // Scroll wheel.
    if wheel != 0.0 {
        if let Some(h) = hit {
            if let Some(s) = tree.handler(h, |n| tree.nodes[n].scroll.is_some()) {
                tree.scroll_by(s, wheel * 20.0);
            }
        }
    }
    // Keyboard for a focused input field.
    let focused = tree.selected.filter(|&s| matches!(tree.nodes[s].widget, Widget::Input { focused: true, .. }));
    for ev in keys.read() {
        let Some(f) = focused else { continue };
        if !ev.state.is_pressed() {
            continue;
        }
        let Widget::Input { text, .. } = tree.nodes[f].widget.clone() else { continue };
        match &ev.logical_key {
            Key::Enter => {
                tree.input_deactivate(f);
                tree.select(None);
            }
            Key::Escape => {
                if let Widget::Input { original, .. } = tree.nodes[f].widget.clone() {
                    tree.input_set(f, &original, false);
                }
                tree.input_deactivate(f);
            }
            Key::Backspace => {
                let mut t = text.clone();
                t.pop();
                tree.input_set(f, &t, true);
            }
            Key::Character(c) => {
                let t = format!("{text}{c}");
                tree.input_set(f, &t, true);
            }
            _ => {}
        }
    }
    for i in 0..tree.nodes.len() {
        if let Widget::Input { focused, caret_t, .. } = &mut tree.nodes[i].widget {
            if *focused {
                *caret_t += dt;
            }
        }
        if let Some(s) = tree.nodes[i].sel.as_mut() {
            s.submit_t = (s.submit_t - dt).max(0.0);
        }
    }
    // Gamepad UI actions (Navigate / Submit / Cancel).
    tree.process_navigation(time.elapsed_secs(), &mut rend.despawn);
    tree.update_selectables();
    flush(tree, &mut calls_out, &mut ptr_out);
}

fn flush(tree: &mut UiTree, calls_out: &mut MessageWriter<UiCall>, ptr_out: &mut MessageWriter<UiPointer>) {
    for (n, k) in tree.pointer_events.drain(..) {
        ptr_out.write(UiPointer { node: n, kind: k });
    }
    let calls: Vec<_> = tree.calls.drain(..).collect();
    for (n, c, v) in calls {
        // GameObject.SetActive is executed by the event system itself.
        if c.method == "SetActive" && tree.nodes.get(tree.node(c.target).unwrap_or(usize::MAX)).is_some() && (c.type_name.is_empty() || c.type_name.ends_with("GameObject")) {
            let t = tree.node(c.target).unwrap();
            let v = match (&v, c.mode) {
                (CallValue::Bool(b), 0) => *b,
                _ => c.bool_arg,
            };
            tree.set_active(t, v);
            continue;
        }
        calls_out.write(UiCall { node: n, call: c, value: v });
    }
}

fn on_click(tree: &mut UiTree, p: usize, despawn: &mut Vec<Entity>) {
    if !(tree.nodes[p].sel.as_ref().map(|s| s.interactable).unwrap_or(true) && tree.nodes[p].interactable) {
        return;
    }
    tree.pointer_events.push((p, PointerKind::Click));
    // Dropdown items / blocker.
    if let Some((dd, k)) = tree.dropdown_of_item(p) {
        if k != usize::MAX {
            tree.dropdown_set(dd, k as i32, true);
        }
        tree.dropdown_hide(dd, despawn);
        return;
    }
    match tree.nodes[p].widget.clone() {
        Widget::Button { calls } => tree.queue_calls(p, &calls, CallValue::None),
        Widget::Toggle { is_on, .. } => tree.toggle_set(p, !is_on, true, false),
        Widget::Dropdown { list, .. } => {
            if list.is_some() {
                tree.dropdown_hide(p, despawn);
            } else {
                tree.dropdown_show(p);
            }
        }
        _ => {}
    }
}

/// Simulated pointer for autotests (canvas units).
#[derive(Resource, Default)]
pub struct SimPointer {
    pub active: bool,
    pub pos: Option<Vec2>,
    pub down: bool,
    pub up: bool,
    pub held: bool,
    pub wheel: f32,
}

/// Canvas overlay camera (orthographic, 1 world unit = 1 canvas unit, drawn over the TMP popup / Bevy UI).
fn setup_canvas(mut commands: Commands) {
    commands.spawn((
        Name::new("UguiCanvasCam"),
        UguiCanvasCamera,
        Camera3d::default(),
        Camera { order: 5, clear_color: ClearColorConfig::None, ..default() },
        Projection::Orthographic(OrthographicProjection { scaling_mode: ScalingMode::AutoMin { min_width: REF_RES.x, min_height: REF_RES.y }, near: 0.0, far: 2000.0, ..OrthographicProjection::default_3d() }),
        bevy::core_pipeline::tonemapping::Tonemapping::None,
        Transform::from_xyz(0.0, 0.0, 1000.0),
        RenderLayers::layer(CANVAS_LAYER),
    ));
}

/// Caret of focused input fields (TMP_InputField caret: a quad at the end of the text).
fn input_caret(mut tree: ResMut<UiTree>) {
    for i in 0..tree.nodes.len() {
        let Widget::Input { focused, caret_t, caret_rate, caret_color, caret_width, text_comp, caret, .. } = tree.nodes[i].widget.clone() else { continue };
        let Some(tc) = text_comp else { continue };
        let Some(area) = tree.nodes[tc].parent else { continue };
        let caret_node = match caret {
            Some(c) => c,
            None => {
                // TMP_InputField creates a "Caret" child (first sibling) of the text viewport.
                let mut c = tree.nodes[tc].clone();
                c.name = "Caret".into();
                c.children = vec![];
                c.parent = Some(area);
                c.scripts = vec![];
                c.sel = None;
                c.widget = Widget::None;
                c.graphic = Some(Graphic { kind: GKind::Image { sprite: None, ty: 0, fill_center: true, preserve_aspect: false, ppu_mult: 1.0, fill: ImgFill::default() }, color: caret_color, raycast: false, enabled: true, tint: Vec4::ONE, tint_tween: None, entity: None, last: 0, mesh: None });
                let id = tree.nodes.len();
                tree.nodes.push(c);
                tree.nodes[area].children.insert(0, id);
                if let Widget::Input { caret, .. } = &mut tree.nodes[i].widget {
                    *caret = Some(id);
                }
                id
            }
        };
        let blink = caret_rate <= 0.0 || ((caret_t * caret_rate * 2.0) as i64) % 2 == 0;
        // Caret after the last character, one line high, vertically aligned like the text.
        let text_rect = tree.nodes[tc].rect;
        let (x, h) = match tree.nodes[tc].graphic.as_ref().map(|g| &g.kind) {
            Some(GKind::Text(t)) => {
                let l = TmpUiLayout { font_size: t.size, rect_size: text_rect.size(), pivot: Vec2::splat(0.5), margin: t.margin, h_align: t.h_align, v_align: t.v_align, char_spacing: t.char_spacing, word_spacing: t.word_spacing, line_spacing: 0.0, paragraph_spacing: 0.0, wrap: false, style: t.style, grad_ratio: t.props.grad_ratio() };
                let w = crate::tmp_text::ui_line_width(&t.font, &l, t.text.trim_end_matches('\u{200b}'));
                let sc = crate::tmp_text::ui_font_scale(&t.font, t.size);
                (t.margin.x + w, (t.font.ascent - t.font.descent) * sc)
            }
            _ => (0.0, 10.0),
        };
        let cn = &mut tree.nodes[caret_node];
        cn.active = focused && blink;
        cn.rect_t = Some(RectT { amin: Vec2::new(0.0, 1.0), amax: Vec2::new(0.0, 1.0), apos: Vec2::new(x, 0.0), size_delta: Vec2::new(caret_width, h), pivot: Vec2::new(0.0, 1.0) });
    }
}

pub struct UguiPlugin;

impl Plugin for UguiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((MaterialPlugin::<UiImageMaterial>::default(), MaterialPlugin::<crate::ugui_assets::UnityFxMaterial>::default()))
            .init_resource::<UiTree>()
            .init_resource::<UguiRender>()
            .init_resource::<UnityAssets>()
            .init_resource::<UguiAmbient>()
            .init_resource::<SimPointer>()
            .add_message::<UiCall>()
            .add_message::<UiPointer>()
            .add_systems(Startup, setup_canvas)
            .add_systems(Update, (ugui_input, update_fx_params))
            .add_systems(PostUpdate, (input_caret, ugui_render, ugui_sync_3d, ugui_probe_light).chain().before(bevy::transform::TransformSystems::Propagate));
    }
}

impl UiTree {
    /// A root node standing for a ScreenSpaceOverlay canvas (rect = the scaled screen, origin at its centre).
    pub fn add_canvas_root(&mut self, name: &str) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(UNode {
            go: 0,
            name: name.to_string(),
            parent: None,
            children: vec![],
            active: true,
            rect_t: Some(RectT { amin: Vec2::ZERO, amax: Vec2::ONE, apos: Vec2::ZERO, size_delta: Vec2::ZERO, pivot: Vec2::splat(0.5) }),
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            scale: Vec3::ONE,
            layer: 5,
            rect: Rect::default(),
            world: Mat4::IDENTITY,
            on: false,
            alpha: 1.0,
            interactable: true,
            blocks: true,
            clip: None,
            order: 0,
            graphic: None,
            group: None,
            sel: None,
            widget: Widget::None,
            scroll: None,
            mask: None,
            rect_mask: false,
            layout_group: None,
            scripts: vec![],
            obj3d: None,
            entity: None,
            override_sort: false,
        });
        self.roots.push(idx);
        if self.canvas_size == Vec2::ZERO {
            self.canvas_size = REF_RES;
        }
        idx
    }

    /// OnEnable visuals of the widgets (Toggle.PlayEffect(instant), Slider/Scrollbar UpdateVisuals, dropdown caption,
    /// input field label).
    pub fn init_widgets(&mut self) {
        for i in 0..self.nodes.len() {
            match self.nodes[i].widget.clone() {
                Widget::Toggle { is_on, .. } => self.toggle_set(i, is_on, false, true),
                Widget::Slider { .. } => self.slider_visuals(i),
                Widget::Scrollbar { .. } => self.scrollbar_visuals(i),
                Widget::Dropdown { value, .. } => self.dropdown_set(i, value, false),
                Widget::Input { text, .. } => self.input_set(i, &text, false),
                _ => {}
            }
        }
        self.calls.clear();
    }

    /// Node by '/'-separated path from the roots (first match).
    pub fn find_path(&self, path: &str) -> Option<usize> {
        let mut parts = path.split('/');
        let first = parts.next()?;
        let rest: Vec<&str> = parts.collect();
        for &r in &self.roots {
            for &c in &self.nodes[r].children {
                if self.nodes[c].name == first {
                    if let Some(n) = self.find(c, &rest.join("/")) {
                        return Some(n);
                    }
                }
            }
        }
        None
    }

    /// Canvas-space centre of a node's rect.
    pub fn center(&self, n: usize) -> Vec2 {
        self.nodes[n].world.transform_point3(self.nodes[n].rect.center().extend(0.0)).truncate()
    }
}

/// Canvas scale factor / half screen height for the world-space menu shaders (S_TVScanlines_00 uses world Y).
fn update_fx_params(tree: Res<UiTree>, uassets: Res<UnityAssets>, windows: Query<&Window, With<PrimaryWindow>>, mut fx: ResMut<Assets<crate::ugui_assets::UnityFxMaterial>>) {
    let Ok(w) = windows.single() else { return };
    let half_h = w.physical_height() as f32 * 0.5;
    // Physical pixels = Unity screen pixels; canvas units -> pixels = scale * window scale factor.
    let s = tree.scale * w.scale_factor();
    for h in &uassets.fx_mats {
        let cur = fx.get(h).map(|m| (m.p.kind.y, m.p.kind.z));
        if cur.is_some() && cur != Some((s, half_h)) {
            if let Some(mut m) = fx.get_mut(h) {
                m.p.kind.y = s;
                m.p.kind.z = half_h;
            }
        }
    }
}

#[cfg(test)]
mod image_mesh_tests {
    use super::*;

    /// Area and centroid x of the quads (shoelace), on the unit square.
    fn area(qs: &[Quad]) -> (f32, f32) {
        let (mut a, mut cx) = (0.0, 0.0);
        for q in qs {
            for k in 0..4 {
                let (p, n) = (q.pos[k], q.pos[(k + 1) % 4]);
                let c = p.x * n.y - n.x * p.y;
                a += c;
                cx += (p.x + n.x) * c;
            }
        }
        // BL TL TR BR winding is clockwise: negate.
        (-a / 2.0, if a != 0.0 { cx / (3.0 * a) } else { 0.0 })
    }

    #[test]
    fn filled_areas() {
        let r = Rect::new(0.0, 0.0, 1.0, 1.0);
        let f = |method, amount, clockwise, origin| area(&filled_quads(r, r, &ImgFill { method, amount, clockwise, origin }));
        assert!((f(0, 0.25, true, 0).0 - 0.25).abs() < 1e-5);
        assert!((f(0, 0.25, true, 1).1 - 0.875).abs() < 1e-5);
        assert!((f(1, 0.4, true, 0).0 - 0.4).abs() < 1e-5);
        for m in 2..5 {
            assert!((f(m, 0.5, true, 0).0 - 0.5).abs() < 1e-4, "method {m}");
            assert!((f(m, 1.0, false, 3).0 - 1.0).abs() < 1e-5);
            assert!(f(m, 0.0005, true, 0).0.abs() < 1e-6);
        }
        assert!((f(4, 0.25, true, 0).0 - 0.25).abs() < 1e-4);
        assert!((f(4, 0.75, false, 2).0 - 0.75).abs() < 1e-4);
        // Radial360 from the bottom, clockwise: half fill = the left half; counter-clockwise = the right half.
        assert!((f(4, 0.5, true, 0).1 - 0.25).abs() < 1e-3);
        assert!((f(4, 0.5, false, 0).1 - 0.75).abs() < 1e-3);
        // Radial90 at 45 degrees from any corner: half the square.
        for o in 0..4 {
            assert!((f(2, 0.5, o % 2 == 0, o).0 - 0.5).abs() < 1e-4);
        }
    }
}
