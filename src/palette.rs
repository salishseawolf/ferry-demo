//! Color ramps and fixed colors for the base map styles and overlays.
//!
//! This is the one source for both the shader, which receives
//! [`gpu_table`] as a storage buffer, and the on-page color key, which
//! draws the same stops as CSS gradients.

/// A color (sRGB, 0..1) at a value along a ramp.
#[derive(Clone, Copy, Debug)]
pub struct Stop {
    pub value: f64,
    pub srgb: [f64; 3],
}

const fn stop(value: f64, r: f64, g: f64, b: f64) -> Stop {
    Stop {
        value,
        srgb: [r, g, b],
    }
}

/// The slice where no terrain tile has loaded yet.
pub const BACKDROP: [f64; 3] = [0.10, 0.12, 0.16];

/// Terrain by height (m): seabed blues darkening with depth, then muted
/// land tones up to snow. The jump at sea level keeps shorelines crisp.
/// Hillshading supplies the relief, so the land ramp stays gentle.
pub const TERRAIN: &[Stop] = &[
    stop(-3000.0, 0.04, 0.09, 0.20),
    stop(-1000.0, 0.07, 0.17, 0.33),
    stop(-200.0, 0.12, 0.29, 0.46),
    stop(-50.0, 0.21, 0.42, 0.58),
    stop(-5.0, 0.36, 0.58, 0.68),
    stop(0.0, 0.45, 0.66, 0.73),
    stop(0.01, 0.42, 0.50, 0.36),
    stop(400.0, 0.54, 0.58, 0.42),
    stop(1200.0, 0.64, 0.60, 0.48),
    stop(2200.0, 0.74, 0.71, 0.66),
    stop(3200.0, 0.95, 0.95, 0.95),
];

/// Cartographic style, land by height: pale tints that lighten with height
/// (aerial perspective), leaving the relief to the hillshade.
pub const CARTO_LAND: &[Stop] = &[
    stop(0.0, 0.88, 0.90, 0.82),
    stop(150.0, 0.85, 0.88, 0.77),
    stop(600.0, 0.86, 0.85, 0.74),
    stop(1200.0, 0.88, 0.84, 0.75),
    stop(2000.0, 0.92, 0.90, 0.87),
    stop(2800.0, 0.97, 0.97, 0.97),
];

/// Cartographic style, water by depth: light, so data drawn on it stands
/// out, deepening offshore.
pub const CARTO_WATER: &[Stop] = &[
    stop(-3000.0, 0.42, 0.58, 0.74),
    stop(-500.0, 0.55, 0.71, 0.84),
    stop(-150.0, 0.64, 0.78, 0.89),
    stop(-30.0, 0.73, 0.85, 0.93),
    stop(0.0, 0.80, 0.89, 0.95),
];

/// Cartographic style lines.
pub const CONTOUR: [f64; 3] = [0.55, 0.47, 0.38];
pub const DEPTH_CONTOUR: [f64; 3] = [0.38, 0.56, 0.74];
pub const COASTLINE: [f64; 3] = [0.24, 0.40, 0.56];

/// Ferries in map ink: solid under way, paper with an ink outline at the
/// dock, grey when out of service; the UI's chart magenta (index.html
/// --mark, #b0146a) marks the one hovered or highlighted. Their speed vectors and wakes are ink too, and their names
/// (index.html) ink or, out of service, a lighter grey.
pub const VESSEL_INK: [f64; 3] = [0.114, 0.153, 0.200];
pub const VESSEL_PAPER: [f64; 3] = [0.969, 0.961, 0.933];
pub const VESSEL_IDLE: [f64; 3] = [0.490, 0.522, 0.565];
pub const VESSEL_HOVER: [f64; 3] = [0.690, 0.078, 0.416];
/// A terminal's space gauge when its next sailing is nearly full: the
/// map's one warm signal color.
pub const TERMINAL_NEARLY_FULL: [f64; 3] = [0.80, 0.33, 0.16];

/// Entries per ramp in the GPU table; shorter ramps repeat their last stop.
pub const RAMP_LEN: usize = 12;

/// Layout of the GPU table, in entries: must match surface.wgsl.
pub const TERRAIN_AT: usize = 0;
pub const CARTO_LAND_AT: usize = RAMP_LEN;
pub const CARTO_WATER_AT: usize = 2 * RAMP_LEN;
pub const BACKDROP_AT: usize = 3 * RAMP_LEN;
pub const CONTOUR_AT: usize = BACKDROP_AT + 1;
pub const DEPTH_CONTOUR_AT: usize = BACKDROP_AT + 2;
pub const COASTLINE_AT: usize = BACKDROP_AT + 3;

/// All ramps and fixed colors as `[r, g, b, value]` in linear RGB, for the
/// shader.
pub fn gpu_table() -> Vec<[f32; 4]> {
    let entry = |srgb: [f64; 3], value: f64| {
        let [r, g, b] = srgb.map(srgb_to_linear);
        [r as f32, g as f32, b as f32, value as f32]
    };
    let mut table = vec![[0.0; 4]; COASTLINE_AT + 1];
    for (at, ramp) in [
        (TERRAIN_AT, TERRAIN),
        (CARTO_LAND_AT, CARTO_LAND),
        (CARTO_WATER_AT, CARTO_WATER),
    ] {
        assert!(ramp.len() <= RAMP_LEN);
        let last = ramp[ramp.len() - 1];
        for i in 0..RAMP_LEN {
            let s = ramp.get(i).copied().unwrap_or(last);
            table[at + i] = entry(s.srgb, s.value);
        }
    }
    for (at, color) in [
        (BACKDROP_AT, BACKDROP),
        (CONTOUR_AT, CONTOUR),
        (DEPTH_CONTOUR_AT, DEPTH_CONTOUR),
        (COASTLINE_AT, COASTLINE),
    ] {
        table[at] = entry(color, 0.0);
    }
    table
}

/// An sRGB color as linear RGBA, as the shaders take colors outside the
/// palette table.
pub fn linear_rgba(srgb: [f64; 3], alpha: f32) -> [f32; 4] {
    let [r, g, b] = srgb.map(|c| srgb_to_linear(c) as f32);
    [r, g, b, alpha]
}

pub fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// CSS hex color for an sRGB color.
pub fn css(srgb: [f64; 3]) -> String {
    let [r, g, b] = srgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramps_ascend() {
        for ramp in [TERRAIN, CARTO_LAND, CARTO_WATER] {
            assert!(ramp.windows(2).all(|w| w[0].value < w[1].value));
        }
    }

    #[test]
    fn table_layout() {
        let t = gpu_table();
        assert_eq!(t.len(), COASTLINE_AT + 1);
        assert_eq!(t[CARTO_LAND_AT][3], 0.0);
        // Short ramps pad with their last stop.
        assert_eq!(t[CARTO_LAND_AT + RAMP_LEN - 1], t[CARTO_LAND_AT + 5]);
    }

    #[test]
    fn shader_offsets_match() {
        let wgsl = include_str!("shaders/surface.wgsl");
        for (name, at) in [
            ("TERRAIN_AT", TERRAIN_AT),
            ("CARTO_LAND_AT", CARTO_LAND_AT),
            ("CARTO_WATER_AT", CARTO_WATER_AT),
            ("BACKDROP_AT", BACKDROP_AT),
            ("CONTOUR_AT", CONTOUR_AT),
            ("DEPTH_CONTOUR_AT", DEPTH_CONTOUR_AT),
            ("COASTLINE_AT", COASTLINE_AT),
            ("RAMP_LEN", RAMP_LEN),
        ] {
            let decl = format!("const {name}: u32 = {at}u;");
            assert!(wgsl.contains(&decl), "surface.wgsl lacks `{decl}`");
        }
    }

    #[test]
    fn css_hex() {
        assert_eq!(css([1.0, 0.0, 0.5]), "#ff0080");
    }
}
