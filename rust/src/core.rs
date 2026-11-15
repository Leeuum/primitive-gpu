//! Exact CPU versions of the color / draw / difference math.
//! Images are tightly packed RGBA8, premultiplied.

use crate::raster::Scanline;

// The color and blend math below exists a second time in common.wgsl (the
// GPU copy). The two must give identical whole-number results. The matching
// names there are given on each function. Run ./check.sh after any change.

const M: u32 = 0xffff;

/// How the gap between the target and the drawing is counted.
/// `Squared`: each channel gap squared (the classic score, root mean square).
/// `Plain`: each channel gap as it is, without squaring (mean absolute gap).
/// The best color is the average in both cases, so `Plain` only changes
/// which shape wins, not the color math. common.wgsl: the `PLAIN` constant
/// that gpu.rs puts in front of the shaders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Metric {
    #[default]
    Squared,
    Plain,
}

impl Metric {
    pub fn parse(s: &str) -> Option<Metric> {
        match s {
            "squared" | "sq" | "rms" => Some(Metric::Squared),
            "plain" | "abs" => Some(Metric::Plain),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Metric::Squared => "squared",
            Metric::Plain => "plain",
        }
    }

    /// The error of one channel gap `d`.
    #[inline]
    pub fn gap(self, d: i64) -> i64 {
        match self {
            Metric::Squared => d * d,
            Metric::Plain => d.abs(),
        }
    }
}

/// One channel of the best color from the sums over the shape's pixels:
/// `sd` = sum of (target - current), `sc` = sum of current.
/// common.wgsl: best_color_channel.
fn best_color_channel(sd: i64, sc: i64, count: i64, alpha: i32) -> u8 {
    let a = (0x101 * 255 / alpha) as i64;
    (((sd * a + sc * 0x101) / count) >> 8).clamp(0, 255) as u8
}

/// The color as 16-bit premultiplied [r, g, b, a], ready for blending.
/// common.wgsl: `src` and `sa`, set at the end of finish_color.
fn blend_source(c: [u8; 4]) -> [u32; 4] {
    let ca = c[3] as u32;
    let f = |v: u8| (v as u32 * 0x101) * ca / 0xff;
    [f(c[0]), f(c[1]), f(c[2]), ca * 0x101]
}

/// How much of the old pixel stays, for source alpha `sa` and coverage `ma`.
/// common.wgsl: `af` in visit.
fn keep_factor(sa: u32, ma: u32) -> u32 {
    (M - sa * ma / M) * 0x101
}

/// One blended channel: old value `d`, source channel `s`.
/// common.wgsl: `o` in visit.
fn blend_channel(d: u32, s: u32, keep: u32, ma: u32) -> u8 {
    ((d * keep + s * ma) / M >> 8) as u8
}

/// Best color for a shape. Returns [r, g, b, a].
pub fn compute_color(target: &[u8], current: &[u8], w: i32, lines: &[Scanline], alpha: i32) -> [u8; 4] {
    let (mut sd, mut sc, mut count) = ([0i64; 3], [0i64; 3], 0i64);
    for l in lines {
        let mut i = ((l.y * w + l.x1) * 4) as usize;
        for _ in l.x1..=l.x2 {
            for k in 0..3 {
                sd[k] += target[i + k] as i64 - current[i + k] as i64;
                sc[k] += current[i + k] as i64;
            }
            i += 4;
            count += 1;
        }
    }
    if count == 0 {
        return [0, 0, 0, 0];
    }
    let f = |k: usize| best_color_channel(sd[k], sc[k], count, alpha);
    [f(0), f(1), f(2), alpha as u8]
}

/// Blend a color into the image along the scanlines.
pub fn draw_lines(im: &mut [u8], w: i32, c: [u8; 4], lines: &[Scanline]) {
    let s = blend_source(c);
    for l in lines {
        let ma = l.alpha;
        let keep = keep_factor(s[3], ma);
        let mut i = ((l.y * w + l.x1) * 4) as usize;
        for _ in l.x1..=l.x2 {
            for k in 0..4 {
                im[i + k] = blend_channel(im[i + k] as u32, s[k], keep, ma);
            }
            i += 4;
        }
    }
}

/// Exact change in the error total if a shape (given by its
/// scanlines) were drawn with its best color. Used by `-check`.
pub fn delta_lines(m: Metric, target: &[u8], current: &[u8], w: i32, lines: &[Scanline], alpha: i32) -> i64 {
    let s = blend_source(compute_color(target, current, w, lines, alpha));
    let mut t = 0i64;
    for l in lines {
        let ma = l.alpha;
        let keep = keep_factor(s[3], ma);
        let mut i = ((l.y * w + l.x1) * 4) as usize;
        for _ in l.x1..=l.x2 {
            for k in 0..4 {
                let d = current[i + k] as i64;
                let o = blend_channel(current[i + k] as u32, s[k], keep, ma) as i64;
                let d1 = target[i + k] as i64 - d;
                let d2 = target[i + k] as i64 - o;
                t += m.gap(d2) - m.gap(d1);
            }
            i += 4;
        }
    }
    t
}

/// Sum of channel errors over the whole image.
pub fn difference_full(m: Metric, a: &[u8], b: &[u8]) -> u64 {
    a.iter().zip(b).map(|(&x, &y)| m.gap(x as i64 - y as i64) as u64).sum()
}

/// Update an error total after `before` became `after` along `lines`.
pub fn difference_partial(m: Metric, target: &[u8], before: &[u8], after: &[u8], w: i32, total: u64, lines: &[Scanline]) -> u64 {
    let mut t = total as i64;
    for l in lines {
        let s = ((l.y * w + l.x1) * 4) as usize;
        let e = ((l.y * w + l.x2 + 1) * 4) as usize;
        for i in s..e {
            let d1 = target[i] as i64 - before[i] as i64;
            let d2 = target[i] as i64 - after[i] as i64;
            t += m.gap(d2) - m.gap(d1);
        }
    }
    t.max(0) as u64
}

/// The printed score in 0..1: root mean square error (`Squared`) or mean
/// absolute error (`Plain`).
pub fn score_of(m: Metric, total: u64, w: i32, h: i32) -> f64 {
    let mean = total as f64 / (w as f64 * h as f64 * 4.0);
    match m {
        Metric::Squared => mean.sqrt() / 255.0,
        Metric::Plain => mean / 255.0,
    }
}
