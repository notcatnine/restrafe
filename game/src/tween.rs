//! Minimal DOTween re-implementation covering the calls STRAFTAT uses
//! (DOPunchRotation, DOPunchPosition, DOLocalMove, DOLocalRotate, DOShakeRotation...).
//!
//! Tweens on the same entity run in creation order, so the newest tween writes last,
//! matching DOTween's update order.

use crate::unity::{to_unity_euler, unity_euler};
use bevy::prelude::*;

/// DOTween `Ease` enum (numeric values match DOTween, so prefab ints map directly).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[repr(u8)]
pub enum Ease {
    Unset = 0,
    Linear = 1,
    InSine = 2,
    OutSine = 3,
    InOutSine = 4,
    InQuad = 5,
    #[default]
    OutQuad = 6,
    InOutQuad = 7,
    InCubic = 8,
    OutCubic = 9,
    InOutCubic = 10,
    InQuart = 11,
    OutQuart = 12,
    InOutQuart = 13,
    InQuint = 14,
    OutQuint = 15,
    InOutQuint = 16,
    InExpo = 17,
    OutExpo = 18,
    InOutExpo = 19,
    InCirc = 20,
    OutCirc = 21,
    InOutCirc = 22,
    InElastic = 23,
    OutElastic = 24,
    InOutElastic = 25,
    InBack = 26,
    OutBack = 27,
    InOutBack = 28,
    InBounce = 29,
    OutBounce = 30,
    InOutBounce = 31,
}

impl Ease {
    pub fn from_i32(v: i32) -> Ease {
        const ALL: [Ease; 32] = [
            Ease::Unset,
            Ease::Linear,
            Ease::InSine,
            Ease::OutSine,
            Ease::InOutSine,
            Ease::InQuad,
            Ease::OutQuad,
            Ease::InOutQuad,
            Ease::InCubic,
            Ease::OutCubic,
            Ease::InOutCubic,
            Ease::InQuart,
            Ease::OutQuart,
            Ease::InOutQuart,
            Ease::InQuint,
            Ease::OutQuint,
            Ease::InOutQuint,
            Ease::InExpo,
            Ease::OutExpo,
            Ease::InOutExpo,
            Ease::InCirc,
            Ease::OutCirc,
            Ease::InOutCirc,
            Ease::InElastic,
            Ease::OutElastic,
            Ease::InOutElastic,
            Ease::InBack,
            Ease::OutBack,
            Ease::InOutBack,
            Ease::InBounce,
            Ease::OutBounce,
            Ease::InOutBounce,
        ];
        ALL.get(v as usize).copied().unwrap_or(Ease::OutQuad)
    }

    /// Port of DOTween `EaseManager.Evaluate` (overshoot 1.70158, period 0 defaults).
    pub fn eval(self, t: f32) -> f32 {
        use std::f32::consts::PI;
        let t = t.clamp(0.0, 1.0);
        const S: f32 = 1.70158;
        match self {
            Ease::Linear => t,
            Ease::InSine => 1.0 - (t * PI / 2.0).cos(),
            Ease::OutSine => (t * PI / 2.0).sin(),
            Ease::InOutSine => -0.5 * ((PI * t).cos() - 1.0),
            Ease::InQuad => t * t,
            Ease::Unset | Ease::OutQuad => -t * (t - 2.0),
            Ease::InOutQuad => {
                let t2 = t * 2.0;
                if t2 < 1.0 {
                    0.5 * t2 * t2
                } else {
                    let t2 = t2 - 1.0;
                    -0.5 * (t2 * (t2 - 2.0) - 1.0)
                }
            }
            Ease::InCubic => t * t * t,
            Ease::OutCubic => {
                let u = t - 1.0;
                u * u * u + 1.0
            }
            Ease::InOutCubic => {
                let t2 = t * 2.0;
                if t2 < 1.0 {
                    0.5 * t2 * t2 * t2
                } else {
                    let u = t2 - 2.0;
                    0.5 * (u * u * u + 2.0)
                }
            }
            Ease::InQuart => t.powi(4),
            Ease::OutQuart => {
                let u = t - 1.0;
                -(u.powi(4) - 1.0)
            }
            Ease::InOutQuart => {
                let t2 = t * 2.0;
                if t2 < 1.0 {
                    0.5 * t2.powi(4)
                } else {
                    let u = t2 - 2.0;
                    -0.5 * (u.powi(4) - 2.0)
                }
            }
            Ease::InQuint => t.powi(5),
            Ease::OutQuint => {
                let u = t - 1.0;
                u.powi(5) + 1.0
            }
            Ease::InOutQuint => {
                let t2 = t * 2.0;
                if t2 < 1.0 {
                    0.5 * t2.powi(5)
                } else {
                    let u = t2 - 2.0;
                    0.5 * (u.powi(5) + 2.0)
                }
            }
            Ease::InExpo => {
                if t == 0.0 {
                    0.0
                } else {
                    2f32.powf(10.0 * (t - 1.0))
                }
            }
            Ease::OutExpo => {
                if t == 1.0 {
                    1.0
                } else {
                    1.0 - 2f32.powf(-10.0 * t)
                }
            }
            Ease::InOutExpo => {
                if t == 0.0 || t == 1.0 {
                    return t;
                }
                let t2 = t * 2.0;
                if t2 < 1.0 {
                    0.5 * 2f32.powf(10.0 * (t2 - 1.0))
                } else {
                    0.5 * (2.0 - 2f32.powf(-10.0 * (t2 - 1.0)))
                }
            }
            Ease::InCirc => -((1.0 - t * t).sqrt() - 1.0),
            Ease::OutCirc => {
                let u = t - 1.0;
                (1.0 - u * u).sqrt()
            }
            Ease::InOutCirc => {
                let t2 = t * 2.0;
                if t2 < 1.0 {
                    -0.5 * ((1.0 - t2 * t2).sqrt() - 1.0)
                } else {
                    let u = t2 - 2.0;
                    0.5 * ((1.0 - u * u).sqrt() + 1.0)
                }
            }
            Ease::InBack => t * t * ((S + 1.0) * t - S),
            Ease::OutBack => {
                let u = t - 1.0;
                u * u * ((S + 1.0) * u + S) + 1.0
            }
            Ease::InOutBack => {
                let s = S * 1.525;
                let t2 = t * 2.0;
                if t2 < 1.0 {
                    0.5 * (t2 * t2 * ((s + 1.0) * t2 - s))
                } else {
                    let u = t2 - 2.0;
                    0.5 * (u * u * ((s + 1.0) * u + s) + 2.0)
                }
            }
            Ease::InElastic | Ease::OutElastic | Ease::InOutElastic => elastic(self, t),
            Ease::InBounce => 1.0 - bounce_out(1.0 - t),
            Ease::OutBounce => bounce_out(t),
            Ease::InOutBounce => {
                if t < 0.5 {
                    (1.0 - bounce_out(1.0 - t * 2.0)) * 0.5
                } else {
                    bounce_out(t * 2.0 - 1.0) * 0.5 + 0.5
                }
            }
        }
    }
}

fn bounce_out(t: f32) -> f32 {
    if t < 1.0 / 2.75 {
        7.5625 * t * t
    } else if t < 2.0 / 2.75 {
        let u = t - 1.5 / 2.75;
        7.5625 * u * u + 0.75
    } else if t < 2.5 / 2.75 {
        let u = t - 2.25 / 2.75;
        7.5625 * u * u + 0.9375
    } else {
        let u = t - 2.625 / 2.75;
        7.5625 * u * u + 0.984375
    }
}

fn elastic(e: Ease, t: f32) -> f32 {
    // DOTween EaseManager with the default overshootOrAmplitude 1.70158 (DOTweenSettings) and period 0
    // (-> duration * 0.3, InOut: * 0.45): amplitude a = 1.70158, phase s = p / 2pi * asin(1 / a), in normalized time.
    use std::f32::consts::PI;
    const A: f32 = 1.70158;
    if t == 0.0 || t == 1.0 {
        return t;
    }
    match e {
        Ease::InElastic => {
            let p = 0.3;
            let s = p / (2.0 * PI) * (1.0 / A).asin();
            let u = t - 1.0;
            -(A * 2f32.powf(10.0 * u) * ((u - s) * (2.0 * PI) / p).sin())
        }
        Ease::OutElastic => {
            let p = 0.3;
            let s = p / (2.0 * PI) * (1.0 / A).asin();
            A * 2f32.powf(-10.0 * t) * ((t - s) * (2.0 * PI) / p).sin() + 1.0
        }
        _ => {
            // time /= duration * 0.5, then time -= 1 (u = 2t - 1); period = duration * 0.45.
            let p = 0.45;
            let s = p / (2.0 * PI) * (1.0 / A).asin();
            let t2 = t * 2.0 - 1.0;
            if t2 < 0.0 {
                -0.5 * (A * 2f32.powf(10.0 * t2) * ((t2 - s) * (2.0 * PI) / p).sin())
            } else {
                A * 2f32.powf(-10.0 * t2) * ((t2 - s) * (2.0 * PI) / p).sin() * 0.5 + 1.0
            }
        }
    }
}

/// Builds DOTween's punch keyframes (`DOTween.Punch`): returns (offsets, segment durations).
pub fn punch_keyframes(
    direction: Vec3,
    duration: f32,
    vibrato: i32,
    elasticity: f32,
) -> (Vec<Vec3>, Vec<f32>) {
    let elasticity = elasticity.clamp(0.0, 1.0);
    let mut power = direction.length();
    let mut tot = (vibrato as f32 * duration) as i32;
    if tot < 2 {
        tot = 2;
    }
    let n = tot as usize;
    let decay = power / tot as f32;
    let mut durs = Vec::with_capacity(n);
    let mut sum = 0.0;
    for i in 0..n {
        let d = duration * ((i + 1) as f32 / tot as f32);
        sum += d;
        durs.push(d);
    }
    let mult = duration / sum;
    for d in &mut durs {
        *d *= mult;
    }
    let mut tos = Vec::with_capacity(n);
    for i in 0..n {
        if i < n - 1 {
            if i == 0 {
                tos.push(direction);
            } else if i % 2 != 0 {
                tos.push(-crate::unity::clamp_mag3(direction, power * elasticity));
            } else {
                tos.push(crate::unity::clamp_mag3(direction, power));
            }
            power -= decay;
        } else {
            tos.push(Vec3::ZERO);
        }
    }
    (tos, durs)
}

/// Port of `DOTween.Shake` (vector-based, used by DOShakeRotation/DOShakePosition).
/// `harmonic` = ShakeRandomnessMode.Harmonic (1); Full (0) otherwise.
pub fn shake_keyframes(
    mut strength: Vec3,
    duration: f32,
    vibrato: i32,
    randomness: f32,
    fade_out: bool,
    harmonic: bool,
) -> (Vec<Vec3>, Vec<f32>) {
    let mut mag = strength.length();
    let mut tot = (vibrato as f32 * duration) as i32;
    if tot < 2 {
        tot = 2;
    }
    let n = tot as usize;
    let decay = mag / tot as f32;
    let mut durs = Vec::with_capacity(n);
    let mut sum = 0.0;
    for i in 0..n {
        let perc = (i + 1) as f32 / tot as f32;
        let d = if fade_out { duration * perc } else { duration / tot as f32 };
        sum += d;
        durs.push(d);
    }
    let mult = duration / sum;
    for d in &mut durs {
        *d *= mult;
    }
    let mut ang: f32 = crate::unity::random_range_f(0.0, 360.0);
    let mut tos = Vec::with_capacity(n);
    for i in 0..n {
        if i < n - 1 {
            let axis_angle = if harmonic {
                if i > 0 {
                    ang = ang - 180.0 + crate::unity::random_range_f(0.0, randomness);
                }
                crate::unity::random_range_f(0.0, randomness)
            } else {
                if i > 0 {
                    ang = ang - 180.0 + crate::unity::random_range_f(-randomness, randomness);
                }
                crate::unity::random_range_f(-randomness, randomness)
            };
            // Quaternion.AngleAxis(a, Vector3.up) * Vector3FromAngle(ang, mag), in Unity space.
            let base = Vec3::new(mag * ang.to_radians().cos(), mag * ang.to_radians().sin(), 0.0);
            let (s, c) = axis_angle.to_radians().sin_cos();
            // Unity left-handed rotation about +Y: x' = x*c + z*s, z' = -x*s + z*c
            let mut to = Vec3::new(base.x * c + base.z * s, base.y, -base.x * s + base.z * c);
            to.x = crate::unity::clamp_mag3(to, strength.x).x;
            to.y = crate::unity::clamp_mag3(to, strength.y).y;
            to.z = crate::unity::clamp_mag3(to, strength.z).z;
            to = to.normalize_or_zero() * mag;
            tos.push(to);
            if fade_out {
                mag -= decay;
            }
            strength = crate::unity::clamp_mag3(strength, mag);
        } else {
            tos.push(Vec3::ZERO);
        }
    }
    (tos, durs)
}

/// `DOTween.Shake` with vectorBased = false, ignoreZAxis = false (float-strength DOShakeRotation):
/// to = AngleAxis(rand(-r, r), up) * Vector3FromAngle(ang, strength), strength decaying when fadeOut.
pub fn shake_keyframes_scalar(strength: f32, duration: f32, vibrato: i32, randomness: f32, fade_out: bool) -> (Vec<Vec3>, Vec<f32>) {
    let mut mag = strength;
    let tot = ((vibrato as f32 * duration) as i32).max(2);
    let n = tot as usize;
    let decay = mag / tot as f32;
    let mut durs: Vec<f32> = (0..n).map(|i| if fade_out { duration * ((i + 1) as f32 / tot as f32) } else { duration / tot as f32 }).collect();
    let sum: f32 = durs.iter().sum();
    for d in &mut durs {
        *d *= duration / sum;
    }
    let mut ang: f32 = crate::unity::random_range_f(0.0, 360.0);
    let mut tos = Vec::with_capacity(n);
    for i in 0..n {
        if i < n - 1 {
            if i > 0 {
                ang = ang - 180.0 + crate::unity::random_range_f(-randomness, randomness);
            }
            let axis_angle = crate::unity::random_range_f(-randomness, randomness);
            let base = Vec3::new(mag * ang.to_radians().cos(), mag * ang.to_radians().sin(), 0.0);
            let (s, c) = axis_angle.to_radians().sin_cos();
            tos.push(Vec3::new(base.x * c + base.z * s, base.y, -base.x * s + base.z * c));
            if fade_out {
                mag -= decay;
            }
        } else {
            tos.push(Vec3::ZERO);
        }
    }
    (tos, durs)
}

/// Keyframed Vector3 path evaluated segment-by-segment (DOTween `Vector3ArrayPlugin`).
#[derive(Clone, Debug)]
pub struct KeyPath {
    pub tos: Vec<Vec3>,
    pub durs: Vec<f32>,
    pub ease: Ease,
}

impl KeyPath {
    pub fn eval(&self, start: Vec3, elapsed: f32) -> Vec3 {
        let mut from = start;
        let mut t = elapsed;
        for (to_rel, d) in self.tos.iter().zip(&self.durs) {
            let to = start + *to_rel;
            if t <= *d {
                let k = if *d > 0.0 { self.ease.eval(t / d) } else { 1.0 };
                return from + (to - from) * k;
            }
            t -= d;
            from = to;
        }
        from
    }
}

#[derive(Clone, Debug)]
pub enum TweenKind {
    /// Punch on local Euler angles (Unity degrees), relative to the start Euler.
    PunchRotation { start: Option<Vec3>, path: KeyPath },
    /// Punch on local scale, relative to the start scale (DOPunchScale).
    PunchScale { start: Option<Vec3>, path: KeyPath },
    /// Punch on local position, relative to the start position.
    PunchPosition { start: Option<Vec3>, path: KeyPath },
    /// Move local position to `to`.
    LocalMove {
        from: Option<Vec3>,
        to: Vec3,
        ease: Ease,
    },
    /// Scale local scale to `to` (DOScale).
    ScaleTo {
        from: Option<Vec3>,
        to: Vec3,
        ease: Ease,
    },
    /// Rotate local Euler (Unity degrees) to `to`.
    LocalRotate {
        from: Option<Vec3>,
        to: Vec3,
        ease: Ease,
    },
}

#[derive(Clone, Debug)]
pub struct Tween {
    pub kind: TweenKind,
    pub duration: f32,
    pub elapsed: f32,
}

/// Active DOTween-style tweens targeting this entity's local `Transform`.
#[derive(Component, Default, Debug)]
pub struct Tweens(pub Vec<Tween>);

impl Tweens {
    // DOTween's DOPunch* / DOShake* return null (no tween) for duration <= 0, and their startup (SetPunch /
    // SetShake) forces the ease to OutQuad (punch) or Linear (shake) per segment, so a chained SetEase is ignored.

    pub fn punch_rotation(
        &mut self,
        punch_unity_euler: Vec3,
        duration: f32,
        vibrato: i32,
        elasticity: f32,
    ) {
        if duration <= 0.0 {
            return;
        }
        let (tos, durs) = punch_keyframes(punch_unity_euler, duration, vibrato, elasticity);
        self.0.push(Tween {
            kind: TweenKind::PunchRotation {
                start: None,
                path: KeyPath {
                    tos,
                    durs,
                    ease: Ease::OutQuad,
                },
            },
            duration,
            elapsed: 0.0,
        });
    }

    /// `DOShakeRotation(duration, Vector3 strength, vibrato, randomness, fadeOut, mode)` (vector-based shake).
    /// A chained `.SetEase(...)` has no effect (SetShake forces Linear).
    pub fn shake_rotation(&mut self, duration: f32, strength: Vec3, vibrato: i32, randomness: f32, fade_out: bool, harmonic: bool) {
        if duration <= 0.0 {
            return;
        }
        let (tos, durs) = shake_keyframes(strength, duration, vibrato, randomness, fade_out, harmonic);
        self.0.push(Tween {
            kind: TweenKind::PunchRotation { start: None, path: KeyPath { tos, durs, ease: Ease::Linear } },
            duration,
            elapsed: 0.0,
        });
    }

    /// `transform.DOShakeRotation(duration, float strength, vibrato, randomness)` (fadeOut true, Full randomness):
    /// DOTween.Shake with vectorBased = false, ignoreZAxis = false.
    pub fn shake_rotation_scalar(&mut self, duration: f32, strength: f32, vibrato: i32, randomness: f32) {
        if duration <= 0.0 {
            return;
        }
        let (tos, durs) = shake_keyframes_scalar(strength, duration, vibrato, randomness, true);
        self.0.push(Tween {
            kind: TweenKind::PunchRotation { start: None, path: KeyPath { tos, durs, ease: Ease::Linear } },
            duration,
            elapsed: 0.0,
        });
    }

    pub fn punch_position(&mut self, punch: Vec3, duration: f32, vibrato: i32, elasticity: f32) {
        if duration <= 0.0 {
            return;
        }
        let (tos, durs) = punch_keyframes(punch, duration, vibrato, elasticity);
        self.0.push(Tween {
            kind: TweenKind::PunchPosition {
                start: None,
                path: KeyPath {
                    tos,
                    durs,
                    ease: Ease::OutQuad,
                },
            },
            duration,
            elapsed: 0.0,
        });
    }

    /// DOPunchScale whose start value is known (DOTween captures it at the tween's first update; callers
    /// that reset the scale right before punching, e.g. `localScale = Vector3.one; DOPunchScale(...)`, pass it here
    /// so an overlapping older tween cannot shift the base).
    pub fn punch_scale_from(&mut self, start: Vec3, punch: Vec3, duration: f32, vibrato: i32, elasticity: f32) {
        if duration <= 0.0 {
            return;
        }
        let (tos, durs) = punch_keyframes(punch, duration, vibrato, elasticity);
        self.0.push(Tween { kind: TweenKind::PunchScale { start: Some(start), path: KeyPath { tos, durs, ease: Ease::OutQuad } }, duration, elapsed: 0.0 });
    }

    /// `DOScale(to, duration)` (default ease OutQuad).
    pub fn scale_to(&mut self, to: Vec3, duration: f32, ease: Ease) {
        self.0.push(Tween { kind: TweenKind::ScaleTo { from: None, to, ease }, duration, elapsed: 0.0 });
    }

    pub fn local_move(&mut self, to: Vec3, duration: f32, ease: Ease) {
        self.0.push(Tween {
            kind: TweenKind::LocalMove {
                from: None,
                to,
                ease,
            },
            duration,
            elapsed: 0.0,
        });
    }

    pub fn local_rotate(&mut self, to_unity_euler: Vec3, duration: f32, ease: Ease) {
        self.0.push(Tween {
            kind: TweenKind::LocalRotate {
                from: None,
                to: to_unity_euler,
                ease,
            },
            duration,
            elapsed: 0.0,
        });
    }

    pub fn kill(&mut self) {
        self.0.clear();
    }
}

/// Shortest-path Euler lerp like DOTween's default (non-`RotateMode.FastBeyond360`) rotate.
pub fn lerp_euler(a: Vec3, b: Vec3, k: f32) -> Vec3 {
    let d = |x: f32, y: f32| {
        let mut diff = (y - x).rem_euclid(360.0);
        if diff > 180.0 {
            diff -= 360.0;
        }
        diff
    };
    a + Vec3::new(d(a.x, b.x), d(a.y, b.y), d(a.z, b.z)) * k
}

pub fn update_tweens(time: Res<Time>, mut q: Query<(&mut Tweens, &mut Transform)>) {
    let dt = time.delta_secs();
    for (mut tweens, mut tf) in &mut q {
        if tweens.0.is_empty() {
            continue;
        }
        for tw in tweens.0.iter_mut() {
            tw.elapsed += dt;
            let k = if tw.duration > 0.0 {
                (tw.elapsed / tw.duration).min(1.0)
            } else {
                1.0
            };
            match &mut tw.kind {
                TweenKind::PunchRotation { start, path } => {
                    let s = *start.get_or_insert_with(|| to_unity_euler(tf.rotation));
                    let e = path.eval(s, tw.elapsed);
                    tf.rotation = unity_euler(e.x, e.y, e.z);
                }
                TweenKind::PunchScale { start, path } => {
                    let s = *start.get_or_insert(tf.scale);
                    tf.scale = path.eval(s, tw.elapsed);
                }
                TweenKind::PunchPosition { start, path } => {
                    let s = *start.get_or_insert(tf.translation);
                    tf.translation = path.eval(s, tw.elapsed);
                }
                TweenKind::LocalMove { from, to, ease } => {
                    let s = *from.get_or_insert(tf.translation);
                    tf.translation = s + (*to - s) * ease.eval(k);
                }
                TweenKind::ScaleTo { from, to, ease } => {
                    let s = *from.get_or_insert(tf.scale);
                    tf.scale = s + (*to - s) * ease.eval(k);
                }
                TweenKind::LocalRotate { from, to, ease } => {
                    let s = *from.get_or_insert_with(|| to_unity_euler(tf.rotation));
                    let e = lerp_euler(s, *to, ease.eval(k));
                    tf.rotation = unity_euler(e.x, e.y, e.z);
                }
            }
        }
        tweens.0.retain(|t| t.elapsed < t.duration);
    }
}
