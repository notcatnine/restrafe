//! Weapon VFX: turns `GunFx` messages (muzzle flash, bullet trail, surface/body impacts) into the game's
//! real effect prefabs via [`crate::vfx`]. Prefab choice mirrors `Gun.cs` (`SpawnVFX` surface switch,
//! `RpcLogic___ShootObserversEffect`, `SpawnBulletTrail`); `Settings.reduce_vfx` skips muzzle flashes and
//! surface impacts like `Gun.cs` does (trails and direct blood still play).
//!
//! Explosions: `ExplosionFx` (launcher) / `MineBoom` (mines) placeholder spheres are hidden and replaced by
//! the explosion prefab (`explosion_prefab`).

use crate::player::fpc::PlayerCamera;
use crate::settings::Settings;
use crate::vfx::{SpawnOpts, Vfx, VfxPlugin};
use crate::weapons::data::{UField, WeaponDef};
use crate::weapons::gun::{ChargeOp, ChargeVfx, GunFx};
use crate::weapons::markers::WeaponMarkers;
use crate::vfx::sim::FxRoot;
use crate::weapons::markers::viewmodel_layers;
use crate::weapons::{Hand, Item};
use bevy::prelude::*;
use crate::physics::{self, layer};
use bevy::light::{NotShadowCaster};
use bevy_rapier3d::prelude::ReadRapierContext;
use std::sync::Arc;

const DEFAULT_TRAIL: &str = "GameObject/VFX_BulletTrailLocal_03_Thin.prefab";

/// Prefix of the in-memory `RenderObject` ray lines (PhysicsGrenade / ShrapnelBallistic frag rays), see [`ray_line_def`].
pub const RAY_LINE: &str = "RaycastLine:";

/// `RenderObject(direction, maxDistance)`: `new GameObject("RaycastLine")` + LineRenderer (Sprites/Default, white) +
/// LineFade (colour white, alpha lerp to 0 at `speed`), destroyed after 2.5 s.
///   PhysicsGrenade:    width 0.02 -> 0.01, speed 9, no shrinking.
///   ShrapnelBallistic: width 0.05 -> 0.03, speed lineFadeSpeed (9), decreaseInSize at lineDecreaseSpeed (10).
/// Key: "RaycastLine:grenade" or "RaycastLine:shrapnel:<fadeSpeed>:<decreaseSpeed>".
pub fn ray_line_def(key: &str) -> crate::vfx::def::PrefabDef {
    use crate::vfx::def::{Curve, LineDef, ObjDef, PrefabDef};
    let parts: Vec<&str> = key.split(':').collect();
    let shrapnel = parts.get(1) == Some(&"shrapnel");
    let num = |i: usize, d: f32| parts.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let (w0, w1, speed, shrink) = if shrapnel { (0.05, 0.03, num(2, 9.0), num(3, 10.0)) } else { (0.02, 0.01, 9.0, 0.0) };
    let line = LineDef {
        // Sprites/Default (tex white * vertex colour, alpha blended) ~ Mobile/Particles/Alpha Blended without a texture.
        material: Some("asset-guid:0000000000000000f000000000000000:10721".into()),
        width: w0,
        width_curve: Curve::linear(&[(0.0, 1.0), (1.0, w1 / w0)]),
        gradient: Default::default(),
        apply_cs: false,
        fade: Some((Vec4::ONE, speed)),
        shrink,
        life: 2.5,
    };
    PrefabDef {
        name: "RaycastLine".into(),
        objects: vec![ObjDef { path: "RaycastLine".into(), parent: None, pos: Vec3::ZERO, rot: Quat::IDENTITY, scale: Vec3::ONE, active: true, ps: None, line: Some(line), decal: None, light: None, sound: None, pp: None, vfx_tag: false, layer: 0 }],
    }
}

/// `Quaternion.LookRotation(normal)` in port space (forward = -Z).
pub fn look_rotation(normal: Vec3) -> Quat {
    let n = normal.normalize_or_zero();
    if n == Vec3::ZERO {
        return Quat::IDENTITY;
    }
    let up = if n.y.abs() > 0.999 { Vec3::Z } else { Vec3::Y };
    Transform::IDENTITY.looking_to(n, up).rotation
}

/// (impact prefab field, hole prefab field) for a surface tag (`Gun.SpawnVFX`).
pub fn surface_fields(tag: &str) -> Option<(&'static str, &'static str)> {
    Some(match tag {
        "Footsteps/Concrete/Solide" | "Footsteps/Concrete/PleinAir" | "Footsteps/Graviers" | "Footsteps/Concrete/Default" | "Footsteps/Concrete/Dalles" => ("concreteHitImpact", "bulletHole"),
        "Footsteps/Wood/Creux" | "Footsteps/Wood/Sec" => ("woodHitImpact", "woodHitFx"),
        "Footsteps/Dirt" | "Footsteps/Grass" => ("dirtHitImpact", "dirtHitFx"),
        "Footsteps/Sand" => ("sandHitImpact", "sandHitFx"),
        "Footsteps/Water" => ("waterHitImpact", "waterHitFx"),
        "Footsteps/Metal/Pipe2" => ("metalHitImpact", "metalHitFx"),
        "Footsteps/Metal/Grille" | "Footsteps/Metal/Pipe" => ("tauleHitImpact", "tauleHitFx"),
        "Footsteps/Matelas" | "Footsteps/Moquette" => ("softbodyHitImpact", "softbodyHitFx"),
        "NoSound" | "Mine" | "Grenade" | "Hat" => return None,
        _ => ("concreteHitImpact", "bulletHole"),
    })
}

/// Spawn-an-effect request for code that has `Commands` but no message writer (mines): consumed next frame.
#[derive(Component)]
pub struct VfxRequest {
    pub prefab: Option<String>,
    pub pos: Vec3,
    pub normal: Vec3,
}

fn spawn_requests(mut vfx: Vfx, q: Query<(Entity, &VfxRequest)>) {
    for (e, r) in &q {
        if let Some(p) = &r.prefab {
            vfx.spawn(p, r.pos, look_rotation(r.normal), SpawnOpts::default());
        }
        vfx.commands.entity(e).try_despawn();
    }
}

fn spawn_fx(
    mut vfx: Vfx,
    settings: Res<Settings>,
    mut reader: MessageReader<GunFx>,
    items: Query<(Entity, &Item)>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    gtfs: Query<&GlobalTransform>,
    temp_beams: Query<&ChargeVfx>,
    markers: Query<&WeaponMarkers>,
    roots: Query<&FxRoot>,
    (parts, healths): (Query<&crate::health::HitPart>, Query<&crate::health::PlayerHealth>),
) {
    let held_e: Vec<(Entity, &Item)> = items.iter().filter(|(_, i)| i.hand != Hand::None).collect();
    let held: Vec<&Item> = held_e.iter().map(|(_, i)| *i).collect();
    let mut cur: Option<Arc<WeaponDef>> = None;
    for e in reader.read() {
        match e {
            GunFx::Muzzle { at, prefab } => {
                // The firing weapon is the held item whose muzzleFlash prefab matches.
                let firing = held.iter().find(|i| i.def.w.prefab_ref("muzzleFlash") == *prefab || i.def.w.prefab_ref("muzzleFlash2") == *prefab);
                let flash_rot = firing.and_then(|i| i.shoot_rot);
                cur = firing.map(|i| i.def.clone()).or(cur);
                if settings.reduce_vfx {
                    continue;
                }
                if std::env::var_os("RESTRAFE_FX_LOG").is_some() {
                    println!("FXMUZZLE prefab={prefab:?} firing={:?} class={:?} smoke={:?}", firing.map(|f| &f.def.name), firing.map(|f| &f.def.class), firing.map(|f| f.def.item.s("smokeTrail")));
                }
                let (Some(p), Ok(cam)) = (prefab, cam.single()) else { continue };
                // An object inside the weapon prefab (Instantiate clones it): register that subtree as an effect.
                let p = &match (p.strip_prefix("ref:"), firing) {
                    (Some(path), Some(w)) => {
                        let key = format!("weaponfx:{}:{path}", w.def.name);
                        if !vfx.is_registered(&key) {
                            match crate::vfx::def::PrefabDef::load_sub(&format!("assets/data/weapons/{}.json", w.def.name), Some(path)) {
                                Ok(def) => vfx.register(&key, def),
                                Err(e) => {
                                    warn!("muzzle {key}: {e}");
                                    continue;
                                }
                            }
                        }
                        key
                    }
                    (Some(_), None) => continue,
                    _ => p.clone(),
                };
                // Gun / LargeRaycastGun: behaviour.smokeTrail.Play() unless it is playing (Gun.cs:1081, LRG:1250).
                if let Some((e, w)) = held_e.iter().find(|(_, i)| firing.is_some_and(|f| std::ptr::eq(*f, *i))) {
                    if matches!(w.def.class.as_str(), "Gun" | "LargeRaycastGun") {
                        if let Some(path) = w.def.item.prefab_ref("smokeTrail").and_then(|r| r.strip_prefix("ref:").map(str::to_string)) {
                            vfx.commands.entity(*e).try_insert(crate::weapons::attached_fx::FxPlay { path });
                        }
                    }
                }
                // Gun / LargeRaycastGun / Minigun / Shotgun / DualLauncher / RepulsiveGun (and BeamGun's muzzleFlash2 shot):
                // every Light in the instantiated flash gets the weapon's lightIntensity.
                let light_intensity = firing.and_then(|w| {
                    let all = matches!(w.def.class.as_str(), "Gun" | "LargeRaycastGun" | "Minigun" | "Shotgun" | "DualLauncher" | "RepulsiveGun");
                    let beam2 = w.def.class == "BeamGun" && w.def.w.prefab_ref("muzzleFlash2") == *prefab;
                    (all || beam2).then(|| w.def.w.f("lightIntensity"))
                });
                // Instantiate(muzzleFlash, flashPoint, shootPoint.rotation); owner flashes are on the HeldWeapon layer.
                if let Ok(demo) = std::env::var("RESTRAFE_VFX_DEMO") {
                    // Debug: same flash 1.5 m in front of the camera on the world layer. RESTRAFE_VFX_DEMO=<prefab stem>[,<m>]
                    // spawns that prefab instead, <m> (default 4) m in front (cross-system sorting checks).
                    let mut it = demo.split(',');
                    match it.next().filter(|k| k.len() > 1) {
                        Some(k) => {
                            let d: f32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(4.0);
                            vfx.spawn(k, cam.translation() + cam.forward().as_vec3() * d, Quat::IDENTITY, SpawnOpts::default());
                        }
                        None => {
                            vfx.spawn(p, cam.translation() + cam.forward().as_vec3() * 1.5, cam.rotation(), SpawnOpts::default());
                        }
                    }
                }
                // `behaviour.vfxAttachedOnGun ? base.transform : null` (BlankState, Phoenix, Taser): the flash follows the gun,
                // so its local-space emitters (Phoenix flames) keep pointing where the gun points.
                let attached = held_e
                    .iter()
                    .find(|(_, i)| firing.is_some_and(|f| std::ptr::eq(*f, *i)))
                    .filter(|(_, i)| i.def.item.b("vfxAttachedOnGun"))
                    .and_then(|(e, _)| gtfs.get(*e).ok().map(|g| (*e, g.to_matrix())));
                // Gun / LRG / Minigun / Shotgun / DualLauncher / RepulsiveGun and BeamGun's muzzleFlash2 move every non-Light,
                // non-"vfx" object to layer 8 (HeldWeapon); Taser / BumpGun flashes keep their prefab layers.
                let layer_loop = firing.is_some_and(|w| {
                    matches!(w.def.class.as_str(), "Gun" | "LargeRaycastGun" | "Minigun" | "Shotgun" | "DualLauncher" | "RepulsiveGun")
                        || (w.def.class == "BeamGun" && w.def.w.prefab_ref("muzzleFlash2") == *prefab)
                });
                let layers = if layer_loop || firing.is_none() { Some(viewmodel_layers()) } else { None };
                vfx.spawn(p, *at, flash_rot.unwrap_or(cam.rotation()), SpawnOpts { layers, keep_vfx_tag: layer_loop, parent: attached, light_intensity, ..default() });
            }
            GunFx::Charge { item, op } => {
                let cur_beam = temp_beams.get(*item).ok().map(|c| c.0);
                debug!("CHARGEFX {op:?} beam={cur_beam:?}");
                match op {
                    ChargeOp::Start { prefab, at, rot, to_flash_point } => {
                        if let Some(i) = held.iter().find(|i| i.def.w.asset("muzzleFlash") == *prefab) {
                            cur = Some(i.def.clone());
                        }
                        let Some(p) = prefab else { continue };
                        let parent_e = if *to_flash_point { markers.get(*item).ok().and_then(|m| m.flash_point).unwrap_or(*item) } else { *item };
                        let parent = gtfs.get(parent_e).ok().map(|g| (parent_e, g.to_matrix()));
                        // ShootObserversEffect2: Instantiate(muzzleFlash, .., muzzleFlashPoint), no layer change (prefab layers).
                        if let Some(root) = vfx.spawn(p, *at, *rot, SpawnOpts { parent, ..default() }) {
                            vfx.commands.entity(*item).try_insert(ChargeVfx(root));
                        }
                    }
                    ChargeOp::Detach => {
                        // SetParent(null) keeps the world pose.
                        if let Some(b) = cur_beam {
                            if let Ok(g) = gtfs.get(b) {
                                let tf = Transform::from_matrix(g.to_matrix());
                                vfx.commands.entity(b).try_remove::<ChildOf>().try_insert(tf);
                            }
                        }
                    }
                    ChargeOp::Hide | ChargeOp::Cancel => {
                        if let Some(b) = cur_beam {
                            if let Ok(r) = roots.get(b) {
                                for x in &r.extra {
                                    vfx.commands.entity(*x).try_despawn();
                                }
                            }
                            vfx.commands.entity(b).try_despawn();
                        }
                        if matches!(op, ChargeOp::Cancel) {
                            vfx.commands.entity(*item).try_remove::<ChargeVfx>();
                        }
                    }
                }
            }
            GunFx::Tracer { from, to, prefab } => {
                if from.distance(*to) < 0.01 {
                    continue;
                }
                let p = prefab.as_deref().unwrap_or(DEFAULT_TRAIL);
                if p.starts_with(RAY_LINE) && !vfx.is_registered(p) {
                    vfx.register(p, ray_line_def(p));
                }
                // SpawnBulletTrail: Destroy(obj, 0.4f), BeamGun 1.2f.
                let life = cur.as_ref().filter(|d| d.class == "BeamGun").map(|_| 1.2);
                vfx.spawn(p, *from, Quat::IDENTITY, SpawnOpts { line: Some((*from, *to)), life, ..default() });
            }
            GunFx::Attached { prefab, item, offset, scale } => {
                let (Some(p), Ok(g)) = (prefab, gtfs.get(*item)) else { continue };
                let rot = look_rotation(g.up().as_vec3());
                // Instantiate(grenadeSmoke, .., shootPoint) keeps its prefab layer (no layer change; DualLauncher.cs:133).
                vfx.spawn(p, g.translation() + *offset, rot, SpawnOpts { parent: Some((*item, g.to_matrix())), scale: Some(*scale), ..default() });
            }
            GunFx::SpawnBall { prefab, at, ball } => {
                if let Some(root) = prefab.as_deref().and_then(|p| vfx.spawn(p, *at, Quat::IDENTITY, SpawnOpts::default())) {
                    if *ball != Vec3::ONE {
                        vfx.commands.entity(root).try_insert(ScaleBall(*ball));
                    }
                }
            }
            GunFx::SpawnOn { prefab, at, normal, parent } => {
                // Instantiate(prefab, at, LookRotation(normal), parent): keeps the world pose, follows the parent.
                if settings.reduce_vfx {
                    continue;
                }
                let parent = parent.and_then(|h| gtfs.get(h).ok().map(|g| (h, g.to_matrix())));
                if let Some(p) = prefab {
                    vfx.spawn(p, *at, look_rotation(*normal), SpawnOpts { parent, ..default() });
                }
            }
            GunFx::Spawn { prefab, at, normal } => {
                if prefab.as_deref().is_some_and(|p| p.contains("GrenadeDecal") || p.contains("PF_BloodSplatter")) {
                    continue; // EasyDecal prefabs: handled by `explosion_decals`
                }
                if let Some(p) = prefab.as_deref().filter(|p| p.contains("GameObject/DF_") && p.ends_with("_Zone.prefab")) {
                    // PredictedProjectile.OnHit: Instantiate(objToSpawn, position, identity).transform.up = normal.
                    let rot = Quat::from_rotation_arc(Vec3::Y, normal.normalize_or(Vec3::Y));
                    if let Some(root) = vfx.spawn(p, *at, rot, SpawnOpts { persistent: true, ..default() }) {
                        if let Some(z) = DamageZone::from_prefab(p, *at, rot) {
                            vfx.commands.entity(root).try_insert(z);
                        }
                    }
                    continue;
                }
                if let Some(p) = prefab {
                    vfx.spawn(p, *at, look_rotation(*normal), SpawnOpts::default());
                }
            }
            GunFx::Impact { point, normal, hit, tag } => {
                // `Instantiate(.., parent: hit.transform)`: keeps the world pose but follows the hit object.
                let parent = hit.and_then(|h| gtfs.get(h).ok().map(|g| (h, g.to_matrix())));
                let po = SpawnOpts { parent, ..default() };
                let def = cur.clone().or_else(|| held.iter().find(|i| i.is_clicked || i.shot).or(held.first()).map(|i| i.def.clone()));
                let Some(def) = def else { continue };
                let w = &def.w;
                let rot = look_rotation(*normal);
                let class = def.class.as_str();
                if tag == "Body" || tag == "Head" {
                    if matches!(class, "BeamGun" | "ChargeGun") {
                        // BeamGun.cs:311 / ChargeGun.cs:189: bodyImpact at identity only.
                        if let Some(p) = w.asset("bodyImpact") {
                            vfx.spawn(&p, *point, Quat::IDENTITY, SpawnOpts::default());
                        }
                        continue;
                    }
                    // Gun.cs 259-269 (Shotgun / Minigun / LargeRaycastGun alike): headImpact on head hits (bodyImpact when
                    // the weapon has none), bodyImpact otherwise, + genericBodyImpact. Parented to the hit collider only if
                    // the victim survives the shot.
                    let head = tag == "Head";
                    let survives = hit.and_then(|h| parts.get(h).ok()).and_then(|p| healths.get(p.owner).ok()).is_some_and(|ph| ph.health > 0.0 && !ph.dead);
                    let po = if survives { po.clone() } else { SpawnOpts::default() };
                    match w.asset("headImpact").filter(|_| head) {
                        Some(p) => {
                            vfx.spawn(&p, *point, Quat::IDENTITY, po.clone());
                        }
                        None => {
                            if let Some(p) = w.asset("bodyImpact") {
                                vfx.spawn(&p, *point, rot, SpawnOpts::default());
                            }
                        }
                    }
                    if w.b("playGenericBodyImpactOnBody") || head {
                        if let Some(p) = w.asset("genericBodyImpact") {
                            vfx.spawn(&p, *point, Quat::IDENTITY, po.clone());
                        }
                    }
                    continue;
                }
                if settings.reduce_vfx {
                    continue;
                }
                // SpawnVFXServer(0, ..) + SpawnVFXServer(1, ..) per hit.
                if class == "ChargeGun" {
                    // ChargeGun.SpawnVFX: index 0 = genericImpact, index 1 = the surface's hole, on listed surfaces only.
                    let Some((_, hole)) = surface_fields(tag) else { continue };
                    if let Some(p) = w.asset("genericImpact") {
                        vfx.spawn(&p, *point, rot, po.clone());
                    }
                    if let Some(p) = w.asset(hole) {
                        vfx.spawn(&p, *point, rot, po.clone());
                    }
                    continue;
                }
                // Gun / Shotgun / LargeRaycastGun / Minigun / BeamGun SpawnVFX: genericImpact first (on every call, any
                // tag), then index 0 = surface impact, index 1 = hole.
                if let Some(p) = w.asset("genericImpact") {
                    vfx.spawn(&p, *point, rot, po.clone());
                    vfx.spawn(&p, *point, rot, po.clone());
                }
                let Some((impact, hole)) = surface_fields(tag) else { continue };
                if let Some(p) = w.asset(impact) {
                    vfx.spawn(&p, *point, rot, po.clone());
                }
                if let Some(p) = w.asset(hole) {
                    vfx.spawn(&p, *point, rot, po.clone());
                }
            }
        }
    }
}

/// Pending `transform.Find("ball").localScale = scale` on a just-spawned effect root (its children appear next frame).
#[derive(Component)]
struct ScaleBall(Vec3);

fn scale_ball(mut commands: Commands, q: Query<(Entity, &ScaleBall)>, kids: Query<&Children>, mut nodes: Query<(&Name, &mut Transform)>) {
    for (root, s) in &q {
        // Transform.Find("ball"): a direct child of the instance root.
        let mut done = false;
        for c in kids.get(root).into_iter().flatten() {
            for d in std::iter::once(*c).chain(kids.get(*c).into_iter().flatten().copied()) {
                if let Ok((n, mut tf)) = nodes.get_mut(d) {
                    if n.as_str().rsplit('/').next() == Some("ball") && n.as_str().matches('/').count() == 1 {
                        tf.scale = s.0;
                        done = true;
                    }
                }
            }
        }
        if done || kids.get(root).is_ok() {
            commands.entity(root).remove::<ScaleBall>();
        }
    }
}

pub struct FxPlugin;

impl Plugin for FxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DmgZoneTimer>().add_message::<GunFx>().add_plugins(VfxPlugin).add_systems(Update, (spawn_fx, scale_ball, spawn_requests, explosion_decals, decal_fade, damage_zone_update, damage_zone_player));
    }
}

// ---- EasyDecal "GrenadeDecal" (ch.sycoforge.Decal) explosion scorch marks --------------------------------------------
// Prefab values (ripped GrenadeDecal*.prefab, EasyDecal component): material BurnedGround (T_Burned_Decals_03, tint a .92),
// lifetime 30, fadeinTime 1, fadeoutTime 1, maxDistance 5, projectionDistance 1; root local scale = footprint (x, z).
// `Instantiate(explosionDecal, position, identity)` keeps the prefab scale, so the decal box is centred on the explosion and
// projects (-Y) onto what is below; here: a ray down plus 4 horizontal rays (walls inside the box get a mark too).

const DECAL_TEX: &str = "vfx/tex/T_Burned_Decals_03.png";
const DECAL_LIFETIME: f32 = 30.0;
const DECAL_FADE_IN: f32 = 1.0;
const DECAL_FADE_OUT: f32 = 1.0;
const DECAL_MAX_DISTANCE: f32 = 5.0;
const DECAL_TINT_ALPHA: f32 = 0.921_568_6;

/// Footprint (scale.x, scale.z) of the three GrenadeDecal prefab variants.
fn decal_footprint(prefab: &str) -> Vec2 {
    if prefab.contains("GrenadeDecal 1") {
        Vec2::new(4.853_146, 5.662_965)
    } else if prefab.contains("GrenadeDecal 2") {
        Vec2::new(2.965_709, 3.460_581_3)
    } else {
        Vec2::new(7.620_35, 10.549_068)
    }
}

#[derive(Component)]
pub struct DecalFade {
    age: f32,
    mat: Handle<StandardMaterial>,
}

// ---- EasyDecal "PF_BloodSplatter_00_{S,M,L}" (Weapon.bloodSplatter) -------------------------------------------------
// Prefab: EasyDecal technique 6 (Deferred -> SSDProjector box mesh drawn with the material), source Material (the
// "Grafiti DSSD" atlas is ignored: TransformUV only applies for SourceMode.Atlas), autoDestroy 0 (never fades or is
// destroyed), material BloodSplatter_00 = "Easy Decal/SSD/Unlit Multiply SSD" (recovered from DXBC: blend Zero
// SrcColor, colour = lerp(1, tex * _Tint, tex.a * _Tint.a); the fade alpha only feeds the _Threshold clip (0)).
// _Tint (0.877, 0.070, 0.070, 1) is a Color property (gamma -> linear), texture T_Blood_Splatter_06 (white rgb, alpha
// shape). Root local scale = box size (x, z footprint, y height); spawned with identity rotation (projects -Y).
const BLOOD_TEX: &str = "vfx/tex/T_Blood_Splatter_06.png";
const BLOOD_TINT: [f32; 3] = [0.877_358_5, 0.070_354_23, 0.070_354_23];

#[derive(Component)]
pub struct BloodDecal;

/// Box size (local scale) of an EasyDecal prefab from its extracted JSON.
fn decal_box(prefab: &str) -> Option<Vec3> {
    let stem = prefab.rsplit('/').next()?.trim_end_matches(".prefab");
    let t = crate::owned::data_text(&format!("assets/data/vfx/{stem}.json"))?;
    let v: serde_json::Value = serde_json::from_str(&t).ok()?;
    let s = &v["objects"][0]["components"][0]["fields"]["m_LocalScale"];
    Some(Vec3::new(s["x"].as_f64()? as f32, s["y"].as_f64()? as f32, s["z"].as_f64()? as f32))
}

/// "Easy Decal/SSD/Unlit Multiply SSD" blood splatter (see explosion_decals).
pub fn blood_decal_material(assets: &AssetServer) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::srgb(BLOOD_TINT[0], BLOOD_TINT[1], BLOOD_TINT[2]),
        base_color_texture: Some(assets.load(BLOOD_TEX)),
        unlit: true,
        alpha_mode: AlphaMode::Multiply,
        cull_mode: None,
        depth_bias: 4.0,
        ..default()
    }
}

/// Fading SSD decal (grenade scorch, damage zone); alpha starts at 0 (DecalFade fades it in).
pub fn fade_decal_material(assets: &AssetServer, tex: &'static str) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.0),
        base_color_texture: Some(assets.load(tex)),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        depth_bias: 4.0,
        ..default()
    }
}

/// Decal materials spawned outside the VFX prefabs, for the pipeline warm-up.
pub fn warmup_decal_materials(assets: &AssetServer) -> Vec<StandardMaterial> {
    vec![blood_decal_material(assets), fade_decal_material(assets, DECAL_TEX), fade_decal_material(assets, ZONE_TEX)]
}

fn explosion_decals(
    mut commands: Commands,
    mut reader: MessageReader<GunFx>,
    rapier: ReadRapierContext,
    gtfs: Query<&GlobalTransform>,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let Ok(ctx) = rapier.single() else {
        reader.clear();
        return;
    };
    let mask = (1u32 << layer::DEFAULT) | (1 << layer::SHOOT_THROUGH) | (1 << layer::INTERACT_ENVIRONMENT) | (1 << layer::DONT_COLLIDE_WITH_PLAYER);
    for e in reader.read() {
        let GunFx::Spawn { prefab: Some(prefab), at, normal: fwd } = e else { continue };
        // Instantiate rotation: only its yaw matters for the down-projecting box (Euler(0, r, 0) blood; identity otherwise).
        let yaw = Quat::from_rotation_arc(Vec3::NEG_Z, Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::NEG_Z));
        let blood = prefab.contains("PF_BloodSplatter");
        if !prefab.contains("GrenadeDecal") && !blood {
            continue;
        }
        if std::env::var_os("RESTRAFE_FX_LOG").is_some() {
            println!("FXSPAWN {prefab} at=({:.2},{:.2},{:.2}) fwd=(0.00,0.00,-1.00) parent=false light=None scale=None persistent=false decal=true", at.x, at.y, at.z);
        }
        let (size, down) = if blood {
            let Some(b) = decal_box(prefab) else { continue };
            (Vec2::new(b.x, b.z), b.y * 0.5) // the SSD box reaches half its height below the centre
        } else {
            (decal_footprint(prefab), DECAL_MAX_DISTANCE)
        };
        let reach = DECAL_MAX_DISTANCE.min(size.max_element() * 0.5 + 1.0);
        for dir in [Vec3::NEG_Y, Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z] {
            let max = if dir == Vec3::NEG_Y { down } else { reach.min(if dir.x != 0.0 { size.x } else { size.y } * 0.5) };
            let Some(h) = physics::raycast(&ctx, *at, dir, max, mask) else { continue };
            if dir != Vec3::NEG_Y && h.normal.y.abs() > 0.7 {
                continue; // horizontal rays only mark walls
            }
            let n = h.normal.normalize_or_zero();
            if n == Vec3::ZERO {
                continue;
            }
            let mat = if blood {
                // Bevy Multiply: dst * lerp(1, rgb, a) = the SSD shader's lerp(1, tex * tint, tex.a) with Zero SrcColor.
                mats.add(blood_decal_material(&assets))
            } else {
                mats.add(fade_decal_material(&assets, DECAL_TEX))
            };
            // Quad faces +Z; spin about the normal by world yaw so the footprint stays axis-aligned like the identity-rotated box.
            let rot = Quat::from_rotation_arc(Vec3::Z, n);
            let rot = if dir == Vec3::NEG_Y { yaw * rot } else { rot };
            let world = Transform { translation: h.point + n * 0.01, rotation: rot, scale: Vec3::ONE };
            let mut tf = world;
            let parent = gtfs.get(h.entity).ok();
            if let Some(p) = parent {
                tf = Transform::from_matrix(p.to_matrix().inverse() * world.to_matrix());
            }
            let mut c = commands.spawn((
                Name::new(if blood { "vfx-blood-decal" } else { "vfx-explosion-decal" }),
                // Easy Decal SSD shaders: Queue Transparent+1 (data.unity3d tags).
                crate::transparent_sort::RenderQueue(3001),
                Mesh3d(meshes.add(Rectangle::new(size.x, size.y))),
                MeshMaterial3d(mat.clone()),
                tf,
                NotShadowCaster,
            ));
            if blood {
                c.insert(BloodDecal); // autoDestroy 0: stays
            } else {
                c.insert(DecalFade { age: 0.0, mat });
            }
            if parent.is_some() {
                c.try_insert(ChildOf(h.entity));
            }
        }
    }
}

fn decal_fade(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut DecalFade)>, mut mats: ResMut<Assets<StandardMaterial>>) {
    for (e, mut d) in &mut q {
        d.age += time.delta_secs();
        if d.age >= DECAL_LIFETIME {
            mats.remove(&d.mat);
            commands.entity(e).try_despawn();
            continue;
        }
        let a = (d.age / DECAL_FADE_IN).min(1.0) * ((DECAL_LIFETIME - d.age) / DECAL_FADE_OUT).min(1.0);
        if let Some(mut m) = mats.get_mut(&d.mat) {
            // Unlit Alpha SSD (DXBC data.unity3d PathID 1906 blob 6): alpha = tex.a * _Tint.a * fade * _Tint.a (tint alpha twice).
            m.base_color = Color::srgba(1.0, 1.0, 1.0, DECAL_TINT_ALPHA * DECAL_TINT_ALPHA * a);
        }
    }
}

// ---- DF_Cyst_Zone / DF_Blister_Zone (CystBullet / BlisterBullet objToSpawn) ------------------------------------------
// Prefab: root (scale 9 / 4.5, tag DamageZone) -> child "DF_Cyst_Zone" at local y 0.4 holding DamageZone (damageAmount
// 0.4, damageInterval 0.1; its only logic is StartNewRound -> Destroy), an EasyDecal (technique 6 SSD, material
// ToxicSplatter_00 = "Easy Decal/SSD/Unlit Alpha SSD" with T_Toxic_Splatter_00, source Material so the atlas is ignored,
// autoDestroy 1, lifetime 30, fadein 1, fadeout 1 -> destroys its GameObject = the whole zone child), VFX_toxicvape
// (ParticleSystem, rendered by `vfx`) and "Cylinder" (convex MeshCollider of the builtin cylinder, trigger, tag
// DamageZone, local scale (1, 0.5, 1) -> radius 0.5 s, height s). The damage is FirstPersonController.OnTriggerStay:
// tag DamageZone && isTouchingAnything && dmgZoneTimer < 0 -> (lethal: suicide + DespawnObject) RemoveHealth(
// damageAmount), dmgZoneTimer = damageInterval; Update: dmgZoneTimer -= dt.
// Port: the SSD box projection is drawn as a quad on the hit surface (box footprint s x s, its height covers the
// surface); trigger overlap = player capsule vs the cylinder treated as a capped segment (approximation).

const ZONE_TEX: &str = "vfx/tex/T_Toxic_Splatter_00.png";

#[derive(Component)]
pub struct DamageZone {
    age: f32,
    scale: f32,
    damage: f32,
    interval: f32,
    pos: Vec3,
    rot: Quat,
    decal: Option<(Entity, Handle<StandardMaterial>)>,
}

impl DamageZone {
    fn from_prefab(prefab: &str, pos: Vec3, rot: Quat) -> Option<DamageZone> {
        let stem = prefab.rsplit('/').next()?.trim_end_matches(".prefab");
        let t = crate::owned::data_text(&format!("assets/data/vfx/{stem}.json"))?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        let scale = v["objects"][0]["components"][0]["fields"]["m_LocalScale"]["x"].as_f64()? as f32;
        let dz = v["objects"].as_array()?.iter().flat_map(|o| o["components"].as_array().cloned().unwrap_or_default()).find(|c| c["type"] == "DamageZone")?;
        let f = &dz["fields"];
        Some(DamageZone { age: 0.0, scale, damage: f["damageAmount"].as_f64()? as f32, interval: f["damageInterval"].as_f64()? as f32, pos, rot, decal: None })
    }
}

/// FirstPersonController.dmgZoneTimer.
#[derive(Resource, Default)]
pub struct DmgZoneTimer(f32);

fn damage_zone_update(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut zones: Query<(Entity, &mut DamageZone, &FxRoot, &mut Transform)>,
    rapier: ReadRapierContext,
) {
    let ctx = rapier.single().ok();
    for (e, mut z, root, mut root_tf) in &mut zones {
        z.age += time.delta_secs();
        if z.decal.is_none() {
            // The SSD box (height s) projects onto whatever surface it straddles: find that surface along -up. The
            // port's projectile can end slightly inside the hit collider; then the zone snaps back onto the surface.
            let n = z.rot * Vec3::Y;
            let mask = (1u32 << layer::DEFAULT) | (1 << layer::SHOOT_THROUGH) | (1 << layer::INTERACT_ENVIRONMENT) | (1 << layer::DONT_COLLIDE_WITH_PLAYER);
            if let Some(h) = ctx.as_ref().and_then(|c| physics::raycast(c, z.pos + n, -n, 1.0 + z.scale * 0.5, mask)) {
                if h.distance < 1.0 {
                    root_tf.translation += h.point - z.pos;
                }
                z.pos = h.point;
            }
            let mat = mats.add(fade_decal_material(&assets, ZONE_TEX));
            let tf = Transform { translation: z.pos + n * 0.01, rotation: Quat::from_rotation_arc(Vec3::Z, n), scale: Vec3::ONE };
            let d = commands.spawn((Name::new("vfx-zone-decal"), crate::transparent_sort::RenderQueue(3001), Mesh3d(meshes.add(Rectangle::new(z.scale, z.scale))), MeshMaterial3d(mat.clone()), tf, NotShadowCaster)).id();
            z.decal = Some((d, mat));
        }
        if z.age >= DECAL_LIFETIME {
            // EasyDecal autoDestroy: Destroy(gameObject) (the zone child with the particles and the trigger).
            if let Some((d, m)) = z.decal.take() {
                mats.remove(&m);
                commands.entity(d).try_despawn();
            }
            for x in &root.extra {
                commands.entity(*x).try_despawn();
            }
            commands.entity(e).try_despawn();
            continue;
        }
        let a = (z.age / DECAL_FADE_IN).min(1.0) * ((DECAL_LIFETIME - z.age) / DECAL_FADE_OUT).min(1.0);
        if let Some(mut m) = z.decal.as_ref().and_then(|(_, m)| mats.get_mut(m)) {
            m.base_color = Color::srgba(1.0, 1.0, 1.0, a); // _Tint (1,1,1,1) * fade
        }
    }
}

/// Capsule (segment p0-p1, radius r) vs the zone's trigger: the convex MeshCollider of Unity's built-in cylinder
/// (radius 0.5, height 2) at local scale (1, 0.5, 1) on the child at local y 0.4, all under the root's scale s:
/// a cylinder of radius 0.5 s and height s centred 0.4 s up the zone's axis. Exact distance from sampled capsule axis
/// points to the solid cylinder.
fn capsule_in_zone(p0: Vec3, p1: Vec3, r: f32, z: &DamageZone) -> bool {
    let up = z.rot * Vec3::Y;
    let center = z.pos + up * 0.4 * z.scale;
    let (rad, half) = (0.5 * z.scale, 0.5 * z.scale);
    (0..=16).any(|i| {
        let p = p0.lerp(p1, i as f32 / 16.0) - center;
        let a = p.dot(up);
        let radial = (p - up * a).length();
        let (dr, da) = ((radial - rad).max(0.0), (a.abs() - half).max(0.0));
        dr * dr + da * da < r * r
    })
}

/// Per-entity FirstPersonController.dmgZoneTimer for the training dummy (it stands in for another player, whose own
/// controller runs the same OnTriggerStay).
#[derive(Component, Default)]
pub struct DummyZoneTimer(f32);

fn damage_zone_player(
    mut commands: Commands,
    time: Res<Time>,
    mut timer: ResMut<DmgZoneTimer>,
    zones: Query<&DamageZone>,
    mut players: Query<(&Transform, &crate::player::fpc::Fpc, &mut crate::health::PlayerHealth)>,
    mut dummies: Query<
        (Entity, &Transform, &crate::health::DummyMotor, &mut crate::health::PlayerHealth, Option<&mut DummyZoneTimer>),
        (With<crate::health::Dummy>, Without<crate::player::fpc::Fpc>),
    >,
) {
    let dt = time.delta_secs();
    // FirstPersonController.OnTriggerStay: tag DamageZone && isTouchingAnything && dmgZoneTimer < 0 ->
    // (lethal: suicide) RemoveHealth(damageAmount), dmgZoneTimer = damageInterval. Update: dmgZoneTimer -= dt.
    let hit = |p0: Vec3, p1: Vec3, r: f32, t: &mut f32, ph: &mut crate::health::PlayerHealth| {
        for z in &zones {
            if *t >= 0.0 {
                return;
            }
            if capsule_in_zone(p0, p1, r, z) {
                if ph.health - z.damage <= 0.0 {
                    ph.suicide = true;
                    ph.killed = true; // FirstPersonController.DespawnObject
                    ph.health = -8.0;
                } else {
                    ph.health -= z.damage;
                }
                *t = z.interval;
                debug!("DMGZONE hp={}", ph.health);
            }
        }
    };
    timer.0 -= dt;
    if let Ok((tf, fpc, mut ph)) = players.single_mut() {
        if fpc.touched_last_move && ph.health > 0.0 {
            let half = (fpc.cc_height * 0.5 - fpc.p.cc_radius).max(0.0);
            let c = tf.translation + tf.rotation * fpc.cc_center;
            hit(c - Vec3::Y * half, c + Vec3::Y * half, fpc.p.cc_radius, &mut timer.0, &mut ph);
        }
    }
    for (e, tf, motor, mut ph, zt) in &mut dummies {
        let Some(mut zt) = zt else {
            commands.entity(e).insert(DummyZoneTimer::default());
            continue;
        };
        zt.0 -= dt;
        if !motor.grounded || ph.health <= 0.0 || ph.dead {
            continue;
        }
        // Dummy CharacterController capsule: centre (0, 1, 0), half height 0.6, radius 0.4.
        let c = tf.translation + Vec3::Y;
        hit(c - Vec3::Y * 0.6, c + Vec3::Y * 0.6, 0.4, &mut zt.0, &mut ph);
    }
}
