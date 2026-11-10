//! Unity ParticleSystem / LineRenderer / decal prefab data (JSON from `tools/asset_manifest.tsv`
//! `extract` lines into `assets/data/vfx/`) parsed into a small, simulation-friendly form.
//!
//! Unity space is converted to the port's Z-mirrored space at parse time (`u2p`): positions and
//! directions get `z -> -z`, quaternions `(x, y, z, w) -> (-x, -y, z, w)`.

use bevy::prelude::*;
use serde_json::Value;

pub fn f(v: &Value, k: &str) -> f32 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32
}
pub fn i(v: &Value, k: &str) -> i32 {
    v.get(k).and_then(|x| x.as_i64().or_else(|| x.as_bool().map(|b| b as i64))).unwrap_or(0) as i32
}
fn raw3(v: Option<&Value>) -> Vec3 {
    match v {
        Some(v) => Vec3::new(f(v, "x"), f(v, "y"), f(v, "z")),
        None => Vec3::ZERO,
    }
}
/// Unity vector -> port vector.
pub fn u2p(v: Vec3) -> Vec3 {
    Vec3::new(v.x, v.y, -v.z)
}
fn quat_u2p(v: Option<&Value>) -> Quat {
    match v {
        Some(v) => Quat::from_xyzw(-f(v, "x"), -f(v, "y"), f(v, "z"), f(v, "w")).normalize(),
        None => Quat::IDENTITY,
    }
}

// ---- curves -------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Curve {
    keys: Vec<[f32; 4]>, // time, value, inSlope, outSlope
}

impl Curve {
    pub fn parse(v: &Value) -> Curve {
        let keys = v
            .get("m_Curve")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().map(|k| [f(k, "time"), f(k, "value"), f(k, "inSlope"), f(k, "outSlope")]).collect())
            .unwrap_or_default();
        Curve { keys }
    }
    /// Linear curve through (time, value) points.
    pub fn linear(pts: &[(f32, f32)]) -> Curve {
        let mut keys: Vec<[f32; 4]> = pts.iter().map(|&(t, v)| [t, v, 0.0, 0.0]).collect();
        for n in 0..keys.len() {
            let s_in = if n > 0 { (keys[n][1] - keys[n - 1][1]) / (keys[n][0] - keys[n - 1][0]) } else { 0.0 };
            let s_out = if n + 1 < keys.len() { (keys[n + 1][1] - keys[n][1]) / (keys[n + 1][0] - keys[n][0]) } else { 0.0 };
            keys[n][2] = s_in;
            keys[n][3] = s_out;
        }
        Curve { keys }
    }
    pub fn eval(&self, t: f32) -> f32 {
        let k = &self.keys;
        if k.is_empty() {
            return 0.0;
        }
        if t <= k[0][0] {
            return k[0][1];
        }
        let last = k[k.len() - 1];
        if t >= last[0] {
            return last[1];
        }
        let n = k.iter().position(|x| x[0] > t).unwrap_or(k.len() - 1);
        let (a, b) = (k[n - 1], k[n]);
        let dt = b[0] - a[0];
        if dt <= 1e-8 || !a[3].is_finite() || !b[2].is_finite() || a[3].abs() > 1e30 || b[2].abs() > 1e30 {
            return a[1];
        }
        let s = (t - a[0]) / dt;
        let (s2, s3) = (s * s, s * s * s);
        (2.0 * s3 - 3.0 * s2 + 1.0) * a[1] + (s3 - 2.0 * s2 + s) * a[3] * dt + (-2.0 * s3 + 3.0 * s2) * b[1] + (s3 - s2) * b[2] * dt
    }
}

/// `ParticleSystem.MinMaxCurve`.
#[derive(Clone, Debug)]
pub struct MinMax {
    state: u8,
    scalar: f32,
    min_scalar: f32,
    max: Curve,
    min: Curve,
}

impl Default for MinMax {
    fn default() -> Self {
        MinMax { state: 0, scalar: 0.0, min_scalar: 0.0, max: Curve::default(), min: Curve::default() }
    }
}

impl MinMax {
    fn parse(v: Option<&Value>) -> MinMax {
        let Some(v) = v else { return MinMax::default() };
        MinMax {
            state: i(v, "minMaxState") as u8,
            scalar: f(v, "scalar"),
            min_scalar: f(v, "minScalar"),
            max: v.get("maxCurve").map(Curve::parse).unwrap_or_default(),
            min: v.get("minCurve").map(Curve::parse).unwrap_or_default(),
        }
    }
    /// `t` = normalised curve time, `r` = per-particle random in [0,1).
    pub fn eval(&self, t: f32, r: f32) -> f32 {
        match self.state {
            1 => self.scalar * self.max.eval(t),
            2 => self.scalar * (self.min.eval(t) + (self.max.eval(t) - self.min.eval(t)) * r),
            3 => self.min_scalar + (self.scalar - self.min_scalar) * r,
            _ => self.scalar,
        }
    }
    pub fn is_const_zero(&self) -> bool {
        self.state == 0 && self.scalar == 0.0
    }
}

// ---- gradients ----------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Grad {
    ck: Vec<(f32, Vec3)>,
    ak: Vec<(f32, f32)>,
    fixed: bool,
}

impl Grad {
    fn parse(g: &Value) -> Grad {
        let nc = i(g, "m_NumColorKeys").clamp(0, 8) as usize;
        let na = i(g, "m_NumAlphaKeys").clamp(0, 8) as usize;
        let ck = (0..nc)
            .map(|n| {
                let k = &g[format!("key{n}")];
                (f(g, &format!("ctime{n}")) / 65535.0, Vec3::new(f(k, "r"), f(k, "g"), f(k, "b")))
            })
            .collect();
        let ak = (0..na).map(|n| (f(g, &format!("atime{n}")) / 65535.0, f(&g[format!("key{n}")], "a"))).collect();
        Grad { ck, ak, fixed: i(g, "m_Mode") == 1 }
    }
    /// sRGB rgba.
    pub fn eval(&self, t: f32) -> Vec4 {
        fn lerp_keys<T: Copy>(k: &[(f32, T)], t: f32, fixed: bool, mix: impl Fn(T, T, f32) -> T, default: T) -> T {
            if k.is_empty() {
                return default;
            }
            if t <= k[0].0 {
                return k[0].1;
            }
            for w in k.windows(2) {
                if t <= w[1].0 {
                    if fixed {
                        return w[1].1;
                    }
                    let d = (w[1].0 - w[0].0).max(1e-6);
                    return mix(w[0].1, w[1].1, ((t - w[0].0) / d).clamp(0.0, 1.0));
                }
            }
            k[k.len() - 1].1
        }
        let c = lerp_keys(&self.ck, t, self.fixed, |a: Vec3, b: Vec3, s| a.lerp(b, s), Vec3::ONE);
        let a = lerp_keys(&self.ak, t, self.fixed, |a: f32, b: f32, s| a + (b - a) * s, 1.0);
        c.extend(a)
    }
}

/// `ParticleSystem.MinMaxGradient`.
#[derive(Clone, Debug)]
pub struct MinMaxGrad {
    state: u8,
    max_c: Vec4,
    min_c: Vec4,
    max_g: Grad,
    min_g: Grad,
}

impl Default for MinMaxGrad {
    fn default() -> Self {
        MinMaxGrad { state: 0, max_c: Vec4::ONE, min_c: Vec4::ONE, max_g: Grad::default(), min_g: Grad::default() }
    }
}

fn col4(v: &Value) -> Vec4 {
    Vec4::new(f(v, "r"), f(v, "g"), f(v, "b"), f(v, "a"))
}

impl MinMaxGrad {
    fn parse(v: Option<&Value>) -> MinMaxGrad {
        let Some(v) = v else { return MinMaxGrad::default() };
        MinMaxGrad {
            state: i(v, "minMaxState") as u8,
            max_c: v.get("maxColor").map(col4).unwrap_or(Vec4::ONE),
            min_c: v.get("minColor").map(col4).unwrap_or(Vec4::ONE),
            max_g: v.get("maxGradient").map(Grad::parse).unwrap_or_default(),
            min_g: v.get("minGradient").map(Grad::parse).unwrap_or_default(),
        }
    }
    pub fn eval(&self, t: f32, r: f32) -> Vec4 {
        match self.state {
            1 => self.max_g.eval(t),
            2 => self.min_c.lerp(self.max_c, r),
            3 => self.min_g.eval(t).lerp(self.max_g.eval(t), r),
            4 => self.max_g.eval(r),
            _ => self.max_c,
        }
    }
}

// ---- particle system ----------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Burst {
    pub time: f32,
    pub count: MinMax,
    /// `cycleCount`; 0 = infinite (repeats every `interval` while the system emits).
    pub cycles: i32,
    pub interval: f32,
    pub prob: f32,
}

#[derive(Clone, Debug)]
pub struct ShapeDef {
    pub kind: i32,
    pub angle: f32,
    pub radius: f32,
    pub thickness: f32,
    pub arc: f32,
    pub pos: Vec3,
    pub rot: Quat,
    pub scale: Vec3,
    pub random_dir: f32,
    pub spherical_dir: f32,
}

#[derive(Clone, Debug)]
pub struct Axes {
    pub x: MinMax,
    pub y: MinMax,
    pub z: MinMax,
    pub world: bool,
}

#[derive(Clone, Debug)]
pub struct SizeLife {
    pub x: MinMax,
    pub y: MinMax,
    pub z: MinMax,
    pub separate: bool,
}

#[derive(Clone, Debug)]
pub struct UvDef {
    pub tiles_x: i32,
    pub tiles_y: i32,
    pub frame: MinMax,
    pub start: MinMax,
    pub cycles: f32,
}

#[derive(Clone, Debug)]
pub struct RenderDef {
    pub mode: i32, // 0 billboard, 1 stretched, 2 h-bill, 3 v-bill, 4 mesh
    pub align: i32, // 0 view, 1 world, 2 local, 3 facing, 4 velocity
    pub material: Option<String>,
    pub mesh: Option<String>,
    pub length_scale: f32,
    pub velocity_scale: f32,
    pub pivot: Vec3,
    pub enabled: bool,
    /// `m_ApplyActiveColorSpace`: vertex colours converted to linear (1) or passed raw (0, legacy) in a Linear project.
    pub apply_cs: bool,
    /// `m_MinParticleSize` / `m_MaxParticleSize`: billboard size clamp as a fraction of the viewport (Unity defaults 0 / 0.5).
    pub min_size: f32,
    pub max_size: f32,
    /// `m_SortMode`: 0 None (particle buffer order), 1 By Distance, 2 Oldest in Front, 3 Youngest in Front, 4 By Depth.
    pub sort: i32,
    /// `m_SortingFudge`: added to the camera distance of the renderer's bounds centre in the transparent sort
    /// (lower = drawn later = in front of other transparents).
    pub fudge: f32,
    /// `m_SortingOrder` (all renderers use sorting layer 0): sorted before the distance.
    pub order: i32,
    /// Vertex streams (ParticleSystemVertexStream ids) when `m_UseCustomVertexStreams`, else Unity's default
    /// Position, Normal, Color, UV (0, 1, 3, 4).
    pub streams: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct PsDef {
    pub duration: f32,
    pub looping: bool,
    pub start_delay: MinMax,
    pub sim_world: bool,
    pub scaling: i32,
    pub sim_speed: f32,
    pub start_life: MinMax,
    pub start_speed: MinMax,
    pub start_size: [MinMax; 3],
    pub size3d: bool,
    pub start_rot: [MinMax; 3],
    pub rot3d: bool,
    pub start_color: MinMaxGrad,
    pub gravity: MinMax,
    pub max_particles: usize,
    pub rate: MinMax,
    pub bursts: Vec<Burst>,
    pub shape: Option<ShapeDef>,
    pub vel: Option<Axes>,
    pub speed_mod: Option<MinMax>,
    pub force: Option<Axes>,
    /// Limit Velocity: magnitude, dampen, drag, multiplyDragByParticleSize, multiplyDragByParticleVelocity.
    pub clamp: Option<(MinMax, f32, MinMax, bool, bool)>,
    pub color_life: Option<MinMaxGrad>,
    pub size_life: Option<SizeLife>,
    pub rot_life: Option<MinMax>,
    /// Rotation over Lifetime with separateAxes: X / Y angular velocities (rad/s) for 3D rotation.
    pub rot_life_xy: Option<(MinMax, MinMax)>,
    /// Main `flipRotation` (serialized `randomizeRotationDirection`): fraction of particles whose rotation is mirrored.
    pub flip_rot: f32,
    /// Velocity over Lifetime radial speed (away from the system origin).
    pub radial: Option<MinMax>,
    /// Size by Speed: multipliers (x, y, z as SizeLife) over the speed range (x..y).
    pub size_speed: Option<(SizeLife, Vec2)>,
    /// Rotation by Speed (Z): angular velocity (rad/s) over the speed range (x..y).
    pub rot_speed: Option<(MinMax, Vec2)>,
    pub lights: Option<LightsDef>,
    pub collision: Option<CollisionDef>,
    pub noise: Option<NoiseDef>,
    /// Sub Emitters module entries with a non-null emitter (the target object index is resolved by `PrefabDef::load`).
    pub subs: Vec<SubDef>,
    /// Used as a sub-emitter by another system: never plays on its own.
    pub is_sub: bool,
    pub trail: Option<TrailDef>,
    /// Custom Data module, stream Custom1 (Custom2 has no vertex-stream reader in the data).
    pub custom1: Option<CustomDef>,
    pub uv: Option<UvDef>,
    pub render: RenderDef,
}

fn parse_axes(m: &Value) -> Axes {
    Axes { x: MinMax::parse(m.get("x")), y: MinMax::parse(m.get("y")), z: MinMax::parse(m.get("z")), world: i(m, "inWorldSpace") != 0 }
}

impl PsDef {
    fn parse(ps: &Value, rd: &Value) -> PsDef {
        let enabled = |k: &str| ps.get(k).map(|m| i(m, "enabled") != 0).unwrap_or(false);
        let init = &ps["InitialModule"];
        let em = &ps["EmissionModule"];
        let bursts = if enabled("EmissionModule") {
            em.get("m_Bursts")
                .and_then(|b| b.as_array())
                .map(|a| {
                    a.iter()
                        .map(|b| Burst {
                            time: f(b, "time"),
                            count: MinMax::parse(b.get("countCurve")),
                            cycles: i(b, "cycleCount").max(0),
                            interval: f(b, "repeatInterval"),
                            prob: f(b, "probability"),
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let shape = enabled("ShapeModule").then(|| {
            let s = &ps["ShapeModule"];
            let e = raw3(s.get("m_Rotation"));
            ShapeDef {
                kind: i(s, "type"),
                angle: f(s, "angle"),
                radius: s.get("radius").map(|r| f(r, "value")).unwrap_or(0.0),
                thickness: f(s, "radiusThickness"),
                arc: s.get("arc").map(|r| f(r, "value")).unwrap_or(360.0),
                pos: raw3(s.get("m_Position")),
                // Unity-space euler -> Unity quaternion (kept in Unity space; sampling happens before u2p).
                rot: Quat::from_euler(EulerRot::YXZ, e.y.to_radians(), e.x.to_radians(), e.z.to_radians()),
                scale: raw3(s.get("m_Scale")),
                random_dir: f(s, "randomDirectionAmount"),
                spherical_dir: f(s, "sphericalDirectionAmount"),
            }
        });
        let sz = |k: &str, fallback: &MinMax| -> MinMax { init.get(k).map(|v| MinMax::parse(Some(v))).unwrap_or_else(|| fallback.clone()) };
        let sx = MinMax::parse(init.get("startSize"));
        let rot = MinMax::parse(init.get("startRotation"));
        let vel = enabled("VelocityModule").then(|| parse_axes(&ps["VelocityModule"]));
        let render = {
            let mat = rd.get("m_Materials").and_then(|m| m.as_array()).and_then(|a| a.first()).and_then(|m| m.as_str()).map(str::to_string);
            RenderDef {
                mode: i(rd, "m_RenderMode"),
                align: i(rd, "m_RenderAlignment"),
                material: mat,
                mesh: rd.get("m_Mesh").and_then(|m| m.as_str()).map(str::to_string),
                length_scale: f(rd, "m_LengthScale"),
                velocity_scale: f(rd, "m_VelocityScale"),
                pivot: raw3(rd.get("m_Pivot")),
                enabled: rd.get("m_Enabled").map(|v| v.as_i64() != Some(0)).unwrap_or(true),
                apply_cs: rd.get("m_ApplyActiveColorSpace").map(|v| v.as_i64() != Some(0)).unwrap_or(true),
                min_size: rd.get("m_MinParticleSize").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                max_size: rd.get("m_MaxParticleSize").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
                sort: i(rd, "m_SortMode"),
                fudge: f(rd, "m_SortingFudge"),
                order: i(rd, "m_SortingOrder"),
                streams: rd
                    .get("m_VertexStreams")
                    .and_then(|v| v.as_str())
                    .filter(|_| i(rd, "m_UseCustomVertexStreams") != 0)
                    .map(|h| (0..h.len() / 2).filter_map(|k| u8::from_str_radix(&h[2 * k..2 * k + 2], 16).ok()).collect())
                    .unwrap_or_else(|| vec![0, 1, 3, 4]),
            }
        };
        PsDef {
            duration: f(ps, "lengthInSec").max(0.01),
            looping: i(ps, "looping") != 0,
            start_delay: MinMax::parse(ps.get("startDelay")),
            // Serialized simulationSpace (historical name): 0 = Local, 1 = World, 2 = Custom (treated as World).
            sim_world: i(ps, "moveWithTransform") != 0,
            scaling: i(ps, "scalingMode"),
            sim_speed: if ps.get("simulationSpeed").is_some() { f(ps, "simulationSpeed") } else { 1.0 },
            start_life: MinMax::parse(init.get("startLifetime")),
            start_speed: MinMax::parse(init.get("startSpeed")),
            start_size: [sx.clone(), sz("startSizeY", &sx), sz("startSizeZ", &sx)],
            size3d: i(init, "size3D") != 0,
            start_rot: [sz("startRotationX", &MinMax::default()), sz("startRotationY", &MinMax::default()), rot],
            rot3d: i(init, "rotation3D") != 0,
            start_color: MinMaxGrad::parse(init.get("startColor")),
            gravity: MinMax::parse(init.get("gravityModifier")),
            max_particles: (i(init, "maxNumParticles").max(1) as usize).min(2000),
            rate: if enabled("EmissionModule") { MinMax::parse(em.get("rateOverTime")) } else { MinMax::default() },
            bursts,
            shape,
            speed_mod: vel.as_ref().and_then(|_| ps["VelocityModule"].get("speedModifier")).map(|m| MinMax::parse(Some(m))).filter(|m| m.eval(0.5, 0.5) != 1.0 || m.state != 0),
            vel,
            force: enabled("ForceModule").then(|| parse_axes(&ps["ForceModule"])),
            clamp: enabled("ClampVelocityModule").then(|| {
                let c = &ps["ClampVelocityModule"];
                (MinMax::parse(c.get("magnitude")), f(c, "dampen"), MinMax::parse(c.get("drag")), i(c, "multiplyDragByParticleSize") != 0, i(c, "multiplyDragByParticleVelocity") != 0)
            }),
            color_life: enabled("ColorModule").then(|| MinMaxGrad::parse(ps["ColorModule"].get("gradient"))),
            size_life: enabled("SizeModule").then(|| {
                let s = &ps["SizeModule"];
                let x = MinMax::parse(s.get("curve"));
                let separate = i(s, "separateAxes") != 0;
                SizeLife { y: if separate { MinMax::parse(s.get("y")) } else { x.clone() }, z: if separate { MinMax::parse(s.get("z")) } else { x.clone() }, x, separate }
            }),
            lights: enabled("LightsModule").then(|| LightsDef::parse(&ps["LightsModule"])),
            collision: if enabled("CollisionModule") { CollisionDef::parse(&ps["CollisionModule"]) } else { None },
            noise: enabled("NoiseModule").then(|| NoiseDef::parse(&ps["NoiseModule"])),
            subs: if enabled("SubModule") {
                ps["SubModule"]["subEmitters"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| {
                                Some(SubDef {
                                    emitter: s["emitter"].as_str()?.to_string(),
                                    kind: i(s, "type"),
                                    props: i(s, "properties") as u32,
                                    prob: if s.get("emitProbability").is_some() { f(s, "emitProbability") } else { 1.0 },
                                    idx: None,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            is_sub: false,
            // Trails module (Particles + Ribbon modes): drawn with the renderer's second material (m_Materials[1]).
            trail: if enabled("TrailModule") {
                rd.get("m_Materials").and_then(|m| m.as_array()).and_then(|a| a.get(1)).and_then(|m| m.as_str()).map(|m| TrailDef::parse(&ps["TrailModule"], m))
            } else {
                None
            },
            custom1: enabled("CustomDataModule").then(|| CustomDef::parse(&ps["CustomDataModule"], 0)).flatten(),
            rot_life: enabled("RotationModule").then(|| MinMax::parse(ps["RotationModule"].get("curve"))),
            rot_life_xy: (enabled("RotationModule") && i(&ps["RotationModule"], "separateAxes") != 0).then(|| (MinMax::parse(ps["RotationModule"].get("x")), MinMax::parse(ps["RotationModule"].get("y")))),
            flip_rot: f(init, "randomizeRotationDirection"),
            radial: enabled("VelocityModule").then(|| MinMax::parse(ps["VelocityModule"].get("radial"))).filter(|m| !m.is_const_zero()),
            size_speed: enabled("SizeBySpeedModule").then(|| {
                let s = &ps["SizeBySpeedModule"];
                let x = MinMax::parse(s.get("curve"));
                let separate = i(s, "separateAxes") != 0;
                (SizeLife { y: if separate { MinMax::parse(s.get("y")) } else { x.clone() }, z: if separate { MinMax::parse(s.get("z")) } else { x.clone() }, x, separate }, Vec2::new(f(&s["range"], "x"), f(&s["range"], "y")))
            }),
            rot_speed: enabled("RotationBySpeedModule").then(|| {
                let s = &ps["RotationBySpeedModule"];
                (MinMax::parse(s.get("curve")), Vec2::new(f(&s["range"], "x"), f(&s["range"], "y")))
            }),
            uv: enabled("UVModule").then(|| {
                let u = &ps["UVModule"];
                UvDef {
                    tiles_x: i(u, "tilesX").max(1),
                    tiles_y: i(u, "tilesY").max(1),
                    frame: MinMax::parse(u.get("frameOverTime")),
                    start: MinMax::parse(u.get("startFrame")),
                    cycles: f(u, "cycles").max(1.0),
                }
            }),
            render,
        }
    }
}

// ---- other object components --------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct LineDef {
    pub material: Option<String>,
    pub width: f32,
    pub width_curve: Curve,
    pub gradient: Grad,
    /// LineRenderer `m_ApplyActiveColorSpace` (see `RenderDef::apply_cs`).
    pub apply_cs: bool,
    /// LineFade script: constant colour + alpha decay speed (replaces the gradient).
    pub fade: Option<(Vec4, f32)>,
    /// LineFade `decreaseInSize`: end point lerps to the start by dt * speed per frame (0 = off).
    pub shrink: f32,
    /// Seconds before the line object is destroyed.
    pub life: f32,
}

#[derive(Clone, Debug)]
pub struct DecalDef {
    pub material: Option<String>,
    pub lifetime: f32,
    pub fadeout_percent: f32,
    pub frames: Vec2,
    pub random_rotation: bool,
}

#[derive(Clone, Debug)]
pub struct LightDef {
    pub color: Vec3,
    pub intensity: f32,
    pub range: f32,
    /// Light component enabled (disabled ones are only templates for a particle Lights module).
    pub enabled: bool,
    /// Light.m_RenderMode (0 Auto, 1 Important, 2 Not Important).
    pub render_mode: i64,
}

#[derive(Clone, Debug)]
pub struct ObjDef {
    pub path: String,
    pub parent: Option<usize>,
    pub pos: Vec3,
    pub rot: Quat,
    pub scale: Vec3,
    pub active: bool,
    pub ps: Option<PsDef>,
    pub line: Option<LineDef>,
    pub decal: Option<DecalDef>,
    pub light: Option<LightDef>,
    pub sound: Option<SoundDef>,
    /// Local PPv2 `PostProcessVolume` with its SphereCollider (see `crate::postfx::PpVolume`).
    pub pp: Option<crate::postfx::PpVolume>,
    /// GameObject tag "vfx": the muzzle-flash layer loop leaves it on its prefab layer (`layer`).
    pub vfx_tag: bool,
    pub layer: u32,
}

/// `BulletSound` (Start: `GetComponent<AudioSource>().PlayOneShot(hitClips[RoundToInt(Random.Range(0, n))])`) with
/// its AudioSource settings.
#[derive(Clone, Debug)]
pub struct SoundDef {
    pub clips: Vec<String>,
    pub volume: f32,
    pub pitch: f32,
    /// 3D (spatialBlend 1) vs 2D.
    pub spatial: bool,
    /// [`crate::weapons::WeaponSound::linear_max`] for this source's rolloff.
    pub linear_max: f32,
    /// The exact custom rolloff curve (rolloffMode 2).
    pub rolloff: Option<std::sync::Arc<crate::audio::RolloffCurve>>,
}

impl SoundDef {
    pub fn parse(bs: &Value, src: Option<&Value>) -> Option<SoundDef> {
        let clips: Vec<String> = bs["hitClips"].as_array()?.iter().filter_map(|c| c.as_str()).map(|c| c.trim_start_matches("asset:").to_string()).collect();
        if clips.is_empty() {
            return None;
        }
        let src = src.cloned().unwrap_or(Value::Null);
        let keys = |k: &str| src[k]["m_Curve"].as_array().cloned().unwrap_or_default();
        let spatial = keys("panLevelCustomCurve").first().and_then(|k| k["value"].as_f64()).unwrap_or(0.0) > 0.5;
        let max = f(&src, "MaxDistance").max(1.0);
        let linear_max = match i(&src, "rolloffMode") {
            1 => max,
            // Custom curve of logarithmic shape: full volume up to its last 1.0 key.
            2 => {
                let full = keys("rolloffCustomCurve").iter().filter(|k| k["value"].as_f64().unwrap_or(0.0) >= 0.999).filter_map(|k| k["time"].as_f64()).fold(0.0f64, f64::max);
                -((full as f32) * max).max(1.0)
            }
            _ => -f(&src, "MinDistance").max(1.0),
        };
        let vol = if src.get("m_Volume").is_some() { f(&src, "m_Volume") } else { 1.0 };
        let pitch = if src.get("m_Pitch").is_some() { f(&src, "m_Pitch") } else { 1.0 };
        let rolloff = (i(&src, "rolloffMode") == 2).then(|| crate::audio::RolloffCurve::from_keys(&keys("rolloffCustomCurve"), f(&src, "MaxDistance"))).flatten().map(std::sync::Arc::new);
        Some(SoundDef { clips, volume: vol, pitch, spatial, linear_max, rolloff })
    }
}

#[derive(Clone, Debug)]
pub struct PrefabDef {
    pub name: String,
    pub objects: Vec<ObjDef>,
}

impl PrefabDef {
    pub fn load(file: &str) -> Result<PrefabDef, String> {
        Self::load_sub(file, None)
    }

    /// Loads only the subtree rooted at object path `sub` (e.g. an effect embedded in the PlayerIK prefab), re-rooted so
    /// its first object is the subtree root.
    pub fn load_sub(file: &str, sub: Option<&str>) -> Result<PrefabDef, String> {
        let text = crate::owned::data_text(file).ok_or_else(|| format!("{file}: not found"))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{file}: {e}"))?;
        let objs = v["objects"].as_array().ok_or("no objects")?;
        let mut out: Vec<ObjDef> = Vec::new();
        for o in objs {
            let mut path = o["path"].as_str().unwrap_or("").to_string();
            if let Some(sub) = sub {
                if path != sub && !path.starts_with(&format!("{sub}/")) {
                    continue;
                }
                let cut = sub.rsplit_once('/').map(|(a, _)| a.len() + 1).unwrap_or(0);
                path = path[cut..].to_string();
            }
            let comps = o["components"].as_array().cloned().unwrap_or_default();
            let comp = |ty: &str| comps.iter().find(|c| c["type"] == ty).map(|c| &c["fields"]);
            let tf = comp("Transform");
            let parent = path.rsplit_once('/').and_then(|(p, _)| out.iter().position(|x| x.path == p));
            let ps = match (comp("ParticleSystem"), comp("ParticleSystemRenderer")) {
                (Some(ps), Some(rd)) => Some(PsDef::parse(ps, rd)),
                _ => None,
            };
            let line = comp("LineRenderer").map(|lr| {
                let p = &lr["m_Parameters"];
                let fade = comp("LineFade").map(|lf| (col4(&lf["color"]), f(lf, "speed")));
                LineDef {
                    material: lr["m_Materials"].as_array().and_then(|a| a.first()).and_then(|m| m.as_str()).map(str::to_string),
                    width: if p.get("widthMultiplier").is_some() { f(p, "widthMultiplier") } else { 1.0 },
                    width_curve: p.get("widthCurve").map(Curve::parse).unwrap_or_default(),
                    gradient: p.get("colorGradient").map(Grad::parse).unwrap_or_default(),
                    apply_cs: lr.get("m_ApplyActiveColorSpace").map(|v| v.as_i64() != Some(0)).unwrap_or(true),
                    fade,
                    shrink: comp("LineFade").filter(|lf| i(lf, "decreaseInSize") != 0).map(|lf| f(lf, "decreaseInSizeSpeed")).unwrap_or(0.0),
                    life: 0.4,
                }
            });
            let decal = comp("WFX_BulletHoleDecal").map(|d| DecalDef {
                material: comp("MeshRenderer").and_then(|r| r["m_Materials"].as_array()).and_then(|a| a.first()).and_then(|m| m.as_str()).map(str::to_string),
                lifetime: f(d, "lifetime"),
                fadeout_percent: f(d, "fadeoutpercent"),
                frames: Vec2::new(f(&d["frames"], "x").max(1.0), f(&d["frames"], "y").max(1.0)),
                random_rotation: i(d, "randomRotation") != 0,
            });
            let light = comp("Light").map(|l| LightDef { color: col4(&l["m_Color"]).truncate(), intensity: f(l, "m_Intensity"), range: f(l, "m_Range"), enabled: l.get("m_Enabled").and_then(|v| v.as_i64()) != Some(0), render_mode: l.get("m_RenderMode").and_then(|v| v.as_i64()).unwrap_or(0) });
            out.push(ObjDef {
                path,
                parent,
                pos: u2p(raw3(tf.and_then(|t| t.get("m_LocalPosition")))),
                rot: quat_u2p(tf.and_then(|t| t.get("m_LocalRotation"))),
                scale: tf.and_then(|t| t.get("m_LocalScale")).map(|s| raw3(Some(s))).unwrap_or(Vec3::ONE),
                active: o["active"].as_bool().unwrap_or(true),
                ps,
                line,
                decal,
                light,
                sound: comp("BulletSound").and_then(|bs| SoundDef::parse(bs, comp("AudioSource"))),
                vfx_tag: o["tag"].as_str() == Some("vfx"),
                layer: o["layer"].as_u64().unwrap_or(0) as u32,
                pp: comps
                    .iter()
                    .map(|c| &c["fields"])
                    .find(|f| f.get("sharedProfile").is_some() && f.get("blendDistance").is_some() && i(f, "m_Enabled") != 0 && i(f, "isGlobal") == 0)
                    .zip(comp("SphereCollider").filter(|c| i(c, "m_Enabled") != 0))
                    .and_then(|(v, c)| {
                        Some(crate::postfx::PpVolume {
                            profile: v["sharedProfile"].as_str()?.trim_start_matches("asset:MonoBehaviour/").trim_end_matches(".asset").to_string(),
                            radius: f(c, "m_Radius"),
                            blend: f(v, "blendDistance"),
                            weight: f(v, "weight"),
                            priority: f(v, "priority"),
                        })
                    }),
            });
        }
        for k in 0..out.len() {
            let r = out[k].ps.as_ref().and_then(|p| p.lights.as_ref()).map(|l| l.light_ref.clone());
            if let Some(r) = r {
                let path = r.trim_start_matches("ref:").split("::").next().unwrap_or("").to_string();
                let idx = out.iter().position(|o| o.path == path && o.light.is_some());
                if let Some(l) = out[k].ps.as_mut().and_then(|p| p.lights.as_mut()) {
                    l.obj = idx;
                }
            }
        }
        for k in 0..out.len() {
            let n = out[k].ps.as_ref().map(|p| p.subs.len()).unwrap_or(0);
            for s in 0..n {
                let r = out[k].ps.as_ref().unwrap().subs[s].emitter.clone();
                let path = r.trim_start_matches("ref:").split("::").next().unwrap_or("").to_string();
                let j = out.iter().position(|o| o.path == path && o.ps.is_some());
                if let Some(j) = j {
                    out[j].ps.as_mut().unwrap().is_sub = true;
                }
                out[k].ps.as_mut().unwrap().subs[s].idx = j;
            }
        }
        let name = match sub {
            Some(s) => s.rsplit('/').next().unwrap_or(s).to_string(),
            None => v["name"].as_str().unwrap_or("").to_string(),
        };
        Ok(PrefabDef { name, objects: out })
    }
}

// ---- Lights / Collision modules ---------------------------------------------------------------

/// ParticleSystem Lights module: per-particle point lights copying a prefab `Light` component.
#[derive(Clone, Debug)]
pub struct LightsDef {
    /// `ref:<prefab>/<path>::Light`, resolved to an object index by [`PrefabDef::load`].
    pub light_ref: String,
    pub obj: Option<usize>,
    pub ratio: f32,
    pub intensity: MinMax,
    pub range: MinMax,
    /// Serialized `intensity` = `alphaAffectsIntensity` (the multiplier is `intensityCurve.scalar`).
    pub alpha_affects_intensity: bool,
    /// Serialized `range` = `sizeAffectsRange` (the multiplier is `rangeCurve.scalar`).
    pub size_affects_range: bool,
    pub use_color: bool,
    pub max_lights: usize,
}

impl LightsDef {
    fn parse(m: &Value) -> LightsDef {
        LightsDef {
            light_ref: m.get("light").and_then(|l| l.as_str()).unwrap_or("").to_string(),
            obj: None,
            ratio: f(m, "ratio"),
            intensity: MinMax::parse(m.get("intensityCurve")),
            range: MinMax::parse(m.get("rangeCurve")),
            alpha_affects_intensity: i(m, "intensity") != 0,
            size_affects_range: i(m, "range") != 0,
            use_color: i(m, "color") != 0,
            max_lights: (i(m, "maxLights").max(1) as usize).min(6),
        }
    }
}

/// ParticleSystem Collision module (world mode only).
#[derive(Clone, Debug)]
pub struct CollisionDef {
    pub bounce: MinMax,
    pub dampen: MinMax,
    pub lifetime_loss: MinMax,
    pub min_kill: f32,
    pub max_kill: f32,
    pub radius_scale: f32,
}

impl CollisionDef {
    fn parse(m: &Value) -> Option<CollisionDef> {
        (i(m, "type") == 1).then(|| CollisionDef {
            bounce: MinMax::parse(m.get("m_Bounce")),
            dampen: MinMax::parse(m.get("m_Dampen")),
            lifetime_loss: MinMax::parse(m.get("m_EnergyLossOnCollision")),
            min_kill: f(m, "minKillSpeed"),
            max_kill: if m.get("maxKillSpeed").is_some() { f(m, "maxKillSpeed") } else { 1e9 },
            radius_scale: f(m, "radiusScale"),
        })
    }
}

/// ParticleSystem Noise module: a smooth 3D noise field (sampled at particle position * frequency,
/// scrolled over time) displacing particles each frame (velocity magnitude ~ strength with `damping`).
#[derive(Clone, Debug)]
pub struct NoiseDef {
    pub strength: [MinMax; 3],
    pub frequency: f32,
    pub scroll: MinMax,
    pub damping: bool,
    pub octaves: u32,
    pub octave_mul: f32,
    pub octave_scale: f32,
    pub position_amount: MinMax,
    /// Rotation noise, degrees per second (rotationAmount).
    pub rotation_amount: MinMax,
    /// Size noise multiplier (sizeAmount).
    pub size_amount: MinMax,
    /// 0 Low (1D), 1 Medium (2D), 2 High (3D).
    pub quality: i32,
    /// remapEnabled: curves (x, y, z) mapping the noise value ([-1, 1] -> curve time [0, 1]) to the output value.
    pub remap: Option<[MinMax; 3]>,
}

impl NoiseDef {
    fn parse(m: &Value) -> NoiseDef {
        let x = MinMax::parse(m.get("strength"));
        let sep = i(m, "separateAxes") != 0;
        NoiseDef {
            strength: if sep { [x, MinMax::parse(m.get("strengthY")), MinMax::parse(m.get("strengthZ"))] } else { [x.clone(), x.clone(), x] },
            frequency: f(m, "frequency").max(1e-4),
            scroll: MinMax::parse(m.get("scrollSpeed")),
            damping: i(m, "damping") != 0,
            octaves: i(m, "octaves").clamp(1, 4) as u32,
            octave_mul: f(m, "octaveMultiplier"),
            octave_scale: f(m, "octaveScale"),
            position_amount: MinMax::parse(m.get("positionAmount")),
            rotation_amount: MinMax::parse(m.get("rotationAmount")),
            size_amount: MinMax::parse(m.get("sizeAmount")),
            quality: i(m, "quality"),
            remap: (i(m, "remapEnabled") != 0).then(|| {
                let x = MinMax::parse(m.get("remap"));
                if sep { [x, MinMax::parse(m.get("remapY")), MinMax::parse(m.get("remapZ"))] } else { [x.clone(), x.clone(), x] }
            }),
        }
    }
}

/// ParticleSystem Trails module. Mode 0 "Particles": each particle leaves a ribbon of points; mode 1 "Ribbon": the
/// live particles are connected in age order into `ribbon_count` ribbons.
#[derive(Clone, Debug)]
pub struct TrailDef {
    pub mode: i32,
    pub ribbon_count: usize,
    /// 0 Stretch, 1 Tile, 2 DistributePerSegment, 3 RepeatPerSegment.
    pub texture_mode: i32,
    pub size_life: bool,
    pub attach_ribbons: bool,
    pub ratio: f32,
    /// Point lifetime as a multiplier of the particle's lifetime.
    pub lifetime: MinMax,
    pub min_dist: f32,
    pub world: bool,
    pub die_with: bool,
    pub size_width: bool,
    pub inherit_color: bool,
    pub color_life: MinMaxGrad,
    pub color_trail: MinMaxGrad,
    pub width: MinMax,
    pub material: String,
}

impl TrailDef {
    fn parse(m: &Value, material: &str) -> TrailDef {
        TrailDef {
            mode: i(m, "mode"),
            ribbon_count: i(m, "ribbonCount").max(1) as usize,
            texture_mode: i(m, "textureMode"),
            size_life: i(m, "sizeAffectsLifetime") != 0,
            attach_ribbons: i(m, "attachRibbonsToTransform") != 0,
            ratio: f(m, "ratio"),
            lifetime: MinMax::parse(m.get("lifetime")),
            min_dist: f(m, "minVertexDistance"),
            world: i(m, "worldSpace") != 0,
            die_with: i(m, "dieWithParticles") != 0,
            size_width: i(m, "sizeAffectsWidth") != 0,
            inherit_color: i(m, "inheritParticleColor") != 0,
            color_life: MinMaxGrad::parse(m.get("colorOverLifetime")),
            color_trail: MinMaxGrad::parse(m.get("colorOverTrail")),
            width: MinMax::parse(m.get("widthOverTrail")),
            material: material.to_string(),
        }
    }
}

/// Sub Emitters module entry. `kind`: 0 Birth, 1 Collision, 2 Death, 3 Trigger, 4 Manual. `props` (inherit):
/// 1 Color, 2 Size, 4 Rotation, 8 Lifetime, 16 Duration.
#[derive(Clone, Debug)]
pub struct SubDef {
    pub emitter: String,
    pub kind: i32,
    pub props: u32,
    pub prob: f32,
    pub idx: Option<usize>,
}

/// Custom Data module stream (`mode<n>`: 0 Disabled, 1 Vector = `vectorComponentCount<n>` MinMaxCurves over the
/// particle lifetime, 2 Color = MinMaxGradient over the lifetime).
#[derive(Clone, Debug)]
pub enum CustomDef {
    Vector(Vec<MinMax>),
    Color(MinMaxGrad),
}

impl CustomDef {
    fn parse(m: &Value, n: usize) -> Option<CustomDef> {
        match i(m, &format!("mode{n}")) {
            1 => Some(CustomDef::Vector((0..i(m, &format!("vectorComponentCount{n}")).clamp(0, 4) as usize).map(|k| MinMax::parse(m.get(&format!("vector{n}_{k}")))).collect())),
            2 => Some(CustomDef::Color(MinMaxGrad::parse(m.get(&format!("color{n}"))))),
            _ => None,
        }
    }
    /// Value at normalised age `t`; `r` = per-particle randoms (one per component).
    pub fn eval(&self, t: f32, r: [f32; 4]) -> Vec4 {
        match self {
            CustomDef::Vector(c) => Vec4::from_array(std::array::from_fn(|k| c.get(k).map(|m| m.eval(t, r[k])).unwrap_or(0.0))),
            CustomDef::Color(g) => g.eval(t, r[0]),
        }
    }
}
