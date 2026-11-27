//! The original's random numbers: two Park–Miller generators (multipliers 48271 and 16807,
//! modulus 2^31 − 1) whose difference is TPDF noise (`util.h`).

/// The largest number the generators make (`PM_RAND_MAX`).
pub const PM_RAND_MAX: u32 = 0x7fff_ffff;

#[inline]
fn pm_rand_r(s: &mut u32, a: u64) -> u32 {
    let p = u64::from(*s) * a;
    let mut r = ((p & 0x7fff_ffff) + (p >> 31)) as u32;
    r = (r & 0x7fff_ffff) + (r >> 31);
    *s = r;
    r
}

/// `pm_rand1_r`: multiplier 48271.
#[inline]
pub fn pm_rand1_r(s: &mut u32) -> u32 {
    pm_rand_r(s, 48_271)
}

/// `pm_rand2_r`: multiplier 16807.
#[inline]
pub fn pm_rand2_r(s: &mut u32) -> u32 {
    pm_rand_r(s, 16_807)
}

/// The pair of generators behind `tpdf_noise`. The original keeps this in static variables
/// shared by every noise source of the program; here each effect has its own, which gives
/// the same numbers for a single effect and keeps effects from disturbing one another.
#[derive(Clone, Copy, Debug)]
pub struct Tpdf {
    s0: u32,
    s1: u32,
}

impl Default for Tpdf {
    fn default() -> Self {
        Self { s0: 1, s1: 1 }
    }
}

impl Tpdf {
    /// TPDF noise scaled by `mult`: `(n1 - n2) * mult`.
    #[inline]
    pub fn noise(&mut self, mult: f64) -> f64 {
        let n1 = pm_rand1_r(&mut self.s0) as i32;
        let n2 = pm_rand2_r(&mut self.s1) as i32;
        f64::from(n1 - n2) * mult
    }

    /// The first generator alone (`pm_rand1_r(&r_seed[0])`), for the sloped dither.
    #[inline]
    pub fn next1(&mut self) -> u32 {
        pm_rand1_r(&mut self.s0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generators_follow_park_miller() {
        // 48271^1 mod (2^31 - 1), and the famous 10000th value of the 16807 generator.
        let mut s = 1;
        assert_eq!(pm_rand1_r(&mut s), 48_271);
        let mut s = 1;
        for _ in 0..10_000 {
            pm_rand2_r(&mut s);
        }
        assert_eq!(s, 1_043_618_065);
        let mut t = Tpdf::default();
        let v = t.noise(1.0);
        assert_eq!(v, f64::from(48_271 - 16_807));
    }

    #[test]
    fn the_noise_is_triangular_and_stays_inside_its_range() {
        let mut t = Tpdf::default();
        let m = 1.0 / f64::from(PM_RAND_MAX);
        let v: Vec<f64> = (0..100_000).map(|_| t.noise(m)).collect();
        assert!(v.iter().all(|x| x.abs() <= 1.0));
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / v.len() as f64;
        // Two uniforms on [0, 1) subtracted: variance 1/6.
        assert!(
            mean.abs() < 0.01 && (var - 1.0 / 6.0).abs() < 0.01,
            "{mean} {var}"
        );
    }
}
