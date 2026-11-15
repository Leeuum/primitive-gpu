// Builds the running-sum table that raster_score.wgsl uses to get the color
// sums of a whole row span with two lookups instead of one visit per pixel.
// Entry (y, x), for x in 0..=width, holds the sums over the pixels left of
// column x in row y: (target - current) for r, g, b, then current r, g, b.
// It is rebuilt each time the canvas changes. One thread per row.

struct Size {
    width: u32,
    height: u32,
    p0: u32,
    p1: u32,
}

@group(0) @binding(0) var<uniform> size: Size;
@group(0) @binding(1) var<storage, read> target_img: array<u32>;
@group(0) @binding(2) var<storage, read> current_img: array<u32>;
@group(0) @binding(3) var<storage, read_write> prefix: array<i32>;

fn rgb(v: u32) -> vec3<i32> {
    return vec3<i32>(i32(v & 255u), i32((v >> 8u) & 255u), i32((v >> 16u) & 255u));
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let y = gid.x;
    if (y >= size.height) {
        return;
    }
    var sd = vec3<i32>(0);
    var sc = vec3<i32>(0);
    var o = y * (size.width + 1u) * 6u;
    for (var x = 0u; x <= size.width; x++) {
        prefix[o] = sd.x;
        prefix[o + 1u] = sd.y;
        prefix[o + 2u] = sd.z;
        prefix[o + 3u] = sc.x;
        prefix[o + 4u] = sc.y;
        prefix[o + 5u] = sc.z;
        o += 6u;
        if (x < size.width) {
            let i = y * size.width + x;
            let c = rgb(current_img[i]);
            sd += rgb(target_img[i]) - c;
            sc += c;
        }
    }
}
