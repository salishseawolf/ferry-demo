use glam::dcamera::rh;
use glam::{DMat4, DQuat, DVec3, DVec4};

use crate::geo::LonLat;

/// About 2 km up: low enough to pick out single ferry-scale cells.
const MIN_ALTITUDE_RADII: f64 = 3e-4;
const MAX_ALTITUDE_RADII: f64 = 10.0;
const MAX_PITCH: f64 = 89f64.to_radians();

/// Orbit camera always looking at the planet's center. Math is f64 because
/// world units are meters at planetary scale; values drop to f32 for the GPU.
pub struct OrbitCamera {
    pub radius: f64,
    pub yaw: f64,
    pub pitch: f64,
    /// Distance from the eye to the surface, in meters.
    pub altitude: f64,
    pub fov_y: f64,
    pub aspect: f64,
}

/// A ray in world space with a normalized direction.
pub struct Ray {
    pub origin: DVec3,
    pub dir: DVec3,
}

impl OrbitCamera {
    /// Camera hovering over `target` at `altitude_radii` planet radii up.
    pub fn over(radius: f64, target: LonLat, altitude_radii: f64, aspect: f64) -> Self {
        let d = target.to_unit();
        Self {
            radius,
            yaw: d.x.atan2(d.z),
            pitch: d.y.asin(),
            altitude: altitude_radii * radius,
            fov_y: 45f64.to_radians(),
            aspect,
        }
    }

    /// Unit vector from the planet center toward the eye.
    pub fn eye_dir(&self) -> DVec3 {
        DVec3::new(
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
            self.pitch.cos() * self.yaw.cos(),
        )
    }

    pub fn eye(&self) -> DVec3 {
        self.eye_dir() * (self.radius + self.altitude)
    }

    /// Rotate by a pointer drag of (dx, dy) CSS pixels so the surface roughly
    /// follows the cursor. Slower when zoomed in.
    pub fn drag(&mut self, dx: f64, dy: f64) {
        let rad_per_px = 0.003 * (self.altitude / self.radius).clamp(MIN_ALTITUDE_RADII, 1.5);
        self.yaw -= dx * rad_per_px;
        self.pitch = (self.pitch + dy * rad_per_px).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Orbit so the surface point `grabbed` (a unit vector) lies under the
    /// cursor at normalized device coordinates (x, y), as if the globe were
    /// held there and pushed around. Returns false, leaving the camera
    /// unchanged, when the cursor isn't over the globe.
    ///
    /// Solves for yaw and pitch with Newton's method: the residual is how far
    /// the point under the cursor sits from `grabbed`, measured in the tangent
    /// plane at `grabbed`. One shortest-arc rotation gives the starting guess
    /// for large moves. The target is always the original grab point, so
    /// error never accumulates over a long drag.
    pub fn drag_to(&mut self, grabbed: DVec3, ndc_x: f64, ndc_y: f64) -> bool {
        let (yaw0, pitch0) = (self.yaw, self.pitch);
        let Some(under) = self.unit_under(ndc_x, ndc_y) else {
            return false;
        };
        self.set_eye_dir(DQuat::from_rotation_arc(under, grabbed) * self.eye_dir());

        let (t1, t2) = tangent_basis(grabbed);
        let residual = |cam: &Self| cam.unit_under(ndc_x, ndc_y).map(|u| (u.dot(t1), u.dot(t2)));
        const H: f64 = 1e-7;
        for _ in 0..8 {
            let Some(r) = residual(self) else { break };
            if r.0.hypot(r.1) < 1e-12 {
                break;
            }
            let (yaw, pitch) = (self.yaw, self.pitch);
            self.yaw = yaw + H;
            let ry = residual(self);
            self.yaw = yaw;
            self.pitch = pitch + H;
            let rp = residual(self);
            self.pitch = pitch;
            let (Some(ry), Some(rp)) = (ry, rp) else {
                break;
            };
            // Jacobian columns: d(residual)/d(yaw), d(residual)/d(pitch).
            let (a, c) = ((ry.0 - r.0) / H, (ry.1 - r.1) / H);
            let (b, d) = ((rp.0 - r.0) / H, (rp.1 - r.1) / H);
            let det = a * d - b * c;
            if det.abs() < 1e-18 {
                break;
            }
            self.yaw -= (d * r.0 - b * r.1) / det;
            self.pitch = (self.pitch - (a * r.1 - c * r.0) / det).clamp(-MAX_PITCH, MAX_PITCH);
        }
        if self.unit_under(ndc_x, ndc_y).is_none() {
            (self.yaw, self.pitch) = (yaw0, pitch0);
            return false;
        }
        true
    }

    fn unit_under(&self, ndc_x: f64, ndc_y: f64) -> Option<DVec3> {
        ray_sphere(&self.ray(ndc_x, ndc_y), self.radius).map(DVec3::normalize)
    }

    pub fn set_eye_dir(&mut self, d: DVec3) {
        self.yaw = d.x.atan2(d.z);
        self.pitch = d.y.clamp(-1.0, 1.0).asin().clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Zoom by scaling altitude; positive `amount` zooms out.
    pub fn zoom(&mut self, amount: f64) {
        self.altitude = (self.altitude * amount.exp()).clamp(
            MIN_ALTITUDE_RADII * self.radius,
            MAX_ALTITUDE_RADII * self.radius,
        );
    }

    pub fn view_proj(&self) -> DMat4 {
        let view = rh::view::look_at_mat4(self.eye(), DVec3::ZERO, DVec3::Y);
        // Near and far track altitude to keep depth precision usable from
        // orbit down to low altitude. WebGPU clip space matches DirectX.
        let near = self.altitude * 0.1;
        let far = self.altitude + 2.0 * self.radius;
        let proj = rh::proj::directx::perspective(self.fov_y, self.aspect, near, far);
        proj * view
    }

    /// Ray through normalized device coordinates (x right, y up, in -1..1).
    pub fn ray(&self, ndc_x: f64, ndc_y: f64) -> Ray {
        let inv = self.view_proj().inverse();
        let near = inv * DVec4::new(ndc_x, ndc_y, 0.0, 1.0);
        let far = inv * DVec4::new(ndc_x, ndc_y, 1.0, 1.0);
        let near = near.truncate() / near.w;
        let far = far.truncate() / far.w;
        Ray {
            origin: near,
            dir: (far - near).normalize(),
        }
    }
}

/// Two unit vectors spanning the tangent plane at unit vector `n`.
fn tangent_basis(n: DVec3) -> (DVec3, DVec3) {
    let helper = if n.y.abs() < 0.9 { DVec3::Y } else { DVec3::X };
    let t1 = n.cross(helper).normalize();
    (t1, n.cross(t1))
}

/// A view as a link's fragment: the point under the middle of the view and
/// the camera's height (m) over it, `at=47.51095,-122.46364,6430`. The
/// camera looks straight down, north up, so that's the whole view.
pub fn view_fragment(center: LonLat, altitude_m: f64) -> String {
    format!("at={:.5},{:.5},{:.0}", center.lat, center.lon, altitude_m)
}

/// The view in a link's fragment (`#at=…`, see `view_fragment`), if it
/// names one, its height kept within the camera's range.
pub fn parse_view_fragment(fragment: &str, radius: f64) -> Option<(LonLat, f64)> {
    let at = fragment
        .trim_start_matches('#')
        .split('&')
        .find_map(|part| part.strip_prefix("at="))?;
    let mut parts = at.split(',').map(|p| p.trim().parse::<f64>().ok());
    let (lat, lon, altitude) = (parts.next()??, parts.next()??, parts.next()??);
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) || !altitude.is_finite() {
        return None;
    }
    let altitude = altitude.clamp(MIN_ALTITUDE_RADII * radius, MAX_ALTITUDE_RADII * radius);
    Some((LonLat::new(lon, lat), altitude))
}

/// First point where `ray` enters the sphere of `radius` at the origin.
pub fn ray_sphere(ray: &Ray, radius: f64) -> Option<DVec3> {
    // |o + t d|^2 = r^2 with |d| = 1:  t^2 + 2 b t + c = 0
    let b = ray.origin.dot(ray.dir);
    let c = ray.origin.length_squared() - radius * radius;
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    let t = -b - disc.sqrt();
    (t >= 0.0).then(|| ray.origin + ray.dir * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_views_in_links() {
        let r = 6_371_000.0;
        let center = LonLat::new(-122.463639, 47.51095);
        let fragment = view_fragment(center, 6430.4);
        assert_eq!(fragment, "at=47.51095,-122.46364,6430");
        let (back, altitude) = parse_view_fragment(&format!("#{fragment}"), r).unwrap();
        assert!((back.lat - 47.51095).abs() < 1e-9 && (back.lon + 122.46364).abs() < 1e-9);
        assert_eq!(altitude, 6430.0);
        // Among other parts; too low a camera comes up to the lowest.
        let (_, low) = parse_view_fragment("#x=1&at=47.5,-122.4,10", r).unwrap();
        assert_eq!(low, MIN_ALTITUDE_RADII * r);
        for bad in [
            "",
            "#",
            "#at=47.5,-122.4",
            "#at=95,0,1000",
            "#at=a,b,c",
            "#at=47.5,-122.4,inf",
        ] {
            assert_eq!(parse_view_fragment(bad, r), None, "{bad}");
        }
    }
    use crate::geo::EARTH_RADIUS_M;

    #[test]
    fn center_ray_hits_point_below_camera() {
        let target = LonLat::new(-123.0, 48.5);
        let cam = OrbitCamera::over(EARTH_RADIUS_M, target, 2.0, 1.5);
        let hit = ray_sphere(&cam.ray(0.0, 0.0), EARTH_RADIUS_M).unwrap();
        let p = LonLat::from_vec(hit);
        assert!(
            (p.lon - target.lon).abs() < 1e-6 && (p.lat - target.lat).abs() < 1e-6,
            "{p:?}"
        );
        assert!((hit.length() / EARTH_RADIUS_M - 1.0).abs() < 1e-9);
    }

    #[test]
    fn corner_ray_misses_from_far_away() {
        let cam = OrbitCamera::over(EARTH_RADIUS_M, LonLat::new(0.0, 0.0), 5.0, 1.0);
        assert!(ray_sphere(&cam.ray(0.99, 0.99), EARTH_RADIUS_M).is_none());
    }

    #[test]
    fn screen_right_is_east() {
        let cam = OrbitCamera::over(EARTH_RADIUS_M, LonLat::new(10.0, 0.0), 2.0, 1.0);
        let left = LonLat::from_vec(ray_sphere(&cam.ray(-0.1, 0.0), EARTH_RADIUS_M).unwrap());
        let right = LonLat::from_vec(ray_sphere(&cam.ray(0.1, 0.0), EARTH_RADIUS_M).unwrap());
        assert!(right.lon > left.lon);
    }

    /// The grabbed point must end up back under the cursor.
    fn assert_grab_follows(cam: &mut OrbitCamera, from: (f64, f64), to: (f64, f64)) {
        let grabbed = ray_sphere(&cam.ray(from.0, from.1), cam.radius)
            .unwrap()
            .normalize();
        assert!(cam.drag_to(grabbed, to.0, to.1));
        let under = ray_sphere(&cam.ray(to.0, to.1), cam.radius)
            .unwrap()
            .normalize();
        assert!(
            under.angle_between(grabbed) < 1e-6,
            "off by {} rad",
            under.angle_between(grabbed)
        );
    }

    #[test]
    fn drag_keeps_grabbed_point_under_cursor() {
        let mut cam = OrbitCamera::over(EARTH_RADIUS_M, LonLat::new(-123.0, 48.5), 2.0, 1.6);
        assert_grab_follows(&mut cam, (0.1, 0.05), (-0.25, 0.2));
        assert_grab_follows(&mut cam, (-0.3, -0.2), (0.3, 0.25));
        // Diagonal drag away from screen center, where camera roll matters most.
        assert_grab_follows(&mut cam, (0.35, 0.35), (-0.1, -0.3));
    }

    #[test]
    fn drag_off_globe_leaves_camera_alone() {
        let mut cam = OrbitCamera::over(EARTH_RADIUS_M, LonLat::new(0.0, 0.0), 5.0, 1.0);
        let (yaw, pitch) = (cam.yaw, cam.pitch);
        assert!(!cam.drag_to(DVec3::X, 0.99, 0.99));
        assert_eq!((cam.yaw, cam.pitch), (yaw, pitch));
    }

    #[test]
    fn zoom_is_clamped() {
        let mut cam = OrbitCamera::over(1.0, LonLat::new(0.0, 0.0), 2.0, 1.0);
        cam.zoom(-100.0);
        assert!((cam.altitude - MIN_ALTITUDE_RADII).abs() < 1e-12);
        cam.zoom(100.0);
        assert!((cam.altitude - MAX_ALTITUDE_RADII).abs() < 1e-12);
    }
}
