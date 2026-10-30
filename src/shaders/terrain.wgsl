// Terrain: one patch per tile region (see terrain.rs), shaded from its
// slopes tile (see normals.wgsl) and colored by the chosen style
// (styles.wgsl). Prepended after common.wgsl, surface.wgsl and
// styles.wgsl.

@group(1) @binding(2) var slopes: texture_2d_array<f32>;
@group(1) @binding(3) var tile_sampler: sampler;
// Slope tiles are the tile's 256 texels plus one of each neighbor (see
// normals.wgsl): blending into that ring at the edges matches the
// neighbor's tile there, so tiles meet without a seam.
const TILE_TEXELS: f32 = 256.0;
const SLOPE_TEXELS: f32 = 258.0;
// How wide the water is, as code / 255 with width = 2^(code / 16) m (see
// normals.wgsl), in the same slots and layout as the slopes.
@group(1) @binding(4) var widths: texture_2d_array<f32>;

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) unit: vec3<f32>,
    // Where this point is in the source tile.
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) slot: u32,
};

@vertex
fn vs_main(
    // Position across the region, (0, 0) its north-west corner.
    @location(0) uv: vec2<f32>,
    // The region's zoom, x and y.
    @location(1) key: vec3<u32>,
    // The cache slot holding its source tile, the region's offset in that
    // tile (xy) and its size there (z).
    @location(2) slot: u32,
    @location(3) source: vec4<f32>,
) -> VertexOut {
    let n = f32(1u << key.x);
    let mx = (f32(key.y) + uv.x) / n;
    let my = (f32(key.z) + uv.y) / n;
    let p = unit_at(mx * 2.0 * PI - PI, atan(sinh(PI * (1.0 - 2.0 * my))));
    var out: VertexOut;
    out.clip = world_to_clip(p * globals.params.z);
    out.unit = p;
    out.uv = source.xy + uv * source.z;
    out.slot = slot;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let p = normalize(in.unit);
    let uv = (in.uv * TILE_TEXELS + 1.0) / SLOPE_TEXELS;
    let s = textureSample(slopes, tile_sampler, uv, in.slot);
    let width_m = exp2(textureSample(widths, tile_sampler, uv, in.slot).r * 255.0 / 16.0);

    // The surface normal from its slopes, steepened by the exaggeration.
    let east = normalize(cross(vec3<f32>(0.0, 1.0, 0.0), p));
    let north = cross(p, east);
    let k = globals.terrain.x;
    let n = normalize(p - k * (s.x * east + s.y * north));

    // Hillshade relative to flat ground, which keeps its style color;
    // slopes facing the sun brighten, those facing away darken.
    let sun = globals.light_dir.xyz;
    let flat = max(dot(p, sun), 0.05);
    let light = 0.35 + 0.65 * max(dot(n, sun), 0.0) / flat;

    let height_per_px = length(vec2<f32>(dpdx(s.z), dpdy(s.z)));
    let water_per_px = length(vec2<f32>(dpdx(s.w), dpdy(s.w)));
    // Ground meters across one screen pixel here.
    let m_per_px = max(length(dpdx(p)), length(dpdy(p))) * globals.params.z;
    let ground = Ground(s.z, height_per_px, light, s.w, water_per_px, width_m / max(m_per_px, 1e-3));
    let color = style_color(ground);
    // After the derivatives above and textureSample.
    if !in_slice(p) {
        discard;
    }
    return display(vec4<f32>(color, 1.0));
}
