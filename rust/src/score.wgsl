// Scores a batch of candidate shapes whose pixels the CPU has already
// worked out and uploaded as spans. One workgroup per candidate.
// Phase 1: sum target and current colors under the shape -> best color.
// Phase 2: blend that color into the current image and sum how much the
//          squared error changes.
// The color and blend math is in common.wgsl, which gpu.rs puts in front of
// this file. This file only decides which pixels each thread visits.

struct Desc {
    span_offset: u32,
    span_count: u32,
    pixel_count: u32,
    alpha: u32,
}

struct Span {
    y: u32,
    x1: u32,
    alpha: u32, // coverage 0..65535
    start: u32, // index of the span's first pixel within the candidate
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> target_img: array<u32>;
@group(0) @binding(2) var<storage, read> current_img: array<u32>;
@group(0) @binding(3) var<storage, read> descs: array<Desc>;
@group(0) @binding(4) var<storage, read> spans: array<Span>;
@group(0) @binding(5) var<storage, read_write> results: array<Result>;

// Last span whose first pixel is <= p.
fn find_span(off: u32, cnt: u32, p: u32) -> u32 {
    var lo = 0u;
    var hi = cnt - 1u;
    while (lo < hi) {
        let mid = (lo + hi + 1u) / 2u;
        if (spans[off + mid].start <= p) {
            lo = mid;
        } else {
            hi = mid - 1u;
        }
    }
    return lo;
}

// Thread lid takes the candidate's pixels lid, lid + 64, lid + 128, ...
fn walk_spans(d: Desc, phase: u32, lid: u32) {
    if (lid >= d.pixel_count) {
        return;
    }
    let w = params.width;
    var s = find_span(d.span_offset, d.span_count, lid);
    var p = lid;
    while (p < d.pixel_count) {
        while (s + 1u < d.span_count && spans[d.span_offset + s + 1u].start <= p) {
            s += 1u;
        }
        let sp = spans[d.span_offset + s];
        visit(phase, sp.y * w + sp.x1 + (p - sp.start), sp.alpha);
        p += WG;
    }
}

@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let ci = wid.x + wid.y * params.nx;
    if (ci >= params.count) {
        return;
    }
    let d = descs[ci];
    begin_color();
    walk_spans(d, 0u, lid);
    let c8 = finish_color(d.alpha, lid);
    walk_spans(d, 1u, lid);
    finish_delta(ci, c8, lid);
}
