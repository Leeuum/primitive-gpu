//! Small fast random number generator (xoshiro256++) with the helpers the
//! shape code needs: Intn, Float64 and NormFloat64.

#[derive(Clone)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut z = seed;
        let mut s = [0u64; 4];
        for v in s.iter_mut() {
            // splitmix64
            z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            *v = x ^ (x >> 31);
        }
        Rng { s }
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[0].wrapping_add(s[3]).rotate_left(23).wrapping_add(s[0]);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// Uniform integer in [0, n). n must be > 0.
    #[inline]
    pub fn intn(&mut self, n: i32) -> i32 {
        debug_assert!(n > 0);
        ((self.next_u64() >> 32) * n as u64 >> 32) as i32
    }

    /// Uniform float in [0, 1).
    #[inline]
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Standard normal distribution (mean 0, deviation 1), by the ziggurat
    /// method (Doornik's ZIGNOR). Exact, and most calls need one random
    /// number, one table lookup and one multiply.
    #[inline]
    pub fn norm(&mut self) -> f64 {
        let z = zig();
        loop {
            let bits = self.next_u64();
            let i = (bits & (ZN as u64 - 1)) as usize;
            let u = (bits >> 11) as f64 * (2.0 / (1u64 << 53) as f64) - 1.0; // [-1, 1)
            if u.abs() < z.r[i] {
                return u * z.x[i];
            }
            if i == 0 {
                // The tail beyond ZR.
                loop {
                    let x = (1.0 - self.f64()).ln() / ZR;
                    let y = (1.0 - self.f64()).ln();
                    if -2.0 * y >= x * x {
                        return if u < 0.0 { x - ZR } else { ZR - x };
                    }
                }
            }
            // The curved outer part of layer i.
            let x = u * z.x[i];
            let f0 = (-0.5 * (z.x[i] * z.x[i] - x * x)).exp();
            let f1 = (-0.5 * (z.x[i + 1] * z.x[i + 1] - x * x)).exp();
            if f1 + self.f64() * (f0 - f1) < 1.0 {
                return x;
            }
        }
    }
}

/// Ziggurat layers: the bell curve is cut into ZN layers of equal area ZV.
const ZN: usize = 128;
const ZR: f64 = 3.442619855899;
const ZV: f64 = 9.91256303526217e-3;

struct Zig {
    /// Right edge of each layer; x[ZN] = 0.
    x: [f64; ZN + 1],
    /// x[i + 1] / x[i]: below this, a point is inside the curve for sure.
    r: [f64; ZN],
}

fn zig() -> &'static Zig {
    static T: std::sync::OnceLock<Zig> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut x = [0.0; ZN + 1];
        let mut f = (-0.5 * ZR * ZR).exp();
        x[0] = ZV / f;
        x[1] = ZR;
        for i in 2..ZN {
            x[i] = (-2.0 * (ZV / x[i - 1] + f).ln()).sqrt();
            f = (-0.5 * x[i] * x[i]).exp();
        }
        let r = std::array::from_fn(|i| x[i + 1] / x[i]);
        Zig { x, r }
    })
}
