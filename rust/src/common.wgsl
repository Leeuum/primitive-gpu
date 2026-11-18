// Shared by score.wgsl and raster_score.wgsl: gpu.rs puts this file in front
// of each of them. It holds the ONLY GPU copy of the color and blend math.
// The CPU copy is core.rs (best_color_channel, blend_source, keep_factor,
// blend_channel);
// the two must give identical whole-number results. Run ./check.sh after any
// change here or there.
//
// gpu.rs also puts `const PLAIN: bool` in front: true counts plain gaps
// in place of squared gaps (core.rs: Metric).
//
// Each shader declares its own bindings (params, target_img, current_img,
// results) and decides which pixels to visit. The flow in main() is:
//   begin_color();  visit(0u, idx, 0u) per pixel;  let c8 = finish_color(alpha, lid);
//   visit(1u, idx, coverage) per pixel;            finish_delta(ci, c8, lid);

struct Params {
    width: u32,
    height: u32,
    count: u32,
    nx: u32,
}

// The error change is returned exactly as hi * 65536 + lo.
struct Result {
    r: u32,
    g: u32,
    b: u32,
    hi: i32,
    lo: i32,
    p0: u32,
    p1: u32,
    p2: u32,
}

const WG: u32 = 64u;

// Sums over all threads of the workgroup.
var<workgroup> acc: array<atomic<i32>, 7>;
var<workgroup> acc_d: array<atomic<i32>, 2>;
var<workgroup> col: array<u32, 3>;

// Per-thread running sums.
var<private> sd: vec3<i32>;
var<private> sc: vec3<i32>;
var<private> cnt: i32;
var<private> hi: i32;
var<private> lo: i32;
// Phase-2 constants (set by finish_color).
var<private> src: vec3<u32>;
var<private> sa: u32;

fn rgba(v: u32) -> vec4<i32> {
    return vec4<i32>(i32(v & 255u), i32((v >> 8u) & 255u), i32((v >> 16u) & 255u), i32(v >> 24u));
}

// x / 65535 without a division, exact for every x up to 65535 * 65535
// (the largest value the blend makes); checked for all of them.
fn div65535(x: u32) -> u32 {
    return (x + 1u + (x >> 16u)) >> 16u;
}
fn div65535v(x: vec4<u32>) -> vec4<u32> {
    return (x + vec4<u32>(1u) + (x >> vec4<u32>(16u))) >> vec4<u32>(16u);
}

// One pixel. Phase 0 adds it to the color sums (ma is not used). Phase 1
// blends the chosen color with coverage ma (1..65535) and adds the change in
// squared error. core.rs: the loop in compute_color / keep_factor and blend_channel.
fn visit(phase: u32, idx: u32, ma: u32) {
    let t = rgba(target_img[idx]);
    let cv = current_img[idx];
    let c = rgba(cv);
    if (phase == 0u) {
        sd += t.xyz - c.xyz;
        sc += c.xyz;
        cnt += 1;
    } else {
        let af = (65535u - div65535(sa * ma)) * 257u;
        let cu = vec4<u32>(c);
        let o = vec4<i32>(div65535v(cu * af + vec4<u32>(src, sa) * ma) >> vec4<u32>(8u));
        let d1 = t - c;
        let d2 = t - o;
        // PLAIN is set by gpu.rs (core.rs: Metric::gap).
        if (PLAIN) {
            lo += dot(abs(d2), vec4<i32>(1)) - dot(abs(d1), vec4<i32>(1));
        } else {
            lo += dot(d2, d2) - dot(d1, d1);
        }
        hi += lo >> 16u;
        lo = lo & 0xffff;
    }
}

// ---- unsigned 64-bit helpers; a number is (high 32 bits, low 32 bits) ----
fn mul64(a: u32, b: u32) -> vec2<u32> {
    let a0 = a & 0xffffu;
    let a1 = a >> 16u;
    let b0 = b & 0xffffu;
    let b1 = b >> 16u;
    let ll = a0 * b0;
    let mid = a1 * b0 + ((a0 * b1) & 0xffffu) + (ll >> 16u);
    let lo = (mid << 16u) | (ll & 0xffffu);
    let hi = a1 * b1 + ((a0 * b1) >> 16u) + (mid >> 16u);
    return vec2<u32>(hi, lo);
}

fn le64(a: vec2<u32>, b: vec2<u32>) -> bool {
    return a.x < b.x || (a.x == b.x && a.y <= b.y);
}
fn add64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.y + b.y;
    return vec2<u32>(a.x + b.x + u32(lo < a.y), lo);
}
// a - b, for a >= b.
fn sub64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.x - b.x - u32(a.y < b.y), a.y - b.y);
}

// One channel of the best color: clamp(((a * sd + 257 * sc) / count) >> 8, 0, 255)
// in exact whole numbers, as in core.rs best_color_channel. The sum does not
// fit in 32 bits, so it is kept as a 64-bit pair.
fn best_color_channel(sd: i32, sc: i32, count: u32, a: u32) -> u32 {
    let p = mul64(a, u32(abs(sd)));
    let q = mul64(257u, u32(sc));
    var num: vec2<u32>;
    if (sd >= 0) {
        num = add64(p, q);
    } else if (le64(p, q)) {
        num = sub64(q, p);
    } else {
        return 0u; // negative
    }
    // Wanted: the largest v in 0..255 with v * 256 * count <= num.
    // Floating point gets within 1 of it (cheap); two exact tests fix it.
    let est = (f32(a) * f32(sd) + 257.0 * f32(sc)) / (256.0 * f32(count));
    var v = u32(clamp(floor(est), 0.0, 255.0));
    if (!le64(mul64(v * 256u, count), num)) {
        v -= 1u;
    } else if (v < 255u && le64(mul64((v + 1u) * 256u, count), num)) {
        v += 1u;
    }
    return v;
}

// Start of phase 1.
fn begin_color() {
    sd = vec3<i32>(0);
    sc = vec3<i32>(0);
    cnt = 0;
}

// End of phase 1: add up all threads' sums, pick the best color
// (best_color_channel) and set up phase 2 (core.rs: blend_source).
fn finish_color(alpha: u32, lid: u32) -> vec3<u32> {
    if (cnt > 0) {
        atomicAdd(&acc[0], sd.x);
        atomicAdd(&acc[1], sd.y);
        atomicAdd(&acc[2], sd.z);
        atomicAdd(&acc[3], sc.x);
        atomicAdd(&acc[4], sc.y);
        atomicAdd(&acc[5], sc.z);
        atomicAdd(&acc[6], cnt);
    }
    workgroupBarrier();
    // Threads 0, 1 and 2 each work out one channel.
    if (lid < 3u) {
        var c = 0u;
        let n = u32(atomicLoad(&acc[6]));
        if (n > 0u) {
            let a = 65535u / alpha;
            c = best_color_channel(atomicLoad(&acc[lid]), atomicLoad(&acc[lid + 3u]), n, a);
        }
        col[lid] = c;
    }
    workgroupBarrier();
    let c8 = vec3<u32>(col[0], col[1], col[2]);
    sa = alpha * 257u;
    src = vec3<u32>(c8 * 257u * alpha / 255u);
    hi = 0;
    lo = 0;
    return c8;
}

// End of phase 2: add up all threads' error change and write the result.
fn finish_delta(ci: u32, c8: vec3<u32>, lid: u32) {
    if (hi != 0 || lo != 0) {
        atomicAdd(&acc_d[0], hi);
        atomicAdd(&acc_d[1], lo);
    }
    workgroupBarrier();
    if (lid == 0u) {
        results[ci] = Result(c8.x, c8.y, c8.z, atomicLoad(&acc_d[0]), atomicLoad(&acc_d[1]), 0u, 0u, 0u);
    }
}
