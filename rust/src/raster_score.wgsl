// Draws and scores a batch of candidate shapes. One workgroup per candidate.
// Same two phases as score.wgsl, but the pixels and their coverage are
// computed here from the shape's numbers instead of being uploaded as spans.
// The color and blend math is in common.wgsl, which gpu.rs puts in front of
// this file. This file only decides which pixels each thread visits.
//
// Work split: the 64 threads form 4 row groups of 16 lanes. Row group g
// takes rows y0+g, y0+g+4, ...; within a row, lane l takes columns
// c0+l, c0+l+16, ...

// One candidate. Kinds use the shape mode numbers (circles use 3).
//   1-5 hard-edged shapes: see walk_triangle, walk_rectangle, walk_ellipse,
//              walk_rotated_rect. Numbers are whole, in image coordinates.
//   8 polygon: p0 = (x0, y0, x1, y1), p1 = (x2, y2, x3, y3) in local
//              coordinates (minus ox, oy), p2.x = ox. oy = y0.
//   7 rotated ellipse: p0 = (cx, cy, m, W) local, p1.x = h, p1.y = ox.
//              See walk_rotated_ellipse. oy = y0.
//   6 curve:   p0 = (x0, y0, x1, y1), p1 = (x2, y2, n, j) with the three
//              control points local, n pieces, split point j;
//              p2 = (ox, half width, reach). oy = y0. See walk_curve.
struct Cand {
    kind: u32,
    alpha: u32,
    y0: i32, // first image row
    y1: i32, // last image row (inclusive); y1 < y0 means empty
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> target_img: array<u32>;
@group(0) @binding(2) var<storage, read> current_img: array<u32>;
@group(0) @binding(3) var<storage, read> cands: array<Cand>;
@group(0) @binding(4) var<storage, read_write> results: array<Result>;
@group(0) @binding(5) var<storage, read> prefix: array<i32>;

const LANES: i32 = 16;
const ROWS: i32 = 4;

// ---- polygon ----
// The CPU (raster.rs fill_polygon) adds each edge's signed area into cells
// and takes a running sum along the row. The running sum at cell c equals,
// per edge, dir * (integral over the edge's part in this row of
// clamp(c + 1 - X(y), 0, 1) dy). That is computed directly here.

struct PolyRow {
    xa: vec4<f32>, // edge x where it enters the row (local)
    xb: vec4<f32>, // edge x where it leaves the row
    d: vec4<f32>,  // dir * dy; 0 if the edge misses the row
    c0: i32,       // first local column with possible coverage
    c1: i32,       // last local column
}

fn poly_pt(cd: Cand, i: u32) -> vec2<f32> {
    switch i {
        case 0u: { return cd.p0.xy; }
        case 1u: { return cd.p0.zw; }
        case 2u: { return cd.p1.xy; }
        default: { return cd.p1.zw; }
    }
}

fn poly_row(cd: Cand, r: f32) -> PolyRow {
    var pr: PolyRow;
    var mn = 3.0e38;
    var mx = -3.0e38;
    for (var i = 0u; i < 4u; i++) {
        var a = poly_pt(cd, i);
        var b = poly_pt(cd, (i + 1u) % 4u);
        pr.d[i] = 0.0;
        pr.xa[i] = 0.0;
        pr.xb[i] = 0.0;
        if (abs(a.y - b.y) <= 1.1920929e-7) {
            continue;
        }
        var dir = 1.0;
        if (a.y > b.y) {
            dir = -1.0;
            let t = a;
            a = b;
            b = t;
        }
        let ya = max(a.y, r);
        let yb = min(b.y, r + 1.0);
        if (yb <= ya) {
            continue;
        }
        let dxdy = (b.x - a.x) / (b.y - a.y);
        let xa = a.x + (ya - a.y) * dxdy;
        let xb = a.x + (yb - a.y) * dxdy;
        pr.xa[i] = xa;
        pr.xb[i] = xb;
        pr.d[i] = dir * (yb - ya);
        mn = min(mn, min(xa, xb));
        mx = max(mx, max(xa, xb));
    }
    if (mx < mn) {
        pr.c0 = 1;
        pr.c1 = 0;
    } else {
        pr.c0 = i32(floor(max(mn, 0.0)));
        pr.c1 = i32(floor(mx));
    }
    return pr;
}

// Mean of clamp(1 - u, 0, 1) for u going linearly from ua to ub.
fn edge_frac(ua_in: f32, ub_in: f32) -> f32 {
    let ua = min(ua_in, ub_in);
    let ub = max(ua_in, ub_in);
    if (ub <= 0.0) {
        return 1.0;
    }
    if (ua >= 1.0) {
        return 0.0;
    }
    let len = ub - ua;
    if (len < 1.0e-6) {
        return clamp(1.0 - 0.5 * (ua + ub), 0.0, 1.0);
    }
    var s = max(0.0, min(ub, 0.0) - ua);
    let l = max(ua, 0.0);
    let h = min(ub, 1.0);
    if (h > l) {
        s += (h - l) * (1.0 - 0.5 * (h + l));
    }
    return s / len;
}

fn poly_cov(pr: PolyRow, c: f32) -> f32 {
    var s = 0.0;
    for (var i = 0u; i < 4u; i++) {
        if (pr.d[i] != 0.0) {
            s += pr.d[i] * edge_frac(pr.xa[i] - c, pr.xb[i] - c);
        }
    }
    return s;
}

fn walk_polygon(cd: Cand, phase: u32, lid: u32) {
    let w = i32(params.width);
    let ox = i32(cd.p2.x);
    let lane = i32(lid) % LANES;
    for (var y = cd.y0 + i32(lid) / LANES; y <= cd.y1; y += ROWS) {
        let pr = poly_row(cd, f32(y - cd.y0));
        let c0 = max(pr.c0, -ox);
        let c1 = min(pr.c1, w - 1 - ox);
        for (var c = c0 + lane; c <= c1; c += LANES) {
            let s = poly_cov(pr, f32(c));
            let ma = u32(min(abs(s), 1.0) * 65535.0);
            if (ma > 0u) {
                visit(phase, u32(y * w + ox + c), ma);
            }
        }
    }
}

// ---- rotated ellipse ----
// The CPU draws the ellipse as a polygon with many points (raster.rs
// fill_polygon). Here, in local coordinates, the ellipse at height t is
// x from xc(t) - half(t) to xc(t) + half(t), with xc(t) = cx + m (t - cy) and
// half(t) = W sqrt(1 - ((t - cy) / h)^2). In each pixel row, the left and
// right outlines are cut into EK straight pieces (evenly spaced in the angle
// psi, t = cy + h sin(psi), so the pieces stay short near the top and
// bottom), and each piece is handled like a polygon edge.

const EK: u32 = 4u;

struct EllRow {
    xl: array<f32, 5>, // left outline x at the piece ends (EK + 1)
    xr: array<f32, 5>, // right outline x
    dt: array<f32, 4>, // height of each piece
    full: f32,         // covered height of the row
    lmax: f32,         // pixels between lmax and rmin are fully covered
    rmin: f32,
    c0: i32,
    c1: i32,
}

fn ell_row(cd: Cand, r: f32) -> EllRow {
    var er: EllRow;
    let cx = cd.p0.x;
    let cy = cd.p0.y;
    let m = cd.p0.z;
    let wd = cd.p0.w;
    let h = cd.p1.x;
    let ta = max(r, cy - h);
    let tb = min(r + 1.0, cy + h);
    er.c0 = 1;
    er.c1 = 0;
    if (tb <= ta || h <= 0.0) {
        return er;
    }
    er.full = tb - ta;
    let pa = asin(clamp((ta - cy) / h, -1.0, 1.0));
    let pb = asin(clamp((tb - cy) / h, -1.0, 1.0));
    var lmin = 3.0e38;
    var lmax = -3.0e38;
    var rmin = 3.0e38;
    var rmax = -3.0e38;
    var tp = ta;
    for (var j = 0u; j <= EK; j++) {
        let p = pa + (pb - pa) * f32(j) / f32(EK);
        var t = cy + h * sin(p);
        if (j == 0u) { t = ta; }
        if (j == EK) { t = tb; }
        let xc = cx + m * (t - cy);
        let hw = wd * cos(p);
        er.xl[j] = xc - hw;
        er.xr[j] = xc + hw;
        if (j > 0u) {
            er.dt[j - 1u] = t - tp;
        }
        tp = t;
        lmin = min(lmin, er.xl[j]);
        lmax = max(lmax, er.xl[j]);
        rmin = min(rmin, er.xr[j]);
        rmax = max(rmax, er.xr[j]);
    }
    er.lmax = lmax;
    er.rmin = rmin;
    er.c0 = i32(floor(max(lmin, 0.0)));
    er.c1 = i32(floor(rmax));
    return er;
}

fn ell_cov(er: EllRow, c: f32) -> f32 {
    // Left outline: all of it left of the pixel gives the full height.
    var s = er.full;
    if (c < er.lmax) {
        s = 0.0;
        for (var j = 0u; j < EK; j++) {
            s += er.dt[j] * edge_frac(er.xl[j] - c, er.xl[j + 1u] - c);
        }
    }
    // Right outline: all of it right of the pixel takes nothing away.
    if (c + 1.0 > er.rmin) {
        for (var j = 0u; j < EK; j++) {
            s -= er.dt[j] * edge_frac(er.xr[j] - c, er.xr[j + 1u] - c);
        }
    }
    return s;
}

fn walk_rotated_ellipse(cd: Cand, phase: u32, lid: u32) {
    let w = i32(params.width);
    let ox = i32(cd.p1.y);
    let lane = i32(lid) % LANES;
    for (var y = cd.y0 + i32(lid) / LANES; y <= cd.y1; y += ROWS) {
        let er = ell_row(cd, f32(y - cd.y0));
        let c0 = max(er.c0, -ox);
        let c1 = min(er.c1, w - 1 - ox);
        for (var c = c0 + lane; c <= c1; c += LANES) {
            let s = ell_cov(er, f32(c));
            let ma = u32(clamp(s, 0.0, 1.0) * 65535.0);
            if (ma > 0u) {
                visit(phase, u32(y * w + ox + c), ma);
            }
        }
    }
}

// ---- curve ----
// The CPU (raster.rs stroke_quadratic) cuts the curve into n straight pieces
// and gives each pixel the band coverage of its distance to the nearest
// piece. The same pieces are used here. Their end points are computed once
// per candidate into cv_pts (curve_setup). The curve is split at the point
// nearest its top or bottom (point j), so each half crosses a row only once:
// a row then has at most two column ranges, one per half.

var<workgroup> cv_pts: array<vec2<f32>, 129>;

fn curve_setup(cd: Cand, lid: u32) {
    let n = u32(cd.p1.z);
    for (var i = lid; i <= n; i += WG) {
        let t = f32(i) / f32(n);
        let u = 1.0 - t;
        cv_pts[i] = u * u * cd.p0.xy + 2.0 * u * t * cd.p0.zw + t * t * cd.p1.xy;
    }
}

struct CurveRow {
    a: vec2<i32>,  // columns of the first half (x > y means none)
    b: vec2<i32>,  // columns of the second half
    sa: vec2<i32>, // pieces of the first half that reach the row
    sb: vec2<i32>,
}

// First point index i in [lo, hi] with sg * y_i > v (hi + 1 if none).
fn first_above(lo: i32, hi: i32, sg: f32, v: f32) -> i32 {
    var a = lo;
    var b = hi + 1;
    while (a < b) {
        let m = (a + b) / 2;
        if (sg * cv_pts[m].y > v) {
            b = m;
        } else {
            a = m + 1;
        }
    }
    return a;
}

// Pieces of the half with points lo..hi whose height range may overlap
// (lowy, highy). x > y means none.
fn half_range(lo: i32, hi: i32, lowy: f32, highy: f32) -> vec2<i32> {
    if (hi <= lo) {
        return vec2<i32>(1, 0);
    }
    var sg = 1.0;
    var a = lowy;
    var b = highy;
    if (cv_pts[hi].y < cv_pts[lo].y) {
        sg = -1.0;
        a = -highy;
        b = -lowy;
    }
    // Pieces from the one ending above a, to the one starting below b.
    let s0 = first_above(lo, hi, sg, a) - 1;
    let s1 = first_above(lo, hi, sg, b) - 1;
    return vec2<i32>(max(s0 - 1, lo), min(s1 + 1, hi - 1));
}

fn curve_row(cd: Cand, r: i32) -> CurveRow {
    var cr: CurveRow;
    cr.a = vec2<i32>(1 << 30, -(1 << 30));
    cr.b = cr.a;
    cr.sa = vec2<i32>(1, 0);
    cr.sb = cr.sa;
    let n = i32(cd.p1.z);
    let j = i32(cd.p1.w);
    let reach = cd.p2.z;
    let cy = f32(r) + 0.5;
    var sa0 = n;
    var sa1 = -1;
    var sb0 = n;
    var sb1 = -1;
    // Each half only goes up or only down, so the pieces that can reach the
    // row are found by halving searches. One extra piece on each side
    // covers rounding; the exact test below still decides.
    let lowy = f32(r) - 0.5 - reach;
    let highy = f32(r) + 1.5 + reach;
    let ra = half_range(0, j, lowy, highy);
    let rb = half_range(j, n, lowy, highy);
    for (var q = 0; q < 2; q++) {
        var s0 = ra.x;
        var s1 = ra.y;
        if (q == 1) {
            s0 = rb.x;
            s1 = rb.y;
        }
        for (var s = s0; s <= s1; s++) {
            let pa = cv_pts[s];
            let pb = cv_pts[s + 1];
            let sy0 = i32(floor(min(pa.y, pb.y) - reach - 0.5));
            let sy1 = i32(ceil(max(pa.y, pb.y) + reach - 0.5));
            if (r < sy0 || r > sy1) {
                continue;
            }
            let dx = pb.x - pa.x;
            let dy = pb.y - pa.y;
            var xa = min(pa.x, pb.x);
            var xb = max(pa.x, pb.x);
            if (abs(dy) > 1.0e-4) {
                let t0 = clamp((cy - reach - pa.y) / dy, 0.0, 1.0);
                let t1 = clamp((cy + reach - pa.y) / dy, 0.0, 1.0);
                let x0 = pa.x + t0 * dx;
                let x1 = pa.x + t1 * dx;
                xa = min(x0, x1);
                xb = max(x0, x1);
            }
            let px0 = i32(floor(xa - reach - 0.5));
            let px1 = i32(ceil(xb + reach - 0.5));
            if (s < j) {
                cr.a = vec2<i32>(min(cr.a.x, px0), max(cr.a.y, px1));
                sa0 = min(sa0, s);
                sa1 = max(sa1, s);
            } else {
                cr.b = vec2<i32>(min(cr.b.x, px0), max(cr.b.y, px1));
                sb0 = min(sb0, s);
                sb1 = max(sb1, s);
            }
        }
    }
    cr.sa = vec2<i32>(sa0, sa1);
    cr.sb = vec2<i32>(sb0, sb1);
    // Overlapping ranges become one, so no pixel is visited twice.
    if (cr.a.x <= cr.a.y && cr.b.x <= cr.b.y && cr.a.x <= cr.b.y + 1 && cr.b.x <= cr.a.y + 1) {
        cr.a = vec2<i32>(min(cr.a.x, cr.b.x), max(cr.a.y, cr.b.y));
        cr.b = vec2<i32>(1, 0);
    }
    return cr;
}

fn seg_d2(s0: i32, s1: i32, c: vec2<f32>, d2_in: f32) -> f32 {
    var d2 = d2_in;
    for (var s = s0; s <= s1; s++) {
        let pa = cv_pts[s];
        let d = cv_pts[s + 1] - pa;
        let l2 = dot(d, d);
        var inv = 0.0;
        if (l2 > 0.0) {
            inv = 1.0 / l2;
        }
        let t = clamp(dot(c - pa, d) * inv, 0.0, 1.0);
        let e = c - (pa + t * d);
        d2 = min(d2, dot(e, e));
    }
    return d2;
}

fn walk_curve(cd: Cand, phase: u32, lid: u32) {
    let w = i32(params.width);
    let ox = i32(cd.p2.x);
    let hw = cd.p2.y;
    let reach = cd.p2.z;
    let lane = i32(lid) % LANES;
    for (var y = cd.y0 + i32(lid) / LANES; y <= cd.y1; y += ROWS) {
        let r = y - cd.y0;
        let cr = curve_row(cd, r);
        let a0 = max(cr.a.x, -ox);
        let a1 = min(cr.a.y, w - 1 - ox);
        let b0 = max(cr.b.x, -ox);
        let b1 = min(cr.b.y, w - 1 - ox);
        let na = max(a1 - a0 + 1, 0);
        let nb = max(b1 - b0 + 1, 0);
        let cy = f32(r) + 0.5;
        for (var k = lane; k < na + nb; k += LANES) {
            var c = a0 + k;
            if (k >= na) {
                c = b0 + k - na;
            }
            let p = vec2<f32>(f32(c) + 0.5, cy);
            var d2 = seg_d2(cr.sa.x, cr.sa.y, p, 3.0e38);
            d2 = seg_d2(cr.sb.x, cr.sb.y, p, d2);
            if (d2 >= reach * reach) {
                continue;
            }
            let d = sqrt(d2);
            let cov = max(min(d + hw, 0.5) - max(d - hw, -0.5), 0.0);
            let ma = u32(clamp(cov, 0.0, 1.0) * 65535.0);
            if (ma > 0u) {
                visit(phase, u32(y * w + ox + c), ma);
            }
        }
    }
}

// ---- hard-edged shapes (modes 1-5) ----
// Full coverage, whole-number rows, same rounding as shapes.rs. Each row is
// one column range a..b (image columns, inclusive).

fn visit_row(phase: u32, y: i32, a_in: i32, b_in: i32, lane: i32) {
    let w = i32(params.width);
    let a = max(a_in, 0);
    let b = min(b_in, w - 1);
    if (phase == 0u) {
        // Color sums of the whole span from the running-sum table
        // (prefix.wgsl): the same whole numbers as adding pixel by pixel.
        if (lane == 0 && b >= a) {
            let i0 = u32(y * (w + 1) + a) * 6u;
            let i1 = u32(y * (w + 1) + b + 1) * 6u;
            sd += vec3<i32>(prefix[i1] - prefix[i0], prefix[i1 + 1u] - prefix[i0 + 1u], prefix[i1 + 2u] - prefix[i0 + 2u]);
            sc += vec3<i32>(prefix[i1 + 3u] - prefix[i0 + 3u], prefix[i1 + 4u] - prefix[i0 + 4u], prefix[i1 + 5u] - prefix[i0 + 5u]);
            cnt += b - a + 1;
        }
        return;
    }
    for (var x = a + lane; x <= b; x += LANES) {
        visit(phase, u32(y * w + x), 65535u);
    }
}

fn walk_rectangle(cd: Cand, phase: u32, lid: u32) {
    let lane = i32(lid) % LANES;
    for (var y = cd.y0 + i32(lid) / LANES; y <= cd.y1; y += ROWS) {
        visit_row(phase, y, i32(cd.p0.x), i32(cd.p0.y), lane);
    }
}

// p0 = (x, y, rx, ry). Half width s = trunc(sqrt(ry^2 - dy^2) * rx / ry),
// exact: the largest s with (s ry)^2 <= (ry^2 - dy^2) rx^2. Same as
// ellipse_half_width in shapes.rs.
fn walk_ellipse(cd: Cand, phase: u32, lid: u32) {
    let lane = i32(lid) % LANES;
    let cx = i32(cd.p0.x);
    let cy = i32(cd.p0.y);
    let rx = i32(cd.p0.z);
    let ry = i32(cd.p0.w);
    let fry = f32(ry);
    for (var y = cd.y0 + i32(lid) / LANES; y <= cd.y1; y += ROWS) {
        let dy = abs(y - cy);
        let k = ry * ry - dy * dy;
        let n = mul64(u32(k), u32(rx * rx));
        var s = i32(sqrt(f32(k)) * f32(rx) / fry);
        for (var i = 0; i < 2; i++) {
            let t = u32((s + 1) * ry);
            if (le64(mul64(t, t), n)) { s += 1; }
        }
        for (var i = 0; i < 2; i++) {
            let t = u32(s * ry);
            if (s > 0 && !le64(mul64(t, t), n)) { s -= 1; }
        }
        visit_row(phase, y, cx - s, cx + s, lane);
    }
}

// Triangle, split by the CPU into a part that widens downward from apex
// (p0.x, p0.y) to row p1.x, with end columns p0.z, p0.w (p1.y = 1 if the
// part exists), and a part that narrows downward to apex (p1.z, p1.w) from
// row p2.z, with end columns p2.x, p2.y. Same steps as triangle_bottom and
// triangle_top in shapes.rs, done as exact fractions.
fn walk_triangle(cd: Cand, phase: u32, lid: u32) {
    let w = i32(params.width);
    let lane = i32(lid) % LANES;
    let bx = i32(cd.p0.x);
    let by = i32(cd.p0.y);
    let bl = i32(cd.p0.z);
    let br = i32(cd.p0.w);
    let byb = i32(cd.p1.x);
    let has_b = cd.p1.y > 0.5;
    let tx = i32(cd.p1.z);
    let ty = i32(cd.p1.w);
    let tl = i32(cd.p2.x);
    let tr = i32(cd.p2.y);
    let tyt = i32(cd.p2.z);
    for (var y = cd.y0 + i32(lid) / LANES; y <= cd.y1; y += ROWS) {
        var a: i32;
        var b: i32;
        if (has_b && y <= byb) {
            let d = byb - by;
            let k = y - by;
            a = (bx * d + k * (bl - bx)) / d;
            b = (bx * d + k * (br - bx)) / d;
        } else {
            let d = ty - tyt;
            let k = ty - y + 1;
            a = (tx * d - k * (tx - tl)) / d;
            b = (tx * d - k * (tx - tr)) / d;
        }
        if (a > b) {
            let t = a;
            a = b;
            b = t;
        }
        if (a >= w || b < 0) {
            continue;
        }
        visit_row(phase, y, a, b, lane);
    }
}

// Rotated rectangle: the CPU (rasterize_rotated_rect) walks each edge in
// `count` evenly spaced samples and keeps the smallest and largest sample x
// per row. Sample j of an edge from f to f + d is trunc(f + d j / (count-1)).
// Sample y only goes one way along an edge, so the samples in a row are one
// run of j, found directly (rr_run). p0, p1 = the 4 corners, p2 = the 4
// sample counts.

fn rr_samp(f: i32, d: i32, m: i32, j: i32) -> i32 {
    return (f * m + j * d) / m;
}

// floor(a / b) and ceil(a / b) for b > 0.
fn fdiv(a: i32, b: i32) -> i32 {
    let q = a / b;
    if (q * b > a) {
        return q - 1;
    }
    return q;
}

fn cdiv(a: i32, b: i32) -> i32 {
    return -fdiv(-a, b);
}

// First and last j in 0..=m whose sample y, trunc((f m + j d) / m), is row
// y (y >= 0). x > y means none. Truncation goes toward zero, so for v the
// exact fraction: trunc(v) >= y is v >= y for y >= 1 and v > -1 for y = 0;
// trunc(v) <= y is v < y + 1; trunc(v) <= -1 is v <= -1.
fn rr_run(f: i32, d: i32, m: i32, y: i32) -> vec2<i32> {
    let fm = f * m;
    var ja: i32;
    var jb: i32;
    if (d == 0) {
        if (f == y) {
            return vec2<i32>(0, m);
        }
        return vec2<i32>(1, 0);
    } else if (d > 0) {
        if (y >= 1) {
            ja = cdiv(y * m - fm, d);
        } else {
            ja = fdiv(-m - fm, d) + 1;
        }
        jb = cdiv((y + 1) * m - fm, d) - 1;
    } else {
        let nd = -d;
        ja = fdiv(fm - (y + 1) * m, nd) + 1;
        if (y >= 1) {
            jb = fdiv(fm - y * m, nd);
        } else {
            jb = cdiv(fm + m, nd) - 1;
        }
    }
    return vec2<i32>(max(ja, 0), min(jb, m));
}

fn rr_pt(cd: Cand, i: u32) -> vec2<i32> {
    return vec2<i32>(poly_pt(cd, i));
}

// The column range of row y (first, last); first > last means empty.
fn rr_range(cd: Cand, y: i32) -> vec2<i32> {
    var mn = i32(params.width);
    var mx = 0;
    for (var i = 0u; i < 4u; i++) {
        let count = i32(cd.p2[i]);
        if (count <= 0) {
            continue;
        }
        let pa = rr_pt(cd, i);
        let d = rr_pt(cd, (i + 1u) % 4u) - pa;
        if (y < min(pa.y, pa.y + d.y) || y > max(pa.y, pa.y + d.y)) {
            continue;
        }
        let m = count - 1;
        let jr = rr_run(pa.y, d.y, m, y);
        if (jr.x > jr.y) {
            continue;
        }
        let xa = rr_samp(pa.x, d.x, m, jr.x);
        let xb = rr_samp(pa.x, d.x, m, jr.y);
        mn = min(mn, min(xa, xb));
        mx = max(mx, max(xa, xb));
    }
    return vec2<i32>(mn, mx);
}

// Column range of each of the first RR_ROWS rows, worked out once per
// candidate: thread i does rows y0 + i, y0 + i + 64, ... Both phases and all
// lanes then read the table. Rows past the table are worked out when needed.
const RR_ROWS: i32 = 256;
var<workgroup> rr_rows: array<vec2<i32>, 256>;

fn rr_setup(cd: Cand, lid: u32) {
    for (var i = i32(lid); i < RR_ROWS && cd.y0 + i <= cd.y1; i += 64) {
        rr_rows[i] = rr_range(cd, cd.y0 + i);
    }
}

fn walk_rotated_rect(cd: Cand, phase: u32, lid: u32) {
    let lane = i32(lid) % LANES;
    for (var y = cd.y0 + i32(lid) / LANES; y <= cd.y1; y += ROWS) {
        var r: vec2<i32>;
        if (y - cd.y0 < RR_ROWS) {
            r = rr_rows[y - cd.y0];
        } else {
            r = rr_range(cd, y);
        }
        visit_row(phase, y, r.x, r.y, lane);
    }
}

fn walk(cd: Cand, phase: u32, lid: u32) {
    switch cd.kind {
        case 1u: { walk_triangle(cd, phase, lid); }
        case 2u: { walk_rectangle(cd, phase, lid); }
        case 3u: { walk_ellipse(cd, phase, lid); }
        case 5u: { walk_rotated_rect(cd, phase, lid); }
        case 6u: { walk_curve(cd, phase, lid); }
        case 7u: { walk_rotated_ellipse(cd, phase, lid); }
        case 8u: { walk_polygon(cd, phase, lid); }
        default: {}
    }
}

@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let ci = wid.x + wid.y * params.nx;
    if (ci >= params.count) {
        return;
    }
    let cd = cands[ci];
    if (cd.kind == 6u && cd.y1 >= cd.y0) {
        curve_setup(cd, lid);
    }
    if (cd.kind == 5u) {
        rr_setup(cd, lid);
    }
    workgroupBarrier();

    begin_color();
    walk(cd, 0u, lid);
    let c8 = finish_color(cd.alpha, lid);
    walk(cd, 1u, lid);
    finish_delta(ci, c8, lid);
}
