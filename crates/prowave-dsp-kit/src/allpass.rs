//! All-pass filters for fractional delays (`allpass.h` and `allpass.c`): first and second order
//! in closed form, and the Thiran all-pass of any order.

/// First-order all-pass.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ap1 {
    pub c0: f64,
    pub(crate) i0: f64,
    pub(crate) o0: f64,
}

impl Ap1 {
    /// A first-order all-pass with coefficient `c0`.
    pub fn new(c0: f64) -> Self {
        Self {
            c0,
            ..Self::default()
        }
    }

    pub fn reset(&mut self) {
        self.i0 = 0.0;
        self.o0 = 0.0;
    }

    #[inline]
    pub fn run(&mut self, s: f64) -> f64 {
        let r = self.i0 + self.c0 * (s - self.o0);
        self.i0 = s;
        self.o0 = r;
        r
    }
}

/// Second-order all-pass.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ap2 {
    pub c0: f64,
    pub c1: f64,
    i0: f64,
    o0: f64,
    i1: f64,
    o1: f64,
}

impl Ap2 {
    /// A second-order all-pass with coefficients `c0` and `c1`.
    pub fn new(c0: f64, c1: f64) -> Self {
        Self {
            c0,
            c1,
            ..Self::default()
        }
    }

    pub fn reset(&mut self) {
        self.i0 = 0.0;
        self.i1 = 0.0;
        self.o0 = 0.0;
        self.o1 = 0.0;
    }

    #[inline]
    pub fn run(&mut self, s: f64) -> f64 {
        let r = self.i1 + self.c0 * (self.i0 - self.o0) + self.c1 * (s - self.o1);
        self.i1 = self.i0;
        self.i0 = s;
        self.o1 = self.o0;
        self.o0 = r;
        r
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ThiranStage {
    c0: f64,
    c1: f64,
    c2: f64,
    m0: f64,
    m1: f64,
}

/// Thiran all-pass of order `n` with a fractional delay of `delay` samples (stable for
/// `delay > n - 1`), as a lattice of `n` stages.
#[derive(Clone, Debug)]
pub struct Thiran {
    fb: Vec<ThiranStage>,
}

impl Thiran {
    /// `thiran_ap_new`; `None` when the filter would be unstable.
    pub fn new(n: usize, delay: f64) -> Option<Self> {
        if n < 1 || delay <= n as f64 - 1.0 {
            return None;
        }
        let fb = (0..n)
            .map(|k| ThiranStage {
                c0: delay - k as f64,
                c1: -1.0 / (delay + (k + 1) as f64),
                c2: (2 * k + 1) as f64,
                m0: 0.0,
                m1: 0.0,
            })
            .collect();
        Some(Self { fb })
    }

    pub fn reset(&mut self) {
        for s in self.fb.iter_mut() {
            s.m0 = 0.0;
        }
    }

    #[inline]
    pub fn run(&mut self, s: f64) -> f64 {
        let mut u = s;
        for st in self.fb.iter_mut() {
            u = u * st.c0 + st.m0;
            u *= st.c1;
            st.m1 = u;
        }
        let mut y = 0.0;
        for st in self.fb.iter_mut().rev() {
            y += 2.0 * st.m1;
            st.m0 += y * st.c2;
        }
        s + y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The delay of a filter at low frequency, in samples, from its phase at `w`.
    fn group_delay(mut run: impl FnMut(f64) -> f64) -> f64 {
        // 16 whole cycles in the 2000 samples that are measured.
        let w = 2.0 * std::f64::consts::PI * 16.0 / 2000.0;
        let (mut re, mut im) = (0.0, 0.0);
        let n = 4000;
        for i in 0..n {
            let y = run((w * i as f64).sin());
            if i > n / 2 {
                re += y * (w * i as f64).sin();
                im += y * (w * i as f64).cos();
            }
        }
        -(im / re).atan() / w
    }

    #[test]
    fn the_all_passes_delay_by_about_their_fraction_and_keep_the_level() {
        // The delay code hands the second-order filter a delta between about 1.1 and 2.1.
        let d = 1.4;
        let mut t = Thiran::new(3, 2.4).unwrap();
        let g = group_delay(|x| t.run(x));
        assert!((g - 2.4).abs() < 0.05, "{g}");
        assert!(Thiran::new(3, 2.0).is_none());
        assert!(Thiran::new(0, 5.0).is_none());
        let mut b = Ap2 {
            c0: (4.0 - 2.0 * d) / (1.0 + d),
            c1: ((d - 2.0) * (d - 1.0)) / ((d + 1.0) * (d + 2.0)),
            ..Ap2::default()
        };
        let mut peak = 0f64;
        for i in 0..2000 {
            let y = b.run((0.3 * i as f64).sin());
            if i > 1000 {
                peak = peak.max(y.abs());
            }
        }
        assert!((peak - 1.0).abs() < 0.01, "{peak}");
    }
}
