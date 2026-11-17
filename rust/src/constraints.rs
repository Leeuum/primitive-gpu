//! Creative constraints: optional rules that every candidate shape must
//! follow (angle, size, proportions, grid, allowed area). They only change
//! which shapes the search tries; scoring and drawing are untouched. With
//! no rule set, the search runs exactly as before.

use crate::rng::Rng;
use crate::shapes::{quadratic_valid, rotated_rect_corners, triangle_valid, Shape};
use std::sync::Arc;

/// How the direction of a shape is chosen. Only rotated rectangles,
/// rotated ellipses and curves have a direction.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum AngleRule {
    /// Any angle (or steps of `angle_step`).
    #[default]
    Free,
    /// Every shape has the angle `angle_deg`.
    Fixed,
    /// The long side points at `point`.
    Toward,
    /// The long side runs around `point`, like rings.
    Around,
}

impl AngleRule {
    pub fn parse(s: &str) -> Option<AngleRule> {
        match s {
            "free" => Some(AngleRule::Free),
            "fixed" => Some(AngleRule::Fixed),
            "toward" => Some(AngleRule::Toward),
            "around" => Some(AngleRule::Around),
            _ => None,
        }
    }
}

/// The area where shapes may be, at the working size.
#[derive(Debug, PartialEq)]
pub struct Mask {
    w: i32,
    h: i32,
    ok: Vec<bool>,
    allowed: Vec<u32>,
}

impl Mask {
    /// From `w * h` gray values; 128 and up is allowed. None when the
    /// mask allows nothing or everything (it would do nothing useful).
    pub fn new(w: i32, h: i32, gray: &[u8]) -> Option<Mask> {
        if w < 1 || h < 1 || gray.len() != (w * h) as usize {
            return None;
        }
        let ok: Vec<bool> = gray.iter().map(|&g| g >= 128).collect();
        let allowed: Vec<u32> = (0..ok.len() as u32).filter(|&i| ok[i as usize]).collect();
        if allowed.is_empty() || allowed.len() == ok.len() {
            return None;
        }
        Some(Mask { w, h, ok, allowed })
    }

    fn has(&self, x: f64, y: f64) -> bool {
        let xi = (x.round() as i32).clamp(0, self.w - 1);
        let yi = (y.round() as i32).clamp(0, self.h - 1);
        self.ok[(yi * self.w + xi) as usize]
    }
}

/// The detail area, at the working size: how much each spot wants small
/// shapes, from 0 (big shapes) to 1 (small shapes), with a soft edge.
#[derive(Debug, PartialEq)]
pub struct Detail {
    w: i32,
    h: i32,
    level: Vec<f32>,
}

impl Detail {
    /// From `w * h` gray values; white = small shapes. The edge is blurred
    /// so the size changes over a short distance, not at a hard line. None
    /// when nothing is marked.
    pub fn new(w: i32, h: i32, gray: &[u8]) -> Option<Detail> {
        if w < 1 || h < 1 || gray.len() != (w * h) as usize || gray.iter().all(|&g| g < 128) {
            return None;
        }
        let (wu, hu) = (w as usize, h as usize);
        let mut level: Vec<f32> = gray.iter().map(|&g| g as f32 / 255.0).collect();
        let radius = ((0.03 * w.max(h) as f64).round() as usize).max(1);
        // Two passes of a box blur, across and then down, give a smooth fade.
        let blur = |line: &mut [f32], tmp: &mut Vec<f32>| {
            let n = line.len();
            tmp.clear();
            tmp.extend_from_slice(line);
            for (i, out) in line.iter_mut().enumerate() {
                let (a, b) = (i.saturating_sub(radius), (i + radius).min(n - 1));
                *out = tmp[a..=b].iter().sum::<f32>() / (b - a + 1) as f32;
            }
        };
        let mut tmp = Vec::new();
        let mut col = vec![0.0f32; hu];
        for _ in 0..2 {
            for row in level.chunks_mut(wu) {
                blur(row, &mut tmp);
            }
            for x in 0..wu {
                for y in 0..hu {
                    col[y] = level[y * wu + x];
                }
                blur(&mut col, &mut tmp);
                for y in 0..hu {
                    level[y * wu + x] = col[y];
                }
            }
        }
        Some(Detail { w, h, level })
    }

    fn at(&self, x: f64, y: f64) -> f64 {
        let xi = (x.round() as i32).clamp(0, self.w - 1);
        let yi = (y.round() as i32).clamp(0, self.h - 1);
        self.level[(yi * self.w + xi) as usize] as f64
    }
}

/// The rules as the user sets them. Sizes are fractions of the picture's
/// long side, so they mean the same at every working size. 0 = off.
#[derive(Clone, Debug, PartialEq)]
pub struct Constraints {
    pub angle: AngleRule,
    /// For `Fixed`, and the start of the steps: 0 = flat, 90 = upright,
    /// counted anticlockwise as seen on screen.
    pub angle_deg: f64,
    /// With `Free`: only angles this many degrees apart. 0 = any.
    pub angle_step: f64,
    /// The spot for `Toward` and `Around`: fractions of width and height.
    pub point: (f64, f64),
    /// Smallest and largest long side of a shape.
    pub min_size: f64,
    pub max_size: f64,
    /// The largest size falls from `max_size` to `min_size` over this
    /// many shapes. 0 = off.
    pub shrink_over: usize,
    /// Limits for long side / short side (1 = square).
    pub min_ratio: f64,
    pub max_ratio: f64,
    /// Grid cells along the long side. 0 = off.
    pub grid_cells: u32,
    pub mask: Option<Arc<Mask>>,
    /// Small shapes in the marked area, big shapes in the rest: shapes are
    /// at most `min_size` in the area, and from half of `max_size` up to
    /// `max_size` outside it. A size that is off counts as 3 % (small) or
    /// 20 % (big).
    pub detail: Option<Arc<Detail>>,
}

impl Default for Constraints {
    fn default() -> Self {
        Constraints {
            angle: AngleRule::Free,
            angle_deg: 0.0,
            angle_step: 0.0,
            point: (0.5, 0.5),
            min_size: 0.0,
            max_size: 0.0,
            shrink_over: 0,
            min_ratio: 0.0,
            max_ratio: 0.0,
            grid_cells: 0,
            mask: None,
            detail: None,
        }
    }
}

impl Constraints {
    /// The rules in working pixels for shape number `index`, or None when
    /// no rule is set.
    pub fn resolve(&self, w: i32, h: i32, index: usize) -> Option<Rules> {
        let long = w.max(h) as f64;
        let clean = |v: f64| if v.is_finite() && v > 0.0 { v } else { 0.0 };
        let mut min_px = clean(self.min_size) * long;
        let mut max_px = clean(self.max_size) * long;
        if self.shrink_over > 0 {
            let start = if max_px > 0.0 { max_px } else { long };
            let end = (if min_px > 0.0 { min_px } else { 0.02 * long }).min(start);
            let t = (index as f64 / self.shrink_over as f64).min(1.0);
            max_px = start * (end / start).powf(t);
        }
        if max_px > 0.0 {
            max_px = max_px.max(1.0);
            min_px = min_px.min(max_px);
        }
        let mut min_ratio = clean(self.min_ratio);
        let mut max_ratio = clean(self.max_ratio);
        if min_ratio <= 1.0 {
            min_ratio = 0.0;
        }
        if max_ratio > 0.0 {
            max_ratio = max_ratio.max(1.0);
            min_ratio = min_ratio.min(max_ratio);
        }
        let grid = if self.grid_cells > 0 { ((long / self.grid_cells as f64).round() as i32).max(2) } else { 0 };
        let mask = self.mask.clone().filter(|m| m.w == w && m.h == h);
        // With a detail area the two sizes are used per spot (see
        // `Rules::limits`), not as limits for the whole picture.
        let detail = self.detail.clone().filter(|d| d.w == w && d.h == h).map(|d| {
            let small = (if min_px > 0.0 { min_px } else { 0.03 * long }).max(1.0);
            let big = (if max_px > 0.0 { max_px } else { 0.2 * long }).max(small);
            (d, small, big)
        });
        if detail.is_some() {
            min_px = 0.0;
            max_px = 0.0;
        }
        let angle_step = clean(self.angle_step);
        let active = self.angle != AngleRule::Free
            || angle_step > 0.0
            || min_px > 0.0
            || max_px > 0.0
            || min_ratio > 0.0
            || max_ratio > 0.0
            || grid > 0
            || mask.is_some()
            || detail.is_some();
        active.then_some(Rules {
            angle: self.angle,
            // Image y points down, so anticlockwise on screen is negative.
            angle_img: -self.angle_deg,
            angle_step,
            px: self.point.0 * (w - 1) as f64,
            py: self.point.1 * (h - 1) as f64,
            min_px,
            max_px,
            min_ratio,
            max_ratio,
            grid,
            mask,
            detail,
        })
    }
}

/// The rules for one step, in working pixels. Made by `Constraints::resolve`.
#[derive(Clone, Debug, PartialEq)]
pub struct Rules {
    angle: AngleRule,
    angle_img: f64,
    angle_step: f64,
    px: f64,
    py: f64,
    min_px: f64,
    max_px: f64,
    min_ratio: f64,
    max_ratio: f64,
    grid: i32,
    mask: Option<Arc<Mask>>,
    /// The detail area with its small and big size in pixels.
    detail: Option<(Arc<Detail>, f64, f64)>,
}

impl Rules {
    /// Smallest and largest long side for a shape with its middle at
    /// `c` (0 = no limit).
    fn limits(&self, c: (f64, f64)) -> (f64, f64) {
        match &self.detail {
            Some((d, small, big)) => {
                let v = d.at(c.0, c.1);
                let max = big * (small / big).powf(v);
                (0.5 * max * (1.0 - v), max)
            }
            None => (self.min_px, self.max_px),
        }
    }

    /// What to multiply a shape by so its long side is allowed.
    fn size_factor(long: f64, (min, max): (f64, f64)) -> f64 {
        if max > 0.0 && long > max {
            max / long
        } else if min > 0.0 && long < min && long > 1e-6 {
            min / long
        } else {
            1.0
        }
    }

    /// Allowed side lengths nearest to `a` x `b`.
    fn fit(&self, a: f64, b: f64, circle: bool, lim: (f64, f64)) -> (f64, f64) {
        let f = Rules::size_factor(a.max(b), lim);
        let (mut a, mut b) = (a * f, b * f);
        if !circle {
            let (long, short) = (a.max(b), a.min(b).max(1e-6));
            let ratio = long / short;
            let mut ns = short;
            if self.max_ratio > 0.0 && ratio > self.max_ratio {
                ns = long / self.max_ratio;
            } else if self.min_ratio > 0.0 && ratio < self.min_ratio {
                ns = long / self.min_ratio;
            }
            if a >= b {
                b = ns;
            } else {
                a = ns;
            }
        }
        (a, b)
    }
}

const M: f64 = 16.0;

impl Shape {
    /// `Shape::random` that follows the rules.
    pub fn random_with(t: i32, w: i32, h: i32, rng: &mut Rng, r: Option<&Rules>) -> Shape {
        let Some(r) = r else {
            return Shape::random(t, w, h, rng);
        };
        let mut s = Shape::random(t, w, h, rng);
        for attempt in 0..16 {
            if attempt > 0 {
                s = Shape::random(t, w, h, rng);
            }
            if let Some(m) = &r.mask {
                let p = m.allowed[rng.intn(m.allowed.len() as i32) as usize] as i32;
                s.move_to((p % m.w) as f64, (p / m.w) as f64);
            }
            s.apply(w, h, r);
            if let Some(m) = &r.mask {
                // Too big for the area here: try smaller.
                for _ in 0..4 {
                    if s.inside(m) {
                        break;
                    }
                    s.scale(0.5);
                    s.apply(w, h, r);
                }
            }
            if s.ok(r) {
                break;
            }
        }
        s
    }

    /// `Shape::mutate` that follows the rules. When no allowed change is
    /// found in a few tries the shape stays as it is.
    pub fn mutate_with(&mut self, w: i32, h: i32, rng: &mut Rng, r: Option<&Rules>) {
        let Some(r) = r else {
            return self.mutate(w, h, rng);
        };
        let orig = *self;
        for _ in 0..8 {
            *self = orig;
            self.mutate(w, h, rng);
            self.apply(w, h, r);
            if self.ok(r) {
                return;
            }
        }
        *self = orig;
    }

    /// Change the shape as little as possible so it follows the rules.
    fn apply(&mut self, w: i32, h: i32, r: &Rules) {
        let lim = r.limits(self.centre());
        if let Some((a, b)) = self.extents() {
            let circle = matches!(self, Shape::Ellipse { circle: true, .. });
            let (na, nb) = r.fit(a, b, circle, lim);
            if (na, nb) != (a, b) {
                self.set_extents(na, nb);
            }
        } else if lim.0 > 0.0 || lim.1 > 0.0 {
            let (lo, hi) = self.bbox();
            let f = Rules::size_factor((hi.0 - lo.0).max(hi.1 - lo.1), lim);
            if f != 1.0 {
                self.scale(f);
            }
        }
        self.apply_angle(r);
        if r.grid > 0 {
            self.snap(r.grid, w, h);
        }
        self.tidy(w, h);
    }

    fn ok(&self, r: &Rules) -> bool {
        let valid = match *self {
            Shape::Triangle { x1, y1, x2, y2, x3, y3 } => triangle_valid(x1, y1, x2, y2, x3, y3),
            Shape::Quadratic { x1, y1, x2, y2, x3, y3, .. } => quadratic_valid(x1, y1, x2, y2, x3, y3),
            _ => true,
        };
        valid && r.mask.as_ref().is_none_or(|m| self.inside(m))
    }

    /// The corner points of the kinds that are made of points.
    fn points(&self) -> Option<([(f64, f64); 4], usize)> {
        match *self {
            Shape::Triangle { x1, y1, x2, y2, x3, y3 } => {
                Some(([(x1 as f64, y1 as f64), (x2 as f64, y2 as f64), (x3 as f64, y3 as f64), (0.0, 0.0)], 3))
            }
            Shape::Quadratic { x1, y1, x2, y2, x3, y3, .. } => Some(([(x1, y1), (x2, y2), (x3, y3), (0.0, 0.0)], 3)),
            Shape::Polygon { x, y } => Some((std::array::from_fn(|i| (x[i], y[i])), 4)),
            _ => None,
        }
    }

    fn map_points(&mut self, mut f: impl FnMut(f64, f64) -> (f64, f64)) {
        match self {
            Shape::Triangle { x1, y1, x2, y2, x3, y3 } => {
                for (x, y) in [(x1, y1), (x2, y2), (x3, y3)] {
                    let (a, b) = f(*x as f64, *y as f64);
                    *x = a.round() as i32;
                    *y = b.round() as i32;
                }
            }
            Shape::Quadratic { x1, y1, x2, y2, x3, y3, .. } => {
                for (x, y) in [(x1, y1), (x2, y2), (x3, y3)] {
                    (*x, *y) = f(*x, *y);
                }
            }
            Shape::Polygon { x, y } => {
                for i in 0..4 {
                    (x[i], y[i]) = f(x[i], y[i]);
                }
            }
            _ => {}
        }
    }

    /// Smallest box around the points of a point shape.
    fn bbox(&self) -> ((f64, f64), (f64, f64)) {
        let Some((p, n)) = self.points() else {
            let c = self.centre();
            return (c, c);
        };
        let mut lo = p[0];
        let mut hi = p[0];
        for &(x, y) in &p[1..n] {
            lo = (lo.0.min(x), lo.1.min(y));
            hi = (hi.0.max(x), hi.1.max(y));
        }
        (lo, hi)
    }

    fn centre(&self) -> (f64, f64) {
        match *self {
            Shape::Rectangle { x1, y1, x2, y2 } => ((x1 + x2) as f64 / 2.0, (y1 + y2) as f64 / 2.0),
            Shape::Ellipse { x, y, .. } | Shape::RotatedRectangle { x, y, .. } => (x as f64, y as f64),
            Shape::RotatedEllipse { x, y, .. } => (x, y),
            _ => {
                let (lo, hi) = self.bbox();
                ((lo.0 + hi.0) / 2.0, (lo.1 + hi.1) / 2.0)
            }
        }
    }

    fn move_to(&mut self, cx: f64, cy: f64) {
        let c = self.centre();
        let (dx, dy) = (cx - c.0, cy - c.1);
        let (ix, iy) = (dx.round() as i32, dy.round() as i32);
        match self {
            Shape::Rectangle { x1, y1, x2, y2 } => {
                *x1 += ix;
                *x2 += ix;
                *y1 += iy;
                *y2 += iy;
            }
            Shape::Ellipse { x, y, .. } | Shape::RotatedRectangle { x, y, .. } => {
                *x += ix;
                *y += iy;
            }
            Shape::RotatedEllipse { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            Shape::Triangle { .. } => self.map_points(|x, y| (x + ix as f64, y + iy as f64)),
            _ => self.map_points(|x, y| (x + dx, y + dy)),
        }
    }

    /// Full side lengths along the shape's own axes, for the box-like kinds.
    fn extents(&self) -> Option<(f64, f64)> {
        match *self {
            Shape::Rectangle { x1, y1, x2, y2 } => Some((((x2 - x1).abs() + 1) as f64, ((y2 - y1).abs() + 1) as f64)),
            Shape::Ellipse { rx, ry, .. } => Some((2.0 * rx as f64, 2.0 * ry as f64)),
            Shape::RotatedRectangle { sx, sy, .. } => Some((sx as f64, sy as f64)),
            Shape::RotatedEllipse { rx, ry, .. } => Some((2.0 * rx, 2.0 * ry)),
            _ => None,
        }
    }

    fn set_extents(&mut self, a: f64, b: f64) {
        match self {
            Shape::Rectangle { x1, y1, x2, y2 } => {
                for (p, q, len) in [(x1, x2, a), (y1, y2, b)] {
                    let len = (len.round() as i32).max(1);
                    let lo = ((*p + *q) as f64 / 2.0 - (len - 1) as f64 / 2.0).round() as i32;
                    *p = lo;
                    *q = lo + len - 1;
                }
            }
            Shape::Ellipse { rx, ry, circle, .. } => {
                *rx = ((a / 2.0).round() as i32).max(1);
                *ry = if *circle { *rx } else { ((b / 2.0).round() as i32).max(1) };
            }
            Shape::RotatedRectangle { sx, sy, .. } => {
                *sx = (a.round() as i32).max(1);
                *sy = (b.round() as i32).max(1);
            }
            Shape::RotatedEllipse { rx, ry, .. } => {
                *rx = (a / 2.0).max(1.0);
                *ry = (b / 2.0).max(1.0);
            }
            _ => {}
        }
    }

    /// Grow or shrink around the centre.
    fn scale(&mut self, f: f64) {
        if let Some((a, b)) = self.extents() {
            self.set_extents(a * f, b * f);
        } else {
            let (cx, cy) = self.centre();
            self.map_points(|x, y| (cx + (x - cx) * f, cy + (y - cy) * f));
        }
    }

    fn apply_angle(&mut self, r: &Rules) {
        if r.angle == AngleRule::Free && r.angle_step <= 0.0 {
            return;
        }
        let (cx, cy) = self.centre();
        // The wanted direction of the long side, given the direction now.
        let dir = |cur: f64| -> f64 {
            let to_point = (r.py - cy).atan2(r.px - cx).to_degrees();
            match r.angle {
                AngleRule::Fixed => r.angle_img,
                AngleRule::Toward => to_point,
                AngleRule::Around => to_point + 90.0,
                AngleRule::Free => r.angle_img + ((cur - r.angle_img) / r.angle_step).round() * r.angle_step,
            }
        };
        let free = r.angle == AngleRule::Free;
        match self {
            Shape::RotatedRectangle { sx, sy, angle, .. } => {
                let t = if free { dir(*angle as f64) } else if *sx >= *sy { dir(0.0) } else { dir(0.0) + 90.0 };
                *angle = (t.round() as i32).rem_euclid(360);
            }
            Shape::RotatedEllipse { rx, ry, angle, .. } => {
                let t = if free { dir(*angle) } else if *rx >= *ry { dir(0.0) } else { dir(0.0) + 90.0 };
                *angle = t.rem_euclid(360.0);
            }
            Shape::Quadratic { x1, y1, x2, y2, x3, y3, .. } => {
                // Turn the whole curve so the line from its start to its
                // end has the wanted direction (either way round).
                let cur = (*y3 - *y1).atan2(*x3 - *x1).to_degrees();
                let d = (dir(cur) - cur + 90.0).rem_euclid(180.0) - 90.0;
                if d.abs() > 1e-9 {
                    let (mx, my) = ((*x1 + *x3) / 2.0, (*y1 + *y3) / 2.0);
                    let (s, c) = d.to_radians().sin_cos();
                    for (x, y) in [(x1, y1), (x2, y2), (x3, y3)] {
                        let (ux, uy) = (*x - mx, *y - my);
                        *x = mx + ux * c - uy * s;
                        *y = my + ux * s + uy * c;
                    }
                }
            }
            _ => {}
        }
    }

    /// Snap to a grid of `g` pixels: rectangles fill whole cells, round
    /// shapes sit in the middle of a cell, corner points go to grid crossings.
    fn snap(&mut self, g: i32, w: i32, h: i32) {
        let gf = g as f64;
        match self {
            Shape::Rectangle { x1, y1, x2, y2 } => {
                for (p, q, size) in [(x1, x2, w), (y1, y2, h)] {
                    let (lo, hi) = ((*p).min(*q), (*p).max(*q));
                    let last = (size - 1) / g * g;
                    let l = (((lo as f64 / gf).round() as i32) * g).clamp(0, last);
                    let e = ((((hi + 1) as f64 / gf).round() as i32) * g).max(l + g);
                    *p = l;
                    *q = e - 1;
                }
            }
            Shape::Ellipse { x, y, .. } | Shape::RotatedRectangle { x, y, .. } => {
                *x = x.div_euclid(g) * g + g / 2;
                *y = y.div_euclid(g) * g + g / 2;
            }
            Shape::RotatedEllipse { x, y, .. } => {
                *x = (*x / gf).floor() * gf + gf / 2.0;
                *y = (*y / gf).floor() * gf + gf / 2.0;
            }
            _ => {
                let (wf, hf) = ((w - 1) as f64 + M, (h - 1) as f64 + M);
                let snap1 = |v: f64, hi: f64| {
                    let mut s = (v / gf).round() * gf;
                    while s > hi {
                        s -= gf;
                    }
                    while s < -M {
                        s += gf;
                    }
                    s
                };
                self.map_points(|x, y| (snap1(x, wf), snap1(y, hf)));
            }
        }
    }

    /// Keep every number inside the limits that `mutate` keeps.
    fn tidy(&mut self, w: i32, h: i32) {
        let (wf, hf) = ((w - 1) as f64, (h - 1) as f64);
        match self {
            Shape::Rectangle { x1, y1, x2, y2 } => {
                *x1 = (*x1).clamp(0, w - 1);
                *x2 = (*x2).clamp(0, w - 1);
                *y1 = (*y1).clamp(0, h - 1);
                *y2 = (*y2).clamp(0, h - 1);
            }
            Shape::Ellipse { x, y, rx, ry, circle } => {
                *x = (*x).clamp(0, w - 1);
                *y = (*y).clamp(0, h - 1);
                *rx = (*rx).clamp(1, (w - 1).max(1));
                *ry = (*ry).clamp(1, (h - 1).max(1));
                if *circle {
                    *rx = (*rx).min(*ry);
                    *ry = *rx;
                }
            }
            Shape::RotatedRectangle { x, y, sx, sy, .. } => {
                *x = (*x).clamp(0, w - 1);
                *y = (*y).clamp(0, h - 1);
                *sx = (*sx).clamp(1, (w - 1).max(1));
                *sy = (*sy).clamp(1, (h - 1).max(1));
            }
            Shape::RotatedEllipse { x, y, rx, ry, .. } => {
                *x = x.clamp(0.0, wf);
                *y = y.clamp(0.0, hf);
                *rx = rx.clamp(1.0, wf.max(1.0));
                *ry = ry.clamp(1.0, hf.max(1.0));
            }
            _ => self.map_points(|x, y| (x.clamp(-M, wf + M), y.clamp(-M, hf + M))),
        }
    }

    /// Whether the middle and the outer points of the shape are all in the
    /// allowed area.
    fn inside(&self, m: &Mask) -> bool {
        let (cx, cy) = self.centre();
        if !m.has(cx, cy) {
            return false;
        }
        match *self {
            Shape::Rectangle { x1, y1, x2, y2 } => {
                [(x1, y1), (x2, y1), (x2, y2), (x1, y2)].iter().all(|&(x, y)| m.has(x as f64, y as f64))
            }
            Shape::Ellipse { x, y, rx, ry, .. } => {
                [(x - rx, y), (x + rx, y), (x, y - ry), (x, y + ry)].iter().all(|&(x, y)| m.has(x as f64, y as f64))
            }
            Shape::RotatedRectangle { x, y, sx, sy, angle } => {
                rotated_rect_corners(x, y, sx, sy, angle).iter().all(|&(x, y)| m.has(x as f64, y as f64))
            }
            Shape::RotatedEllipse { x, y, rx, ry, angle } => {
                let (s, c) = angle.to_radians().sin_cos();
                [(rx * c, rx * s), (-rx * c, -rx * s), (-ry * s, ry * c), (ry * s, -ry * c)]
                    .iter()
                    .all(|&(dx, dy)| m.has(x + dx, y + dy))
            }
            Shape::Quadratic { x1, y1, x2, y2, x3, y3, .. } => {
                m.has(x1, y1) && m.has(x3, y3) && m.has(0.25 * x1 + 0.5 * x2 + 0.25 * x3, 0.25 * y1 + 0.5 * y2 + 0.25 * y3)
            }
            _ => {
                let (p, n) = self.points().unwrap();
                p[..n].iter().all(|&(x, y)| m.has(x, y))
            }
        }
    }
}
