//! Sends, buses and automation (Logic: the channel strip's Sends, Aux tracks listening to a
//! bus, and Volume/Pan automation).
//!
//! Plain data, made on the UI thread and handed to the audio thread by [`crate::Command`]s.

/// Sends in one channel strip.
pub const MAX_SENDS: usize = 4;
/// Buses a send can feed (Logic's Bus 1..8); an Aux track listens to one of them.
pub const MAX_BUSES: usize = 8;

/// One send of a channel strip.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SendSpec {
    /// Bus number 1..=[`MAX_BUSES`]; 0 is an empty slot.
    pub bus: u8,
    /// 0..=1, the knob position (see [`send_gain`]).
    pub level: f32,
    /// Taken before the fader and pan (the eye in Logic) instead of after.
    pub pre: bool,
}

/// Linear gain of a send knob at `level` (0..=1): silent at 0, about -6 dB at one half,
/// unity at 0.71 and +6 dB at the top.
pub fn send_gain(level: f32) -> f32 {
    if level.is_finite() {
        let l = level.clamp(0.0, 1.0);
        2.0 * l * l
    } else {
        0.0
    }
}

/// Where a channel strip's signal goes besides the output.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Route {
    /// The bus an Aux strip takes as its input (1..=[`MAX_BUSES`]); 0 for every other strip.
    pub bus_in: u8,
    pub sends: [SendSpec; MAX_SENDS],
}

impl Route {
    /// The same route with every bus number put into range.
    pub fn sanitized(mut self) -> Self {
        let ok = |b: u8| b as usize <= MAX_BUSES;
        if !ok(self.bus_in) {
            self.bus_in = 0;
        }
        for s in self.sends.iter_mut() {
            if !ok(s.bus) {
                s.bus = 0;
            }
            s.level = if s.level.is_finite() {
                s.level.clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
        self
    }

    /// Whether the strip sends anything.
    pub fn has_sends(&self) -> bool {
        self.sends.iter().any(|s| s.bus > 0)
    }
}

/// What an automation lane moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AutoTarget {
    /// The fader, in dB.
    #[default]
    Volume,
    /// The pan knob, -1 (left) to 1 (right).
    Pan,
    /// Parameter `param` of the effect in slot `slot` of the strip, as a position 0..=1
    /// along the parameter's range (see `ParamInfo::from_norm`).
    Fx { slot: u8, param: u8 },
}

/// One point of an automation lane. `curve` bends the stretch from this point to the next:
/// 0 is a straight line, above 0 it stays near this point's value and then moves (slow
/// start), below 0 it moves first and then settles (fast start); -1..=1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AutoPoint {
    pub beat: f64,
    pub value: f32,
    pub curve: f32,
}

impl AutoPoint {
    /// A point joined to the next one by a straight line.
    pub fn new(beat: f64, value: f32) -> Self {
        Self {
            beat,
            value,
            curve: 0.0,
        }
    }
}

/// How far along a bent stretch the value is when a straight one would be at `t` (0..=1).
pub fn curve_shape(t: f32, curve: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let c = if curve.is_finite() {
        curve.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    if c > 0.0 {
        t.powf(1.0 + 4.0 * c)
    } else if c < 0.0 {
        1.0 - (1.0 - t).powf(1.0 - 4.0 * c)
    } else {
        t
    }
}

/// The automation of one parameter of one strip: points in beats, sorted, joined by straight
/// lines; before the first point and after the last it keeps their value.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoLane {
    pub track: u16,
    pub target: AutoTarget,
    /// The strip plays the lane (Read, or Touch and Latch while the fader is let go); else its
    /// own fader or knob rules.
    pub follow: bool,
    /// Added to a Volume lane's value, in dB (the level of the strip's VCA).
    pub offset_db: f32,
    pub points: Vec<AutoPoint>,
}

impl AutoLane {
    /// The value at `beat`, or `None` for a lane without points.
    pub fn value_at(&self, beat: f64) -> Option<f32> {
        lane_value(&self.points, beat)
    }

    /// The lane with its points made safe: finite, in order, values in range, at most
    /// `MAX_AUTO_POINTS` of them.
    pub fn sanitized(mut self) -> Self {
        let (lo, hi) = match self.target {
            AutoTarget::Volume => (-96.0, 6.0),
            AutoTarget::Pan => (-1.0, 1.0),
            AutoTarget::Fx { .. } => (0.0, 1.0),
        };
        self.points
            .retain(|p| p.beat.is_finite() && p.beat >= 0.0 && p.value.is_finite());
        for p in self.points.iter_mut() {
            p.value = p.value.clamp(lo, hi);
            p.curve = if p.curve.is_finite() {
                p.curve.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
        self.points.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        self.points.truncate(MAX_AUTO_POINTS);
        if !self.offset_db.is_finite() {
            self.offset_db = 0.0;
        }
        self
    }
}

/// The value at `beat` of points in order of beat, joined by lines (bent by each point's
/// `curve`); before the first and after the last it holds their value. `None` without points.
pub fn lane_value(pts: &[AutoPoint], beat: f64) -> Option<f32> {
    let first = pts.first()?;
    let last = pts.last()?;
    if beat <= first.beat {
        return Some(first.value);
    }
    if beat >= last.beat {
        return Some(last.value);
    }
    let i = pts.partition_point(|p| p.beat <= beat);
    let (a, b) = (pts[i - 1], pts[i]);
    let span = b.beat - a.beat;
    if span <= 0.0 {
        return Some(b.value);
    }
    let t = curve_shape(((beat - a.beat) / span) as f32, a.curve);
    Some(a.value + (b.value - a.value) * t)
}

/// Most points one lane keeps.
pub const MAX_AUTO_POINTS: usize = 20_000;

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(points: &[(f64, f32)]) -> AutoLane {
        AutoLane {
            track: 0,
            target: AutoTarget::Volume,
            follow: true,
            offset_db: 0.0,
            points: points.iter().map(|p| AutoPoint::new(p.0, p.1)).collect(),
        }
    }

    #[test]
    fn a_bent_stretch_starts_slow_or_fast_and_still_joins_its_ends() {
        let mut l = lane(&[(0.0, 0.0), (10.0, 10.0)]);
        assert_eq!(l.value_at(5.0), Some(5.0));
        l.points[0].curve = 1.0;
        let slow = l.value_at(5.0).unwrap_or(0.0);
        assert!(slow < 1.0, "slow start: {slow}");
        l.points[0].curve = -1.0;
        let fast = l.value_at(5.0).unwrap_or(0.0);
        assert!(fast > 9.0, "fast start: {fast}");
        for c in [-1.0, -0.4, 0.0, 0.7, 1.0] {
            l.points[0].curve = c;
            assert_eq!(l.value_at(0.0), Some(0.0));
            assert_eq!(l.value_at(10.0), Some(10.0));
            let mut last = -1.0;
            for k in 0..=20 {
                let v = l.value_at(k as f64 * 0.5).unwrap_or(f32::NAN);
                assert!(v >= last, "never goes back: {c} {k}");
                last = v;
            }
        }
        assert_eq!(curve_shape(0.5, f32::NAN), 0.5);
        assert_eq!(curve_shape(2.0, 0.5), 1.0);
        // Damaged curves are put into range.
        l.points[0].curve = 40.0;
        l.points[1].curve = f32::NAN;
        let l = l.sanitized();
        assert_eq!((l.points[0].curve, l.points[1].curve), (1.0, 0.0));
        // A lane of an effect parameter holds positions 0..1.
        let mut f = lane(&[(0.0, 4.0), (1.0, -3.0)]);
        f.target = AutoTarget::Fx { slot: 1, param: 2 };
        let f = f.sanitized();
        assert_eq!((f.points[0].value, f.points[1].value), (1.0, 0.0));
    }

    #[test]
    fn a_lane_holds_before_and_after_and_joins_points_with_lines() {
        let l = lane(&[(4.0, -6.0), (8.0, 0.0), (12.0, -60.0)]);
        assert_eq!(l.value_at(0.0), Some(-6.0));
        assert_eq!(l.value_at(6.0), Some(-3.0));
        assert_eq!(l.value_at(8.0), Some(0.0));
        assert_eq!(l.value_at(10.0), Some(-30.0));
        assert_eq!(l.value_at(99.0), Some(-60.0));
        assert_eq!(lane(&[]).value_at(1.0), None);
        // Two points at the same beat make a jump, not a division by zero.
        let j = lane(&[(2.0, 0.0), (2.0, -20.0)]);
        assert!(j.value_at(2.0).is_some_and(f32::is_finite));
    }

    #[test]
    fn damaged_lanes_and_routes_are_made_safe() {
        let l = lane(&[
            (f64::NAN, 1.0),
            (3.0, f32::NAN),
            (2.0, 100.0),
            (1.0, -500.0),
            (-1.0, 0.0),
        ])
        .sanitized();
        assert_eq!(
            l.points,
            [AutoPoint::new(1.0, -96.0), AutoPoint::new(2.0, 6.0)]
        );
        let r = Route {
            bus_in: 200,
            sends: [
                SendSpec {
                    bus: 9,
                    level: f32::NAN,
                    pre: true,
                },
                SendSpec {
                    bus: 3,
                    level: 5.0,
                    pre: false,
                },
                SendSpec::default(),
                SendSpec::default(),
            ],
        }
        .sanitized();
        assert_eq!((r.bus_in, r.sends[0].bus, r.sends[0].level), (0, 0, 0.0));
        assert_eq!((r.sends[1].bus, r.sends[1].level), (3, 1.0));
        assert!(r.has_sends() && !Route::default().has_sends());
    }

    #[test]
    fn the_send_knob_is_silent_at_zero_and_unity_near_three_quarters() {
        assert_eq!(send_gain(0.0), 0.0);
        assert!((send_gain(std::f32::consts::FRAC_1_SQRT_2) - 1.0).abs() < 0.001);
        assert!((send_gain(1.0) - 2.0).abs() < 1e-6);
        assert_eq!(send_gain(f32::NAN), 0.0);
        assert_eq!(send_gain(-4.0), 0.0);
    }
}
