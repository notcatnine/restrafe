//! Port convenience (not in the original): "`" opens a weapon menu listing every weapon by the name its pickup
//! popup shows (`ItemBehaviour.weaponName.ToLower()`); clicking one spawns it in front of the camera and takes it
//! through the normal interact flow (`Hands::give`). Replaces the old test-range weapon rack.

use crate::input::PlayerInput;
use crate::player::fpc::PlayerCamera;
use crate::weapons::data::WeaponDb;
use crate::weapons::pickup::Hands;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

/// Open state; `esc_consumed` tells the pause menu that this frame's Escape closed the weapon menu.
#[derive(Resource, Default)]
pub struct WeaponMenu {
    pub open: bool,
    pub esc_consumed: bool,
    /// Closed by a button click: gameplay input comes back once the mouse button is released (the click must not fire
    /// the new weapon: automatic weapons fire on held).
    reenable: bool,
}

#[derive(Component)]
pub struct MenuRoot;

/// Same as clicking that weapon's button (WeaponDb name; used by the `weaponmenu` autotest).
#[derive(Message)]
pub struct GiveWeapon(pub String);

/// A weapon button (WeaponDb name).
#[derive(Component)]
struct WeaponButton(String);

const BG: Color = Color::srgba(0.0, 0.0, 0.0, 0.72);
const BUTTON: Color = Color::srgba(0.12, 0.12, 0.12, 0.9);
const BUTTON_HOVER: Color = Color::srgba(0.25, 0.32, 0.22, 0.95);
const TEXT: Color = Color::srgb(0.78, 0.95, 0.72);

pub struct WeaponMenuPlugin;

impl Plugin for WeaponMenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WeaponMenu>().add_message::<GiveWeapon>().add_systems(Update, (build_menu, toggle, buttons).chain().before(crate::input::gather_input));
    }
}

/// (Re)builds the menu for the window size: laid out for 1280x720 and scaled by min(w / 1280, h / 720), so it keeps
/// its proportions (and its labels fit their buttons) at any resolution.
fn build_menu(mut commands: Commands, db: Res<WeaponDb>, assets: Res<AssetServer>, menu: Res<WeaponMenu>, windows: Query<&Window, With<PrimaryWindow>>, old: Query<Entity, With<MenuRoot>>, mut built: Local<Option<f32>>) {
    let Ok(win) = windows.single() else { return };
    let k = (win.width() / 1280.0).min(win.height() / 720.0).max(0.25);
    if built.is_some_and(|b| (b - k).abs() < 0.005) {
        return;
    }
    *built = Some(k);
    for e in &old {
        commands.entity(e).despawn();
    }
    let px = |v: f32| px(v * k);
    let font: Handle<Font> = assets.load("fonts/centurygothic.ttf");
    let mut names: Vec<(String, String)> = db.defs.iter().map(|d| (d.weapon_name().to_lowercase(), d.name.clone())).collect();
    names.sort();
    commands
        .spawn((
            MenuRoot,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100.0),
                height: percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(BG),
            GlobalZIndex(50),
            if menu.open { Visibility::Inherited } else { Visibility::Hidden },
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("weapons"),
                TextFont { font: font.clone().into(), font_size: FontSize::Px(30.0 * k), ..default() },
                TextColor(TEXT),
                Node { margin: UiRect::bottom(px(14.0)), ..default() },
            ));
            root.spawn(Node {
                width: percent(86.0),
                max_width: px(1250.0),
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                justify_content: JustifyContent::Center,
                column_gap: px(6.0),
                row_gap: px(6.0),
                ..default()
            })
            .with_children(|grid| {
                for (label, name) in names {
                    grid.spawn((
                        Button,
                        WeaponButton(name),
                        Node {
                            width: px(162.0),
                            height: px(34.0),
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::Center,
                            border: UiRect::all(px(1.0)),
                            ..default()
                        },
                        BackgroundColor(BUTTON),
                        BorderColor::all(Color::srgba(0.5, 0.7, 0.45, 0.5)),
                    ))
                    .with_children(|b| {
                        b.spawn((Text::new(label), TextFont { font: font.clone().into(), font_size: FontSize::Px(17.0 * k), ..default() }, TextColor(TEXT), TextLayout { linebreak: bevy::text::LineBreak::NoWrap, ..default() }));
                    });
                }
            });
            root.spawn((
                Text::new("[`] close"),
                TextFont { font: font.into(), font_size: FontSize::Px(15.0 * k), ..default() },
                TextColor(TEXT.with_alpha(0.6)),
                Node { margin: UiRect::top(px(14.0)), ..default() },
            ));
        });
}

#[allow(clippy::too_many_arguments)]
pub fn toggle(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut menu: ResMut<WeaponMenu>,
    paused: Res<crate::settings_menu::MenuOpen>,
    cosmetics: Res<crate::cosmetics::CosmeticsMenuOpen>,
    players: Query<&crate::health::PlayerHealth, With<crate::player::fpc::Fpc>>,
    mut root: Query<&mut Visibility, With<MenuRoot>>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut input: ResMut<PlayerInput>,
) {
    menu.esc_consumed = false;
    if menu.reenable && !mouse.any_pressed([MouseButton::Left, MouseButton::Right]) {
        menu.reenable = false;
        if !menu.open && !paused.0 && !cosmetics.0 {
            input.enabled = true;
        }
    } else if menu.reenable {
        input.enabled = false;
    }
    let dead = players.iter().any(|h| h.dead);
    let mut want = menu.open;
    if keys.just_pressed(KeyCode::Backquote) && !paused.0 && !cosmetics.0 && !dead {
        want = !want;
    }
    if keys.just_pressed(KeyCode::Escape) && menu.open {
        want = false;
        menu.esc_consumed = true;
    }
    if dead {
        want = false;
    }
    set_open(&mut menu, want, &mut root, &mut cursor, &mut input);
}

fn set_open(menu: &mut WeaponMenu, want: bool, root: &mut Query<&mut Visibility, With<MenuRoot>>, cursor: &mut Query<&mut CursorOptions, With<PrimaryWindow>>, input: &mut PlayerInput) {
    if want != menu.open {
        menu.open = want;
        for mut v in root.iter_mut() {
            *v = if want { Visibility::Inherited } else { Visibility::Hidden };
        }
        if let Ok(mut c) = cursor.single_mut() {
            c.grab_mode = if want { CursorGrabMode::None } else { CursorGrabMode::Locked };
            c.visible = want;
        }
        input.enabled = !want;
    }
    if menu.open {
        input.enabled = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn buttons(
    mut commands: Commands,
    mut menu: ResMut<WeaponMenu>,
    db: Res<WeaponDb>,
    assets: Res<AssetServer>,
    mut q: Query<(&Interaction, &WeaponButton, &mut BackgroundColor), Changed<Interaction>>,
    cams: Query<&GlobalTransform, With<PlayerCamera>>,
    mut hands: Query<&mut Hands>,
    mut root: Query<&mut Visibility, With<MenuRoot>>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut input: ResMut<PlayerInput>,
    mut gives: MessageReader<GiveWeapon>,
) {
    // GiveWeapon messages (autotests) give directly; button presses only while the menu is open.
    let given = gives.read().last().map(|g| g.0.clone());
    let mut picked = None;
    for (i, b, mut bg) in &mut q {
        match i {
            Interaction::Pressed => picked = Some(b.0.clone()),
            Interaction::Hovered => bg.0 = BUTTON_HOVER,
            Interaction::None => bg.0 = BUTTON,
        }
    }
    let Some(name) = given.or(picked.filter(|_| menu.open)) else { return };
    let (Some(def), Ok(cam), Ok(mut hands)) = (db.get(&name), cams.single(), hands.single_mut()) else { return };
    // Port-only menu: the weapon goes straight into the right hand (pickup.rs `given`), so it never exists as a world
    // collider next to the weapons being dropped.
    let pos = cam.translation();
    let e = crate::weapons::spawn_world_weapon(&mut commands, def, pos, &assets);
    commands.entity(e).insert(bevy_rapier3d::prelude::ColliderDisabled);
    hands.give = Some(e);
    set_open(&mut menu, false, &mut root, &mut cursor, &mut input);
    input.enabled = false;
    menu.reenable = true;
}
