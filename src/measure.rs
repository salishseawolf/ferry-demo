//! Distance and bearing measurement between two points on the planet.
//!
//! The first click sets the source; until the second click the measurement
//! follows the cursor, and the second click fixes the target. The finished
//! measurement stays until the next click starts a new one.

use crate::geo::{LonLat, compass_point, distance_m, group_thousands, initial_bearing_deg};

#[derive(Default)]
pub struct Measure {
    pub source: Option<LonLat>,
    pub target: Option<LonLat>,
}

impl Measure {
    pub fn click(&mut self, p: LonLat) {
        if self.source.is_none() || self.target.is_some() {
            self.source = Some(p);
            self.target = None;
        } else {
            self.target = Some(p);
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn is_finished(&self) -> bool {
        self.target.is_some()
    }

    /// The measured pair: fixed when finished, else from the source to
    /// `hover` while measuring.
    pub fn span(&self, hover: Option<LonLat>) -> Option<(LonLat, LonLat)> {
        Some((self.source?, self.target.or(hover)?))
    }
}

/// A measurement's results, as shown and copied.
pub struct Reading {
    pub source: LonLat,
    /// Initial great-circle bearing (degrees from true north); None when
    /// undefined (the same point, or exactly opposite points).
    pub bearing_deg: Option<f64>,
    pub distance_m: f64,
}

impl Reading {
    pub fn new(source: LonLat, target: LonLat, radius: f64) -> Self {
        Self {
            source,
            bearing_deg: initial_bearing_deg(source, target),
            distance_m: distance_m(source, target, radius),
        }
    }

    /// e.g. `247.50° (WSW)`.
    pub fn bearing_text(&self) -> String {
        self.bearing_deg.map_or_else(
            || "undefined".to_string(),
            |b| format!("{b:.2}° ({})", compass_point(b)),
        )
    }

    /// e.g. `1,234.56 km`.
    pub fn distance_text(&self) -> String {
        let hundredths = (self.distance_m / 10.0).round() as u64;
        format!(
            "{}.{:02} km",
            group_thousands(hundredths / 100),
            hundredths % 100
        )
    }

    /// One line for the clipboard.
    pub fn clipboard_text(&self) -> String {
        format!(
            "Source {}; bearing {}; distance {}",
            self.source.display(),
            self.bearing_text(),
            self.distance_text()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::EARTH_RADIUS_M;

    #[test]
    fn clicks_set_source_then_target_then_restart() {
        let (a, b, c) = (
            LonLat::new(0.0, 0.0),
            LonLat::new(10.0, 0.0),
            LonLat::new(20.0, 0.0),
        );
        let cursor = LonLat::new(5.0, 5.0);
        let hover = Some(cursor);
        let mut m = Measure::default();
        assert!(m.span(hover).is_none());
        m.click(a);
        assert_eq!(m.span(hover), Some((a, cursor)), "follows the cursor");
        assert_eq!(m.span(None), None);
        m.click(b);
        assert!(m.is_finished());
        assert_eq!(m.span(hover), Some((a, b)), "fixed once finished");
        m.click(c);
        assert_eq!((m.source, m.target), (Some(c), None), "next click restarts");
        m.clear();
        assert!(m.source.is_none());
    }

    #[test]
    fn reading_text() {
        let r = Reading::new(
            LonLat::new(0.0, 0.0),
            LonLat::new(90.0, 0.0),
            EARTH_RADIUS_M,
        );
        assert_eq!(r.bearing_text(), "90.00° (E)");
        // A quarter of the circumference.
        assert_eq!(r.distance_text(), "10,007.56 km");
        assert_eq!(
            r.clipboard_text(),
            "Source 0.0000° N, 0.0000° E; bearing 90.00° (E); distance 10,007.56 km"
        );
        let same = Reading::new(LonLat::new(1.0, 1.0), LonLat::new(1.0, 1.0), EARTH_RADIUS_M);
        assert_eq!(same.bearing_text(), "undefined");
        assert_eq!(same.distance_text(), "0.00 km");
    }
}
