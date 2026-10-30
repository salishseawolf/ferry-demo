//! Turns app state into per-frame GPU instance data.

use bytemuck::{Pod, Zeroable};
use glam::DVec3;

use crate::geo::{LonLat, great_circle_arc};
use crate::measure::Measure;

/// Lines float this far above the surface (fraction of radius, about 30 m)
/// so they don't z-fight with the cells.
const LINE_LIFT: f64 = 1.0 + 5e-6;
/// Densify paths so each drawn segment spans at most this many degrees
/// (about 5 km), keeping its chord within the lift of the surface.
const ARC_STEP_DEG: f64 = 0.05;

const MEASURE_COLOR: [f32; 4] = [0.2, 0.9, 1.0, 1.0];
/// The line while it still follows the cursor.
const MEASURING_COLOR: [f32; 4] = [0.2, 0.9, 1.0, 0.6];
/// The hex grid's lines, dark and faint so the map shows through, and the
/// cell under the cursor.
pub const GRID_COLOR: [f32; 4] = [0.05, 0.07, 0.10, 0.35];
pub const HOVER_CELL_COLOR: [f32; 4] = [0.9, 0.25, 0.1, 0.9];

/// A screen-space-width line segment between two world points.
/// Fields are tightly packed to match `vertex_attr_array!` offsets.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Segment {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub color: [f32; 4],
    /// Times the standard line width.
    pub width: f32,
    /// Meters along the path at `a` and `b`, for dashes.
    pub along: [f32; 2],
    /// Dash period in CSS px, or 0 for a solid line.
    pub dash: f32,
}

/// A cylinder standing on the surface along its normal. Tightly packed, as
/// for [`Segment`].
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Column {
    pub base: [f32; 3],
    pub height: f32,
    pub normal: [f32; 3],
    pub width: f32,
    pub color: [f32; 4],
}

/// A point symbol (markers.wgsl) at `position` lying along `heading` (a
/// unit tangent there). Sizes are in physical pixels. Tightly packed, as
/// for [`Segment`].
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Marker {
    pub position: [f32; 3],
    pub heading: [f32; 3],
    /// Length and width.
    pub size: [f32; 2],
    /// Outline width (0 for none), 1 to dash it, the `MarkerKind`, and for
    /// anchors the share of space left for the gauge (negative for none).
    pub style: [f32; 4],
    /// A hull's fill, or an anchor's gauge.
    pub fill: [f32; 4],
    /// A hull's outline, or the anchor itself.
    pub outline: [f32; 4],
    /// A soft halo around the symbol, to highlight it (alpha 0: none).
    pub glow: [f32; 4],
}

/// Shapes markers.wgsl draws (its KIND_* constants).
#[derive(Clone, Copy)]
pub enum MarkerKind {
    /// A ferry: a double-ended hull.
    Hull = 0,
    /// A terminal: an upright anchor, with a space gauge beneath.
    Anchor = 1,
}

#[derive(Default)]
pub struct Scene {
    pub segments: Vec<Segment>,
    pub columns: Vec<Column>,
    pub markers: Vec<Marker>,
}

impl Scene {
    /// `marker_scale` (meters) sizes the column markers; callers vary it with
    /// zoom so markers keep a steady on-screen size.
    pub fn build(measure: &Measure, hover: Option<LonLat>, radius: f64, marker_scale: f64) -> Self {
        let mut scene = Scene::default();
        let marker = marker_scale;

        // The measurement: a great-circle line, with a marker at each end
        // that is fixed (a live end follows the cursor).
        if let Some((a, b)) = measure.span(hover) {
            let color = if measure.is_finished() {
                MEASURE_COLOR
            } else {
                MEASURING_COLOR
            };
            scene.add_path(&[a.to_unit(), b.to_unit()], false, radius, color);
        }
        for p in [measure.source, measure.target].into_iter().flatten() {
            scene.add_column(
                p.to_unit(),
                radius,
                marker * 0.03,
                marker * 0.003,
                MEASURE_COLOR,
            );
        }
        scene
    }

    /// Open paths of unit vectors (such as the hex grid's edges) as lines.
    pub fn lines(paths: &[Vec<DVec3>], radius: f64, color: [f32; 4]) -> Vec<Segment> {
        let mut scene = Scene::default();
        for path in paths {
            scene.add_path(path, false, radius, color);
        }
        scene.segments
    }

    /// Outlines the ring of unit vectors `ring` (such as the cell under the
    /// cursor).
    pub fn add_outline(&mut self, ring: &[DVec3], radius: f64, color: [f32; 4]) {
        self.add_path(ring, true, radius, color);
    }

    /// An arrow at each of `sites` (unit vectors) along `velocity` there (a
    /// tangent vector, in any unit), `deg_per_unit` degrees of arc long per
    /// unit of speed and at most `max_deg`. Sites with no motion get none.
    // Kept for the vessel layers to come.
    #[allow(dead_code)]
    pub fn arrows(
        sites: &[DVec3],
        velocity: impl Fn(DVec3) -> DVec3,
        deg_per_unit: f64,
        max_deg: f64,
        radius: f64,
        color: [f32; 4],
    ) -> Vec<Segment> {
        let mut scene = Scene::default();
        for &p in sites {
            let v = velocity(p);
            if v.length() == 0.0 {
                continue;
            }
            let length = (v.length() * deg_per_unit).min(max_deg).to_radians();
            scene.add_arrow(p, v.normalize(), length, radius, color);
        }
        scene.segments
    }

    /// An arrow from `p` along tangent `dir`, `length` radians long.
    fn add_arrow(&mut self, p: DVec3, dir: DVec3, length: f64, radius: f64, color: [f32; 4]) {
        let tip = p * length.cos() + dir * length.sin();
        self.add_path(&[p, tip], false, radius, color);
        // Two barbs swept back from the tip.
        let back = (p - tip * p.dot(tip)).normalize();
        let side = tip.cross(back);
        let barb = 0.35 * length;
        for s in [-1.0, 1.0] {
            let d = (back * 0.9 + side * (0.45 * s)).normalize();
            let end = tip * barb.cos() + d * barb.sin();
            self.add_path(&[tip, end], false, radius, color);
        }
    }

    /// An open path of unit vectors, `width` times the standard line width.
    pub fn add_line(&mut self, points: &[DVec3], radius: f64, color: [f32; 4], width: f32) {
        self.add_dashed_line(points, radius, color, width, 0.0);
    }

    /// As `add_line`, dashed with a period of `dash_px` CSS px (0: solid).
    pub fn add_dashed_line(
        &mut self,
        points: &[DVec3],
        radius: f64,
        color: [f32; 4],
        width: f32,
        dash_px: f32,
    ) {
        let start = self.segments.len();
        self.add_path(points, false, radius, color);
        for s in &mut self.segments[start..] {
            s.width = width;
            s.dash = dash_px;
        }
    }

    fn add_path(&mut self, points: &[DVec3], closed: bool, radius: f64, color: [f32; 4]) {
        if points.len() < 2 {
            return;
        }
        let edges = points.len() - usize::from(!closed);
        let mut dense: Vec<DVec3> = (0..edges)
            .flat_map(|i| {
                great_circle_arc(
                    points[i],
                    points[(i + 1) % points.len()],
                    ARC_STEP_DEG.to_radians(),
                )
            })
            .collect();
        dense.push(if closed {
            points[0]
        } else {
            points[points.len() - 1]
        });

        let to_world = |v: DVec3| (v * radius * LINE_LIFT).as_vec3().to_array();
        let mut along = 0.0;
        self.segments.extend(dense.windows(2).map(|w| {
            let start = along;
            along += w[0].angle_between(w[1]) * radius;
            Segment {
                a: to_world(w[0]),
                b: to_world(w[1]),
                color,
                width: 1.0,
                along: [start as f32, along as f32],
                dash: 0.0,
            }
        }));
    }

    fn add_column(&mut self, unit: DVec3, radius: f64, height: f64, width: f64, color: [f32; 4]) {
        self.columns.push(Column {
            base: (unit * radius).as_vec3().to_array(),
            height: height as f32,
            normal: unit.as_vec3().to_array(),
            width: width as f32,
            color,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::EARTH_RADIUS_M;

    #[test]
    fn measurement_line_follows_cursor_then_stays() {
        let mut m = Measure::default();
        m.click(LonLat::new(0.0, 0.0));
        let hover = Some(LonLat::new(10.0, 0.0));
        let s = Scene::build(&m, hover, EARTH_RADIUS_M, EARTH_RADIUS_M);
        assert!(s.segments.len() > 3, "great circle is densified");
        assert!(s.segments.iter().all(|seg| seg.color == MEASURING_COLOR));
        for w in s.segments.windows(2) {
            assert_eq!(w[0].b, w[1].a, "continuous");
        }
        // The source is marked; the cursor isn't.
        assert_eq!(s.columns.len(), 1);
        assert_eq!(s.columns[0].color, MEASURE_COLOR);

        m.click(LonLat::new(10.0, 0.0));
        let s = Scene::build(&m, None, EARTH_RADIUS_M, EARTH_RADIUS_M);
        assert!(s.segments.iter().all(|seg| seg.color == MEASURE_COLOR));
        assert_eq!(s.columns.len(), 2, "both ends marked");
    }

    #[test]
    fn arrows_point_along_velocity_and_scale_with_speed() {
        let site = LonLat::new(0.0, 0.0).to_unit();
        // How far (degrees) the arrow at `site` reaches, and how far north.
        let reach = |speed: f64, max_deg: f64| {
            let segments = Scene::arrows(
                &[site],
                |_| DVec3::Y * speed,
                1.0,
                max_deg,
                1.0,
                MEASURE_COLOR,
            );
            let ends = segments.iter().flat_map(|s| [s.a, s.b]);
            let far = ends
                .map(|e| {
                    glam::Vec3::from_array(e)
                        .as_dvec3()
                        .angle_between(site)
                        .to_degrees()
                })
                .fold(0.0, f64::max);
            let north = segments
                .iter()
                .map(|s| s.b[1].max(s.a[1]))
                .fold(f32::MIN, f32::max);
            (far, north)
        };
        let (slow, north) = reach(1.0, 10.0);
        assert!((slow - 1.0).abs() < 1e-3 && north > 0.0, "{slow} {north}");
        assert!((reach(3.0, 10.0).0 - 3.0).abs() < 1e-3);
        assert!((reach(100.0, 2.0).0 - 2.0).abs() < 1e-3, "capped");
        assert!(Scene::arrows(&[site], |_| DVec3::ZERO, 1.0, 2.0, 1.0, MEASURE_COLOR).is_empty());
    }

    #[test]
    fn lines_and_outlines_follow_their_points() {
        let a = LonLat::new(0.0, 0.0).to_unit();
        let b = LonLat::new(0.01, 0.0).to_unit();
        let c = LonLat::new(0.01, 0.01).to_unit();
        let open = Scene::lines(&[vec![a, b, c]], 1.0, GRID_COLOR);
        assert_eq!(open.len(), 2);
        let mut scene = Scene::default();
        scene.add_outline(&[a, b, c], 1.0, HOVER_CELL_COLOR);
        // Closed: back to the start.
        assert_eq!(scene.segments.len(), 3);
        assert_eq!(scene.segments[2].b, scene.segments[0].a);
        scene.add_line(&[a, b], 1.0, GRID_COLOR, 0.5);
        assert_eq!(scene.segments.len(), 4);
        assert!(scene.segments[..3].iter().all(|s| s.width == 1.0));
        assert_eq!(scene.segments[3].width, 0.5);
        // Dashed paths carry their distance along, continuing segment to
        // segment.
        let mut dashed = Scene::default();
        dashed.add_dashed_line(&[a, b, c], EARTH_RADIUS_M, GRID_COLOR, 1.0, 6.0);
        let s = &dashed.segments;
        assert!(s.iter().all(|s| s.dash == 6.0));
        assert_eq!(s[0].along[0], 0.0);
        assert_eq!(s[0].along[1], s[1].along[0]);
        // About 1.1 km per 0.01° here.
        assert!((s[1].along[1] - 2224.0).abs() < 2.0, "{}", s[1].along[1]);
    }

    #[test]
    fn nothing_without_a_measurement() {
        // A cursor on the map alone draws nothing.
        for hover in [None, Some(LonLat::new(10.0, 0.0))] {
            let s = Scene::build(&Measure::default(), hover, EARTH_RADIUS_M, EARTH_RADIUS_M);
            assert!(s.segments.is_empty() && s.columns.is_empty());
        }
    }
}
