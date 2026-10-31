//! Elevation tiles: their Web Mercator XYZ keys, which ones a view needs at
//! which detail, and which GPU cache slot holds each.
//!
//! The tiles come from tools/tiles/elevation.py: 256 px heights plus a 1 px
//! border, Terrarium-encoded PNGs. The GPU turns each into slopes once (see
//! normals.wgsl); this module is the bookkeeping around that.

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

use glam::{DMat4, DVec2, DVec3, DVec4};

use crate::camera::{OrbitCamera, ray_sphere};
use crate::geo::{Cap, LonLat};

pub const TILE_PX: u32 = 256;
/// Height tiles carry this many pixels of their neighbors on every side
/// (see tools/tiles/elevation.py).
pub const BORDER_PX: u32 = 2;
/// A tile is replaced by its children once it would cover more than this
/// many pixels across, so tiles show at 1 to 1.4 texels per pixel.
const SPLIT_PX: f64 = 360.0;
/// Below this zoom, tiles are too big for corner sampling to cull reliably.
const ALWAYS_VISIBLE_BELOW_Z: u8 = 3;
/// Regions are never coarser than this zoom: a coarser tile's patch quads
/// would sag kilometers under the sphere, below the backdrop. At zoom 4
/// (with renderer::PATCH_SEGMENTS) they sag about 120 m.
pub const MIN_REGION_Z: u8 = 4;

/// A tile of the Web Mercator XYZ pyramid: x east, y south from the
/// north-west corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileKey {
    pub const ROOT: TileKey = TileKey { z: 0, x: 0, y: 0 };

    pub fn parent(self) -> Option<TileKey> {
        (self.z > 0).then(|| TileKey {
            z: self.z - 1,
            x: self.x / 2,
            y: self.y / 2,
        })
    }

    pub fn children(self) -> [TileKey; 4] {
        let (z, x, y) = (self.z + 1, self.x * 2, self.y * 2);
        [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(dx, dy)| TileKey {
            z,
            x: x + dx,
            y: y + dy,
        })
    }

    /// The point at (u, v) across the tile, (0, 0) its north-west corner.
    pub fn lon_lat(self, u: f64, v: f64) -> LonLat {
        let n = f64::from(1u32 << self.z);
        let mx = (f64::from(self.x) + u) / n;
        let my = (f64::from(self.y) + v) / n;
        LonLat::new(
            mx * 360.0 - 180.0,
            (PI * (1.0 - 2.0 * my)).sinh().atan().to_degrees(),
        )
    }

    /// Where `self`, a descendant of `ancestor` (or `ancestor` itself),
    /// sits in it: (u, v) offset of its north-west corner and its size, as
    /// fractions of the ancestor.
    pub fn within(self, ancestor: TileKey) -> (DVec2, f64) {
        let dz = self.z - ancestor.z;
        let scale = 1.0 / f64::from(1u32 << dz);
        let offset = DVec2::new(
            f64::from(self.x - (ancestor.x << dz)),
            f64::from(self.y - (ancestor.y << dz)),
        ) * scale;
        (offset, scale)
    }

    /// Whether `p` lies in the tile.
    pub fn contains(self, p: LonLat) -> bool {
        let (nw, se) = (self.lon_lat(0.0, 0.0), self.lon_lat(1.0, 1.0));
        (nw.lon..=se.lon).contains(&p.lon) && (se.lat..=nw.lat).contains(&p.lat)
    }

    /// Where the tile of `layer` (a folder of tiles/) is served.
    pub fn url(self, layer: &str, extension: &str) -> String {
        format!("tiles/{layer}/{}/{}/{}.{extension}", self.z, self.x, self.y)
    }

    /// Unit vectors at the corners, edge midpoints and center.
    fn samples(self) -> impl Iterator<Item = DVec3> {
        (0..9).map(move |i| {
            let (u, v) = ((i % 3) as f64 / 2.0, (i / 3) as f64 / 2.0);
            self.lon_lat(u, v).to_unit()
        })
    }

    /// Whether any of the tile reaches into `cap`: its point nearest the
    /// cap's center (the center clamped into its lon/lat box; exact enough
    /// for tiles that don't reach a pole) lies in it.
    fn touches(self, cap: &Cap) -> bool {
        let (nw, se) = (self.lon_lat(0.0, 0.0), self.lon_lat(1.0, 1.0));
        let c = cap.center;
        let nearest = LonLat::new(c.lon.clamp(nw.lon, se.lon), c.lat.clamp(se.lat, nw.lat));
        cap.contains(nearest.to_unit())
    }

    /// Width across the tile's middle on a sphere of `radius`.
    fn width_m(self, radius: f64) -> f64 {
        let lat = self.lon_lat(0.5, 0.5).lat.to_radians();
        2.0 * PI / f64::from(1u32 << self.z) * lat.cos() * radius
    }
}

/// Pixels per side of a height tile, border included.
pub const HEIGHT_TILE_PX: u32 = TILE_PX + 2 * BORDER_PX;

/// A height tile PNG as RGBA bytes, row by row from the north-west: height,
/// water width and water fraction (see tools/tiles/elevation.py). Decoded here rather
/// than by the browser, which might color-manage or premultiply them.
pub fn decode_tile(png_bytes: &[u8]) -> Result<Vec<u8>, String> {
    // The decoder reads from something it can seek in.
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or("tile too large to decode")?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if info.width != HEIGHT_TILE_PX || info.height != HEIGHT_TILE_PX {
        return Err(format!("tile is {}x{} px", info.width, info.height));
    }
    let pixels = &buf[..info.buffer_size()];
    match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => Ok(pixels.to_vec()),
        other => Err(format!("unexpected PNG format {other:?}")),
    }
}

/// The tiles that exist, from the pipeline's index.json, and the area
/// they're drawn over.
pub struct Manifest {
    tiles: HashSet<TileKey>,
    area: Cap,
}

impl Manifest {
    pub fn new(tiles: impl IntoIterator<Item = TileKey>, area: Cap) -> Self {
        Self {
            tiles: tiles.into_iter().collect(),
            area,
        }
    }

    pub fn contains(&self, key: TileKey) -> bool {
        self.tiles.contains(&key)
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }
}

/// How big tiles look in the current view, and which can be seen.
pub trait View {
    fn visible(&self, key: TileKey) -> bool;
    /// About how many pixels across the tile is drawn.
    fn pixels(&self, key: TileKey) -> f64;
}

/// Screen positions (per axis) whose surface points mark tiles as seen,
/// for tiles too big for their own samples to land on screen.
const SCREEN_PROBES: usize = 5;

/// The globe seen from the orbit camera.
pub struct GlobeView {
    eye: DVec3,
    view_proj: DMat4,
    radius: f64,
    /// Viewport height over twice the tangent of half the vertical field
    /// of view: pixels per unit of size over distance.
    focal_px: f64,
    /// Surface points under a grid of screen positions.
    probes: Vec<LonLat>,
}

impl GlobeView {
    pub fn new(camera: &OrbitCamera, height_px: f64) -> Self {
        let steps = SCREEN_PROBES - 1;
        let probes = (0..SCREEN_PROBES * SCREEN_PROBES)
            .filter_map(|i| {
                let ndc = |k: usize| 2.0 * k as f64 / steps as f64 - 1.0;
                let ray = camera.ray(ndc(i % SCREEN_PROBES), ndc(i / SCREEN_PROBES));
                ray_sphere(&ray, camera.radius).map(LonLat::from_vec)
            })
            .collect();
        Self {
            eye: camera.eye(),
            view_proj: camera.view_proj(),
            radius: camera.radius,
            focal_px: height_px / (2.0 * (camera.fov_y / 2.0).tan()),
            probes,
        }
    }
}

impl View for GlobeView {
    fn visible(&self, key: TileKey) -> bool {
        if key.z < ALWAYS_VISIBLE_BELOW_Z || self.probes.iter().any(|&p| key.contains(p)) {
            return true;
        }
        let points: Vec<DVec3> = key.samples().map(|u| u * self.radius).collect();
        // On the near side of the planet: in front of the horizon plane.
        let horizon = self.radius * self.radius;
        if !points.iter().any(|p| p.dot(self.eye) > horizon) {
            return false;
        }
        // Not wholly beyond any one side of the view.
        let clip: Vec<DVec4> = points
            .iter()
            .map(|p| self.view_proj * p.extend(1.0))
            .collect();
        let outside = |f: &dyn Fn(&DVec4) -> bool| clip.iter().all(f);
        !(outside(&|c| c.x < -c.w)
            || outside(&|c| c.x > c.w)
            || outside(&|c| c.y < -c.w)
            || outside(&|c| c.y > c.w)
            || outside(&|c| c.w <= 0.0))
    }

    fn pixels(&self, key: TileKey) -> f64 {
        let nearest = key
            .samples()
            .map(|u| (u * self.radius).distance(self.eye))
            .fold(f64::INFINITY, f64::min);
        key.width_m(self.radius) / nearest.max(1.0) * self.focal_px
    }
}

/// The regions to draw: tiles from the manifest, refined until they look
/// small enough, plus the quarters of the manifest's area a split leaves
/// without a tile of their own (outside the detail area), which draw from
/// an ancestor.
pub fn select(manifest: &Manifest, view: &impl View) -> Vec<TileKey> {
    let mut out = Vec::new();
    if manifest.contains(TileKey::ROOT) {
        visit(manifest, view, TileKey::ROOT, &mut out);
    }
    out
}

fn visit(manifest: &Manifest, view: &impl View, key: TileKey, out: &mut Vec<TileKey>) {
    if !view.visible(key) {
        return;
    }
    let children = key.children();
    let wants_split = key.z < MIN_REGION_Z || view.pixels(key) > SPLIT_PX;
    if wants_split && children.iter().any(|&c| manifest.contains(c)) {
        for child in children {
            if manifest.contains(child) {
                visit(manifest, view, child, out);
            } else if child.touches(&manifest.area) && view.visible(child) {
                out.push(child);
            }
        }
    } else {
        out.push(key);
    }
}

/// Which GPU cache slot holds which tile, evicting the least recently used.
pub struct Cache {
    slots: Vec<Option<(TileKey, u64)>>,
    index: HashMap<TileKey, u32>,
}

/// Where a region's slopes come from: a cache slot, and the region's
/// offset and size within that slot's tile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Source {
    pub slot: u32,
    pub offset: DVec2,
    pub scale: f64,
}

impl Cache {
    pub fn new(slots: u32) -> Self {
        Self {
            slots: vec![None; slots as usize],
            index: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn contains(&self, key: TileKey) -> bool {
        self.index.contains_key(&key)
    }

    /// The nearest loaded tile at or above `region`, marking it used in
    /// `frame`.
    pub fn source(&mut self, region: TileKey, frame: u64) -> Option<Source> {
        let mut key = Some(region);
        while let Some(k) = key {
            if let Some(&slot) = self.index.get(&k) {
                self.slots[slot as usize] = Some((k, frame));
                let (offset, scale) = region.within(k);
                return Some(Source {
                    slot,
                    offset,
                    scale,
                });
            }
            key = k.parent();
        }
        None
    }

    /// A slot for `key`: a free one, else the least recently used one not
    /// used in `frame`. None if every slot is in use this frame.
    pub fn insert(&mut self, key: TileKey, frame: u64) -> Option<u32> {
        if let Some(&slot) = self.index.get(&key) {
            return Some(slot);
        }
        let slot = self.slots.iter().position(Option::is_none).or_else(|| {
            self.slots
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.map(|(_, used)| (i, used)))
                .filter(|&(_, used)| used < frame)
                .min_by_key(|&(_, used)| used)
                .map(|(i, _)| i)
        })?;
        if let Some((old, _)) = self.slots[slot] {
            self.index.remove(&old);
        }
        self.slots[slot] = Some((key, frame));
        self.index.insert(key, slot as u32);
        Some(slot as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::EARTH_RADIUS_M;

    const SEATTLE: LonLat = LonLat::new(-122.3321, 47.6062);
    /// Covers the test manifest's box.
    const AREA: Cap = Cap {
        center: SEATTLE,
        radius_m: 300_000.0,
    };

    fn key_at(p: LonLat, z: u8) -> TileKey {
        let n = f64::from(1u32 << z);
        let lat = p.lat.to_radians();
        TileKey {
            z,
            x: ((p.lon + 180.0) / 360.0 * n) as u32,
            y: ((1.0 - lat.tan().asinh() / PI) / 2.0 * n) as u32,
        }
    }

    /// Every tile down to `max_z` over a box around Seattle, and the
    /// ancestors of those.
    fn manifest(max_z: u8) -> Manifest {
        let mut tiles = HashSet::new();
        for z in 0..=max_z {
            let a = key_at(LonLat::new(-125.0, 50.0), z);
            let b = key_at(LonLat::new(-120.0, 45.0), z);
            for x in a.x..=b.x {
                for y in a.y..=b.y {
                    tiles.insert(TileKey { z, x, y });
                }
            }
        }
        Manifest::new(tiles, AREA)
    }

    fn globe(altitude_radii: f64) -> GlobeView {
        let cam = OrbitCamera::over(EARTH_RADIUS_M, SEATTLE, altitude_radii, 1.5);
        GlobeView::new(&cam, 1000.0)
    }

    #[test]
    fn keys_match_the_xyz_scheme() {
        // The tile over Seattle at zoom 10, as any slippy map numbers it.
        assert_eq!(
            key_at(SEATTLE, 10),
            TileKey {
                z: 10,
                x: 164,
                y: 357
            }
        );
        let k = TileKey {
            z: 10,
            x: 164,
            y: 357,
        };
        let nw = k.lon_lat(0.0, 0.0);
        let se = k.lon_lat(1.0, 1.0);
        assert!(nw.lon < SEATTLE.lon && SEATTLE.lon < se.lon);
        assert!(se.lat < SEATTLE.lat && SEATTLE.lat < nw.lat);
        assert_eq!(k.children().map(|c| c.parent()), [Some(k); 4]);
        assert_eq!(k.url("elevation", "png"), "tiles/elevation/10/164/357.png");
    }

    #[test]
    fn within_locates_descendants() {
        let a = TileKey { z: 3, x: 1, y: 2 };
        assert_eq!(a.within(a), (DVec2::ZERO, 1.0));
        let child = a.children()[3];
        assert_eq!(child.within(a), (DVec2::splat(0.5), 0.5));
        let grandchild = child.children()[1];
        assert_eq!(grandchild.within(a), (DVec2::new(0.75, 0.5), 0.25));
    }

    #[test]
    fn closer_views_select_finer_tiles() {
        let m = manifest(9);
        let finest = |view: &GlobeView| select(&m, view).iter().map(|k| k.z).max().unwrap();
        let far = finest(&globe(2.0));
        let near = finest(&globe(0.01));
        assert!(far < near && near == 9, "{far} {near}");
        let coarsest = select(&m, &globe(9.0)).iter().map(|k| k.z).min().unwrap();
        assert_eq!(coarsest, MIN_REGION_Z, "never coarser, even far out");
        // Tiles cover the view without overlap: none is another's ancestor.
        let regions = select(&m, &globe(0.01));
        for a in &regions {
            let mut p = a.parent();
            while let Some(k) = p {
                assert!(!regions.contains(&k), "{k:?} overlaps {a:?}");
                p = k.parent();
            }
        }
        assert!(regions.len() < 400, "{}", regions.len());
    }

    #[test]
    fn far_side_is_culled() {
        let m = manifest(6);
        let cam = OrbitCamera::over(EARTH_RADIUS_M, LonLat::new(57.0, -47.0), 0.5, 1.5);
        let view = GlobeView::new(&cam, 1000.0);
        assert!(
            select(&m, &view)
                .iter()
                .all(|k| k.z < ALWAYS_VISIBLE_BELOW_Z)
        );
    }

    #[test]
    fn split_regions_outside_the_manifest_fall_back() {
        // Only one zoom-1 tile exists over an area covering the planet:
        // splitting the root leaves three quarters drawn from it.
        let everywhere = Cap {
            center: SEATTLE,
            radius_m: PI * EARTH_RADIUS_M,
        };
        let m = Manifest::new([TileKey::ROOT, TileKey { z: 1, x: 0, y: 0 }], everywhere);
        let regions = select(&m, &globe(0.5));
        assert_eq!(regions.len(), 4);
        // Over a small area, the quarters outside it are left out.
        let m = Manifest::new([TileKey::ROOT, TileKey { z: 1, x: 0, y: 0 }], AREA);
        assert_eq!(select(&m, &globe(0.5)), [TileKey { z: 1, x: 0, y: 0 }]);
        let mut cache = Cache::new(4);
        cache.insert(TileKey::ROOT, 0);
        let s = cache.source(TileKey { z: 1, x: 1, y: 1 }, 1).unwrap();
        assert_eq!((s.offset, s.scale), (DVec2::splat(0.5), 0.5));
    }

    fn png(color: png::ColorType, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, HEIGHT_TILE_PX, HEIGHT_TILE_PX);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(pixels)
            .unwrap();
        out
    }

    #[test]
    fn decodes_tiles_to_rgba() {
        let n = (HEIGHT_TILE_PX * HEIGHT_TILE_PX) as usize;
        let rgba: Vec<u8> = (0..n * 4).map(|i| (i % 251) as u8).collect();
        assert_eq!(
            decode_tile(&png(png::ColorType::Rgba, &rgba)).unwrap(),
            rgba
        );
        // Tiles from before the water channel are rejected, not misread.
        assert!(decode_tile(&png(png::ColorType::Rgb, &vec![7; n * 3])).is_err());
        assert!(decode_tile(b"not a png").is_err());
    }

    #[test]
    fn cache_falls_back_to_ancestors_and_evicts_least_recent() {
        let mut cache = Cache::new(2);
        let root = TileKey::ROOT;
        let [a, b, c, _] = root.children();
        assert_eq!(cache.source(a, 0), None);
        cache.insert(root, 0);
        assert_eq!(cache.source(a, 1).unwrap().scale, 0.5);
        cache.insert(a, 1);
        assert_eq!(cache.source(a, 2).unwrap().scale, 1.0);
        // Both slots used in frame 2: nothing can go.
        cache.source(root, 2);
        assert_eq!(cache.insert(b, 2), None);
        // Frame 3 only uses `a`, so the root goes.
        cache.source(a, 3);
        assert!(cache.insert(c, 3).is_some());
        assert!(!cache.contains(root) && cache.contains(a) && cache.contains(c));
    }
}
