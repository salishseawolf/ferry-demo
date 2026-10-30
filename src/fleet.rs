//! The Washington State Ferries fleet, as the server reports it
//! (`/api/v1/live/vessels`, with names from `/vessels` and `/network`):
//! moving each boat on from its last report, and describing one for the
//! HUD.

use std::collections::HashMap;

use ferry_model::api::{LiveVessels, Vessels};
use glam::DVec3;

use crate::geo::{EARTH_RADIUS_M, LonLat};

/// Meters per second in one knot.
const KNOT_MPS: f64 = 1852.0 / 3600.0;
/// Slower than this (knots) counts as stopped: GPS jitter at the dock
/// reads as a knot or so.
const MOVING_KN: f64 = 1.0;
/// A report is carried forward at most this long (s); past it the boat
/// waits at its last position for fresh data.
const MAX_EXTRAPOLATE_S: f64 = 60.0;
/// Departures within this many minutes of schedule are on time.
const ON_TIME_MIN: f64 = 1.0;
/// A boat's wake covers its positions from this long ago (ms).
pub const WAKE_MS: f64 = 180_000.0;
/// The speed vector reaches where the boat will be this long ahead (s),
/// the chart convention for course and speed.
pub const VECTOR_S: f64 = 180.0;

/// The fleet's latest report.
#[derive(Default)]
pub struct Fleet {
    pub vessels: Vec<Vessel>,
    /// Each boat's length and beam (m), from the server's boat details.
    pub dimensions: HashMap<u32, (f64, f64)>,
}

/// A boat whose dimensions haven't loaded: about an Issaquah-class hull.
const TYPICAL_HULL_M: (f64, f64) = (100.0, 24.0);

impl Fleet {
    /// `v`'s length and beam in meters.
    pub fn size_m(&self, v: &Vessel) -> (f64, f64) {
        self.dimensions
            .get(&v.id)
            .copied()
            .unwrap_or(TYPICAL_HULL_M)
    }

    /// `v`'s wake at `now_ms`: its reports within a wake's age, then where
    /// it's drawn now, each with its age as a fraction of `WAKE_MS` (0 for
    /// now).
    pub fn wake(&self, v: &Vessel, now_ms: f64) -> Vec<(DVec3, f64)> {
        let mut points: Vec<(DVec3, f64)> = v
            .wake
            .iter()
            .map(|&(t, p)| (p.to_unit(), (now_ms - t) / WAKE_MS))
            .filter(|&(_, age)| age <= 1.0)
            .collect();
        points.push((v.position_at(now_ms), 0.0));
        points
    }
}

/// The boats in a `/live/vessels` document, named from `vessel_names` and
/// `terminal_names` (by id; a boat not yet named is called by its id).
pub fn vessels_from(
    doc: &LiveVessels,
    vessel_names: &HashMap<u32, String>,
    terminal_names: &HashMap<u32, String>,
) -> Vec<Vessel> {
    let terminal = |id: Option<u32>| id.and_then(|id| terminal_names.get(&id).cloned());
    let ms = |t: Option<i64>| t.map(|t| t as f64);
    doc.vessels
        .iter()
        .map(|v| Vessel {
            id: v.id,
            name: vessel_names
                .get(&v.id)
                .cloned()
                .unwrap_or_else(|| format!("Vessel {}", v.id)),
            position: LonLat::new(v.lon, v.lat),
            heading_deg: v.heading_deg,
            speed_kn: v.speed_kn,
            at_dock: v.at_dock,
            in_service: v.in_service,
            departing: terminal(v.departing),
            arriving: terminal(v.arriving),
            departing_id: v.departing,
            arriving_id: v.arriving,
            scheduled_departure_ms: ms(v.scheduled_departure_ms),
            left_dock_ms: ms(v.left_dock_ms),
            eta_ms: ms(v.eta_ms),
            reported_ms: v.reported_ms as f64,
            note: v.note.clone(),
            wake: v
                .wake
                .iter()
                .map(|p| (p.0 as f64, LonLat::new(p.1, p.2)))
                .collect(),
        })
        .collect()
}

/// Each boat's length and beam (m) from a `/vessels` document, for the
/// boats that list both.
pub fn dimensions_from(doc: &Vessels) -> HashMap<u32, (f64, f64)> {
    doc.vessels
        .iter()
        .filter_map(|v| Some((v.id, (v.length_m?, v.beam_m?))))
        .collect()
}

/// One vessel's last report.
#[derive(Clone, Debug, PartialEq)]
pub struct Vessel {
    pub id: u32,
    pub name: String,
    pub position: LonLat,
    /// Degrees clockwise from true north.
    pub heading_deg: f64,
    pub speed_kn: f64,
    pub at_dock: bool,
    /// Assigned to a route today (out-of-service boats lie at Eagle Harbor
    /// and the like).
    pub in_service: bool,
    pub departing: Option<String>,
    pub arriving: Option<String>,
    /// The same terminals by WSF's id (see `terminals`).
    pub departing_id: Option<u32>,
    pub arriving_id: Option<u32>,
    /// Times are milliseconds since the Unix epoch.
    pub scheduled_departure_ms: Option<f64>,
    pub left_dock_ms: Option<f64>,
    pub eta_ms: Option<f64>,
    /// When the position was taken.
    pub reported_ms: f64,
    /// WSF's service note for the boat, when it's current.
    pub note: Option<String>,
    /// Its reports over the last few minutes (when, where), oldest first.
    pub wake: Vec<(f64, LonLat)>,
}

impl Vessel {
    pub fn is_moving(&self) -> bool {
        !self.at_dock && self.speed_kn >= MOVING_KN
    }

    /// Where the boat is at `now_ms`: its reported position, carried on
    /// along its heading at its speed for up to `MAX_EXTRAPOLATE_S`.
    pub fn position_at(&self, now_ms: f64) -> DVec3 {
        let p = self.position.to_unit();
        if !self.is_moving() {
            return p;
        }
        let seconds = ((now_ms - self.reported_ms) / 1000.0).clamp(0.0, MAX_EXTRAPOLATE_S);
        let angle = self.speed_kn * KNOT_MPS * seconds / EARTH_RADIUS_M;
        p * angle.cos() + self.heading_at() * angle.sin()
    }

    /// The heading as a unit tangent vector at the reported position.
    pub fn heading_at(&self) -> DVec3 {
        let (east, north) = self.position.east_north();
        let h = self.heading_deg.to_radians();
        east * h.sin() + north * h.cos()
    }

    pub fn speed_mps(&self) -> f64 {
        self.speed_kn * KNOT_MPS
    }

    /// The heading as a unit tangent at `p` (a unit vector near the boat).
    pub fn heading_from(&self, p: DVec3) -> DVec3 {
        let h = self.heading_at();
        (h - p * h.dot(p)).normalize_or_zero()
    }

    /// The tip of the speed vector from `p`: where the boat will be in
    /// `VECTOR_S` at its speed and heading.
    pub fn ahead_of(&self, p: DVec3) -> DVec3 {
        let angle = self.speed_mps() * VECTOR_S / EARTH_RADIUS_M;
        p * angle.cos() + self.heading_from(p) * angle.sin()
    }

    /// Lines for the HUD at `now_ms`, with times written by `clock`.
    pub fn describe(&self, now_ms: f64, clock: impl Fn(f64) -> String) -> Vec<String> {
        let state = if !self.in_service {
            "out of service".to_string()
        } else if self.at_dock {
            "at dock".to_string()
        } else {
            format!("under way, {:.1} kn", self.speed_kn)
        };
        let mut lines = vec![format!("{} · {state}", self.name)];
        let route = match (&self.departing, &self.arriving) {
            (Some(a), Some(b)) => Some(format!("{a} → {b}")),
            (Some(a), None) => Some(format!("From {a}")),
            _ => None,
        };
        if let Some(mut route) = route {
            if let Some(eta) = self.eta_ms {
                let minutes = ((eta - now_ms) / 60_000.0).round().max(0.0);
                route += &format!(" · ETA {} ({minutes:.0} min)", clock(eta));
            }
            lines.push(route);
        }
        match (self.left_dock_ms, self.scheduled_departure_ms) {
            (Some(left), Some(scheduled)) => {
                let late = (left - scheduled) / 60_000.0;
                let timing = if late.abs() <= ON_TIME_MIN {
                    "on time".to_string()
                } else if late > 0.0 {
                    format!("{late:.0} min late")
                } else {
                    format!("{:.0} min early", -late)
                };
                lines.push(format!("Left {}, {timing}", clock(left)));
            }
            (None, Some(scheduled)) if self.at_dock => {
                lines.push(format!("Scheduled to leave {}", clock(scheduled)));
            }
            _ => {}
        }
        if let Some(note) = &self.note {
            lines.push(note.clone());
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two boats as the server sends them.
    const SAMPLE: &str = r#"{
      "source":{"fetched_ms":1793297890000,"next_update_ms":1793297900000,"stale":false,"error":null},
      "vessels":[
        {"id":28,"lon":-122.38148,"lat":47.603768,"speed_kn":17.3,"heading_deg":256,
         "at_dock":false,"in_service":true,"departing":7,"arriving":4,
         "scheduled_departure_ms":1793297400000,"left_dock_ms":1793297484000,
         "eta_ms":1793300760000,"reported_ms":1793297887000,"note":null,
         "wake":[[1793297877000,-122.3790,47.6040],[1793297887000,-122.38148,47.603768]]},
        {"id":15,"lon":-122.513137,"lat":47.620033,"speed_kn":0,"heading_deg":291,
         "at_dock":true,"in_service":false,"departing":null,"arriving":null,
         "scheduled_departure_ms":null,"left_dock_ms":null,"eta_ms":null,
         "reported_ms":1793297830000,"note":"Vessel out of service due to lack of crew",
         "wake":[]}]}"#;

    fn sample() -> Vec<Vessel> {
        let doc: LiveVessels = serde_json::from_str(SAMPLE).unwrap();
        let boats = HashMap::from([(28, "Sealth".to_string())]);
        let terminals = HashMap::from([(7, "Seattle".to_string()), (4, "Bremerton".to_string())]);
        vessels_from(&doc, &boats, &terminals)
    }

    #[test]
    fn reads_the_servers_boats() {
        let v = sample();
        assert_eq!(v.len(), 2);
        let sealth = &v[0];
        assert_eq!((sealth.id, sealth.name.as_str()), (28, "Sealth"));
        assert_eq!(sealth.position, LonLat::new(-122.38148, 47.603768));
        assert_eq!(sealth.arriving.as_deref(), Some("Bremerton"));
        assert_eq!(
            (sealth.departing_id, sealth.arriving_id),
            (Some(7), Some(4))
        );
        assert_eq!(sealth.left_dock_ms, Some(1_793_297_484_000.0));
        assert_eq!(sealth.reported_ms, 1_793_297_887_000.0);
        assert_eq!(sealth.wake.len(), 2);
        assert!(sealth.is_moving());

        // Not named yet: called by its id.
        let issaquah = &v[1];
        assert_eq!(issaquah.name, "Vessel 15");
        assert!(!issaquah.in_service && !issaquah.is_moving());
        assert_eq!(
            (issaquah.departing.as_deref(), issaquah.eta_ms),
            (None, None)
        );
        assert_eq!(
            issaquah.note.as_deref(),
            Some("Vessel out of service due to lack of crew")
        );
    }

    #[test]
    fn reads_boat_dimensions() {
        let doc: Vessels = serde_json::from_str(
            r#"{"vessels":[
              {"id":17,"name":"Kaleetan","class":null,"length_m":116.5,"beam_m":22.3,"car_capacity":202},
              {"id":99,"name":"Unknown","class":null,"length_m":null,"beam_m":18.0,"car_capacity":null}]}"#,
        )
        .unwrap();
        let d = dimensions_from(&doc);
        assert_eq!(d[&17], (116.5, 22.3));
        assert!(!d.contains_key(&99));
    }

    #[test]
    fn carries_moving_boats_along_their_heading() {
        let mut v = sample().remove(0);
        v.heading_deg = 90.0;
        v.speed_kn = 20.0;
        let start = v.position.to_unit();
        let after = |s: f64| v.position_at(v.reported_ms + s * 1000.0);
        let meters = |p: DVec3| p.angle_between(start) * EARTH_RADIUS_M;
        // 30 s at 20 kn is about 309 m, due east.
        assert!((meters(after(30.0)) - 308.7).abs() < 0.5);
        assert!(LonLat::from_vec(after(30.0)).lon > v.position.lon);
        // Not before the report, nor on past the limit.
        assert!(meters(after(-10.0)) < 1e-6);
        assert!((meters(after(600.0)) - meters(after(MAX_EXTRAPOLATE_S))).abs() < 1e-6);
        // Docked boats stay put.
        let docked = sample().remove(1);
        assert_eq!(
            docked.position_at(docked.reported_ms + 30_000.0),
            docked.position.to_unit()
        );
    }

    #[test]
    fn wakes_age_out() {
        let fleet = Fleet::default();
        let v = sample().remove(0);
        let t0 = v.reported_ms;
        let wake = fleet.wake(&v, t0);
        // Two reports, then the boat where it's drawn now.
        assert_eq!(wake.len(), 3);
        assert!((wake[0].1 - 10_000.0 / WAKE_MS).abs() < 1e-9);
        assert_eq!(wake[2].1, 0.0);
        // Past a wake's age, old reports drop out.
        assert_eq!(fleet.wake(&v, t0 + WAKE_MS - 5_000.0).len(), 2);
    }

    #[test]
    fn speed_vector_reaches_three_minutes_ahead() {
        let mut v = sample().remove(0);
        v.speed_kn = 20.0;
        v.heading_deg = 0.0;
        let p = v.position.to_unit();
        let tip = v.ahead_of(p);
        // 3 min at 20 kn is 1852 m, due north.
        assert!((tip.angle_between(p) * EARTH_RADIUS_M - 1852.0).abs() < 0.5);
        assert!(LonLat::from_vec(tip).lat > v.position.lat);
    }

    #[test]
    fn describes_route_timing_and_notes() {
        let v = sample();
        let clock = |ms: f64| format!("t{}", (ms / 1000.0) as u64 % 10_000);
        let now = 1_793_297_887_000.0;
        let sealth = v[0].describe(now, clock);
        assert_eq!(sealth[0], "Sealth · under way, 17.3 kn");
        assert_eq!(sealth[1], "Seattle → Bremerton · ETA t760 (48 min)");
        // Left 84 s after the scheduled time.
        assert_eq!(sealth[2], "Left t7484, 1 min late");
        let mut punctual = v[0].clone();
        punctual.left_dock_ms = punctual.scheduled_departure_ms.map(|t| t + 50_000.0);
        assert_eq!(punctual.describe(now, clock)[2], "Left t7450, on time");
        let issaquah = v[1].describe(now, clock);
        assert_eq!(
            issaquah,
            [
                "Vessel 15 · out of service",
                "Vessel out of service due to lack of crew"
            ]
        );
    }
}
