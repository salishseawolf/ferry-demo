// Colors shared by the surface shaders. Prepended after common.wgsl.

// Cell id of the backdrop, which belongs to no grid cell (mesh::NO_CELL).
const NO_CELL: u32 = 0xffffffffu;

// Color ramps from palette.rs: entries are (linear rgb, value). Offsets
// must match the *_AT constants there.
@group(1) @binding(1) var<storage, read> palettes: array<vec4<f32>>;
const RAMP_LEN: u32 = 12u;
const TERRAIN_AT: u32 = 0u;
const CARTO_LAND_AT: u32 = 12u;
const CARTO_WATER_AT: u32 = 24u;
const BACKDROP_AT: u32 = 36u;
const CONTOUR_AT: u32 = 37u;
const DEPTH_CONTOUR_AT: u32 = 38u;
const COASTLINE_AT: u32 = 39u;

// Color of `value` along the ramp starting at entry `at`, interpolating
// between stops and clamping at the ends.
fn ramp(at: u32, value: f32) -> vec3<f32> {
    var c = palettes[at].rgb;
    for (var i = 1u; i < RAMP_LEN; i++) {
        let a = palettes[at + i - 1u];
        let b = palettes[at + i];
        if value > a.w {
            // Padding repeats the last stop, so guard the zero-width span.
            let t = select(1.0, clamp((value - a.w) / (b.w - a.w), 0.0, 1.0), b.w > a.w);
            c = mix(a.rgb, b.rgb, t);
        }
    }
    return c;
}

// The backdrop's color (it's the only surface drawn by cell; terrain is
// colored by styles.wgsl).
fn surface_albedo(cell: u32) -> vec3<f32> {
    return palettes[BACKDROP_AT].rgb;
}
