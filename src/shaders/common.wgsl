// Prepended to every shader.
struct Globals {
    view_proj: mat4x4<f32>,
    light_dir: vec4<f32>,
    // x, y: viewport size in physical pixels; z: planet radius (m); w: line width (px)
    params: vec4<f32>,
    // x: bitmask of visible LAYER_* flags; y unused; z: base map style
    // (STYLE_*)
    flags: vec4<u32>,
    // The slice of the planet shown: center (unit vector, xyz) and the
    // cosine of its angular radius (w)
    slice: vec4<f32>,
    // x: terrain vertical exaggeration; y: ground meters per CSS pixel at
    // the view's center (dashes keep a steady length on screen); zw unused
    terrain: vec4<f32>,
};

// Must match the constants in view.rs.
const LAYER_CELLS: u32 = 1u;
const LAYER_FERRIES: u32 = 2u;
const STYLE_CARTOGRAPHIC: u32 = 0u;
const STYLE_HYPSOMETRIC: u32 = 1u;

const PI: f32 = 3.14159265358979;

@group(0) @binding(0) var<uniform> globals: Globals;

// The canvas takes sRGB bytes as they are (no sRGB view: see
// renderer.rs), so every fragment shader passes its linear color through
// this last. Blending then happens on sRGB values, a small difference for
// the translucent cells and lines.
fn display(color: vec4<f32>) -> vec4<f32> {
    let c = clamp(color.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let encoded = select(
        1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055,
        c * 12.92,
        c <= vec3<f32>(0.0031308),
    );
    return vec4<f32>(encoded, color.a);
}

fn lambert(n: vec3<f32>) -> f32 {
    return max(dot(normalize(n), globals.light_dir.xyz), 0.0);
}

// Whether unit vector p is on the slice of the planet shown.
fn in_slice(p: vec3<f32>) -> bool {
    return dot(p, globals.slice.xyz) >= globals.slice.w;
}

fn unit_at(lon: f32, lat: f32) -> vec3<f32> {
    return vec3<f32>(cos(lat) * cos(lon), sin(lat), -cos(lat) * sin(lon));
}

// Clip position of a world-space point (meters).
fn world_to_clip(world: vec3<f32>) -> vec4<f32> {
    return globals.view_proj * vec4<f32>(world, 1.0);
}
