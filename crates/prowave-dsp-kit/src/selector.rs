//! Channel selectors (`parse_selector` in the original): `0,2`, `1-3`, `-` or an empty string
//! for all channels, `2-` for channel 2 and up.

use crate::error::{Error, Result};

fn set_range(b: &mut [bool], start: i64, end: i64, dash: bool) {
    let n = b.len() as i64;
    let (mut start, mut end) = (start, end);
    if start == -1 && end == -1 {
        start = 0;
        end = n - 1;
    } else if start == -1 {
        start = 0;
    } else if end == -1 {
        end = if dash { n - 1 } else { start };
    }
    while start <= end {
        b[start as usize] = true;
        start += 1;
    }
}

/// Parses `s` for a stream of `n` channels.
pub fn parse_selector(s: &str, n: usize) -> Result<Vec<bool>> {
    let mut b = vec![false; n];
    if s.is_empty() || s == "-" {
        b.iter_mut().for_each(|x| *x = true);
        return Ok(b);
    }
    let (mut start, mut end, mut dash) = (-1i64, -1i64, false);
    let bytes = s.as_bytes();
    let mut i = 0;
    let err = |m: String| Error::Message(format!("parse_selector: {m}"));
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            let from = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let v: i64 = s[from..i].parse().unwrap_or(i64::MAX);
            if v > n as i64 - 1 || v < 0 {
                return Err(err(format!("error: value out of range: {v}")));
            }
            if dash {
                if v < start {
                    return Err(err(format!(
                        "error: malformed range: {}-{v}",
                        if start == -1 { 0 } else { start }
                    )));
                }
                end = v;
            } else {
                start = v;
            }
        } else if c == b'-' {
            if dash {
                return Err(err("syntax error: '-' unexpected".into()));
            }
            dash = true;
            i += 1;
        } else if c == b',' {
            if start == -1 && end == -1 && !dash {
                return Err(err("syntax error: ',' unexpected".into()));
            }
            set_range(&mut b, start, end, dash);
            start = -1;
            end = -1;
            dash = false;
            i += 1;
        } else {
            return Err(err(format!(
                "syntax error: invalid character: {}",
                c as char
            )));
        }
    }
    if start == -1 && end == -1 && !dash {
        return Err(err("syntax error: ',' unexpected".into()));
    }
    set_range(&mut b, start, end, dash);
    Ok(b)
}

/// `parse_selector_masked`: the selector counts only the channels set in `mask`.
pub fn parse_selector_masked(s: &str, mask: &[bool]) -> Result<Vec<bool>> {
    let nb = mask.iter().filter(|m| **m).count();
    let inner = parse_selector(s, nb)?;
    let mut b = vec![false; mask.len()];
    let mut k = 0;
    for set in inner {
        while k < mask.len() && !mask[k] {
            k += 1;
        }
        if k == mask.len() {
            return Err(Error::Message(
                "parse_selector_masked(): BUG: too many channels".into(),
            ));
        }
        if set {
            b[k] = true;
        }
        k += 1;
    }
    Ok(b)
}

/// How many channels a selector has set (`num_bits_set`).
pub fn num_bits_set(b: &[bool]) -> usize {
    b.iter().filter(|x| **x).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(s: &str, n: usize) -> Vec<u8> {
        parse_selector(s, n)
            .map(|b| b.into_iter().map(u8::from).collect())
            .unwrap_or_default()
    }

    #[test]
    fn selectors_follow_the_original_syntax() {
        assert_eq!(sel("", 4), vec![1, 1, 1, 1]);
        assert_eq!(sel("-", 3), vec![1, 1, 1]);
        assert_eq!(sel("0,2", 4), vec![1, 0, 1, 0]);
        assert_eq!(sel("1-2", 4), vec![0, 1, 1, 0]);
        assert_eq!(sel("2-", 4), vec![0, 0, 1, 1]);
        assert_eq!(sel("-1", 4), vec![1, 1, 0, 0]);
        assert_eq!(sel("0-1,3", 4), vec![1, 1, 0, 1]);
        assert!(parse_selector("4", 4).is_err());
        assert!(parse_selector("2-1", 4).is_err());
        assert!(parse_selector("1,,2", 4).is_err());
        assert!(parse_selector("1--2", 4).is_err());
        assert!(parse_selector("a", 4).is_err());
        assert!(parse_selector("1,", 4).is_err());
    }

    #[test]
    fn a_masked_selector_counts_only_the_channels_in_the_mask() {
        let mask = [false, true, false, true, true];
        // Among the three channels 1, 3 and 4: the second one is channel 3.
        let b = parse_selector_masked("1", &mask).unwrap_or_default();
        assert_eq!(b, vec![false, false, false, true, false]);
        let b = parse_selector_masked("1-", &mask).unwrap_or_default();
        assert_eq!(b, vec![false, false, false, true, true]);
        assert_eq!(num_bits_set(&b), 2);
    }
}
