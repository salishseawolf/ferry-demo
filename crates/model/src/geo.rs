//! Spherical GIS math.
//!
//! Coordinates are WGS84 longitude/latitude in degrees (EPSG:4326), stored in
//! GeoJSON axis order (lon, lat). The planet is modeled as a sphere.
//!
//! Render-space axes are ECEF renamed for Y-up:
//! north pole = +Y, (lon 0, lat 0) = +X, (lon 90°E, lat 0) = -Z.

use glam::DVec3;

/// Mean Earth radius (IUGG R1) in meters.
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Below this angle (radians) two points count as the same vertex.
const SAME_POINT_RAD: f64 = 1e-9;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LonLat {
    /// Degrees east, in [-180, 180].
    pub lon: f64,
    /// Degrees north, in [-90, 90].
    pub lat: f64,
}

impl LonLat {
    pub const fn new(lon: f64, lat: f64) -> Self {
        Self { lon, lat }
    }

    pub fn to_unit(self) -> DVec3 {
        let (lon, lat) = (self.lon.to_radians(), self.lat.to_radians());
        DVec3::new(lat.cos() * lon.cos(), lat.sin(), -lat.cos() * lon.sin())
    }

    /// Unit vectors pointing east and north at this point. At the poles,
    /// "east" follows this point's longitude.
    pub fn east_north(self) -> (DVec3, DVec3) {
        let (lon, lat) = (self.lon.to_radians(), self.lat.to_radians());
        (
            DVec3::new(-lon.sin(), 0.0, -lon.cos()),
            DVec3::new(-lat.sin() * lon.cos(), lat.cos(), lat.sin() * lon.sin()),
        )
    }

    /// `v` need not be normalized, only non-zero.
    pub fn from_vec(v: DVec3) -> Self {
        let v = v.normalize();
        Self {
            lon: (-v.z).atan2(v.x).to_degrees(),
            lat: v.y.clamp(-1.0, 1.0).asin().to_degrees(),
        }
    }

    /// Human-readable form, e.g. `47.6062° N, 122.3321° W`.
    pub fn display(self) -> String {
        let ns = if self.lat >= 0.0 { 'N' } else { 'S' };
        let ew = if self.lon >= 0.0 { 'E' } else { 'W' };
        format!("{:.4}° {ns}, {:.4}° {ew}", self.lat.abs(), self.lon.abs())
    }
}

/// A spherical cap: every point within `radius_m` of `center` along the
/// surface.
#[derive(Clone, Copy, Debug)]
pub struct Cap {
    pub center: LonLat,
    pub radius_m: f64,
}

impl Cap {
    /// The cap's radius as an angle at the planet's center (radians).
    pub fn angle(&self) -> f64 {
        self.radius_m / EARTH_RADIUS_M
    }

    /// Whether the direction `p` (not necessarily normalized) is on the cap.
    pub fn contains(&self, p: DVec3) -> bool {
        p.normalize().dot(self.center.to_unit()) >= self.angle().cos()
    }
}

/// Points along the great-circle arc from `a` to `b` (unit vectors), spaced at
/// most `max_step` radians apart. Includes `a`, excludes `b`, so consecutive
/// arcs chain without duplicates. Antipodal endpoints have no unique arc and
/// yield just `a`.
pub fn great_circle_arc(a: DVec3, b: DVec3, max_step: f64) -> Vec<DVec3> {
    let angle = a.angle_between(b);
    if angle < SAME_POINT_RAD || (std::f64::consts::PI - angle) < 1e-6 {
        return vec![a];
    }
    let steps = (angle / max_step).ceil().max(1.0) as usize;
    let sin = angle.sin();
    (0..steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            (a * ((1.0 - t) * angle).sin() + b * (t * angle).sin()) / sin
        })
        .collect()
}

/// Signed area in steradians of the spherical polygon with great-circle edges
/// through `ring` (unit vectors, open ring). Positive when the vertices run
/// counterclockwise seen from outside the sphere, i.e. the interior is on the
/// left. Fan-triangulates from vertex 0 using the Van Oosterom-Strackee
/// triangle formula, so it is exact for any ring smaller than a hemisphere.
#[cfg(test)]
pub fn signed_area(ring: &[DVec3]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let a = ring[0];
    ring[1..]
        .windows(2)
        .map(|w| {
            let (b, c) = (w[0], w[1]);
            let numerator = a.dot(b.cross(c));
            let denominator = 1.0 + a.dot(b) + b.dot(c) + c.dot(a);
            2.0 * numerator.atan2(denominator)
        })
        .sum()
}

/// Great-circle distance (m) between two points on a sphere of `radius`.
pub fn distance_m(a: LonLat, b: LonLat, radius: f64) -> f64 {
    a.to_unit().angle_between(b.to_unit()) * radius
}

/// Initial bearing of the great circle from `a` toward `b`, in degrees
/// clockwise from true north (0 to 360). None when the direction is
/// undefined: the same point, or exactly opposite points.
pub fn initial_bearing_deg(a: LonLat, b: LonLat) -> Option<f64> {
    let (pa, pb) = (a.to_unit(), b.to_unit());
    // Direction toward b in the tangent plane at a.
    let toward = pb - pa * pa.dot(pb);
    if toward.length() < SAME_POINT_RAD {
        return None;
    }
    let (east, north) = a.east_north();
    Some(
        toward
            .dot(east)
            .atan2(toward.dot(north))
            .to_degrees()
            .rem_euclid(360.0),
    )
}

/// The nearest of the 16 compass points, e.g. `WSW` for 247.5°.
pub fn compass_point(bearing_deg: f64) -> &'static str {
    const POINTS: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    POINTS[((bearing_deg.rem_euclid(360.0) / 22.5).round() as usize) % 16]
}

/// Thousands separators, e.g. 1,234,567.
pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn axis_convention() {
        assert!(LonLat::new(0.0, 0.0).to_unit().abs_diff_eq(DVec3::X, 1e-12));
        assert!(
            LonLat::new(0.0, 90.0)
                .to_unit()
                .abs_diff_eq(DVec3::Y, 1e-12)
        );
        assert!(
            LonLat::new(90.0, 0.0)
                .to_unit()
                .abs_diff_eq(-DVec3::Z, 1e-12)
        );
    }

    #[test]
    fn round_trip() {
        for &(lon, lat) in &[
            (0.0, 0.0),
            (-122.3321, 47.6062),
            (179.999, -33.0),
            (-179.999, 12.0),
            (45.0, 89.9),
            (-90.0, -89.9),
        ] {
            let p = LonLat::from_vec(LonLat::new(lon, lat).to_unit());
            assert!(
                close(p.lon, lon, 1e-9) && close(p.lat, lat, 1e-9),
                "{lon},{lat} -> {p:?}"
            );
        }
    }

    #[test]
    fn poles_keep_latitude() {
        assert!(close(LonLat::from_vec(DVec3::Y).lat, 90.0, 1e-12));
        assert!(close(LonLat::from_vec(-DVec3::Y).lat, -90.0, 1e-12));
    }

    #[test]
    fn arc_stays_on_sphere_and_respects_step() {
        let a = LonLat::new(-120.0, 10.0).to_unit();
        let b = LonLat::new(60.0, 70.0).to_unit();
        let step = 1f64.to_radians();
        let mut pts = great_circle_arc(a, b, step);
        assert_eq!(pts[0], a);
        pts.push(b);
        for w in pts.windows(2) {
            assert!(close(w[0].length(), 1.0, 1e-12));
            assert!(w[0].angle_between(w[1]) <= step + 1e-12);
        }
    }

    #[test]
    fn octant_area() {
        // One eighth of the sphere: 4π / 8 steradians.
        let ring = [
            LonLat::new(0.0, 0.0),
            LonLat::new(-90.0, 0.0),
            LonLat::new(0.0, 90.0),
        ];
        let units: Vec<DVec3> = ring.iter().map(|p| p.to_unit()).collect();
        assert!(close(signed_area(&units).abs(), PI / 2.0, 1e-12));
    }

    #[test]
    fn distance_and_bearing() {
        let origin = LonLat::new(0.0, 0.0);
        let quarter = PI / 2.0 * EARTH_RADIUS_M;
        let east = LonLat::new(90.0, 0.0);
        assert!(close(
            distance_m(origin, east, EARTH_RADIUS_M),
            quarter,
            1e-6
        ));
        assert!(close(
            initial_bearing_deg(origin, east).unwrap(),
            90.0,
            1e-9
        ));
        let north = initial_bearing_deg(origin, LonLat::new(0.0, 10.0)).unwrap();
        assert!(north < 1e-9 || north > 360.0 - 1e-9, "{north}");
        let west = initial_bearing_deg(origin, LonLat::new(-10.0, 0.0)).unwrap();
        assert!(close(west, 270.0, 1e-9));

        // London to New York: about 5,570 km on a mean-radius sphere,
        // setting off at about 288.3° (west-northwest).
        let london = LonLat::new(-0.1278, 51.5074);
        let new_york = LonLat::new(-74.0060, 40.7128);
        let km = distance_m(london, new_york, EARTH_RADIUS_M) / 1000.0;
        assert!(close(km, 5570.2, 1.0), "{km}");
        let bearing = initial_bearing_deg(london, new_york).unwrap();
        assert!(close(bearing, 288.3, 0.1), "{bearing}");
        assert_eq!(compass_point(bearing), "WNW");

        assert!(initial_bearing_deg(london, london).is_none());
        let opposite = LonLat::new(london.lon + 180.0, -london.lat);
        assert!(initial_bearing_deg(london, opposite).is_none());
    }

    #[test]
    fn compass_points_and_grouping() {
        assert_eq!(compass_point(0.0), "N");
        assert_eq!(compass_point(359.0), "N");
        assert_eq!(compass_point(247.5), "WSW");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
        assert_eq!(group_thousands(12), "12");
    }
}
