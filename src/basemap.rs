//! The baked basemap: the terrain as the GPU draws it (in its default
//! style), saved as images in the map's own projection, for browsers that
//! can't draw it themselves (web/flat.rs). `mise run tiles:basemap` bakes
//! them with the app's renderer, so they look the same.
//!
//! Tiles are squares on the map (camera::Projection), quartered level by
//! level from one covering the whole area: x east and y south from the
//! north-west corner, `TILE_PX` pixels a side, as `TileKey`s. Unlike the
//! elevation tiles' Web Mercator, every tile at a level is the same size
//! on the ground.

use glam::DVec2;

use crate::camera::Projection;
use crate::geo::{Cap, LonLat};
use crate::terrain::{Manifest, TileKey};

pub const TILE_PX: u32 = 512;
/// Levels go no finer than about the finest elevation tiles.
pub const MAX_LEVEL: u8 = 6;
/// The elevation tiles are this many pixels a side (elevation.py).
const ELEVATION_TILE_PX: f64 = 256.0;

/// The tiling of an area's map.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub projection: Projection,
    /// The level-0 tile's side (m): the area's diameter on the map.
    side_m: f64,
}

impl Grid {
    pub fn of(area: Cap, radius: f64) -> Self {
        Self {
            projection: Projection::about(area.center, radius),
            side_m: 2.0 * area.angle().sin() * radius,
        }
    }

    /// A tile's side at level `z` (m).
    pub fn tile_m(&self, z: u8) -> f64 {
        self.side_m / f64::from(1u32 << z)
    }

    /// Ground meters per pixel in level `z`'s images.
    pub fn m_per_px(&self, z: u8) -> f64 {
        self.tile_m(z) / f64::from(TILE_PX)
    }

    /// The level whose images are at least as sharp as `m_per_px` (but no
    /// finer than `MAX_LEVEL`).
    pub fn level_for(&self, m_per_px: f64) -> u8 {
        let z = (self.m_per_px(0) / m_per_px.max(1e-9)).log2().ceil();
        z.clamp(0.0, f64::from(MAX_LEVEL)) as u8
    }

    /// `key`'s south-west and north-east corners on the map (m).
    pub fn bounds(&self, key: TileKey) -> (DVec2, DVec2) {
        let t = self.tile_m(key.z);
        let half = self.side_m / 2.0;
        let west = -half + f64::from(key.x) * t;
        let north = half - f64::from(key.y) * t;
        (DVec2::new(west, north - t), DVec2::new(west + t, north))
    }

    /// The level-`z` tiles over the part of the map from `min` to `max`
    /// (south-west and north-east corners, m), as far as the grid goes.
    pub fn covering(&self, z: u8, min: DVec2, max: DVec2) -> impl Iterator<Item = TileKey> + use<> {
        let t = self.tile_m(z);
        let half = self.side_m / 2.0;
        let n = (1u32 << z) as f64;
        let index = |v: f64| (v / t).floor().clamp(0.0, n - 1.0) as u32;
        let (x0, x1) = (index(min.x + half), index(max.x + half));
        let (y0, y1) = (index(half - max.y), index(half - min.y));
        (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| TileKey { z, x, y }))
    }

    /// Whether any of `key` lies within `reach_m` of the map's center: on
    /// the area, if that's its radius.
    fn touches_disk(&self, key: TileKey, reach_m: f64) -> bool {
        let (min, max) = self.bounds(key);
        DVec2::ZERO.clamp(min, max).length() < reach_m
    }

    /// The tiles to bake: every tile over the area at each level, as far
    /// as there are elevation tiles about as fine (`elevation`) under its
    /// center.
    pub fn plan(&self, elevation: &Manifest) -> Vec<TileKey> {
        let reach = self.side_m / 2.0;
        let mut tiles = Vec::new();
        for z in 0..=MAX_LEVEL {
            let zoom = self.elevation_zoom(z);
            let all = self.covering(z, DVec2::splat(-reach), DVec2::splat(reach));
            tiles.extend(all.filter(|&key| {
                let (min, max) = self.bounds(key);
                let center = self.projection.point_at((min + max) / 2.0);
                self.touches_disk(key, reach)
                    && center
                        .is_some_and(|c| elevation.contains(TileKey::at(LonLat::from_vec(c), zoom)))
            }));
        }
        tiles
    }

    /// The elevation tiles' zoom about as sharp as level `z`'s images, at
    /// the map's center.
    fn elevation_zoom(&self, z: u8) -> u8 {
        let lat = LonLat::from_vec(self.projection.up).lat.to_radians();
        let equator = 2.0 * std::f64::consts::PI * self.projection.radius * lat.cos();
        let zoom = (equator / (ELEVATION_TILE_PX * self.m_per_px(z)))
            .log2()
            .round();
        zoom.max(0.0) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::EARTH_RADIUS_M;
    use crate::grid::KITSAP_SLICE;

    fn grid() -> Grid {
        Grid::of(KITSAP_SLICE, EARTH_RADIUS_M)
    }

    #[test]
    fn levels_quarter_the_area() {
        let g = grid();
        // The area's 400 km, a little less on the map.
        assert!((g.tile_m(0) - 399_934.0).abs() < 1.0, "{}", g.tile_m(0));
        assert!((g.m_per_px(6) - 12.2).abs() < 0.1);
        // The level at least as sharp as asked, within the levels baked.
        assert_eq!(g.level_for(g.m_per_px(0)), 0);
        assert_eq!(g.level_for(g.m_per_px(0) * 0.999), 1);
        assert_eq!(g.level_for(30.0), 5);
        assert_eq!(g.level_for(1.0), MAX_LEVEL);
        assert_eq!(g.level_for(1e6), 0);
    }

    #[test]
    fn tiles_cover_their_places() {
        let g = grid();
        let (min, max) = g.bounds(TileKey::ROOT);
        assert!((min + max).length() < 1e-6 && (max - min).x == g.tile_m(0));
        // The north-west quarter is (0, 0), the south-east (1, 1).
        let (min, max) = g.bounds(TileKey { z: 1, x: 0, y: 0 });
        assert!(max.x.abs() < 1e-6 && min.y.abs() < 1e-6 && min.x < 0.0 && max.y > 0.0);
        let found: Vec<_> = g
            .covering(1, DVec2::new(1.0, -5.0), DVec2::new(2.0, -1.0))
            .collect();
        assert_eq!(found, [TileKey { z: 1, x: 1, y: 1 }]);
        // Across the middle: all four, in rows.
        let all: Vec<_> = g
            .covering(1, DVec2::splat(-1.0), DVec2::splat(1.0))
            .collect();
        assert_eq!(all.len(), 4);
        assert_eq!(all[1], TileKey { z: 1, x: 1, y: 0 });
        // Beyond the grid: its edge tiles.
        let edge: Vec<_> = g
            .covering(2, DVec2::splat(1e7), DVec2::splat(2e7))
            .collect();
        assert_eq!(edge, [TileKey { z: 2, x: 3, y: 0 }]);
    }

    #[test]
    fn plans_tiles_where_elevation_is_as_fine() {
        let g = grid();
        // Elevation everywhere down to zoom 12 (about 26 m a pixel here),
        // and zoom 13 only around Seattle, about 16 km square.
        let seattle = LonLat::new(-122.34, 47.6);
        let mut elevation = Vec::new();
        for zoom in 0..=12 {
            let a = TileKey::at(LonLat::new(-126.0, 49.5), zoom);
            let b = TileKey::at(LonLat::new(-119.0, 45.7), zoom);
            for x in a.x..=b.x {
                for y in a.y..=b.y {
                    elevation.push(TileKey { z: zoom, x, y });
                }
            }
        }
        let middle = TileKey::at(seattle, 13);
        for x in middle.x - 2..middle.x + 3 {
            for y in middle.y - 2..middle.y + 3 {
                elevation.push(TileKey { z: 13, x, y });
            }
        }
        let plan = g.plan(&Manifest::new(elevation, KITSAP_SLICE));
        let at = |z: u8| plan.iter().filter(|k| k.z == z).count();
        assert_eq!(at(0), 1);
        assert_eq!(at(1), 4);
        // Level 5 (24 m) over the whole disk, its corners left out.
        assert!(at(5) > 700 && at(5) < 1024, "{}", at(5));
        // Level 6 (12 m) only where zoom 13 is.
        assert!(at(6) >= 4 && at(6) <= 9, "{}", at(6));
    }
}
