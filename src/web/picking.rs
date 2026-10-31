//! What's under the pointer, and what to light up for it or for the
//! finder's selection.

use std::collections::BTreeSet;

use crate::finder::Target;
use crate::geo::LonLat;
use crate::routes::legs_between;
use glam::DVec3;

use super::{ANCHOR_CLEAR_M, ANCHOR_INLAND, ANCHOR_M, App, ROUTE_PICK_PX};

/// A lone boat or terminal is fitted as if it reached this far (m).
const FIT_MIN_REACH_M: f64 = 1500.0;

impl App {
    /// The route whose line passes within `ROUTE_PICK_PX` of `cursor`
    /// (CSS px), the nearest if several. Slip approaches aren't picked.
    pub(super) fn route_at(&self, cursor: (f64, f64)) -> Option<usize> {
        let (cx, cy) = cursor;
        let to_segment = |a: (f64, f64), b: (f64, f64)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let t = (((cx - a.0) * dx + (cy - a.1) * dy) / (dx * dx + dy * dy).max(1e-9))
                .clamp(0.0, 1.0);
            (cx - a.0 - dx * t).hypot(cy - a.1 - dy * t)
        };
        self.routes
            .iter()
            .enumerate()
            .filter(|&(r, _)| {
                self.legs
                    .get(r)
                    .is_none_or(|l| l.ends.is_none_or(|(a, b)| a != b))
            })
            .filter_map(|(r, route)| {
                let screen: Vec<Option<(f64, f64)>> =
                    route.points.iter().map(|&p| self.screen_px(p)).collect();
                let d = screen
                    .windows(2)
                    .filter_map(|w| Some(to_segment(w[0]?, w[1]?)))
                    .fold(f64::INFINITY, f64::min);
                (d <= ROUTE_PICK_PX).then_some((r, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(r, _)| r)
    }

    /// What to highlight. A hovered boat: itself, the legs of its trip and
    /// the terminal it's bound for. A hovered leg: the route riders know
    /// it as, with all its legs and terminals and the boats sailing
    /// between them (or, until the terminals arrive, just the leg).
    pub(super) fn highlight(&self, hovered: Option<Picked>, route: Option<usize>) -> Highlight {
        if let Some(Picked::Vessel(i)) = hovered {
            return self.lit_vessel(i);
        }
        let Some(r) = route else {
            return Highlight::default();
        };
        let service = self.service_routes.iter().position(|s| s.legs.contains(&r));
        let mut lit = service.map_or_else(
            || Highlight {
                routes: vec![r],
                dim: true,
                ..Highlight::default()
            },
            |s| self.lit_service(s),
        );
        lit.leg = Some(r);
        lit
    }

    /// What to highlight for the finder's selection: a boat or route as
    /// when hovered, and a terminal with the legs from it.
    pub(super) fn highlight_target(&self, target: Target) -> Highlight {
        match target {
            Target::Vessel(id) => self
                .fleet
                .vessels
                .iter()
                .position(|v| v.id == id)
                .map(|i| self.lit_vessel(i))
                .unwrap_or_default(),
            Target::Route(s) if s < self.service_routes.len() => self.lit_service(s),
            Target::Route(_) => Highlight::default(),
            Target::Terminal(id) => Highlight {
                terminals: BTreeSet::from([id]),
                routes: (0..self.legs.len())
                    .filter(|&i| self.legs[i].ends.is_some_and(|(a, b)| a == id || b == id))
                    .collect(),
                ..Highlight::default()
            },
        }
    }

    /// Boat `i` (in the fleet) with, under way, the legs of its trip and
    /// the terminal it's bound for, or at the dock, the terminal it's at.
    fn lit_vessel(&self, i: usize) -> Highlight {
        let v = &self.fleet.vessels[i];
        let mut lit = Highlight {
            vessels: BTreeSet::from([i]),
            ..Highlight::default()
        };
        if v.at_dock {
            // WSF gives a docked boat's terminal as the one it departs
            // from next; laid up elsewhere (Eagle Harbor), it has none.
            lit.terminals.extend(v.departing_id);
        } else if let Some((from, to)) = v.departing_id.zip(v.arriving_id) {
            lit.routes = legs_between(&self.legs, from, to);
            lit.terminals.insert(to);
        }
        lit
    }

    /// Route `s` (of the routes riders know): its legs and terminals and
    /// the boats sailing between them, with everything else faded.
    fn lit_service(&self, s: usize) -> Highlight {
        let service = &self.service_routes[s];
        let serves = |id: Option<u32>| id.is_some_and(|id| service.terminals.contains(&id));
        Highlight {
            routes: service.legs.clone(),
            leg: None,
            service: Some(s),
            vessels: (0..self.fleet.vessels.len())
                .filter(|&i| {
                    let v = &self.fleet.vessels[i];
                    serves(v.departing_id) && serves(v.arriving_id)
                })
                .collect(),
            terminals: service.terminals.clone(),
            dim: true,
        }
    }

    /// Points around everything in `lit` (unit vectors), to fit the view
    /// to: route lines, and each boat and terminal as far as it's drawn
    /// out to. Empty if nothing's lit.
    pub(super) fn points_around(&self, lit: &Highlight, now: f64) -> Vec<DVec3> {
        let radius = self.camera.radius;
        let mut points = Vec::new();
        // With no route lit, a lone boat or terminal is fitted with some
        // water around it rather than filling the canvas.
        let least = if lit.routes.is_empty() {
            FIT_MIN_REACH_M
        } else {
            0.0
        };
        let mut around = |p: DVec3, reach_m: f64| {
            let (east, north) = LonLat::from_vec(p).east_north();
            let k = reach_m.max(least) / radius;
            for d in [east, -east, north, -north] {
                points.push((p + d * k).normalize());
            }
        };
        for &i in &lit.vessels {
            let v = &self.fleet.vessels[i];
            around(v.position_at(now), self.fleet.size_m(v).0 / 2.0);
        }
        let terminals = &self.terminals;
        let inland = (ANCHOR_INLAND * ANCHOR_M + ANCHOR_CLEAR_M) / radius;
        for t in terminals
            .list
            .iter()
            .filter(|t| lit.terminals.contains(&t.id))
        {
            let p = t.position.to_unit();
            // Where the anchor stands, as add_ferries places it; its gauge
            // hangs below, so it reaches about 3/4 of its height around.
            let at = terminals.facing_deg(t).map_or(p, |deg| {
                let (east, north) = t.position.east_north();
                let d = deg.to_radians();
                (p - (east * d.sin() + north * d.cos()) * inland).normalize()
            });
            around(at, 0.75 * ANCHOR_M);
        }
        for &r in &lit.routes {
            points.extend_from_slice(&self.routes[r].points);
        }
        points
    }

    /// HUD lines for the route of leg `r`: its name, the leg if it has
    /// several, and the boats on it.
    pub(super) fn describe_route(&self, r: usize, lit: &Highlight) -> String {
        let name_of = |id: u32| {
            let t = self.terminals.list.iter().find(|t| t.id == id);
            t.map_or_else(|| id.to_string(), |t| t.name.clone())
        };
        let service = lit.service.map(|s| &self.service_routes[s]);
        let mut lines = vec![match service {
            Some(s) => format!("{} route", s.name),
            None => self.routes[r].name.clone(),
        }];
        let joins = |i: &usize| self.legs[*i].joins().is_some();
        let several = service.is_some_and(|s| s.legs.iter().filter(|i| joins(i)).count() > 1);
        if several && let Some((a, b)) = self.legs[r].joins() {
            lines.push(format!("This leg: {} ↔ {}", name_of(a), name_of(b)));
        }
        let mut boats: Vec<&str> = lit
            .vessels
            .iter()
            .map(|&i| self.fleet.vessels[i].name.as_str())
            .collect();
        boats.sort_unstable();
        lines.push(match boats.len() {
            0 => "No boats on it now".to_string(),
            _ => format!("On it now: {}", boats.join(", ")),
        });
        lines.join("\n")
    }
}

/// A leg of a ferry route as drawn: a name for the HUD, and its points as
/// unit vectors.
pub(super) struct RouteLine {
    pub(super) name: String,
    pub(super) points: Vec<DVec3>,
}

/// What's highlighted: routes by index, boats by index into the fleet,
/// terminals by WSF id.
#[derive(Default)]
pub(super) struct Highlight {
    /// Fade what isn't highlighted (for a route).
    pub(super) dim: bool,
    pub(super) routes: Vec<usize>,
    /// The leg under the cursor, and the route it's part of (by index
    /// into `App::service_routes`).
    pub(super) leg: Option<usize>,
    pub(super) service: Option<usize>,
    pub(super) vessels: BTreeSet<usize>,
    pub(super) terminals: BTreeSet<u32>,
}

/// What the cursor is over, by index into the fleet or the terminals.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Picked {
    Vessel(usize),
    Terminal(usize),
}
