//! Where the view looks, flying between views, and fitting things into the
//! part of the window the panels leave free.

use web_sys::Element;

use crate::finder::Sheet;
use crate::geo::LonLat;
use crate::placement::Rect;
use glam::DVec3;

use super::dom::{phone_layout, prefers_reduced_motion};
use super::{App, FIT_ITERATIONS, FIT_SHARE};

/// A fit keeps this far (CSS px) from the panels around the free part
/// of the map. On phones the details sheet is, at its peek and half
/// heights, this tall (index.html's .sheet-peek and .sheet-half).
const FIT_PANEL_GAP_PX: f64 = 8.0;
const SHEET_PEEK_PX: f64 = 124.0;
const SHEET_HALF_SHARE: f64 = 0.52;

impl App {
    /// Where the view looks and how far in.
    pub(super) fn pose(&self) -> Pose {
        Pose {
            focus: self.camera.eye_dir(),
            scale: self.camera.altitude,
        }
    }

    /// Looks at `pose`, within the zoom limits.
    pub(super) fn set_pose(&mut self, pose: Pose) {
        self.camera.set_eye_dir(pose.focus);
        self.camera.altitude = pose.scale;
        self.camera.zoom(0.0);
    }

    /// The part of the canvas the panels leave clear (CSS px): below the
    /// search bar; on a phone, above the details sheet; on a wider
    /// screen, beside the finder's card and the debug panel when they're
    /// open.
    fn free_area(&self) -> Rect {
        let (w, h) = (
            self.canvas.client_width() as f64,
            self.canvas.client_height() as f64,
        );
        let mut free = Rect::at(0.0, 0.0, w, h);
        let rect = |el: &Element| {
            let r = el.get_bounding_client_rect();
            (r.width() > 0.0 && r.height() > 0.0)
                .then(|| Rect::at(r.x(), r.y(), r.width(), r.height()))
        };
        let gap = FIT_PANEL_GAP_PX;
        let finder = self.finder.input.closest("#finder").ok().flatten();
        if let Some(bar) = finder
            .as_ref()
            .and_then(|f| f.query_selector(".bar").ok().flatten())
            && let Some(r) = rect(&bar)
        {
            free.y0 = free.y0.max(r.y1 + gap);
        }
        if phone_layout() {
            if self.finder.matched() {
                let sheet = match self.finder.sheet() {
                    Sheet::Peek => SHEET_PEEK_PX,
                    Sheet::Half | Sheet::Full => SHEET_HALF_SHARE * h,
                };
                free.y1 = free.y1.min(h - sheet - gap);
            }
        } else {
            let open = finder
                .as_ref()
                .is_some_and(|f| f.class_list().contains("open"));
            if open
                && self.finder.matched()
                && let Some(r) = finder.as_ref().and_then(rect)
            {
                free.x0 = free.x0.max(r.x1 + gap);
            }
            if self.perf.visible
                && let Some(r) = rect(&self.perf.card)
            {
                free.x1 = free.x1.min(r.x0 - gap);
            }
        }
        // Never less than a third of the canvas either way.
        if free.x1 - free.x0 < w / 3.0 {
            (free.x0, free.x1) = (0.0, w);
        }
        if free.y1 - free.y0 < h / 3.0 {
            (free.y0, free.y1) = (0.0, h);
        }
        free
    }

    /// The pose that fits `points` (unit vectors) in `FIT_SHARE` of the
    /// free part of the canvas (`free_area`), centered in it. Sizes on
    /// screen go about as 1/scale, so each pass rescales by how far off
    /// the fit is and moves the points' box toward the free part's
    /// middle, until it settles. None without points.
    fn fit_pose(&mut self, points: &[DVec3]) -> Option<Pose> {
        let focus = points.iter().sum::<DVec3>().try_normalize()?;
        let start = self.pose();
        let (w, h) = (
            self.canvas.client_width() as f64,
            self.canvas.client_height() as f64,
        );
        let free = self.free_area();
        let (fw, fh) = (free.x1 - free.x0, free.y1 - free.y0);
        let aim = free.center();
        let mut pose = Pose {
            focus,
            scale: start.scale,
        };
        for _ in 0..FIT_ITERATIONS {
            self.set_pose(pose);
            pose = self.pose();
            // Some points beyond the horizon or off the map: back out.
            let Some(screen) = points
                .iter()
                .map(|&p| self.screen_px(p))
                .collect::<Option<Vec<_>>>()
            else {
                pose.scale *= 2.0;
                continue;
            };
            let (mut min, mut max) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
            for (x, y) in screen {
                min = (min.0.min(x), min.1.min(y));
                max = (max.0.max(x), max.1.max(y));
            }
            let mid = ((min.0 + max.0) / 2.0, (min.1 + max.1) / 2.0);
            let factor = ((max.0 - min.0) / (FIT_SHARE * fw))
                .max((max.1 - min.1) / (FIT_SHARE * fh))
                .max(1e-3);
            let settled =
                (factor - 1.0).abs() < 0.005 && (mid.0 - aim.0).hypot(mid.1 - aim.1) < 0.5;
            // Centering the view on what's under `under` moves the points'
            // middle onto `aim`.
            let under = (mid.0 + w / 2.0 - aim.0, mid.1 + h / 2.0 - aim.1);
            pose = Pose {
                focus: self.surface_at(under).unwrap_or(pose.focus),
                scale: pose.scale * factor,
            };
            if settled {
                break;
            }
        }
        self.set_pose(pose);
        let fitted = self.pose();
        self.set_pose(start);
        Some(fitted)
    }

    /// Flies the view to fit what the pointer has highlighted; says
    /// whether anything is.
    pub(super) fn fly_to_focus(&mut self) -> bool {
        let points = std::mem::take(&mut self.focus_points);
        self.fly_to(&points)
    }

    /// Flies the view to fit `points` (unit vectors); says whether there
    /// were any. With reduced motion asked for, it goes there at once.
    pub(super) fn fly_to(&mut self, points: &[DVec3]) -> bool {
        let Some(to) = self.fit_pose(points) else {
            return false;
        };
        if prefers_reduced_motion() {
            self.set_pose(to);
        } else {
            self.flight = Some(Flight {
                from: self.pose(),
                to,
                started_ms: None,
            });
        }
        self.redraw = true;
        true
    }

    /// Where unit vector `p` is drawn, in CSS px from the canvas's top
    /// left, if it's on the visible side of the planet.
    pub(super) fn screen_px(&self, p: DVec3) -> Option<(f64, f64)> {
        let (w, h) = (
            self.canvas.client_width() as f64,
            self.canvas.client_height() as f64,
        );
        let radius = self.camera.radius;
        if p.dot(self.camera.eye()) <= radius {
            return None;
        }
        let clip = self.camera.view_proj() * (p * radius).extend(1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let (x, y) = (clip.x / clip.w, clip.y / clip.w);
        Some(((x + 1.0) / 2.0 * w, (1.0 - y) / 2.0 * h))
    }

    /// The view's center on the ground, the ground meters across one CSS
    /// pixel there, and about how far (m) the view reaches from its center.
    pub(super) fn view_scale(&self) -> (LonLat, f64, f64) {
        let height_px = self.canvas.client_height().max(1) as f64;
        let corner = (1.0 + self.camera.aspect * self.camera.aspect).sqrt();
        let center = LonLat::from_vec(self.camera.eye_dir());
        let half_m = self.camera.altitude * (self.camera.fov_y / 2.0).tan();
        (center, 2.0 * half_m / height_px, half_m * corner)
    }
}

/// Where the view looks: the point at the screen center (a unit vector),
/// and how far in, as the camera's altitude (m). Sizes on screen go about
/// as 1/scale.
#[derive(Clone, Copy)]
pub(super) struct Pose {
    pub(super) focus: DVec3,
    pub(super) scale: f64,
}

/// The view moving from one pose to another, eased in and out: along the
/// great circle between the focuses, and zooming evenly in log scale.
pub(super) struct Flight {
    pub(super) from: Pose,
    pub(super) to: Pose,
    /// The animation frame time it began, from its first frame.
    pub(super) started_ms: Option<f64>,
}

impl Flight {
    /// The pose `t` (0 to 1) of the way along.
    pub(super) fn at(&self, t: f64) -> Pose {
        let e = if t < 0.5 {
            4.0 * t * t * t
        } else {
            1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
        };
        let (a, b) = (self.from.focus, self.to.focus);
        let angle = a.dot(b).clamp(-1.0, 1.0).acos();
        let focus = if angle < 1e-9 {
            b
        } else {
            (a * ((1.0 - e) * angle).sin() + b * (e * angle).sin()) / angle.sin()
        };
        let scale = (self.from.scale.ln() * (1.0 - e) + self.to.scale.ln() * e).exp();
        Pose { focus, scale }
    }
}
