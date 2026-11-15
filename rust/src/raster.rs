//! Scanlines and the anti-aliased rasterizers used for filled paths
//! (rotated ellipses, polygons) and stroked quadratic curves.

#[derive(Clone, Copy, Debug)]
pub struct Scanline {
    pub y: i32,
    pub x1: i32,
    pub x2: i32, // inclusive
    pub alpha: u32, // coverage, 0..=0xffff
}

/// Per-thread reusable buffers, so rasterizing does not allocate.
#[derive(Default)]
pub struct Scratch {
    pub lines: Vec<Scanline>,
    pub acc: Vec<f32>,
    pub ints_a: Vec<i32>,
    pub ints_b: Vec<i32>,
    pub pts: Vec<(f32, f32)>,
}

pub fn crop_scanlines(lines: &mut Vec<Scanline>, w: i32, h: i32) {
    lines.retain_mut(|l| {
        if l.y < 0 || l.y >= h || l.x1 >= w || l.x2 < 0 {
            return false;
        }
        l.x1 = l.x1.clamp(0, w - 1);
        l.x2 = l.x2.clamp(0, w - 1);
        l.x1 <= l.x2
    });
}

/// Turn one row of coverage values into spans of equal coverage.
/// `cov(i)` returns the coverage (0..1) of local column i; `x0` is the image
/// x of column 0.
#[inline]
fn emit_row(out: &mut Vec<Scanline>, y: i32, x0: i32, n: usize, w: i32, mut cov: impl FnMut(usize) -> f32) {
    let mut run: Option<(i32, i32, u32)> = None; // x1, x2, alpha
    for i in 0..n {
        let c = cov(i);
        let x = x0 + i as i32;
        let alpha = (c.clamp(0.0, 1.0) * 65535.0) as u32;
        if x < 0 || x >= w || alpha == 0 {
            if let Some((a, b, al)) = run.take() {
                out.push(Scanline { y, x1: a, x2: b, alpha: al });
            }
            continue;
        }
        match run {
            Some((a, b, al)) if al == alpha && b + 1 == x => run = Some((a, x, al)),
            Some((a, b, al)) => {
                out.push(Scanline { y, x1: a, x2: b, alpha: al });
                run = Some((x, x, alpha));
            }
            None => run = Some((x, x, alpha)),
        }
    }
    if let Some((a, b, al)) = run {
        out.push(Scanline { y, x1: a, x2: b, alpha: al });
    }
}

/// Fill a closed polygon (non-zero-ish winding, anti-aliased) using
/// signed-area accumulation. Pixel (x, y) covers the square [x, x+1) x [y, y+1).
pub fn fill_polygon(pts: &[(f32, f32)], w: i32, h: i32, acc: &mut Vec<f32>, out: &mut Vec<Scanline>) {
    out.clear();
    if pts.len() < 3 {
        return;
    }
    let (mut minx, mut maxx, mut miny, mut maxy) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for &(x, y) in pts {
        minx = minx.min(x);
        maxx = maxx.max(x);
        miny = miny.min(y);
        maxy = maxy.max(y);
    }
    if !(minx.is_finite() && maxx.is_finite() && miny.is_finite() && maxy.is_finite()) {
        return;
    }
    let ry0 = (miny.floor() as i32).max(0);
    let ry1 = (maxy.ceil() as i32).min(h);
    if ry1 <= ry0 || maxx < 0.0 || minx >= w as f32 {
        return;
    }
    let bx0 = minx.floor() as i32;
    let bw = (maxx.ceil() as i32 - bx0 + 3) as usize;
    let rows = (ry1 - ry0) as usize;
    // The buffer is kept all-zero between calls (each row is cleared after
    // it is read), so it only needs zeroing when it grows.
    if acc.len() < bw * rows {
        acc.clear();
        acc.resize(bw * rows, 0.0);
    }
    let ox = bx0 as f32;
    let oy = ry0 as f32;
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        draw_line(acc, bw, rows, (a.0 - ox, a.1 - oy), (b.0 - ox, b.1 - oy));
    }
    for r in 0..rows {
        let row = &acc[r * bw..(r + 1) * bw];
        let y = ry0 + r as i32;
        let mut sum = 0.0f32;
        let mut i = 0;
        while i < bw {
            sum += row[i];
            let alpha = (sum.abs().min(1.0) * 65535.0) as u32;
            // Cells with no change keep the same coverage: one span.
            let mut j = i + 1;
            while j < bw && row[j] == 0.0 {
                j += 1;
            }
            if alpha > 0 {
                let x1 = (bx0 + i as i32).max(0);
                let x2 = (bx0 + j as i32 - 1).min(w - 1);
                if x1 <= x2 {
                    match out.last_mut() {
                        Some(l) if l.y == y && l.alpha == alpha && l.x2 + 1 == x1 => l.x2 = x2,
                        _ => out.push(Scanline { y, x1, x2, alpha }),
                    }
                }
            }
            i = j;
        }
        acc[r * bw..(r + 1) * bw].fill(0.0);
    }
}

/// Signed-area line accumulation (same idea as font-rs). Coordinates are
/// local to the buffer: x >= 0, rows outside 0..rows are skipped.
fn draw_line(acc: &mut [f32], bw: usize, rows: usize, p0: (f32, f32), p1: (f32, f32)) {
    if (p0.1 - p1.1).abs() <= f32::EPSILON {
        return;
    }
    let (dir, p0, p1) = if p0.1 < p1.1 { (1.0f32, p0, p1) } else { (-1.0f32, p1, p0) };
    let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
    let mut x = p0.0;
    let y0 = if p0.1 < 0.0 {
        x -= p0.1 * dxdy;
        0usize
    } else {
        p0.1 as usize
    };
    if p1.1 <= 0.0 {
        return;
    }
    let y1 = (p1.1.ceil() as usize).min(rows);
    let maxi = bw - 1;
    for y in y0..y1 {
        let ls = y * bw;
        let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
        let xnext = x + dxdy * dy;
        let d = dy * dir;
        let (x0, x1) = if x < xnext { (x, xnext) } else { (xnext, x) };
        let x0 = x0.max(0.0);
        let x1 = x1.max(0.0);
        let x0floor = x0.floor();
        let x0i = (x0floor as usize).min(maxi - 1);
        let x1ceil = x1.ceil();
        let x1i = (x1ceil as usize).min(maxi);
        if x1i <= x0i + 1 {
            let xmf = 0.5 * (x + xnext).max(0.0) - x0floor;
            acc[ls + x0i] += d - d * xmf;
            acc[ls + x0i + 1] += d * xmf;
        } else {
            let s = 1.0 / (x1 - x0);
            let x0f = x0 - x0floor;
            let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
            let x1f = x1 - x1ceil + 1.0;
            let am = 0.5 * s * x1f * x1f;
            acc[ls + x0i] += d * a0;
            if x1i == x0i + 2 {
                acc[ls + x0i + 1] += d * (1.0 - a0 - am);
            } else {
                let a1 = s * (1.5 - x0f);
                acc[ls + x0i + 1] += d * (a1 - a0);
                for xi in x0i + 2..x1i - 1 {
                    acc[ls + xi] += d * s;
                }
                let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                acc[ls + x1i - 1] += d * (1.0 - a2 - am);
            }
            acc[ls + x1i] += d * am;
        }
        x = xnext;
    }
}

/// Anti-aliased stroke of a quadratic Bezier with round caps, `width` wide.
/// The curve is cut into straight pieces; each pixel near a piece gets the
/// coverage of a band `width` wide at that distance (union = max).
pub fn stroke_quadratic(
    p: [(f64, f64); 3],
    width: f64,
    w: i32,
    h: i32,
    pts: &mut Vec<(f32, f32)>,
    acc: &mut Vec<f32>,
    ranges: &mut Vec<i32>,
    out: &mut Vec<Scanline>,
) {
    out.clear();
    // Number of pieces so the chord error stays under ~0.05 px.
    let (ddx, ddy) = (p[0].0 - 2.0 * p[1].0 + p[2].0, p[0].1 - 2.0 * p[1].1 + p[2].1);
    let dd = (ddx * ddx + ddy * ddy).sqrt();
    if !dd.is_finite() {
        return;
    }
    let n = ((dd * 2.5).sqrt().ceil() as usize).clamp(1, 128);
    pts.clear();
    for i in 0..=n {
        let t = i as f64 / n as f64;
        let u = 1.0 - t;
        let x = u * u * p[0].0 + 2.0 * u * t * p[1].0 + t * t * p[2].0;
        let y = u * u * p[0].1 + 2.0 * u * t * p[1].1 + t * t * p[2].1;
        pts.push((x as f32, y as f32));
    }
    let hw = (width / 2.0) as f32;
    let reach = hw + 0.5; // farther than this from the curve: no coverage
    let reach2 = reach * reach;
    let (mut minx, mut maxx, mut miny, mut maxy) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for &(x, y) in pts.iter() {
        minx = minx.min(x);
        maxx = maxx.max(x);
        miny = miny.min(y);
        maxy = maxy.max(y);
    }
    let bx0 = ((minx - reach).floor() as i32).max(0);
    let bx1 = ((maxx + reach).ceil() as i32).min(w - 1);
    let by0 = ((miny - reach).floor() as i32).max(0);
    let by1 = ((maxy + reach).ceil() as i32).min(h - 1);
    if bx1 < bx0 || by1 < by0 {
        return;
    }
    let bw = (bx1 - bx0 + 1) as usize;
    let bh = (by1 - by0 + 1) as usize;
    // Only touched cells are read and then cleared, so the buffer only
    // needs zeroing when it grows. ranges = per-row [min, max] touched x.
    if acc.len() < bw * bh {
        acc.clear();
        acc.resize(bw * bh, 0.0);
    }
    ranges.clear();
    ranges.resize(bh * 2, 0);
    for r in 0..bh {
        ranges[r * 2] = i32::MAX;
        ranges[r * 2 + 1] = i32::MIN;
    }
    // 1D box-filter coverage of a band of half-width hw at distance d.
    let coverage = |d: f32| ((d + hw).min(0.5) - (d - hw).max(-0.5)).max(0.0);
    for s in 0..n {
        let (ax, ay) = pts[s];
        let (bx, by) = pts[s + 1];
        let (dx, dy) = (bx - ax, by - ay);
        let l2 = dx * dx + dy * dy;
        let inv_l2 = if l2 > 0.0 { 1.0 / l2 } else { 0.0 };
        let sy0 = ((ay.min(by) - reach - 0.5).floor() as i32).max(by0);
        let sy1 = ((ay.max(by) + reach - 0.5).ceil() as i32).min(by1);
        for py in sy0..=sy1 {
            let cy = py as f32 + 0.5;
            // x range of the piece's points within `reach` rows of cy.
            let (xa, xb) = if dy.abs() > 1e-4 {
                let t0 = ((cy - reach - ay) / dy).clamp(0.0, 1.0);
                let t1 = ((cy + reach - ay) / dy).clamp(0.0, 1.0);
                let (x0, x1) = (ax + t0 * dx, ax + t1 * dx);
                (x0.min(x1), x0.max(x1))
            } else {
                (ax.min(bx), ax.max(bx))
            };
            let px0 = ((xa - reach - 0.5).floor() as i32).max(bx0);
            let px1 = ((xb + reach - 0.5).ceil() as i32).min(bx1);
            if px0 > px1 {
                continue;
            }
            let ri = (py - by0) as usize * 2;
            ranges[ri] = ranges[ri].min(px0);
            ranges[ri + 1] = ranges[ri + 1].max(px1);
            let row = (py - by0) as usize * bw;
            for px in px0..=px1 {
                let cx = px as f32 + 0.5;
                let t = (((cx - ax) * dx + (cy - ay) * dy) * inv_l2).clamp(0.0, 1.0);
                let (ex, ey) = (cx - (ax + t * dx), cy - (ay + t * dy));
                let d2 = ex * ex + ey * ey;
                if d2 >= reach2 {
                    continue;
                }
                let c = coverage(d2.sqrt());
                let cell = &mut acc[row + (px - bx0) as usize];
                if c > *cell {
                    *cell = c;
                }
            }
        }
    }
    for r in 0..bh {
        let (a, b) = (ranges[r * 2], ranges[r * 2 + 1]);
        if a > b {
            continue;
        }
        let row = &mut acc[r * bw + (a - bx0) as usize..=r * bw + (b - bx0) as usize];
        emit_row(out, by0 + r as i32, a, row.len(), w, |i| row[i]);
        row.fill(0.0);
    }
}
