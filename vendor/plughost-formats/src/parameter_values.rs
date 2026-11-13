//! The rule every format applies to the parameter ranges and values a plugin reports: one that
//! is not finite has no normalized form, so the call that needs it fails, and a value outside its
//! range is clamped to it. A value a plugin reports as an edit is clamped only when it is past an
//! end by rounding; one further out is not a value of the parameter, and the format asks the
//! application to read the values again instead of reporting it.

/// `min..=max` when it is a range: finite, in order, and with a finite span.
pub(crate) fn range(min: f64, max: f64) -> Option<(f64, f64)> {
    (min.is_finite() && max.is_finite() && min <= max && (max - min).is_finite())
        .then_some((min, max))
}

/// `plain` normalized over `min..=max` and clamped to it, or `None` when either is not finite. A
/// range of one value normalizes to 0.
pub(crate) fn normalized(plain: f64, min: f64, max: f64) -> Option<f64> {
    let (min, max) = range(min, max)?;
    if !plain.is_finite() {
        return None;
    }
    Some(if max > min {
        ((plain - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
    })
}

/// How far past an end of its range a plugin's reported value may be and still be that end:
/// rounding in its arithmetic, as a share of the range's span (or of 1, for a narrower range).
const ROUNDING: f64 = 1e-5;

/// `plain` normalized over `min..=max` when it lies in the range or past an end by rounding,
/// which is clamped; `None` when it is further out or either is not finite.
pub(crate) fn reported(plain: f64, min: f64, max: f64) -> Option<f64> {
    let (min, max) = range(min, max)?;
    let slack = ROUNDING * (max - min).max(1.0);
    (plain >= min - slack && plain <= max + slack)
        .then(|| normalized(plain, min, max))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reported_value_past_an_end_by_rounding_is_that_end() {
        assert_eq!(reported(1.000_000_1, 0.0, 1.0), Some(1.0));
        assert_eq!(reported(-0.000_001, 0.0, 1.0), Some(0.0));
        assert_eq!(reported(20_000.05, 20.0, 20_000.0), Some(1.0));
        assert_eq!(reported(0.5, 0.0, 1.0), Some(0.5));
    }

    #[test]
    fn a_reported_value_further_out_is_not_a_value() {
        assert_eq!(reported(1.5, 0.0, 1.0), None);
        assert_eq!(reported(-0.01, 0.0, 1.0), None);
        assert_eq!(reported(20_001.0, 20.0, 20_000.0), None);
        assert_eq!(reported(f64::NAN, 0.0, 1.0), None);
        assert_eq!(reported(0.5, 1.0, 0.0), None);
    }
}
