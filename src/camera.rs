use glam::{DMat4, DVec2, DVec3, DVec4};

use crate::geo::{Cap, LonLat};

/// About 2 km up: low enough to pick out single ferry-scale cells.
const MIN_ALTITUDE_RADII: f64 = 3e-4;
/// The zoom is given as the height a camera with this vertical field of
/// view would look down from to see as much ground (`altitude`), as the
/// globe's camera did: links and the scales tuned to it keep working.
const FOV_Y: f64 = std::f64::consts::FRAC_PI_4;
/// Depth runs from this high above the surface (m), clear of any terrain
/// at any exaggeration, down to the planet's center.
const DEPTH_TOP_M: f64 = 20_000.0;

/// The flat map: the planet seen straight down its `area`'s center from
/// infinitely far (an orthographic projection), north up. Over an area a
/// few hundred kilometers across that's as good as flat, and every point
/// on the near half of the planet has one place on it, the same however
/// the view moves. Math is f64 because world units are meters at
/// planetary scale; values drop to f32 for the GPU.
pub struct MapCamera {
    pub radius: f64,
    pub projection: Projection,
    /// What the view keeps within: none of the screen ever shows beyond
    /// it, so zooming out stops once the screen's corners reach its edge,
    /// and panning stops there.
    area: Cap,
    /// The point under the view's center (a unit vector).
    pub focus: DVec3,
    /// How far in, as the height (m) the globe's camera would have seen
    /// as much from (see `FOV_Y`).
    pub altitude: f64,
    pub aspect: f64,
}

/// The orthographic projection down a point's vertical: positions on the
/// plane through the planet's center square to it, in meters east and
/// north of that point.
#[derive(Clone, Copy, Debug)]
pub struct Projection {
    pub radius: f64,
    pub up: DVec3,
    pub east: DVec3,
    pub north: DVec3,
}

impl Projection {
    pub fn about(center: LonLat, radius: f64) -> Self {
        let (east, north) = center.east_north();
        Self {
            radius,
            up: center.to_unit(),
            east,
            north,
        }
    }

    /// Where the direction `p` (a unit vector) lands on the map (m).
    pub fn place(&self, p: DVec3) -> DVec2 {
        DVec2::new(p.dot(self.east), p.dot(self.north)) * self.radius
    }

    /// The point on the near half of the planet (a unit vector) that lands
    /// at `xy` on the map, if any does.
    pub fn point_at(&self, xy: DVec2) -> Option<DVec3> {
        let (e, n) = (xy.x / self.radius, xy.y / self.radius);
        let up = 1.0 - e * e - n * n;
        (up >= 0.0).then(|| self.east * e + self.north * n + self.up * up.sqrt())
    }

    /// Whether the direction `p` is on the near half, the half shown.
    pub fn shows(&self, p: DVec3) -> bool {
        p.dot(self.up) > 0.0
    }
}

impl MapCamera {
    /// The map of `area`, over `focus` at `altitude_m` (see `altitude`),
    /// both kept within its limits.
    pub fn new(radius: f64, area: Cap, focus: LonLat, altitude_m: f64, aspect: f64) -> Self {
        let mut camera = Self {
            radius,
            projection: Projection::about(area.center, radius),
            area,
            focus: area.center.to_unit(),
            altitude: altitude_m,
            aspect,
        };
        // The zoom first: how near the edge the view can go depends on it.
        camera.zoom(0.0);
        camera.set_focus(focus.to_unit());
        camera
    }

    /// Half the view's height on the ground (m).
    pub fn half_height_m(&self) -> f64 {
        self.altitude * (FOV_Y / 2.0).tan()
    }

    /// Ground meters across one pixel, for a view `height_px` tall.
    pub fn m_per_px(&self, height_px: f64) -> f64 {
        2.0 * self.half_height_m() / height_px.max(1.0)
    }

    /// The view's center on the map (m).
    pub fn center(&self) -> DVec2 {
        self.projection.place(self.focus)
    }

    /// Moves the view's center over `p` (a unit vector), or as near as it
    /// can with the whole screen over the area.
    pub fn set_focus(&mut self, p: DVec3) {
        let reach = (self.area_radius_m() - self.half_diagonal_m()).max(0.0);
        let xy = if self.projection.shows(p) {
            self.projection.place(p)
        } else {
            self.projection.place(p).normalize_or_zero() * reach
        };
        let xy = if xy.length() > reach {
            xy.normalize_or_zero() * reach
        } else {
            xy
        };
        if let Some(p) = self.projection.point_at(xy) {
            self.focus = p;
        }
    }

    /// The window's shape changed (width over height): the view keeps its
    /// height on the ground, within the limits.
    pub fn set_aspect(&mut self, aspect: f64) {
        if aspect != self.aspect {
            self.aspect = aspect;
            self.zoom(0.0);
        }
    }

    /// The area's radius on the map (m).
    fn area_radius_m(&self) -> f64 {
        self.area.angle().sin() * self.radius
    }

    /// From the view's center to a corner, on the ground (m).
    fn half_diagonal_m(&self) -> f64 {
        self.half_height_m() * (1.0 + self.aspect * self.aspect).sqrt()
    }

    /// Moves the view so the surface point `grabbed` (a unit vector) lies
    /// under the cursor at normalized device coordinates (x, y), as if the
    /// map were held there and pushed around.
    pub fn drag_to(&mut self, grabbed: DVec3, ndc_x: f64, ndc_y: f64) {
        let offset = self.ndc_to_m(ndc_x, ndc_y);
        let center = self.projection.place(grabbed) - offset;
        if let Some(p) = self.projection.point_at(center) {
            self.set_focus(p);
        }
    }

    /// Zoom by scaling altitude; positive `amount` zooms out.
    /// The screen stays over the area: zoomed out, the view moves in from
    /// its edge.
    pub fn zoom(&mut self, amount: f64) {
        let corner = (FOV_Y / 2.0).tan() * (1.0 + self.aspect * self.aspect).sqrt();
        let max = self.area_radius_m() / corner;
        self.altitude = (self.altitude * amount.exp()).clamp(MIN_ALTITUDE_RADII * self.radius, max);
        self.set_focus(self.focus);
    }

    /// Zoom as `zoom` does, keeping the point at normalized device
    /// coordinates (x, y) where it is on screen.
    pub fn zoom_at(&mut self, amount: f64, ndc_x: f64, ndc_y: f64) {
        let Some(held) = self.surface_at(ndc_x, ndc_y) else {
            self.zoom(amount);
            return;
        };
        self.zoom(amount);
        self.drag_to(held, ndc_x, ndc_y);
    }

    /// Shows exactly the square of the map `half_m` (m) either side of
    /// `center`, whatever the limits, in a square window: for baking the
    /// basemap.
    pub fn show_square(&mut self, center: DVec2, half_m: f64) {
        if let Some(p) = self.projection.point_at(center) {
            self.focus = p;
        }
        self.altitude = half_m / (FOV_Y / 2.0).tan();
        self.aspect = 1.0;
    }

    /// Meters on the map from the view's center to normalized device
    /// coordinates (x, y).
    fn ndc_to_m(&self, ndc_x: f64, ndc_y: f64) -> DVec2 {
        let half = self.half_height_m();
        DVec2::new(ndc_x * half * self.aspect, ndc_y * half)
    }

    /// The surface point (a unit vector) at normalized device coordinates
    /// (x right, y up, in -1..1), if the planet is there.
    pub fn surface_at(&self, ndc_x: f64, ndc_y: f64) -> Option<DVec3> {
        self.projection
            .point_at(self.center() + self.ndc_to_m(ndc_x, ndc_y))
    }

    /// From world positions (m) to clip space: east and north across the
    /// view, and depth down from `DEPTH_TOP_M` above the surface to the
    /// planet's center (WebGPU's 0..1). Clip w is always 1.
    pub fn view_proj(&self) -> DMat4 {
        let Projection {
            up, east, north, ..
        } = self.projection;
        let half_h = self.half_height_m();
        let half_w = half_h * self.aspect;
        let center = self.center();
        let top = self.radius + DEPTH_TOP_M;
        let row = |axis: DVec3, scale: f64, offset: f64| (axis * scale).extend(offset);
        DMat4::from_cols(
            row(east, 1.0 / half_w, -center.x / half_w),
            row(north, 1.0 / half_h, -center.y / half_h),
            row(-up, 1.0 / top, 1.0),
            DVec4::W,
        )
        .transpose()
    }
}

/// A view as a link's fragment: the point under the middle of the view and
/// the camera's height (m) over it, `at=47.51095,-122.46364,6430`. The
/// view looks straight down, north up, so that's the whole view.
pub fn view_fragment(center: LonLat, altitude_m: f64) -> String {
    format!("at={:.5},{:.5},{:.0}", center.lat, center.lon, altitude_m)
}

/// The view in a link's fragment (`#at=…`, see `view_fragment`), if it
/// names one. The camera keeps it within its limits.
pub fn parse_view_fragment(fragment: &str) -> Option<(LonLat, f64)> {
    let at = fragment
        .trim_start_matches('#')
        .split('&')
        .find_map(|part| part.strip_prefix("at="))?;
    let mut parts = at.split(',').map(|p| p.trim().parse::<f64>().ok());
    let (lat, lon, altitude) = (parts.next()??, parts.next()??, parts.next()??);
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) || !altitude.is_finite() {
        return None;
    }
    Some((LonLat::new(lon, lat), altitude))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::EARTH_RADIUS_M;

    const AREA: Cap = Cap {
        center: LonLat::new(-122.65, 47.64),
        radius_m: 200_000.0,
    };

    fn camera(focus: LonLat, altitude_m: f64) -> MapCamera {
        MapCamera::new(EARTH_RADIUS_M, AREA, focus, altitude_m, 1.5)
    }

    /// Where the camera draws `p` (a unit vector), in normalized device
    /// coordinates.
    fn ndc(cam: &MapCamera, p: DVec3) -> DVec2 {
        let clip = cam.view_proj() * (p * cam.radius).extend(1.0);
        assert_eq!(clip.w, 1.0);
        clip.truncate().truncate()
    }

    /// Within a millimeter on the ground (angle_between can't resolve that).
    fn close(a: DVec3, b: DVec3) -> bool {
        (a - b).length() * EARTH_RADIUS_M < 1e-3
    }

    #[test]
    fn writes_and_reads_views_in_links() {
        let center = LonLat::new(-122.463639, 47.51095);
        let fragment = view_fragment(center, 6430.4);
        assert_eq!(fragment, "at=47.51095,-122.46364,6430");
        let (back, altitude) = parse_view_fragment(&format!("#{fragment}")).unwrap();
        assert!((back.lat - 47.51095).abs() < 1e-9 && (back.lon + 122.46364).abs() < 1e-9);
        assert_eq!(altitude, 6430.0);
        assert!(parse_view_fragment("#x=1&at=47.5,-122.4,10").is_some());
        for bad in [
            "",
            "#",
            "#at=47.5,-122.4",
            "#at=95,0,1000",
            "#at=a,b,c",
            "#at=47.5,-122.4,inf",
        ] {
            assert_eq!(parse_view_fragment(bad), None, "{bad}");
        }
    }

    #[test]
    fn projects_to_the_plane_and_back() {
        let p = Projection::about(AREA.center, EARTH_RADIUS_M);
        assert!(p.place(AREA.center.to_unit()).length() < 1e-6);
        let seattle = LonLat::new(-122.34, 47.6).to_unit();
        let xy = p.place(seattle);
        // About 23 km east and 4.5 km south of the area's center.
        assert!(
            (xy.x - 23_270.0).abs() < 100.0 && (xy.y + 4_450.0).abs() < 100.0,
            "{xy}"
        );
        assert!(close(p.point_at(xy).unwrap(), seattle));
        assert_eq!(p.point_at(DVec2::new(EARTH_RADIUS_M, 1.0)), None);
    }

    #[test]
    fn the_center_of_the_view_is_its_focus() {
        let target = LonLat::new(-122.5, 47.7);
        let cam = camera(target, 20_000.0);
        assert!(close(cam.surface_at(0.0, 0.0).unwrap(), target.to_unit()));
        assert!(ndc(&cam, target.to_unit()).length() < 1e-9);
    }

    #[test]
    fn east_is_right_and_north_is_up() {
        let cam = camera(AREA.center, 20_000.0);
        let at = |x, y| LonLat::from_vec(cam.surface_at(x, y).unwrap());
        assert!(at(0.5, 0.0).lon > at(-0.5, 0.0).lon);
        assert!(at(0.0, 0.5).lat > at(0.0, -0.5).lat);
    }

    #[test]
    fn surface_points_draw_where_they_are_found() {
        let cam = camera(LonLat::new(-122.4, 47.9), 80_000.0);
        for (x, y) in [(0.3, -0.7), (-0.9, 0.9), (0.0, 0.2)] {
            let p = cam.surface_at(x, y).unwrap();
            let back = ndc(&cam, p);
            assert!((back - DVec2::new(x, y)).length() < 1e-9, "{back}");
        }
        // Higher points are nearer the eye: smaller depth.
        let p = cam.surface_at(0.1, 0.1).unwrap();
        let depth = |h: f64| (cam.view_proj() * (p * (cam.radius + h)).extend(1.0)).z;
        assert!(depth(1000.0) < depth(0.0) && depth(0.0) < 1.0 && depth(DEPTH_TOP_M - 1.0) > 0.0);
    }

    #[test]
    fn drag_keeps_grabbed_point_under_cursor() {
        let mut cam = camera(AREA.center, 30_000.0);
        let grabbed = cam.surface_at(0.1, 0.05).unwrap();
        cam.drag_to(grabbed, -0.25, 0.2);
        assert!(close(cam.surface_at(-0.25, 0.2).unwrap(), grabbed));
    }

    #[test]
    fn zooming_at_a_point_keeps_it_there() {
        let mut cam = camera(AREA.center, 30_000.0);
        let held = cam.surface_at(0.6, -0.4).unwrap();
        cam.zoom_at(-0.5, 0.6, -0.4);
        assert!(cam.altitude < 30_000.0);
        assert!(close(cam.surface_at(0.6, -0.4).unwrap(), held));
    }

    #[test]
    fn the_view_stays_over_the_area() {
        let edge = AREA.angle().sin() * EARTH_RADIUS_M;
        // Far away, or on the other side of the planet: brought in until
        // the screen's corners reach the edge.
        for far in [LonLat::new(-110.0, 47.64), LonLat::new(57.35, -47.64)] {
            let cam = camera(far, 30_000.0);
            let off = cam.center().length() + cam.half_diagonal_m();
            assert!((off - edge).abs() < 1.0, "{off}");
        }
        // Zoomed out from near the edge: the view moves in.
        let mut cam = camera(LonLat::new(-122.65, 49.3), 30_000.0);
        cam.zoom(100.0);
        assert!(cam.center().length() < 1.0);
        // A narrower window can see more of the area's height.
        let wide = cam.altitude;
        cam.set_aspect(0.5);
        cam.zoom(100.0);
        assert!(cam.altitude > wide);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut cam = camera(AREA.center, 30_000.0);
        cam.zoom(-100.0);
        assert!((cam.altitude - MIN_ALTITUDE_RADII * EARTH_RADIUS_M).abs() < 1e-6);
        cam.zoom(100.0);
        let corner_m = cam.half_diagonal_m();
        let edge = AREA.angle().sin() * EARTH_RADIUS_M;
        assert!((corner_m - edge).abs() < 1e-6, "{corner_m}");
    }
}
