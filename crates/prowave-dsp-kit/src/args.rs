//! Argument parsing with the behaviour of the original's `util.c`: C's `strtod`/`strtol`
//! (a number from the start of a string, and where it ended), `parse_freq` (`1.5k`), and the
//! small `getopt` the effects use for their options.

use crate::error::{Error, Result};

/// C's `strtod` for decimal numbers: skips leading white space, reads a sign, digits, an
/// optional fraction and an optional exponent. Returns the value and the byte index after the
/// number, which is `0` when nothing could be converted.
pub fn strtod(s: &str) -> (f64, usize) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || (9..=13).contains(&b[i])) {
        i += 1;
    }
    let start = i;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let digits_from = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut n_digits = i - digits_from;
    if i < b.len() && b[i] == b'.' {
        let frac_from = i + 1;
        let mut j = frac_from;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if n_digits > 0 || j > frac_from {
            n_digits += j - frac_from;
            i = j;
        }
    }
    if n_digits == 0 {
        return (0.0, 0);
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let exp_from = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_from {
            i = j;
        }
    }
    let v = s[start..i].parse::<f64>().unwrap_or(0.0);
    (v, i)
}

/// C's `strtol` in base 10: value and byte index after the number (`0`: none).
pub fn strtol(s: &str) -> (i64, usize) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || (9..=13).contains(&b[i])) {
        i += 1;
    }
    let start = i;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let from = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == from {
        return (0, 0);
    }
    (s[start..i].parse::<i64>().unwrap_or(i64::MAX), i)
}

/// A number that must fill the whole argument (`GET_ARG` with `CHECK_ENDPTR`).
pub fn get_number(effect: &str, text: &str, param: &str) -> Result<f64> {
    let (v, end) = strtod(text);
    if end == 0 || end != text.len() {
        return Err(Error::Parse {
            effect: effect.into(),
            param: param.into(),
            text: text.into(),
        });
    }
    Ok(v)
}

/// A frequency in Hz with an optional `k` suffix (`parse_freq`), filling the whole argument.
pub fn get_freq(effect: &str, text: &str, param: &str) -> Result<f64> {
    let (mut f, mut end) = strtod(text);
    if end != 0 && text[end..].starts_with('k') {
        f *= 1000.0;
        end += 1;
    }
    if end == 0 || end != text.len() {
        return Err(Error::Parse {
            effect: effect.into(),
            param: param.into(),
            text: text.into(),
        });
    }
    Ok(f)
}

/// `CHECK_FREQ`: `0 <= f < fs / 2`.
pub fn check_freq(effect: &str, f: f64, fs: u32, param: &str) -> Result<()> {
    if f >= 0.0 && f < f64::from(fs) / 2.0 {
        Ok(())
    } else {
        Err(Error::Range {
            effect: effect.into(),
            param: param.into(),
        })
    }
}

/// The original's `dsp_getopt`: options with an optional (`x::`) or required (`x:`) argument,
/// clustered the way `getopt` does, stopping at the first word that is not an option.
#[derive(Clone, Debug)]
pub struct Getopt {
    /// The argument of the option just returned.
    pub arg: Option<String>,
    /// The index of the next word.
    pub ind: usize,
    /// The option character just seen.
    pub opt: char,
    sp: usize,
}

impl Default for Getopt {
    fn default() -> Self {
        Self {
            arg: None,
            ind: 1,
            opt: '\0',
            sp: 1,
        }
    }
}

impl Getopt {
    /// Next option among `argv[..argc]`, or `None` at the end. An unknown option gives
    /// `Some('?')` and a missing required argument `Some(':')`.
    pub fn next(&mut self, argc: usize, argv: &[&str], opts: &str) -> Option<char> {
        if self.sp == 1 {
            if self.ind >= argc {
                return None;
            }
            let w = argv[self.ind];
            if !(w.starts_with('-') && w.len() > 1) {
                return None;
            }
            if w == "--" {
                self.ind += 1;
                return None;
            }
        }
        let word = argv[self.ind].as_bytes();
        let c = word[self.sp] as char;
        self.opt = c;
        let spec = opts.find(c).filter(|_| c != ':');
        let Some(pos) = spec else {
            self.sp += 1;
            if self.sp >= word.len() {
                self.ind += 1;
                self.sp = 1;
            }
            return Some('?');
        };
        let ob = opts.as_bytes();
        if ob.get(pos + 1) == Some(&b':') {
            if self.sp + 1 < word.len() {
                self.arg = Some(argv[self.ind][self.sp + 1..].to_string());
                self.ind += 1;
            } else if ob.get(pos + 2) == Some(&b':') {
                self.ind += 1;
                self.arg = None;
            } else {
                self.ind += 1;
                if self.ind >= argc {
                    self.sp = 1;
                    return Some(':');
                }
                self.arg = Some(argv[self.ind].to_string());
                self.ind += 1;
            }
            self.sp = 1;
        } else {
            self.sp += 1;
            if self.sp >= word.len() {
                self.ind += 1;
                self.sp = 1;
            }
            self.arg = None;
        }
        Some(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strtod_reads_a_number_from_the_front_like_c() {
        assert_eq!(strtod("1.5k"), (1.5, 3));
        assert_eq!(strtod("  -2e3x"), (-2000.0, 6));
        assert_eq!(strtod(".5"), (0.5, 2));
        assert_eq!(strtod("5."), (5.0, 2));
        assert_eq!(strtod("1e"), (1.0, 1));
        assert_eq!(strtod("1e+"), (1.0, 1));
        assert_eq!(strtod("q"), (0.0, 0));
        assert_eq!(strtod("-"), (0.0, 0));
        assert_eq!(strtod("."), (0.0, 0));
        assert_eq!(strtod(""), (0.0, 0));
        assert_eq!(strtol("-12x"), (-12, 3));
        assert_eq!(strtol("x"), (0, 0));
    }

    #[test]
    fn frequencies_take_a_k_suffix_and_nothing_else() {
        assert_eq!(get_freq("e", "1k", "f0").ok(), Some(1000.0));
        assert_eq!(get_freq("e", "2.5k", "f0").ok(), Some(2500.0));
        assert_eq!(get_freq("e", "440", "f0").ok(), Some(440.0));
        assert!(get_freq("e", "1kk", "f0").is_err());
        assert!(get_freq("e", "1x", "f0").is_err());
        assert!(get_freq("e", "k", "f0").is_err());
        assert!(check_freq("e", 24_000.0, 48_000, "f0").is_err());
        assert!(check_freq("e", 23_999.0, 48_000, "f0").is_ok());
        assert!(check_freq("e", -1.0, 48_000, "f0").is_err());
    }

    #[test]
    fn getopt_walks_clustered_options_with_optional_arguments() {
        let argv = ["lowpass", "-r", "1k", "0.7q"];
        let mut g = Getopt::default();
        assert_eq!(g.next(4, &argv, "r::"), Some('r'));
        assert_eq!(g.arg, None);
        assert_eq!(g.next(4, &argv, "r::"), None);
        assert_eq!(g.ind, 2);
        let argv = ["lowpass", "-r120", "1k", "0.7q"];
        let mut g = Getopt::default();
        assert_eq!(g.next(4, &argv, "r::"), Some('r'));
        assert_eq!(g.arg.as_deref(), Some("120"));
        assert_eq!(g.next(4, &argv, "r::"), None);
        let argv = ["x", "-z"];
        let mut g = Getopt::default();
        assert_eq!(g.next(2, &argv, "r::"), Some('?'));
        assert_eq!(g.opt, 'z');
        // A negative number is an option unless the caller keeps it out of `argc`.
        let argv = ["gain", "-3"];
        let mut g = Getopt::default();
        assert_eq!(g.next(1, &argv, "r::"), None);
    }
}
