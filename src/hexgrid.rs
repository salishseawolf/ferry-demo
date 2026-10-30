//! The hexagonal grid drawn over the map: Uber's H3, a recursive grid in
//! which each resolution's cells are about a seventh the area of the last.
//! The map shows whichever resolution suits the zoom, and only the cells
//! around the view inside the slice, so it covers all of it at any depth.

use std::collections::HashSet;

use glam::DVec3;
use h3o::{CellIndex, LatLng, Resolution};

use crate::geo::{Cap, LonLat};
use crate::grid::h3_cell;

/// Cells are drawn at least this many CSS pixels across.
const MIN_CELL_PX: f64 = 40.0;
/// The coarsest and finest resolutions drawn.
const MIN_RESOLUTION: u8 = 2;
const MAX_RESOLUTION: u8 = 13;
/// At most this many cells on screen; past it, the next coarser resolution.
const MAX_CELLS: usize = 8000;
/// At most this many cells searched around the view (cheap; only those on
/// screen get outlines).
const MAX_SEARCH: u32 = 40_000;
/// H3 cells vary in size across the planet; rings reach this much further
/// than the average edge length suggests, so the view is always covered.
const RING_SLACK: f64 = 1.2;

/// Average edge length of cells at `resolution`, in meters.
pub fn edge_m(resolution: Resolution) -> f64 {
    resolution.edge_length_km() * 1000.0
}

/// The finest resolution whose cells are at least `MIN_CELL_PX` across
/// when one CSS pixel spans `m_per_px` meters of ground.
pub fn resolution_for(m_per_px: f64) -> Resolution {
    let mut chosen = MIN_RESOLUTION;
    for r in MIN_RESOLUTION..=MAX_RESOLUTION {
        let across = 2.0 * edge_m(resolution(r));
        if across / m_per_px < MIN_CELL_PX {
            break;
        }
        chosen = r;
    }
    resolution(chosen)
}

fn resolution(r: u8) -> Resolution {
    Resolution::try_from(r).expect("valid H3 resolution")
}

/// The cells at `resolution` within about `radius_m` of `center` whose
/// centers lie in `area` and pass `visible` (a unit vector test, for the
/// screen), and the resolution used: coarser if there would be more than
/// `MAX_CELLS` of them, or too many to search.
pub fn cells_around(
    center: LonLat,
    radius_m: f64,
    resolution: Resolution,
    area: &Cap,
    visible: impl Fn(DVec3) -> bool,
) -> (Resolution, Vec<CellIndex>) {
    let mut res = resolution;
    loop {
        // A hexagon ring k out is at least 1.5 k edges from the center (its
        // flat sides are nearest).
        let k = (radius_m * RING_SLACK / (1.5 * edge_m(res))).ceil() as u32 + 1;
        let coarsest = u8::from(res) <= MIN_RESOLUTION;
        if 3 * k * (k + 1) < MAX_SEARCH || coarsest {
            let disk: Vec<CellIndex> = h3_cell(center, res).grid_disk(k);
            let cells: Vec<CellIndex> = disk
                .into_iter()
                .filter(|&c| {
                    let p = unit(LatLng::from(c));
                    area.contains(p) && visible(p)
                })
                .collect();
            if cells.len() <= MAX_CELLS || coarsest {
                return (res, cells);
            }
        }
        res = res.pred().expect("coarser resolution");
    }
}

/// Every edge of `cells` once (shared edges aren't drawn twice), as paths
/// of unit vectors, leaving out edges whose middle lies outside `area`.
pub fn outlines(cells: &[CellIndex], area: &Cap) -> Vec<Vec<DVec3>> {
    let set: HashSet<CellIndex> = cells.iter().copied().collect();
    let mut paths = Vec::new();
    for &cell in cells {
        for edge in cell.edges() {
            let other = edge.destination();
            if other < cell && set.contains(&other) {
                continue;
            }
            let path: Vec<DVec3> = edge.boundary().iter().map(|&ll| unit(ll)).collect();
            let middle = path.iter().sum::<DVec3>();
            if path.len() >= 2 && area.contains(middle) {
                paths.push(path);
            }
        }
    }
    paths
}

/// The outline of `cell` as a closed ring of unit vectors.
pub fn boundary(cell: CellIndex) -> Vec<DVec3> {
    cell.boundary().iter().map(|&ll| unit(ll)).collect()
}

fn unit(ll: LatLng) -> DVec3 {
    LonLat::new(ll.lng(), ll.lat()).to_unit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::KITSAP_SLICE;

    #[test]
    fn zooming_in_picks_finer_cells() {
        // About the whole slice on a 900 px tall view, then about 2 km.
        let wide = resolution_for(450.0);
        let close = resolution_for(2.0);
        assert!(u8::from(wide) <= 5, "{wide}");
        assert!(u8::from(close) >= 10, "{close}");
        // Cells at the chosen resolution are at least MIN_CELL_PX across.
        for m_per_px in [5000.0, 450.0, 30.0, 2.0] {
            let r = resolution_for(m_per_px);
            if u8::from(r) > MIN_RESOLUTION {
                assert!(2.0 * edge_m(r) / m_per_px >= MIN_CELL_PX);
            }
        }
    }

    #[test]
    fn covers_the_slice_without_overflowing() {
        let center = KITSAP_SLICE.center;
        let all = |_: DVec3| true;
        let (res, cells) = cells_around(
            center,
            KITSAP_SLICE.radius_m,
            resolution_for(450.0),
            &KITSAP_SLICE,
            all,
        );
        assert!(!cells.is_empty() && cells.len() <= MAX_CELLS);
        assert!(cells.iter().all(|&c| c.resolution() == res));
        // The slice's edge, 190 km out in every direction, is reached.
        for bearing in [0.0f64, 90.0, 180.0, 270.0] {
            let (east, north) = center.east_north();
            let a = 190_000.0 / crate::geo::EARTH_RADIUS_M;
            let dir = east * bearing.to_radians().sin() + north * bearing.to_radians().cos();
            let p = LonLat::from_vec(center.to_unit() * a.cos() + dir * a.sin());
            assert!(cells.contains(&h3_cell(p, res)), "bearing {bearing}");
        }
        // Asking for too fine a resolution over the whole slice backs off.
        let (fine, many) = cells_around(
            center,
            KITSAP_SLICE.radius_m,
            resolution(10),
            &KITSAP_SLICE,
            all,
        );
        assert!(u8::from(fine) < 10 && many.len() <= MAX_CELLS);
        // Only cells on screen count: a narrow view keeps a fine resolution
        // over the same search radius.
        let c = center.to_unit();
        // About full zoom: a few km searched, a few km² on screen.
        let near = |p: DVec3| p.angle_between(c) * crate::geo::EARTH_RADIUS_M < 1500.0;
        let (kept, few) = cells_around(center, 3000.0, resolution(11), &KITSAP_SLICE, near);
        assert_eq!(u8::from(kept), 11);
        assert!(few.len() <= MAX_CELLS);
    }

    #[test]
    fn outlines_draw_each_edge_once() {
        let center = h3_cell(KITSAP_SLICE.center, resolution(8));
        let cells: Vec<CellIndex> = center.grid_disk(2);
        let paths = outlines(&cells, &KITSAP_SLICE);
        // 19 hexagons have 114 sides: the 30 around the outside, and 84
        // shared in pairs, drawn once each.
        assert_eq!(cells.len(), 19);
        assert_eq!(paths.len(), 30 + 84 / 2);
    }
}
