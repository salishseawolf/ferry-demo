// Turns freshly loaded height tiles into slope tiles: one dispatch per
// frame, one z layer of invocations per tile. Standalone (no common.wgsl).
//
// Height tiles arrive with a 2 px border (260 px), each pixel (see
// tools/tiles/elevation.py): R, G height as (R * 256 + G) / 8 - 3500 m;
// B the water's width code; A the water fraction as 1 + 254 * fraction.
// Slopes leave as 258 px, the tile plus one texel of each neighbor, of
// (east slope, north slope, height m, water fraction), where a slope is
// meters of rise per meter of ground; widths leave alongside, as the code
// over 255 in red.

struct Job {
    // Layer of `heights` holding the tile, and of `slopes` to fill.
    staging: u32,
    slot: u32,
    // The tile's zoom and row, for its ground scale.
    z: u32,
    y: u32,
};

@group(0) @binding(0) var heights: texture_2d_array<f32>;
@group(0) @binding(1) var slopes: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(2) var<storage, read> jobs: array<Job>;
@group(0) @binding(3) var widths: texture_storage_2d_array<rgba8unorm, write>;

const PI: f32 = 3.14159265358979;
// Web Mercator's sphere.
const MERCATOR_RADIUS_M: f32 = 6378137.0;
const TILE_PX: f32 = 256.0;

fn water(layer: u32, x: i32, y: i32) -> f32 {
    let a = round(textureLoad(heights, vec2<i32>(x, y), layer, 0).a * 255.0);
    return clamp((a - 1.0) / 254.0, 0.0, 1.0);
}

fn height(layer: u32, x: i32, y: i32) -> f32 {
    let c = round(textureLoad(heights, vec2<i32>(x, y), layer, 0).rg * 255.0);
    return (c.r * 256.0 + c.g) / 8.0 - 3500.0;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(slopes).x;
    if id.x >= size || id.y >= size {
        return;
    }
    let job = jobs[id.z];
    // Slope texel i is tile pixel i - 1, which is height pixel i + 1.
    let x = i32(id.x) + 1;
    let y = i32(id.y) + 1;
    // Ground meters per pixel along this row, from its latitude.
    let n = f32(1u << job.z);
    let my = (f32(job.y) + (f32(id.y) - 0.5) / TILE_PX) / n;
    let lat = atan(sinh(PI * (1.0 - 2.0 * my)));
    let m_per_px = 2.0 * PI * MERCATOR_RADIUS_M * cos(lat) / (TILE_PX * n);

    // Horn's method: a 3x3 weighted difference, rows running south.
    let l = job.staging;
    let a = height(l, x - 1, y - 1);
    let b = height(l, x, y - 1);
    let c = height(l, x + 1, y - 1);
    let d = height(l, x - 1, y);
    let e = height(l, x, y);
    let f = height(l, x + 1, y);
    let g = height(l, x - 1, y + 1);
    let h = height(l, x, y + 1);
    let i = height(l, x + 1, y + 1);
    let east = ((c + 2.0 * f + i) - (a + 2.0 * d + g)) / (8.0 * m_per_px);
    let north = ((a + 2.0 * b + c) - (g + 2.0 * h + i)) / (8.0 * m_per_px);
    textureStore(slopes, vec2<i32>(id.xy), job.slot, vec4<f32>(east, north, e, water(l, x, y)));
    let width = textureLoad(heights, vec2<i32>(x, y), l, 0).b;
    textureStore(widths, vec2<i32>(id.xy), job.slot, vec4<f32>(width, 0.0, 0.0, 1.0));
}
