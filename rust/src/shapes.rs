//! Shape types: random creation, mutation, rasterizing to scanlines,
//! SVG output and high-resolution drawing.

use crate::gpu::GpuCand;
use crate::raster::{crop_scanlines, fill_polygon, stroke_quadratic, Scanline, Scratch};
use crate::rng::Rng;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum Shape {
    Triangle { x1: i32, y1: i32, x2: i32, y2: i32, x3: i32, y3: i32 },
    Rectangle { x1: i32, y1: i32, x2: i32, y2: i32 },
    Ellipse { x: i32, y: i32, rx: i32, ry: i32, circle: bool },
    RotatedRectangle { x: i32, y: i32, sx: i32, sy: i32, angle: i32 },
    Quadratic { x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64, width: f64 },
    RotatedEllipse { x: f64, y: f64, rx: f64, ry: f64, angle: f64 },
    Polygon { x: [f64; 4], y: [f64; 4] },
}

/// Clamp an integer (lo wins if lo > hi).
#[inline]
fn clampi(x: i32, lo: i32, hi: i32) -> i32 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

#[inline]
fn clampf(x: f64, lo: f64, hi: f64) -> f64 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

/// int(rnd.NormFloat64() * s) — truncates toward zero.
#[inline]
fn ni(rng: &mut Rng, s: f64) -> i32 {
    (rng.norm() * s) as i32
}

pub const MODE_NAMES: &str =
    "0=combo 1=triangle 2=rect 3=ellipse 4=circle 5=rotatedrect 6=beziers 7=rotatedellipse 8=polygon";

impl Shape {
    pub fn random(t: i32, w: i32, h: i32, rng: &mut Rng) -> Shape {
        let mut s = match t {
            1 => {
                let x1 = rng.intn(w);
                let y1 = rng.intn(h);
                let x2 = x1 + rng.intn(31) - 15;
                let y2 = y1 + rng.intn(31) - 15;
                let x3 = x1 + rng.intn(31) - 15;
                let y3 = y1 + rng.intn(31) - 15;
                Shape::Triangle { x1, y1, x2, y2, x3, y3 }
            }
            2 => {
                let x1 = rng.intn(w);
                let y1 = rng.intn(h);
                let x2 = clampi(x1 + rng.intn(32) + 1, 0, w - 1);
                let y2 = clampi(y1 + rng.intn(32) + 1, 0, h - 1);
                return Shape::Rectangle { x1, y1, x2, y2 };
            }
            3 => {
                let x = rng.intn(w);
                let y = rng.intn(h);
                let rx = rng.intn(32) + 1;
                let ry = rng.intn(32) + 1;
                return Shape::Ellipse { x, y, rx, ry, circle: false };
            }
            4 => {
                let x = rng.intn(w);
                let y = rng.intn(h);
                let r = rng.intn(32) + 1;
                return Shape::Ellipse { x, y, rx: r, ry: r, circle: true };
            }
            5 => {
                let x = rng.intn(w);
                let y = rng.intn(h);
                let sx = rng.intn(32) + 1;
                let sy = rng.intn(32) + 1;
                let angle = rng.intn(360);
                Shape::RotatedRectangle { x, y, sx, sy, angle }
            }
            6 => {
                let x1 = rng.f64() * w as f64;
                let y1 = rng.f64() * h as f64;
                let x2 = x1 + rng.f64() * 40.0 - 20.0;
                let y2 = y1 + rng.f64() * 40.0 - 20.0;
                let x3 = x2 + rng.f64() * 40.0 - 20.0;
                let y3 = y2 + rng.f64() * 40.0 - 20.0;
                Shape::Quadratic { x1, y1, x2, y2, x3, y3, width: 0.5 }
            }
            7 => {
                let x = rng.f64() * w as f64;
                let y = rng.f64() * h as f64;
                let rx = rng.f64() * 32.0 + 1.0;
                let ry = rng.f64() * 32.0 + 1.0;
                let angle = rng.f64() * 360.0;
                return Shape::RotatedEllipse { x, y, rx, ry, angle };
            }
            8 => {
                let mut x = [0.0; 4];
                let mut y = [0.0; 4];
                x[0] = rng.f64() * w as f64;
                y[0] = rng.f64() * h as f64;
                for i in 1..4 {
                    x[i] = x[0] + rng.f64() * 40.0 - 20.0;
                    y[i] = y[0] + rng.f64() * 40.0 - 20.0;
                }
                Shape::Polygon { x, y }
            }
            _ => return Shape::random(rng.intn(8) + 1, w, h, rng),
        };
        s.mutate(w, h, rng);
        s
    }

    pub fn mutate(&mut self, w: i32, h: i32, rng: &mut Rng) {
        const M: i32 = 16;
        const MF: f64 = 16.0;
        match self {
            Shape::Triangle { x1, y1, x2, y2, x3, y3 } => loop {
                match rng.intn(3) {
                    0 => {
                        *x1 = clampi(*x1 + ni(rng, 16.0), -M, w - 1 + M);
                        *y1 = clampi(*y1 + ni(rng, 16.0), -M, h - 1 + M);
                    }
                    1 => {
                        *x2 = clampi(*x2 + ni(rng, 16.0), -M, w - 1 + M);
                        *y2 = clampi(*y2 + ni(rng, 16.0), -M, h - 1 + M);
                    }
                    _ => {
                        *x3 = clampi(*x3 + ni(rng, 16.0), -M, w - 1 + M);
                        *y3 = clampi(*y3 + ni(rng, 16.0), -M, h - 1 + M);
                    }
                }
                if triangle_valid(*x1, *y1, *x2, *y2, *x3, *y3) {
                    break;
                }
            },
            Shape::Rectangle { x1, y1, x2, y2 } => match rng.intn(2) {
                0 => {
                    *x1 = clampi(*x1 + ni(rng, 16.0), 0, w - 1);
                    *y1 = clampi(*y1 + ni(rng, 16.0), 0, h - 1);
                }
                _ => {
                    *x2 = clampi(*x2 + ni(rng, 16.0), 0, w - 1);
                    *y2 = clampi(*y2 + ni(rng, 16.0), 0, h - 1);
                }
            },
            Shape::Ellipse { x, y, rx, ry, circle } => match rng.intn(3) {
                0 => {
                    *x = clampi(*x + ni(rng, 16.0), 0, w - 1);
                    *y = clampi(*y + ni(rng, 16.0), 0, h - 1);
                }
                1 => {
                    *rx = clampi(*rx + ni(rng, 16.0), 1, w - 1);
                    if *circle {
                        *ry = *rx;
                    }
                }
                _ => {
                    *ry = clampi(*ry + ni(rng, 16.0), 1, h - 1);
                    if *circle {
                        *rx = *ry;
                    }
                }
            },
            Shape::RotatedRectangle { x, y, sx, sy, angle } => match rng.intn(3) {
                0 => {
                    *x = clampi(*x + ni(rng, 16.0), 0, w - 1);
                    *y = clampi(*y + ni(rng, 16.0), 0, h - 1);
                }
                1 => {
                    *sx = clampi(*sx + ni(rng, 16.0), 1, w - 1);
                    *sy = clampi(*sy + ni(rng, 16.0), 1, h - 1);
                }
                _ => *angle += ni(rng, 32.0),
            },
            Shape::Quadratic { x1, y1, x2, y2, x3, y3, .. } => loop {
                match rng.intn(3) {
                    0 => {
                        *x1 = clampf(*x1 + rng.norm() * 16.0, -MF, (w - 1) as f64 + MF);
                        *y1 = clampf(*y1 + rng.norm() * 16.0, -MF, (h - 1) as f64 + MF);
                    }
                    1 => {
                        *x2 = clampf(*x2 + rng.norm() * 16.0, -MF, (w - 1) as f64 + MF);
                        *y2 = clampf(*y2 + rng.norm() * 16.0, -MF, (h - 1) as f64 + MF);
                    }
                    _ => {
                        *x3 = clampf(*x3 + rng.norm() * 16.0, -MF, (w - 1) as f64 + MF);
                        *y3 = clampf(*y3 + rng.norm() * 16.0, -MF, (h - 1) as f64 + MF);
                    }
                }
                if quadratic_valid(*x1, *y1, *x2, *y2, *x3, *y3) {
                    break;
                }
            },
            Shape::RotatedEllipse { x, y, rx, ry, angle } => match rng.intn(3) {
                0 => {
                    *x = clampf(*x + rng.norm() * 16.0, 0.0, (w - 1) as f64);
                    *y = clampf(*y + rng.norm() * 16.0, 0.0, (h - 1) as f64);
                }
                1 => {
                    *rx = clampf(*rx + rng.norm() * 16.0, 1.0, (w - 1) as f64);
                    *ry = clampf(*ry + rng.norm() * 16.0, 1.0, (h - 1) as f64);
                }
                _ => *angle += rng.norm() * 32.0,
            },
            Shape::Polygon { x, y } => {
                // Non-convex polygons are always valid, so one move is enough.
                if rng.f64() < 0.25 {
                    let i = rng.intn(4) as usize;
                    let j = rng.intn(4) as usize;
                    x.swap(i, j);
                    y.swap(i, j);
                } else {
                    let i = rng.intn(4) as usize;
                    x[i] = clampf(x[i] + rng.norm() * 16.0, -MF, (w - 1) as f64 + MF);
                    y[i] = clampf(y[i] + rng.norm() * 16.0, -MF, (h - 1) as f64 + MF);
                }
            }
        }
    }

    /// Fill `sc.lines` with the shape's scanlines, cropped to the image.
    pub fn rasterize(&self, w: i32, h: i32, sc: &mut Scratch) {
        sc.lines.clear();
        match *self {
            Shape::Triangle { x1, y1, x2, y2, x3, y3 } => {
                rasterize_triangle(x1, y1, x2, y2, x3, y3, &mut sc.lines);
                crop_scanlines(&mut sc.lines, w, h);
            }
            Shape::Rectangle { x1, y1, x2, y2 } => {
                let (x1, x2) = (x1.min(x2), x1.max(x2));
                let (y1, y2) = (y1.min(y2), y1.max(y2));
                for y in y1..=y2 {
                    sc.lines.push(Scanline { y, x1, x2, alpha: 0xffff });
                }
            }
            Shape::Ellipse { x, y, rx, ry, .. } => {
                for dy in 0..ry {
                    let y1 = y - dy;
                    let y2 = y + dy;
                    if (y1 < 0 || y1 >= h) && (y2 < 0 || y2 >= h) {
                        continue;
                    }
                    let s = ellipse_half_width(rx, ry, dy);
                    let xa = (x - s).max(0);
                    let xb = (x + s).min(w - 1);
                    if y1 >= 0 && y1 < h {
                        sc.lines.push(Scanline { y: y1, x1: xa, x2: xb, alpha: 0xffff });
                    }
                    if y2 >= 0 && y2 < h && dy > 0 {
                        sc.lines.push(Scanline { y: y2, x1: xa, x2: xb, alpha: 0xffff });
                    }
                }
            }
            Shape::RotatedRectangle { x, y, sx, sy, angle } => {
                rasterize_rotated_rect(x, y, sx, sy, angle, w, h, sc);
            }
            Shape::Quadratic { x1, y1, x2, y2, x3, y3, width } => {
                stroke_quadratic(
                    [(x1, y1), (x2, y2), (x3, y3)],
                    width,
                    w,
                    h,
                    &mut sc.pts,
                    &mut sc.acc,
                    &mut sc.ints_a,
                    &mut sc.lines,
                );
            }
            Shape::RotatedEllipse { x, y, rx, ry, angle } => {
                let perim = std::f64::consts::PI * (rx + ry);
                let n = ((perim / 2.0) as usize).clamp(16, 512);
                let (sa, ca) = angle.to_radians().sin_cos();
                let (sd, cd) = (std::f64::consts::TAU / n as f64).sin_cos();
                let (mut c, mut s) = (1.0f64, 0.0f64); // cos t, sin t by rotation steps
                sc.pts.clear();
                for _ in 0..n {
                    let (ex, ey) = (rx * c, ry * s);
                    sc.pts.push(((ex * ca - ey * sa + x) as f32, (ex * sa + ey * ca + y) as f32));
                    (c, s) = (c * cd - s * sd, s * cd + c * sd);
                }
                fill_polygon(&sc.pts, w, h, &mut sc.acc, &mut sc.lines);
            }
            Shape::Polygon { x, y } => {
                sc.pts.clear();
                for i in 0..4 {
                    sc.pts.push((x[i] as f32, y[i] as f32));
                }
                fill_polygon(&sc.pts, w, h, &mut sc.acc, &mut sc.lines);
            }
        }
    }

    /// Whether shape mode `t` can be drawn by the GPU (raster_score.wgsl).
    pub fn gpu_supported(t: i32) -> bool {
        (0..=8).contains(&t)
    }

    /// The shape as a record for raster_score.wgsl, or None if that program
    /// cannot draw this kind yet. The one-time setup math is done here so it
    /// matches the CPU rasterizer.
    pub fn gpu_record(&self, w: i32, h: i32, alpha: i32) -> Option<GpuCand> {
        let mut c = GpuCand { alpha: alpha as u32, y0: 0, y1: -1, ..Default::default() };
        match *self {
            Shape::Polygon { x, y } => {
                c.kind = 8;
                // Same setup as raster::fill_polygon.
                let pts: [(f32, f32); 4] = std::array::from_fn(|i| (x[i] as f32, y[i] as f32));
                let (mut minx, mut maxx, mut miny, mut maxy) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                for &(px, py) in &pts {
                    minx = minx.min(px);
                    maxx = maxx.max(px);
                    miny = miny.min(py);
                    maxy = maxy.max(py);
                }
                if !(minx.is_finite() && maxx.is_finite() && miny.is_finite() && maxy.is_finite()) {
                    return Some(c);
                }
                let ry0 = (miny.floor() as i32).max(0);
                let ry1 = (maxy.ceil() as i32).min(h);
                if ry1 <= ry0 || maxx < 0.0 || minx >= w as f32 {
                    return Some(c);
                }
                let ox = minx.floor();
                let oy = ry0 as f32;
                for (i, &(px, py)) in pts.iter().enumerate() {
                    c.p[i * 2] = px - ox;
                    c.p[i * 2 + 1] = py - oy;
                }
                c.p[8] = ox;
                c.y0 = ry0;
                c.y1 = ry1 - 1;
            }
            Shape::RotatedEllipse { x, y, rx, ry, angle } => {
                c.kind = 7;
                // The CPU polygon (see rasterize) has n points on the ellipse,
                // so it is slightly smaller. Shrink the radii so the areas match.
                let perim = std::f64::consts::PI * (rx + ry);
                let n = ((perim / 2.0) as usize).clamp(16, 512) as f64;
                let k = (n * (std::f64::consts::TAU / n).sin() / std::f64::consts::TAU).sqrt();
                let (a, b) = (rx * k, ry * k);
                let (sa, ca) = angle.to_radians().sin_cos();
                // Inside: A dx^2 + B dx dy + C dy^2 <= 1. Per height dy the
                // middle is at dx = m dy and the half width is W sqrt(1 - (dy/hh)^2).
                let qa = ca * ca / (a * a) + sa * sa / (b * b);
                let qb = 2.0 * ca * sa * (1.0 / (a * a) - 1.0 / (b * b));
                let m = -qb / (2.0 * qa);
                let wd = 1.0 / qa.sqrt();
                let hh = (a * a * sa * sa + b * b * ca * ca).sqrt();
                let xe = (a * a * ca * ca + b * b * sa * sa).sqrt();
                if !(hh.is_finite() && xe.is_finite() && m.is_finite() && wd.is_finite()) {
                    return Some(c);
                }
                let ry0 = ((y - hh).floor() as i32).max(0);
                let ry1 = ((y + hh).ceil() as i32).min(h);
                if ry1 <= ry0 || x + xe < 0.0 || x - xe >= w as f64 {
                    return Some(c);
                }
                let ox = (x - xe).floor();
                c.p[0] = (x - ox) as f32;
                c.p[1] = (y - ry0 as f64) as f32;
                c.p[2] = m as f32;
                c.p[3] = wd as f32;
                c.p[4] = hh as f32;
                c.p[5] = ox as f32;
                c.y0 = ry0;
                c.y1 = ry1 - 1;
            }
            Shape::Quadratic { x1, y1, x2, y2, x3, y3, width } => {
                c.kind = 6;
                // Same setup as raster::stroke_quadratic.
                let p = [(x1, y1), (x2, y2), (x3, y3)];
                let (ddx, ddy) = (x1 - 2.0 * x2 + x3, y1 - 2.0 * y2 + y3);
                let dd = (ddx * ddx + ddy * ddy).sqrt();
                if !dd.is_finite() {
                    return Some(c);
                }
                let n = ((dd * 2.5).sqrt().ceil() as usize).clamp(1, 128);
                let (mut minx, mut maxx, mut miny, mut maxy) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                for i in 0..=n {
                    let t = i as f64 / n as f64;
                    let u = 1.0 - t;
                    let x = (u * u * p[0].0 + 2.0 * u * t * p[1].0 + t * t * p[2].0) as f32;
                    let y = (u * u * p[0].1 + 2.0 * u * t * p[1].1 + t * t * p[2].1) as f32;
                    minx = minx.min(x);
                    maxx = maxx.max(x);
                    miny = miny.min(y);
                    maxy = maxy.max(y);
                }
                let hw = (width / 2.0) as f32;
                let reach = hw + 0.5;
                let bx0 = ((minx - reach).floor() as i32).max(0);
                let bx1 = ((maxx + reach).ceil() as i32).min(w - 1);
                let by0 = ((miny - reach).floor() as i32).max(0);
                let by1 = ((maxy + reach).ceil() as i32).min(h - 1);
                if bx1 < bx0 || by1 < by0 {
                    return Some(c);
                }
                // Split at the piece end nearest the top or bottom of the
                // curve, so each half goes only up or only down.
                let den = y1 - 2.0 * y2 + y3;
                let te = if den != 0.0 { (y1 - y2) / den } else { -1.0 };
                let j = if te > 0.0 && te < 1.0 { (te * n as f64).round() as usize } else { n };
                let (ox, oy) = (bx0 as f64, by0 as f64);
                for (i, &(px, py)) in p.iter().enumerate() {
                    c.p[i * 2] = (px - ox) as f32;
                    c.p[i * 2 + 1] = (py - oy) as f32;
                }
                c.p[6] = n as f32;
                c.p[7] = j as f32;
                c.p[8] = bx0 as f32;
                c.p[9] = hw;
                c.p[10] = reach;
                c.y0 = by0;
                c.y1 = by1;
            }
            Shape::Rectangle { x1, y1, x2, y2 } => {
                c.kind = 2;
                c.p[0] = x1.min(x2) as f32;
                c.p[1] = x1.max(x2) as f32;
                c.y0 = y1.min(y2).max(0);
                c.y1 = y1.max(y2).min(h - 1);
            }
            Shape::Ellipse { x, y, rx, ry, .. } => {
                c.kind = 3;
                c.p[..4].copy_from_slice(&[x as f32, y as f32, rx as f32, ry as f32]);
                c.y0 = (y - ry + 1).max(0);
                c.y1 = (y + ry - 1).min(h - 1);
            }
            Shape::Triangle { x1, y1, x2, y2, x3, y3 } => {
                c.kind = 1;
                // Same sorting and split as rasterize_triangle.
                let mut v = [(x1, y1), (x2, y2), (x3, y3)];
                if v[0].1 > v[2].1 {
                    v.swap(0, 2);
                }
                if v[0].1 > v[1].1 {
                    v.swap(0, 1);
                }
                if v[1].1 > v[2].1 {
                    v.swap(1, 2);
                }
                let [(x1, y1), (x2, y2), (x3, y3)] = v;
                // Widening part: apex, end columns, end row; narrowing part:
                // start columns, apex, start row (the start row is not drawn).
                let (bottom, top) = if y2 == y3 {
                    (Some([x1, y1, x2, x3, y2]), None)
                } else if y1 == y2 {
                    (None, Some([x1, x2, x3, y3, y1]))
                } else {
                    let x4 = x1 + ((y2 - y1) as f64 / (y3 - y1) as f64 * (x3 - x1) as f64) as i32;
                    (Some([x1, y1, x2, x4, y2]), Some([x2, x4, x3, y3, y2]))
                };
                if let Some(b) = bottom {
                    c.p[..4].copy_from_slice(&[b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32]);
                    c.p[4] = b[4] as f32;
                    c.p[5] = 1.0;
                }
                if let Some(t) = top {
                    c.p[8] = t[0] as f32;
                    c.p[9] = t[1] as f32;
                    c.p[6] = t[2] as f32;
                    c.p[7] = t[3] as f32;
                    c.p[10] = t[4] as f32;
                }
                c.y0 = (if bottom.is_some() { y1 } else { y1 + 1 }).max(0);
                c.y1 = y3.min(h - 1);
            }
            Shape::RotatedRectangle { x, y, sx, sy, angle } => {
                c.kind = 5;
                // Same corners and sample counts as rasterize_rotated_rect.
                let pts = rotated_rect_corners(x, y, sx, sy, angle);
                for i in 0..4 {
                    let (px, py) = pts[i];
                    let (qx, qy) = pts[(i + 1) % 4];
                    let (dx, dy) = ((qx - px) as f64, (qy - py) as f64);
                    c.p[i * 2] = px as f32;
                    c.p[i * 2 + 1] = py as f32;
                    c.p[8 + i] = (((dx * dx + dy * dy).sqrt() as i32) * 2) as f32;
                }
                c.y0 = pts.iter().map(|p| p.1).min().unwrap().max(0);
                c.y1 = pts.iter().map(|p| p.1).max().unwrap().min(h - 1);
            }
        }
        Some(c)
    }

    pub fn svg(&self, attrs: &str) -> String {
        match *self {
            Shape::Triangle { x1, y1, x2, y2, x3, y3 } => {
                format!("<polygon {attrs} points=\"{x1},{y1} {x2},{y2} {x3},{y3}\" />")
            }
            Shape::Rectangle { x1, y1, x2, y2 } => {
                let (x1, x2) = (x1.min(x2), x1.max(x2));
                let (y1, y2) = (y1.min(y2), y1.max(y2));
                format!(
                    "<rect {attrs} x=\"{x1}\" y=\"{y1}\" width=\"{}\" height=\"{}\" />",
                    x2 - x1 + 1,
                    y2 - y1 + 1
                )
            }
            Shape::Ellipse { x, y, rx, ry, .. } => {
                format!("<ellipse {attrs} cx=\"{x}\" cy=\"{y}\" rx=\"{rx}\" ry=\"{ry}\" />")
            }
            Shape::RotatedRectangle { x, y, sx, sy, angle } => format!(
                "<g transform=\"translate({x} {y}) rotate({angle}) scale({sx} {sy})\"><rect {attrs} x=\"-0.5\" y=\"-0.5\" width=\"1\" height=\"1\" /></g>"
            ),
            Shape::Quadratic { x1, y1, x2, y2, x3, y3, width } => {
                let attrs = attrs.replace("fill", "stroke");
                format!(
                    "<path {attrs} fill=\"none\" d=\"M {x1:.6} {y1:.6} Q {x2:.6} {y2:.6}, {x3:.6} {y3:.6}\" stroke-width=\"{width:.6}\" />"
                )
            }
            Shape::RotatedEllipse { x, y, rx, ry, angle } => format!(
                "<g transform=\"translate({x:.6} {y:.6}) rotate({angle:.6}) scale({rx:.6} {ry:.6})\"><ellipse {attrs} cx=\"0\" cy=\"0\" rx=\"1\" ry=\"1\" /></g>"
            ),
            Shape::Polygon { x, y } => {
                let pts: Vec<String> = (0..4).map(|i| format!("{:.6},{:.6}", x[i], y[i])).collect();
                format!("<polygon {attrs} points=\"{}\" />", pts.join(","))
            }
        }
    }

    /// Draw the shape at output resolution. `base` maps model to output pixels.
    pub fn draw(&self, pm: &mut Pixmap, paint: &Paint, base: Transform) {
        let fill = |pm: &mut Pixmap, pb: PathBuilder, t: Transform| {
            if let Some(path) = pb.finish() {
                pm.fill_path(&path, paint, FillRule::Winding, t, None);
            }
        };
        match *self {
            Shape::Triangle { x1, y1, x2, y2, x3, y3 } => {
                let mut pb = PathBuilder::new();
                pb.move_to(x1 as f32, y1 as f32);
                pb.line_to(x2 as f32, y2 as f32);
                pb.line_to(x3 as f32, y3 as f32);
                pb.close();
                fill(pm, pb, base);
            }
            Shape::Rectangle { x1, y1, x2, y2 } => {
                let (x1, x2) = (x1.min(x2), x1.max(x2));
                let (y1, y2) = (y1.min(y2), y1.max(y2));
                if let Some(r) = Rect::from_xywh(x1 as f32, y1 as f32, (x2 - x1 + 1) as f32, (y2 - y1 + 1) as f32) {
                    pm.fill_rect(r, paint, base, None);
                }
            }
            Shape::Ellipse { x, y, rx, ry, .. } => {
                let (x, y, rx, ry) = (x as f32, y as f32, rx as f32, ry as f32);
                if let Some(r) = Rect::from_xywh(x - rx, y - ry, 2.0 * rx, 2.0 * ry) {
                    if let Some(path) = PathBuilder::from_oval(r) {
                        pm.fill_path(&path, paint, FillRule::Winding, base, None);
                    }
                }
            }
            Shape::RotatedRectangle { x, y, sx, sy, angle } => {
                let t = base.pre_translate(x as f32, y as f32).pre_rotate(angle as f32);
                let (sx, sy) = (sx as f32, sy as f32);
                if let Some(r) = Rect::from_xywh(-sx / 2.0, -sy / 2.0, sx, sy) {
                    pm.fill_rect(r, paint, t, None);
                }
            }
            Shape::Quadratic { x1, y1, x2, y2, x3, y3, width } => {
                let mut pb = PathBuilder::new();
                pb.move_to(x1 as f32, y1 as f32);
                pb.quad_to(x2 as f32, y2 as f32, x3 as f32, y3 as f32);
                if let Some(path) = pb.finish() {
                    let stroke = Stroke {
                        width: width as f32,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Round,
                        ..Default::default()
                    };
                    pm.stroke_path(&path, paint, &stroke, base, None);
                }
            }
            Shape::RotatedEllipse { x, y, rx, ry, angle } => {
                let (x, y) = (x as f32, y as f32);
                let t = base.pre_translate(x, y).pre_rotate(angle as f32).pre_translate(-x, -y);
                let (rx, ry) = (rx as f32, ry as f32);
                if let Some(r) = Rect::from_xywh(x - rx, y - ry, 2.0 * rx, 2.0 * ry) {
                    if let Some(path) = PathBuilder::from_oval(r) {
                        pm.fill_path(&path, paint, FillRule::Winding, t, None);
                    }
                }
            }
            Shape::Polygon { x, y } => {
                let mut pb = PathBuilder::new();
                pb.move_to(x[0] as f32, y[0] as f32);
                for i in 1..4 {
                    pb.line_to(x[i] as f32, y[i] as f32);
                }
                pb.close();
                fill(pm, pb, base);
            }
        }
    }
}

pub(crate) fn triangle_valid(x1: i32, y1: i32, x2: i32, y2: i32, x3: i32, y3: i32) -> bool {
    const MIN_DEGREES: f64 = 15.0;
    let angle = |ax: i32, ay: i32, bx: i32, by: i32, cx: i32, cy: i32| {
        let (mut ux, mut uy) = ((bx - ax) as f64, (by - ay) as f64);
        let (mut vx, mut vy) = ((cx - ax) as f64, (cy - ay) as f64);
        let d1 = (ux * ux + uy * uy).sqrt();
        let d2 = (vx * vx + vy * vy).sqrt();
        ux /= d1;
        uy /= d1;
        vx /= d2;
        vy /= d2;
        (ux * vx + uy * vy).acos().to_degrees()
    };
    let a1 = angle(x1, y1, x2, y2, x3, y3);
    let a2 = angle(x2, y2, x1, y1, x3, y3);
    let a3 = 180.0 - a1 - a2;
    a1 > MIN_DEGREES && a2 > MIN_DEGREES && a3 > MIN_DEGREES
}

pub(crate) fn quadratic_valid(x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) -> bool {
    let dx12 = (x1 - x2) as i64;
    let dy12 = (y1 - y2) as i64;
    let dx23 = (x2 - x3) as i64;
    let dy23 = (y2 - y3) as i64;
    let dx13 = (x1 - x3) as i64;
    let dy13 = (y1 - y3) as i64;
    let d12 = dx12 * dx12 + dy12 * dy12;
    let d23 = dx23 * dx23 + dy23 * dy23;
    let d13 = dx13 * dx13 + dy13 * dy13;
    d13 > d12 && d13 > d23
}

fn rasterize_triangle(
    mut x1: i32,
    mut y1: i32,
    mut x2: i32,
    mut y2: i32,
    mut x3: i32,
    mut y3: i32,
    buf: &mut Vec<Scanline>,
) {
    if y1 > y3 {
        std::mem::swap(&mut x1, &mut x3);
        std::mem::swap(&mut y1, &mut y3);
    }
    if y1 > y2 {
        std::mem::swap(&mut x1, &mut x2);
        std::mem::swap(&mut y1, &mut y2);
    }
    if y2 > y3 {
        std::mem::swap(&mut x2, &mut x3);
        std::mem::swap(&mut y2, &mut y3);
    }
    if y2 == y3 {
        triangle_bottom(x1, y1, x2, y2, x3, y3, buf);
    } else if y1 == y2 {
        triangle_top(x1, y1, x2, y2, x3, y3, buf);
    } else {
        let x4 = x1 + ((y2 - y1) as f64 / (y3 - y1) as f64 * (x3 - x1) as f64) as i32;
        triangle_bottom(x1, y1, x2, y2, x4, y2, buf);
        triangle_top(x2, y2, x4, y2, x3, y3, buf);
    }
}

/// Half width of an ellipse row: trunc(sqrt(ry^2 - dy^2) * rx / ry), in exact
/// whole numbers. Floating-point math adds rounding errors here (about 1 row in 2,500 is one
/// pixel narrower); exact math lets the GPU (walk_ellipse) match it fully.
fn ellipse_half_width(rx: i32, ry: i32, dy: i32) -> i32 {
    let n = (ry * ry - dy * dy) as i64 * (rx as i64 * rx as i64);
    let mut q = (n as f64).sqrt() as i64;
    while q * q > n {
        q -= 1;
    }
    while (q + 1) * (q + 1) <= n {
        q += 1;
    }
    (q / ry as i64) as i32
}

// Each triangle row's x is computed as an
// exact fraction instead of adding the slope row by row. The adding piles up
// rounding errors (often one pixel short), which the GPU (walk_triangle)
// could not copy.
fn triangle_bottom(x1: i32, y1: i32, x2: i32, y2: i32, x3: i32, y3: i32, buf: &mut Vec<Scanline>) {
    let (d1, d2) = (y2 - y1, y3 - y1);
    for y in y1..=y2 {
        let k = y - y1;
        let mut a = (x1 * d1 + k * (x2 - x1)) / d1;
        let mut b = (x1 * d2 + k * (x3 - x1)) / d2;
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        buf.push(Scanline { y, x1: a, x2: b, alpha: 0xffff });
    }
}

fn triangle_top(x1: i32, y1: i32, x2: i32, y2: i32, x3: i32, y3: i32, buf: &mut Vec<Scanline>) {
    let (d1, d2) = (y3 - y1, y3 - y2);
    let mut y = y3;
    while y > y1 {
        let k = y3 - y + 1;
        let mut a = (x3 * d1 - k * (x3 - x1)) / d1;
        let mut b = (x3 * d2 - k * (x3 - x2)) / d2;
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        buf.push(Scanline { y, x1: a, x2: b, alpha: 0xffff });
        y -= 1;
    }
}

/// Sine and cosine of a whole number of degrees. Angles from -720 to 1080
/// come from a table that is filled once with the same call as the
/// fallback, so the result is the same either way.
fn sin_cos_deg(angle: i32) -> (f64, f64) {
    const LO: i32 = -720;
    const N: usize = 1801;
    static TABLE: std::sync::OnceLock<Vec<(f64, f64)>> = std::sync::OnceLock::new();
    let calc = |a: i32| (a as f64).to_radians().sin_cos();
    let i = angle.wrapping_sub(LO) as u32 as usize;
    if i < N {
        TABLE.get_or_init(|| (0..N as i32).map(|k| calc(LO + k)).collect())[i]
    } else {
        calc(angle)
    }
}

/// The 4 corners of a rotated rectangle in whole image coordinates. Shared
/// by rasterize_rotated_rect and gpu_record so that they always agree.
#[inline]
pub(crate) fn rotated_rect_corners(x: i32, y: i32, sx: i32, sy: i32, angle: i32) -> [(i32, i32); 4] {
    let (hx, hy) = (sx as f64 / 2.0, sy as f64 / 2.0);
    let (s, c) = sin_cos_deg(angle);
    let corner = |px: f64, py: f64| ((px * c - py * s) as i32 + x, (px * s + py * c) as i32 + y);
    [corner(-hx, -hy), corner(hx, -hy), corner(hx, hy), corner(-hx, hy)]
}

#[allow(clippy::too_many_arguments)]
fn rasterize_rotated_rect(x: i32, y: i32, sx: i32, sy: i32, angle: i32, w: i32, h: i32, sc: &mut Scratch) {
    let [(x1, y1), (x2, y2), (x3, y3), (x4, y4)] = rotated_rect_corners(x, y, sx, sy, angle);
    let miny = y1.min(y2).min(y3).min(y4);
    let maxy = y1.max(y2).max(y3).max(y4);
    let n = (maxy - miny + 1) as usize;
    let mins = &mut sc.ints_a;
    let maxs = &mut sc.ints_b;
    mins.clear();
    mins.resize(n, w);
    maxs.clear();
    maxs.resize(n, 0);
    let xs = [x1, x2, x3, x4, x1];
    let ys = [y1, y2, y3, y4, y1];
    for i in 0..4 {
        // Sample points along the edge, in exact whole-number fractions
        // (not floating point) so that walk_rotated_rect in raster_score.wgsl
        // gets the same pixels: point j is f + trunc(j * d / m).
        let (fx, fy) = (xs[i], ys[i]);
        let (dx, dy) = (xs[i + 1] - xs[i], ys[i + 1] - ys[i]);
        let count = ((((dx * dx + dy * dy) as f64).sqrt()) as i32) * 2;
        let m = count - 1;
        for j in 0..count {
            let xi = (fx * m + j * dx) / m;
            let yi = ((fy * m + j * dy) / m - miny) as usize;
            mins[yi] = mins[yi].min(xi);
            maxs[yi] = maxs[yi].max(xi);
        }
    }
    for i in 0..n {
        let yy = miny + i as i32;
        if yy < 0 || yy >= h {
            continue;
        }
        let a = mins[i].max(0);
        let b = maxs[i].min(w - 1);
        if b >= a {
            sc.lines.push(Scanline { y: yy, x1: a, x2: b, alpha: 0xffff });
        }
    }
}
