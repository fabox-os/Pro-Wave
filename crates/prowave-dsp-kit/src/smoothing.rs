//! The smoothers of the matrix decoders: an exponentially weighted moving average (`ewma.h`)
//! and Andrew Simper's self-modulating filter (`smf.h`).

/// One-pole smoother.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ewma {
    g0: f64,
    m0: f64,
}

/// `EWMA_RISE_TIME`: the time constant in seconds for a 10%–90% rise time of `ms`.
pub fn rise_time(ms: f64) -> f64 {
    ms / 1000.0 / 2.1972
}

impl Ewma {
    /// `ewma_init`: `tc` is the time constant in seconds.
    pub fn new(fs: f64, tc: f64) -> Self {
        Self {
            g0: 1.0 - (-1.0 / (fs * tc)).exp(),
            m0: 0.0,
        }
    }
    pub fn run(&mut self, s: f64) -> f64 {
        self.m0 = self.g0 * (s - self.m0) + self.m0;
        self.m0
    }
    /// `sf > 1` is a faster rise time.
    pub fn run_scale(&mut self, s: f64, sf: f64) -> f64 {
        let mut g = self.g0 * sf;
        if g > 0.39 {
            g = 0.39;
        }
        self.m0 = g * (s - self.m0) + self.m0;
        self.m0
    }
    pub fn run_scale_asym(&mut self, s: f64, rise_sf: f64, fall_sf: f64) -> f64 {
        if s >= self.m0 {
            self.run_scale(s, rise_sf)
        } else {
            self.run_scale(s, fall_sf)
        }
    }
    pub fn run_set_max(&mut self, s: f64) -> f64 {
        if s >= self.m0 {
            self.run(s)
        } else {
            self.m0 = s;
            s
        }
    }
    pub fn set(&mut self, s: f64) -> f64 {
        self.m0 = s;
        s
    }
    pub fn last(&self) -> f64 {
        self.m0
    }
}

/// Self-modulating filter (Simper, "Dynamic Smoothing Using Self-Modulating Filter", 2016).
#[derive(Clone, Copy, Debug, Default)]
pub struct Smf {
    g0: f64,
    m0: f64,
    m1: f64,
    c0: f64,
    c1: f64,
}

impl Smf {
    /// `smf_asym_init`
    pub fn new_asym(fs: f64, tc: f64, sens_rise: f64, sens_fall: f64) -> Self {
        Self {
            g0: 1.0 - (-1.0 / (fs * tc)).exp(),
            c0: sens_rise * 4.0,
            c1: sens_fall * 4.0,
            m0: 0.0,
            m1: 0.0,
        }
    }
    pub fn set(&mut self, v: f64) {
        self.m0 = v;
        self.m1 = v;
    }
    fn run_c(&mut self, s: f64, c: f64) -> f64 {
        let mut g = self.g0 + c * (self.m0 - self.m1).abs();
        if g > 0.39 {
            g = 0.39;
        }
        self.m0 = g * (s - self.m0) + self.m0;
        self.m1 = g * (self.m0 - self.m1) + self.m1;
        self.m1
    }
    pub fn run_asym(&mut self, s: f64) -> f64 {
        let c = if s > self.m1 { self.c0 } else { self.c1 };
        self.run_c(s, c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_smoothers_settle_on_a_constant() {
        let mut e = Ewma::new(1000.0, rise_time(10.0));
        for _ in 0..2000 {
            e.run(0.5);
        }
        assert!((e.last() - 0.5).abs() < 1e-9);
        let mut s = Smf::new_asym(1000.0, rise_time(10.0), 0.1, 0.1);
        let mut y = 0.0;
        for _ in 0..4000 {
            y = s.run_asym(1.0);
        }
        assert!((y - 1.0).abs() < 1e-6);
        let mut e = Ewma::new(1000.0, 0.01);
        assert_eq!(e.run_set_max(0.0), 0.0);
        assert_eq!(e.set(2.0), 2.0);
        assert_eq!(e.run_set_max(1.0), 1.0);
    }
}
