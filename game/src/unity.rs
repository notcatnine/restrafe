//! Unity API semantics needed for a faithful port.
//!
//! Coordinate conventions: Unity is left-handed (+Z forward), Bevy is right-handed (-Z forward).
//! Positions/directions map as (x, y, -z). Euler angles in degrees map as
//! pitch(x) -> -x, yaw(y) -> -y, roll(z) -> z. Gameplay code below works with Bevy-space
//! vectors directly (forward = `transform.forward()`), and only Euler angles taken from the
//! C# source go through [`unity_euler`].

use bevy::prelude::*;

/// `Mathf.Lerp` (t clamped to [0,1]).
#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// `Vector3.Lerp` (t clamped).
#[inline]
pub fn lerp3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// `Mathf.Round(x * 100f) * 0.01f` — Unity rounds half to even (banker's rounding).
#[inline]
pub fn round2(x: f32) -> f32 {
    (x * 100.0).round_ties_even() * 0.01
}

/// `Vector3.Angle` in degrees.
#[inline]
pub fn angle(a: Vec3, b: Vec3) -> f32 {
    let denom = (a.length_squared() * b.length_squared()).sqrt();
    if denom < 1e-15 {
        return 0.0;
    }
    (a.dot(b) / denom).clamp(-1.0, 1.0).acos().to_degrees()
}

/// `Vector2.ClampMagnitude`.
#[inline]
pub fn clamp_mag2(v: Vec2, max: f32) -> Vec2 {
    if v.length_squared() > max * max {
        v.normalize() * max
    } else {
        v
    }
}

/// `Vector3.ClampMagnitude`.
#[inline]
pub fn clamp_mag3(v: Vec3, max: f32) -> Vec3 {
    if v.length_squared() > max * max {
        v.normalize() * max
    } else {
        v
    }
}

/// `Quaternion.Euler(x, y, z)` with Unity-space degrees, returned as a Bevy-space rotation.
/// Unity applies Z, then X, then Y (intrinsic order YXZ).
pub fn unity_euler(x: f32, y: f32, z: f32) -> Quat {
    Quat::from_euler(
        EulerRot::YXZ,
        (-y).to_radians(),
        (-x).to_radians(),
        z.to_radians(),
    )
}

/// Inverse of [`unity_euler`]: Bevy rotation -> Unity Euler degrees in [0, 360).
pub fn to_unity_euler(q: Quat) -> Vec3 {
    let (y, x, z) = q.to_euler(EulerRot::YXZ);
    let wrap = |d: f32| d.rem_euclid(360.0);
    Vec3::new(
        wrap(-x.to_degrees()),
        wrap(-y.to_degrees()),
        wrap(z.to_degrees()),
    )
}

/// `Quaternion.Slerp` (t clamped).
#[inline]
pub fn slerp(a: Quat, b: Quat, t: f32) -> Quat {
    a.slerp(b, t.clamp(0.0, 1.0))
}

/// `Random.Range(int min, int max)` — max exclusive.
pub fn random_range_i(min: i32, max: i32) -> i32 {
    if max <= min {
        min
    } else {
        rand::random_range(min..max)
    }
}

/// `Random.Range(float a, float b)` — inclusive; like Unity, works with a > b.
pub fn random_range_f(a: f32, b: f32) -> f32 {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    if hi <= lo { lo } else { rand::random_range(lo..=hi) }
}

/// `System.Random(seed)` (Unity's Mono / .NET Framework Knuth subtractive generator), for code that seeds it for
/// network determinism (frag grenade / shrapnel ray directions).
pub struct DotNetRandom {
    seeds: [i32; 56],
    inext: usize,
    inextp: usize,
}

impl DotNetRandom {
    pub fn new(seed: i32) -> Self {
        const MBIG: i32 = i32::MAX;
        const MSEED: i32 = 161_803_398;
        let mut seeds = [0i32; 56];
        let subtraction = if seed == i32::MIN { i32::MAX } else { seed.abs() };
        let mut mj = MSEED - subtraction;
        seeds[55] = mj;
        let mut mk = 1i32;
        for i in 1..55 {
            let ii = (21 * i) % 55;
            seeds[ii] = mk;
            mk = mj.wrapping_sub(mk);
            if mk < 0 {
                mk = mk.wrapping_add(MBIG);
            }
            mj = seeds[ii];
        }
        for _ in 1..5 {
            for i in 1..56 {
                seeds[i] = seeds[i].wrapping_sub(seeds[1 + (i + 30) % 55]);
                if seeds[i] < 0 {
                    seeds[i] = seeds[i].wrapping_add(MBIG);
                }
            }
        }
        DotNetRandom { seeds, inext: 0, inextp: 21 }
    }

    fn internal_sample(&mut self) -> i32 {
        self.inext += 1;
        if self.inext >= 56 {
            self.inext = 1;
        }
        self.inextp += 1;
        if self.inextp >= 56 {
            self.inextp = 1;
        }
        let mut r = self.seeds[self.inext].wrapping_sub(self.seeds[self.inextp]);
        if r == i32::MAX {
            r -= 1;
        }
        if r < 0 {
            r = r.wrapping_add(i32::MAX);
        }
        self.seeds[self.inext] = r;
        r
    }

    /// `NextDouble()`.
    pub fn next_double(&mut self) -> f64 {
        self.internal_sample() as f64 * (1.0 / i32::MAX as f64)
    }
}

/// `Mathf.RoundToInt` (Math.Round: halves to even).
pub fn round_to_int(v: f32) -> i32 {
    v.round_ties_even() as i32
}

/// The frag-ray generator of PhysicsGrenade.HandleExplosion / ShrapnelBallistic.Explode: `System.Random(seed)` with
/// seed = RoundToInt(x / 5) * 5 * 10000 + RoundToInt(z / 5) * 5 (Unity world position), uniform directions on the
/// sphere. Returns port-space unit directions (Unity z negated).
pub fn frag_ray_dirs(port_pos: Vec3, n: i32) -> Vec<Vec3> {
    let (ux, uz) = (port_pos.x, -port_pos.z);
    let seed = (round_to_int(ux / 5.0) * 5).wrapping_mul(10000).wrapping_add(round_to_int(uz / 5.0) * 5);
    let mut r = DotNetRandom::new(seed);
    (0..n.max(0))
        .map(|_| {
            let num3 = r.next_double() * 2.0 * 3.1415927410125732;
            let num4 = (2.0 * r.next_double() - 1.0).acos();
            let d = Vec3::new((num4.sin() * num3.cos()) as f32, (num4.sin() * num3.sin()) as f32, num4.cos() as f32).normalize();
            Vec3::new(d.x, d.y, -d.z)
        })
        .collect()
}

#[cfg(test)]
mod dotnet_random_tests {
    #[test]
    fn matches_dotnet() {
        let mut r = super::DotNetRandom::new(0);
        assert_eq!(r.internal_sample(), 1559595546);
    }
}
