// Base map styles: each turns what the terrain shader knows about a point
// into a color. Adding a style means a function here, a STYLE_* constant
// (view.rs and common.wgsl) and a case in `style_color`. Prepended after
// common.wgsl and surface.wgsl.

// What a style gets for one point of terrain.
struct Ground {
    // Height above sea level (m), negative under water.
    height: f32,
    // Meters of height one pixel spans here: for lines of constant
    // pixel width, and antialiasing. From derivatives, which need uniform
    // control flow, so the terrain shader works it out before calling.
    height_per_px: f32,
    // Hillshade at the viewer's exaggeration: 1 on flat ground, more on
    // slopes facing the sun, less on those facing away.
    light: f32,
    // How much of the ground here is water (land cover: lakes, rivers,
    // the sea), 0 to 1, and how much that changes per pixel.
    water: f32,
    water_per_px: f32,
    // About how many screen pixels wide the water here is.
    water_width_px: f32,
};

fn style_color(g: Ground) -> vec3<f32> {
    switch globals.flags.z {
        case STYLE_HYPSOMETRIC: {
            return hypsometric(g);
        }
        default: {
            return cartographic(g);
        }
    }
}

// Coverage of a line `width_px` wide along `level`, antialiased.
fn isoline(g: Ground, level: f32, width_px: f32) -> f32 {
    let px = abs(g.height - level) / max(g.height_per_px, 1e-6);
    return 1.0 - smoothstep(0.5 * width_px, 0.5 * width_px + 1.0, px);
}

// Coverage of contour lines every `interval` m, `width_px` wide, fading
// out where they would crowd closer than a few pixels apart.
fn contours(g: Ground, interval: f32, width_px: f32) -> f32 {
    let spacing_px = interval / max(g.height_per_px, 1e-6);
    let fade = smoothstep(4.0, 10.0, spacing_px);
    let nearest = round(g.height / interval) * interval;
    return isoline(g, nearest, width_px) * fade;
}

// Height colors with full-strength hillshade: the plain physical map.
fn hypsometric(g: Ground) -> vec3<f32> {
    return ramp(TERRAIN_AT, g.height) * g.light;
}

// Cartographic style (Swiss relief): pale tints by height, hillshade with
// cool shadows and warm light, contours, light water deepening with depth,
// depth contours and crisp shorelines around the sea, lakes and rivers.
const SHADOW_TINT: vec3<f32> = vec3<f32>(0.30, 0.34, 0.46);
const HIGHLIGHT_TINT: vec3<f32> = vec3<f32>(0.10, 0.08, 0.05);
// Seabed relief shows at this fraction of the land's.
const WATER_RELIEF: f32 = 0.35;
// Water narrower than this on screen (px) is hidden, and fully shown from
// the second width up.
const WATER_MIN_PX: f32 = 4.0;
const WATER_FULL_PX: f32 = 10.0;

// Shading toward cool shadows and warm sunlit slopes, from `light`.
fn swiss_shade(base: vec3<f32>, light: f32) -> vec3<f32> {
    let shadow = clamp(1.0 - light, 0.0, 1.0);
    let sunlit = clamp(light - 1.0, 0.0, 1.0);
    return base * mix(vec3<f32>(1.0), SHADOW_TINT, shadow) + HIGHLIGHT_TINT * sunlit;
}

fn cartographic(g: Ground) -> vec3<f32> {
    var land = swiss_shade(ramp(CARTO_LAND_AT, g.height), g.light);
    let contour_ink = max(
        max(contours(g, 20.0, 0.6) * 0.18, contours(g, 100.0, 0.8) * 0.35),
        contours(g, 500.0, 1.2) * 0.55,
    );
    land = mix(land, palettes[CONTOUR_AT].rgb, contour_ink);

    // Water by depth where there's bathymetry below sea level; lakes and
    // rivers above it are flat, at the shallowest color.
    let below = f32(g.height < 0.0);
    var water = ramp(CARTO_WATER_AT, min(g.height, 0.0)) * mix(1.0, g.light, WATER_RELIEF * below);
    let depth_ink = max(contours(g, 10.0, 0.6) * 0.15, contours(g, 50.0, 0.8) * 0.35) * below;
    water = mix(water, palettes[DEPTH_CONTOUR_AT].rgb, depth_ink);

    // Land and water split where a pixel is half water, antialiased across
    // one pixel, with the shoreline drawn along it. A river narrower than
    // a pixel never gets there, so partial water also tints faintly.
    let edge_px = (g.water - 0.5) / max(g.water_per_px, 1e-4);
    // Narrow water (ponds, creeks) is detail for close up: it fades in as
    // it grows from a few pixels wide on screen, shoreline and all.
    let shown = smoothstep(WATER_MIN_PX, WATER_FULL_PX, g.water_width_px);
    let wet = max(smoothstep(-0.5, 0.5, edge_px), smoothstep(0.08, 0.5, g.water) * 0.6) * shown;
    // A light line, so many small ponds read as water, not specks.
    let shore = (1.0 - smoothstep(0.3, 1.2, abs(edge_px))) * shown;
    let color = mix(land, water, wet);
    return mix(color, palettes[COASTLINE_AT].rgb, shore * 0.5);
}
