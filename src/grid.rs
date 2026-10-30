//! The areas the map covers, and the H3 cell under a point. The grid drawn
//! over the map is in hexgrid.rs.

use h3o::{CellIndex, LatLng, Resolution};

use crate::geo::{Cap, LonLat};

/// The part of the planet shown: 200 km around Kitsap County (near
/// Silverdale), out to Vancouver, BC, the Cascade crest and the Pacific
/// shelf edge.
pub const KITSAP_SLICE: Cap = Cap {
    center: LonLat::new(-122.65, 47.64),
    radius_m: 200_000.0,
};

/// The H3 cell containing `p`. Note H3 takes (lat, lng), the reverse of our
/// GeoJSON order.
pub fn h3_cell(p: LonLat, resolution: Resolution) -> CellIndex {
    LatLng::new(p.lat, p.lon)
        .expect("finite coordinates")
        .to_cell(resolution)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_h3_reference_example() {
        // The example from the H3 documentation.
        let p = LonLat::new(-122.0553238, 37.3615593);
        assert_eq!(h3_cell(p, Resolution::Five).to_string(), "85283473fffffff");
    }

    #[test]
    fn slice_reaches_victoria_not_portland() {
        // Victoria, BC (about 105 km) is in, Portland (about 240 km) is
        // not.
        assert!(KITSAP_SLICE.contains(LonLat::new(-123.37, 48.43).to_unit()));
        assert!(!KITSAP_SLICE.contains(LonLat::new(-122.68, 45.52).to_unit()));
    }
}
