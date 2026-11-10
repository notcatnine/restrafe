//! Port of `Crosshair.cs` (no-gun / stand / sprint crosshair sprites set by ItemBehaviour.OnGrab,
//! scope overlay when aiming) and the hit marker from `Gun.ShootServer` (HitMarker_00: 50x50,
//! DOPunchScale(head ? 2.5 : 1, 0.3, 8, 2), red on headshot, destroyed after 0.3 s, replaced by
//! each new hit). Sprites are read from the game's atlases (rects from the ripped Sprite assets).

use crate::player::fpc::Fpc;
use crate::weapons::WeaponUi;
use bevy::prelude::*;

/// A sprite from the game: atlas texture + pixel rect.
#[derive(Clone)]
pub struct GameSprite {
    pub image: Handle<Image>,
    /// Rect in Bevy image space (top-left origin), pixels.
    pub rect: Rect,
    pub size: Vec2,
    /// Size of the sprite's slicing cell (see `sprite_cell`); a Simple Image maps the whole cell onto its rect.
    pub cell: Vec2,
}

#[derive(Resource, Default)]
pub struct SpriteDb {
    map: std::collections::HashMap<String, Option<GameSprite>>,
}

impl SpriteDb {
    /// `name` like "Straftat_Crosshair02_0" (Sprite asset name). Rect from the installation's Sprite (Unity:
    /// bottom-left origin), the atlas texture of that name (or the name minus its `_N` index).
    pub fn get(&mut self, name: &str, assets: &AssetServer) -> Option<GameSprite> {
        if let Some(s) = self.map.get(name) {
            return s.clone();
        }
        let s = load_sprite(name, assets);
        self.map.insert(name.to_string(), s.clone());
        s
    }
}

fn load_sprite(name: &str, assets: &AssetServer) -> Option<GameSprite> {
    if name == "InvisibleImage" {
        return None;
    }
    let [x, y, w, h] = crate::owned::Owned::global()?.sprite_rect(name)?;
    // Atlas texture = sprite name, or the name minus its trailing _N index.
    // (a file under assets/hud if there is one, else the texture read in place from the installation)
    let size = |t: &str| png_size(&format!("assets/hud/{t}.png")).or_else(|| crate::owned::Owned::global()?.texture_size(t).map(|(w, h)| Vec2::new(w as f32, h as f32)));
    let mut tex = name.to_string();
    let mut dims = size(&tex);
    if dims.is_none() {
        tex = name.rsplit_once('_').map(|(a, _)| a.to_string()).unwrap_or(tex);
        dims = size(&tex);
    }
    let dims = dims?;
    Some(GameSprite {
        image: assets.load(format!("hud/{tex}.png")),
        rect: Rect::new(x, dims.y - y - h, x + w, dims.y - y),
        size: Vec2::new(w, h),
        cell: sprite_cell(Vec2::new(x, y), Vec2::new(w, h), dims),
    })
}

/// The crosshair / hit-marker sheets are grid-sliced (Straftat_Crosshair02/03: 2x2 cells of 512 px,
/// T_Straftat_Crosshair_00_M: 4x4 of 128, Hitmarkers_00: 64 px cells), but the ripped Sprite rects are trimmed to
/// the content (Straftat_Crosshair02_0 = 43.85 px around the cell centre (256, 768); 02_1, whose content reaches the
/// cell border, keeps the full 512). The Crosshair Image (400x400, Simple, preserveAspect 0) and HitMarker_00 (50x50)
/// therefore draw the whole CELL into their rect: the "+" (40 px of its 512 cell) is 400*40/512 = 31.25 canvas units
/// = 32.6 px at 2000x1125, measured 32.7 px in REFERENCE2. A tight rect stretched to 400 would be a 365-unit cross.
/// Cell = the largest power of two whose cell centre is the rect centre and which contains the rect; else the rect.
fn sprite_cell(pos: Vec2, size: Vec2, tex: Vec2) -> Vec2 {
    let centre = pos + size * 0.5;
    let mut c = tex.x.min(tex.y);
    while c >= 16.0 {
        let ok = |v: f32| ((v.rem_euclid(c)) - c * 0.5).abs() < 0.5;
        if ok(centre.x) && ok(centre.y) && size.x <= c + 0.5 && size.y <= c + 0.5 {
            return Vec2::splat(c);
        }
        c *= 0.5;
    }
    size
}

/// PNG width/height from the IHDR chunk.
fn png_size(path: &str) -> Option<Vec2> {
    let b = std::fs::read(path).ok()?;
    if b.len() < 24 {
        return None;
    }
    let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]) as f32;
    let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]) as f32;
    Some(Vec2::new(w, h))
}

#[derive(Component)]
pub struct CrosshairImage;
#[derive(Component)]
pub struct ScopeImage;
#[derive(Component)]
pub struct HitMarker {
    /// Weapon item / projectile / mine that spawned it (each C# instance owns one `marker`).
    source: Option<Entity>,
    life: f32,
    head: bool,
    t: f32,
    /// Sprite rect / slicing cell (the 50x50 Image draws the whole 64 px cell).
    cell_k: Vec2,
}

/// Crosshair.Instance state written by ItemBehaviour.OnGrab (right hand; it persists after a drop).
#[derive(Resource, Default)]
pub struct CrosshairState {
    stand: Option<String>,
    sprint: Option<String>,
    aim: Option<String>,
    can_scope_aim: bool,
    instant_aim_lens: bool,
    last_right: Option<Entity>,
}

/// Crosshair scene values.
const DEFAULT_CROSSHAIR_SIZE: f32 = 400.0;
/// Crosshair.aimCrosshairLimit (scene value 18).
const AIM_CROSSHAIR_LIMIT: f32 = 18.0;
const NOGUN_CROSSHAIR: &str = "Straftat_Crosshair03_0";
/// Crosshair.defaultFixedCrosshair (scene).
const FIXED_CROSSHAIR: &str = "Straftat_Crosshair02_Fixed_0";

pub fn setup_crosshair(mut commands: Commands) {
    commands.spawn((Node { position_type: PositionType::Absolute, ..default() }, ImageNode::default(), Visibility::Hidden, ScopeImage, GlobalZIndex(1)));
    commands.spawn((Node { position_type: PositionType::Absolute, ..default() }, ImageNode::default(), Visibility::Hidden, CrosshairImage, GlobalZIndex(2)));
}

fn sprite_name(asset: Option<String>) -> Option<String> {
    asset.map(|a| a.trim_start_matches("Sprite/").trim_end_matches(".asset").to_string())
}

type CrossQ<'w, 's> = Query<'w, 's, (&'static mut ImageNode, &'static mut Node, &'static mut Visibility), (With<CrosshairImage>, Without<ScopeImage>, Without<HitMarker>)>;
type ScopeQ<'w, 's> = Query<'w, 's, (&'static mut ImageNode, &'static mut Node, &'static mut Visibility), (With<ScopeImage>, Without<CrosshairImage>, Without<HitMarker>)>;
type MarkerQ<'w, 's> = Query<'w, 's, (Entity, &'static mut HitMarker, &'static mut Node), (Without<CrosshairImage>, Without<ScopeImage>)>;

#[allow(clippy::too_many_arguments)]
pub fn update_crosshair(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<AssetServer>,
    settings: Res<crate::settings::Settings>,
    mut db: ResMut<SpriteDb>,
    mut st: ResMut<CrosshairState>,
    players: Query<(&Fpc, Option<&crate::weapons::pickup::Hands>, Option<&crate::health::PlayerHealth>)>,
    items: Query<&crate::weapons::Item>,
    windows: Query<&Window>,
    mut cross: CrossQ,
    mut scope: ScopeQ,
    mut reader: MessageReader<WeaponUi>,
    mut markers: MarkerQ,
    menu: Res<crate::settings_menu::MenuOpen>,
    weapon_menu: Res<crate::weapon_menu::WeaponMenu>,
) {
    use crate::weapons::data::UField;
    let (Ok((fpc, hands, ph)), Ok(win)) = (players.single(), windows.single()) else { return };
    // `if (Camera.main) sprites = invisibleCrosshair`: the scene Main Camera (KillCam) is enabled only while dead.
    let dead = ph.is_some_and(|h| h.dead);
    // CanvasScaler 1920x1080 + CanvasScalerFixer (match width up to 16:9, height beyond) = min(w/1920, h/1080).
    let s = crate::ugui::canvas_scale(Vec2::new(win.width(), win.height()));
    let right = hands.and_then(|h| h.right);
    let has_any = hands.map(|h| h.right.is_some() || h.left.is_some()).unwrap_or(false);
    // ItemBehaviour.OnGrab(owner, rightHand: true) -> Crosshair sprites.
    if right != st.last_right {
        st.last_right = right;
        if let Some(it) = right.and_then(|e| items.get(e).ok()) {
            let ib = &it.def.item;
            st.stand = sprite_name(ib.asset("standCrosshair"));
            st.sprint = sprite_name(ib.asset("sprintCrosshair"));
            st.instant_aim_lens = ib.b("instantAimLens");
            let aim = sprite_name(ib.asset("aimCrosshair"));
            st.can_scope_aim = ib.b("aimWeapon") && aim.is_some();
            if st.can_scope_aim {
                st.aim = aim;
            }
        }
    }
    let normal = settings.normal_fov_value;
    let sprite = if dead {
        None
    } else if !has_any {
        Some(NOGUN_CROSSHAIR.to_string())
    } else if st.can_scope_aim && right.is_some() && st.aim.is_some() && fpc.is_aiming && fpc.p.zoom_fov < normal - AIM_CROSSHAIR_LIMIT {
        None
    } else if fpc.is_sprinting || !fpc.safe_grounded {
        st.sprint.clone()
    } else {
        st.stand.clone()
    };
    // settings.enableFixedCrosshair: fixedCrosshair (CrosshairThing.Awake: defaultFixedCrosshair, 400x400; custom
    // crosshair images from StreamingAssets are not ported) everywhere except the scope-aim invisible case.
    let scope_hidden = st.can_scope_aim && right.is_some() && st.aim.is_some() && fpc.is_aiming && fpc.p.zoom_fov < normal - AIM_CROSSHAIR_LIMIT;
    let sprite = if settings.enable_fixed_crosshair && !dead && !scope_hidden { Some(FIXED_CROSSHAIR.to_string()) } else { sprite };
    let scope_sprite = if dead {
        None
    } else if right.is_some() && st.can_scope_aim && st.aim.is_some() && fpc.is_aiming && (fpc.fov < normal - AIM_CROSSHAIR_LIMIT || st.instant_aim_lens) {
        st.aim.clone()
    } else {
        None
    };
    let screen = Vec2::new(win.width(), win.height());
    // Crosshair RectTransform: 400x400 (Crosshair.defaultCrosshairSize; fixedCrosshairSize when enableFixedCrosshair).
    let cross_box = Vec2::splat(DEFAULT_CROSSHAIR_SIZE * s);
    // Image Simple, preserveAspect 0: the sprite (its slicing cell, see `sprite_cell`) is stretched over the rect.
    let mut apply = |img: &mut ImageNode, node: &mut Node, vis: &mut Visibility, name: Option<String>, box_px: Vec2, hidden_by_setting: bool| match name.and_then(|n| db.get(&n, &assets)) {
        Some(sp) if !hidden_by_setting => {
            img.image = sp.image;
            img.rect = Some(sp.rect);
            let size = sp.size * box_px / sp.cell;
            node.width = px(size.x);
            node.height = px(size.y);
            node.left = px((win.width() - size.x) * 0.5);
            node.top = px((win.height() - size.y) * 0.5);
            *vis = Visibility::Inherited;
        }
        _ => *vis = Visibility::Hidden,
    };
    if let Ok((mut img, mut node, mut vis)) = cross.single_mut() {
        // image.enabled = !settings.disableCrosshair (the scope image is unaffected).
        // PauseManager.Update: Crosshair.image.enabled = !pauseMenu.activeSelf (and Crosshair.Update: !disableCrosshair).
        apply(&mut img, &mut node, &mut vis, sprite, cross_box, settings.disable_crosshair || menu.0 || weapon_menu.open);
    }
    if let Ok((mut img, mut node, mut vis)) = scope.single_mut() {
        // ScopeCrosshair: anchors 0..1 (full screen), Simple, no preserveAspect -> stretched over the screen.
        apply(&mut img, &mut node, &mut vis, scope_sprite, screen, false);
    }
    // Hit markers: Gun.cs:270-292 etc. -- Destroy(marker) only replaces the same instance's own marker, so
    // markers from different sources (dual-wielded guns, projectiles, mines) coexist.
    for m in reader.read() {
        if let WeaponUi::HitMarker { head, source } = m {
            for (e, hm, _) in &markers {
                if hm.source == *source {
                    commands.entity(e).despawn();
                }
            }
            if let Some(sp) = db.get("Hitmarkers_00_3", &assets) {
                let cell_k = sp.size / sp.cell;
                commands.spawn((
                    Node { position_type: PositionType::Absolute, width: px(0.0), height: px(0.0), ..default() },
                    ImageNode { image: sp.image, rect: Some(sp.rect), color: if *head { Color::srgb(1.0, 0.0, 0.0) } else { Color::WHITE }, ..default() },
                    HitMarker { source: *source, life: 0.3, head: *head, t: 0.0, cell_k },
                    GlobalZIndex(3),
                ));
            }
        }
    }
    let dt = time.delta_secs();
    for (e, mut hm, mut node) in &mut markers {
        hm.t += dt;
        hm.life -= dt;
        if hm.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        let punch = if hm.head { 2.5 } else { 1.0 };
        let (tos, durs) = crate::tween::punch_keyframes(Vec3::splat(punch), 0.3, 8, 2.0);
        let path = crate::tween::KeyPath { tos, durs, ease: crate::tween::Ease::OutQuad };
        let sc = path.eval(Vec3::ONE, hm.t).x.max(0.0);
        let size = hm.cell_k * 50.0 * s * sc;
        node.width = px(size.x);
        node.height = px(size.y);
        node.left = px((win.width() - size.x) * 0.5);
        node.top = px((win.height() - size.y) * 0.5);
    }
}

pub struct CrosshairPlugin;

impl Plugin for CrosshairPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SpriteDb>()
            .init_resource::<CrosshairState>()
            .add_systems(Startup, setup_crosshair)
            .add_systems(PostUpdate, update_crosshair);
    }
}
