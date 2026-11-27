//! Window functions and the windowed-sinc kernel (`window.c` and `window.h` of the original).

use std::f64::consts::PI;

/// The windows `sinc` can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Rect,
    Hann,
    Hamming,
    Blackman,
    Nuttall4c1,
    Albrecht9c3,
    Kaiser,
}

/// `sin(pi fc x) / (pi x)`, the low-pass impulse response with cut-off `fc` (as a fraction of
/// the sample rate / 2).
pub fn norm_sinc(x: f64, fc: f64) -> f64 {
    if x.abs() < 1e-9 {
        return fc;
    }
    (PI * fc * x).sin() / (PI * x)
}

pub fn hann(x: f64) -> f64 {
    if x >= 1.0 || x <= 0.0 {
        return 0.0;
    }
    0.5 - 0.5 * ((2.0 * PI) * x).cos()
}

pub fn hamming(x: f64) -> f64 {
    if !(0.0..=1.0).contains(&x) {
        return 0.0;
    }
    0.54 - 0.46 * ((2.0 * PI) * x).cos()
}

pub fn blackman(x: f64) -> f64 {
    if x >= 1.0 || x <= 0.0 {
        return 0.0;
    }
    0.42 - 0.5 * ((2.0 * PI) * x).cos() + 0.08 * ((4.0 * PI) * x).cos()
}

/// A sum of cosines with alternating signs: `a[0] - a[1] cos(2 pi x) + a[2] cos(4 pi x) - …`
/// (the signs are written the way the original does, `(i & 1) ? -a : a`).
pub fn cosine_sum(x: f64, a: &[f64]) -> f64 {
    if !(0.0..=1.0).contains(&x) {
        return 0.0;
    }
    let mut w = a[0];
    for (i, &ai) in a.iter().enumerate().skip(1) {
        let c = if i & 1 == 1 { -ai } else { ai };
        w += c * ((2.0 * PI) * i as f64 * x).cos();
    }
    w
}

pub fn nuttall4c1(x: f64) -> f64 {
    const A: [f64; 4] = [0.355768, 0.487396, 0.144232, 0.012604];
    if x >= 1.0 || x <= 0.0 {
        return 0.0;
    }
    cosine_sum(x, &A)
}

#[allow(clippy::excessive_precision)] // the digits of the original
pub fn albrecht9c3(x: f64) -> f64 {
    const A: [f64; 9] = [
        2.318028013590306028393e-1,
        3.932575471789488615081e-1,
        2.385434764970747429454e-1,
        1.014370437785239811268e-1,
        2.911516061918003918645e-2,
        5.280988177252078698806e-3,
        5.382909093381945363528e-4,
        2.442086527507867730168e-5,
        2.706153764205043532817e-7,
    ];
    if x >= 1.0 || x <= 0.0 {
        return 0.0;
    }
    cosine_sum(x, &A)
}

/// The modified Bessel function of the first kind, order 0 (power series).
fn mod_bessel0(x: f64) -> f64 {
    let x_2 = x / 2.0;
    let (mut t, mut s) = (1.0f64, 1.0f64);
    let mut k = 1.0;
    while t > f64::EPSILON * s {
        let y = x_2 / k;
        t *= y * y;
        s += t;
        k += 1.0;
    }
    s
}

pub fn kaiser(x: f64, beta: f64) -> f64 {
    if !(0.0..=1.0).contains(&x) {
        return 0.0;
    }
    let y = 2.0 * x - 1.0;
    mod_bessel0(beta * (1.0 - y * y).sqrt()) / mod_bessel0(beta)
}

/// The width of the main lobe in bins.
pub fn window_width(w: Window, param: f64) -> f64 {
    match w {
        Window::Hann | Window::Hamming => 4.0,
        Window::Blackman => 6.0,
        Window::Nuttall4c1 => 8.0,
        Window::Albrecht9c3 => 18.0,
        Window::Kaiser => 2.0 * (1.0 + (param / PI) * (param / PI)).sqrt(),
        Window::Rect => 2.0,
    }
}

/// The window's value at `x` in 0..1 (`param` is the Kaiser window's beta).
pub fn window(w: Window, x: f64, param: f64) -> f64 {
    match w {
        Window::Hann => hann(x),
        Window::Hamming => hamming(x),
        Window::Blackman => blackman(x),
        Window::Nuttall4c1 => nuttall4c1(x),
        Window::Albrecht9c3 => albrecht9c3(x),
        Window::Kaiser => kaiser(x, param),
        Window::Rect => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_zero_outside_and_peak_in_the_middle() {
        for w in [
            Window::Hann,
            Window::Blackman,
            Window::Nuttall4c1,
            Window::Albrecht9c3,
        ] {
            assert_eq!(window(w, -0.1, 0.0), 0.0);
            assert_eq!(window(w, 1.1, 0.0), 0.0);
            assert!(window(w, 0.5, 0.0) > 0.9, "{w:?}");
        }
        assert!((kaiser(0.5, 12.0) - 1.0).abs() < 1e-12);
        assert!((hamming(0.0) - 0.08).abs() < 1e-12);
        assert_eq!(window(Window::Rect, 0.3, 0.0), 1.0);
    }

    #[test]
    fn sinc_at_zero_is_the_cutoff() {
        assert_eq!(norm_sinc(0.0, 0.25), 0.25);
        assert!((norm_sinc(1.0, 0.5) - (PI / 2.0).sin() / PI).abs() < 1e-15);
    }

    #[test]
    fn kaiser_width_grows_with_beta() {
        assert!(window_width(Window::Kaiser, 12.0) > window_width(Window::Kaiser, 4.0));
        assert_eq!(window_width(Window::Blackman, 0.0), 6.0);
    }
}
