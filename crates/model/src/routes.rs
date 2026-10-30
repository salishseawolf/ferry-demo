//! Ferry routes: cubic Béziers fitted to OpenStreetMap's WSF lines by
//! tools/routes/osm_routes.py, sampled into paths to draw; the legs
//! between terminals they make, to find a boat's way along them; and the
//! routes riders know, which group those legs.

use std::collections::BTreeSet;

use glam::DVec2;
use serde::Deserialize;

use crate::geo::{EARTH_RADIUS_M, LonLat, distance_m};
use crate::terminal::Terminal;

/// A route's end reaches the terminal nearest it within this distance (m).
const END_NEAR_M: f64 = 1500.0;

/// Points sampled along each curve (the fit keeps each to a few km).
const SAMPLES_PER_CURVE: usize = 12;

/// ferry-routes.json as the pipeline writes it.
#[derive(Deserialize)]
pub struct RouteFile {
    pub attribution: String,
    /// Routes that serve more than two terminals, by name.
    #[serde(default)]
    pub services: Vec<Service>,
    pub routes: Vec<Route>,
}

/// A route as riders know it, from the pipeline's table: its name and
/// its terminals, named as WSF's terminals service names them.
#[derive(Clone, Debug, Deserialize)]
pub struct Service {
    pub name: String,
    pub terminals: Vec<String>,
}

/// A route as riders know it, worked out against the terminals: the
/// legs (by index) it's made of, slip approaches included, and the
/// terminals (by WSF id) it serves.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceRoute {
    pub name: String,
    pub legs: Vec<usize>,
    pub terminals: BTreeSet<u32>,
}

/// Groups `legs` into the routes riders know: a leg between two of a
/// `services` route's terminals joins it, any other leg is a route of its
/// own two terminals (named "Seattle / Bremerton", as WSF names routes),
/// and slip approaches join every route serving their terminal. Legs that
/// reach no terminal are left out.
pub fn group(legs: &[Leg], services: &[Service], terminals: &[Terminal]) -> Vec<ServiceRoute> {
    let name_of = |id: u32| {
        terminals
            .iter()
            .find(|t| t.id == id)
            .map_or_else(|| id.to_string(), |t| t.name.clone())
    };
    let mut routes: Vec<ServiceRoute> = services
        .iter()
        .map(|s| ServiceRoute {
            name: s.name.clone(),
            legs: Vec::new(),
            terminals: terminals
                .iter()
                .filter(|t| s.terminals.iter().any(|n| n.eq_ignore_ascii_case(&t.name)))
                .map(|t| t.id)
                .collect(),
        })
        .collect();
    for (i, leg) in legs.iter().enumerate() {
        let Some((a, b)) = leg.joins() else {
            continue;
        };
        let serves = |r: &ServiceRoute| r.terminals.contains(&a) && r.terminals.contains(&b);
        match routes.iter_mut().find(|r| serves(r)) {
            Some(route) => route.legs.push(i),
            None => routes.push(ServiceRoute {
                name: format!("{} / {}", name_of(a), name_of(b)),
                legs: vec![i],
                terminals: BTreeSet::from([a, b]),
            }),
        }
    }
    routes.retain(|r| !r.legs.is_empty());
    for (i, leg) in legs.iter().enumerate() {
        if let Some((a, a2)) = leg.ends
            && a == a2
        {
            for route in routes.iter_mut().filter(|r| r.terminals.contains(&a)) {
                route.legs.push(i);
            }
        }
    }
    routes
}

#[derive(Deserialize)]
pub struct Route {
    /// The OpenStreetMap way it was fitted to.
    #[serde(default)]
    pub way: u64,
    pub name: String,
    /// Cubic Béziers, each four [lon, lat] control points, end to end.
    pub curves: Vec<[[f64; 2]; 4]>,
}

pub fn parse(json: &str) -> Result<RouteFile, String> {
    serde_json::from_str(json).map_err(|e| e.to_string())
}

impl Route {
    /// The route as one path of points along its curves. The curves were
    /// fitted in a local plane that's an affine map of lon/lat, so they
    /// are evaluated in lon/lat directly.
    pub fn path(&self) -> Vec<LonLat> {
        let mut points = Vec::with_capacity(self.curves.len() * SAMPLES_PER_CURVE + 1);
        for (i, c) in self.curves.iter().enumerate() {
            let c = c.map(DVec2::from);
            // Each curve starts where the last ended: skip the repeat.
            let first = usize::from(i > 0);
            for s in first..=SAMPLES_PER_CURVE {
                let p = bezier(&c, s as f64 / SAMPLES_PER_CURVE as f64);
                points.push(LonLat::new(p.x, p.y));
            }
        }
        points
    }
}

/// A route between two terminals: OSM draws each leg of WSF's routes on
/// its own (Anacortes to Lopez, Lopez to Shaw, ...), and a few short
/// approaches into a terminal's slips, which start and end at it.
#[derive(Clone, Debug, PartialEq)]
pub struct Leg {
    /// The terminals at its ends (by WSF id), if both reach one.
    pub ends: Option<(u32, u32)>,
    pub length_m: f64,
}

impl Leg {
    /// The leg along `path`, between the terminals nearest its ends.
    pub fn new(path: &[LonLat], terminals: &[Terminal]) -> Self {
        let near = |p: LonLat| {
            terminals
                .iter()
                .map(|t| (t.id, distance_m(p, t.position, EARTH_RADIUS_M)))
                .filter(|&(_, d)| d <= END_NEAR_M)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(id, _)| id)
        };
        let ends = match (path.first(), path.last()) {
            (Some(&a), Some(&b)) => near(a).zip(near(b)),
            _ => None,
        };
        let length_m = path
            .windows(2)
            .map(|w| distance_m(w[0], w[1], EARTH_RADIUS_M))
            .sum();
        Self { ends, length_m }
    }

    /// The two terminals it joins, if it does (rather than approaching
    /// one).
    pub fn joins(&self) -> Option<(u32, u32)> {
        self.ends.filter(|(a, b)| a != b)
    }
}

/// A terminal reached: its id, how far (m), and the terminal and leg it
/// was reached by.
type Reached = (u32, f64, Option<(u32, usize)>);

/// The legs (by index) a boat from terminal `from` to `to` sails: the
/// direct leg if there is one, else the shortest chain of legs through
/// other terminals, with the approaches into each terminal on the way.
/// Empty if the legs don't connect them.
pub fn legs_between(legs: &[Leg], from: u32, to: u32) -> Vec<usize> {
    // Dijkstra over terminals; the network is a few dozen legs at most.
    let mut best: Vec<Reached> = vec![(from, 0.0, None)];
    let mut done: BTreeSet<u32> = BTreeSet::new();
    loop {
        let next = best
            .iter()
            .filter(|(t, ..)| !done.contains(t))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .copied();
        let Some((here, dist, _)) = next else {
            return Vec::new();
        };
        if here == to {
            break;
        }
        done.insert(here);
        for (i, leg) in legs.iter().enumerate() {
            let Some((a, b)) = leg.joins() else {
                continue;
            };
            let there = match here {
                h if h == a => b,
                h if h == b => a,
                _ => continue,
            };
            let d = dist + leg.length_m;
            match best.iter_mut().find(|(t, ..)| *t == there) {
                Some(entry) if entry.1 <= d => {}
                Some(entry) => *entry = (there, d, Some((here, i))),
                None => best.push((there, d, Some((here, i)))),
            }
        }
    }
    let mut chain = Vec::new();
    let mut stops = vec![to];
    let mut at = to;
    while let Some(&(_, _, Some((prev, leg)))) = best.iter().find(|(t, ..)| *t == at) {
        chain.push(leg);
        stops.push(prev);
        at = prev;
    }
    chain.reverse();
    chain.extend(legs.iter().enumerate().filter_map(|(i, leg)| {
        let approach = leg.ends.is_some_and(|(a, b)| a == b && stops.contains(&a));
        approach.then_some(i)
    }));
    chain
}

fn bezier(c: &[DVec2; 4], t: f64) -> DVec2 {
    let s = 1.0 - t;
    c[0] * (s * s * s) + c[1] * (3.0 * s * s * t) + c[2] * (3.0 * s * t * t) + c[3] * (t * t * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"{"attribution":"Ferry routes © OpenStreetMap contributors (ODbL)",
      "fetched":"2026-10-29","routes":[{"way":1,"name":"Here ↔ There","curves":[
        [[-122.5,47.6],[-122.49,47.6],[-122.48,47.6],[-122.47,47.6]],
        [[-122.47,47.6],[-122.46,47.6],[-122.45,47.61],[-122.44,47.62]]]}]}"#;

    #[test]
    fn samples_curves_end_to_end() {
        let file = parse(FILE).unwrap();
        assert!(file.attribution.contains("OpenStreetMap"));
        let path = file.routes[0].path();
        // Two curves share their joint: 2 × 12 segments, 25 points.
        assert_eq!(path.len(), 2 * SAMPLES_PER_CURVE + 1);
        assert_eq!(path[0], LonLat::new(-122.5, 47.6));
        assert_eq!(path[SAMPLES_PER_CURVE], LonLat::new(-122.47, 47.6));
        assert_eq!(*path.last().unwrap(), LonLat::new(-122.44, 47.62));
        // The straight first curve stays on its line, evenly spaced.
        let mid = path[SAMPLES_PER_CURVE / 2];
        assert!((mid.lon + 122.485).abs() < 1e-9 && (mid.lat - 47.6).abs() < 1e-12);
        assert!(parse("[]").is_err());
    }

    fn terminal(id: u32, lon: f64, lat: f64) -> Terminal {
        Terminal {
            id,
            name: format!("T{id}"),
            position: LonLat::new(lon, lat),
        }
    }

    fn leg(a: u32, b: u32, length_m: f64) -> Leg {
        Leg {
            ends: Some((a, b)),
            length_m,
        }
    }

    #[test]
    fn legs_reach_the_terminals_at_their_ends() {
        let terminals = [terminal(1, -122.677, 48.507), terminal(2, -123.014, 48.536)];
        let path = [
            LonLat::new(-122.6775, 48.5074),
            LonLat::new(-122.85, 48.52),
            LonLat::new(-123.0139, 48.5357),
        ];
        let leg = Leg::new(&path, &terminals);
        assert_eq!(leg.ends, Some((1, 2)));
        assert!((24_000.0..30_000.0).contains(&leg.length_m));
        // An end out in the water reaches no terminal.
        assert_eq!(Leg::new(&path[..2], &terminals).ends, None);
    }

    #[test]
    fn boats_follow_direct_legs_or_the_shortest_chain() {
        // Anacortes 1, Lopez 2, Shaw 3, Orcas 4, Friday Harbor 5, with an
        // approach into Anacortes and a leg that reaches no terminal.
        let legs = [
            leg(1, 2, 15_000.0),
            leg(2, 3, 8_000.0),
            leg(1, 3, 20_000.0),
            leg(3, 4, 2_000.0),
            leg(1, 5, 40_000.0),
            leg(1, 1, 200.0),
            Leg {
                ends: None,
                length_m: 1.0,
            },
        ];
        assert_eq!(legs_between(&legs, 1, 5), [4, 5]);
        assert_eq!(legs_between(&legs, 5, 1), [4, 5]);
        // Anacortes to Orcas: via Shaw directly beats via Lopez.
        assert_eq!(legs_between(&legs, 1, 4), [2, 3, 5]);
        assert_eq!(legs_between(&legs, 3, 4), [3]);
        assert!(legs_between(&legs, 4, 9).is_empty());
    }

    #[test]
    fn groups_legs_into_the_routes_riders_know() {
        let terminals = [
            terminal(9, -122.396, 47.523),
            terminal(22, -122.464, 47.511),
            terminal(20, -122.496, 47.513),
            terminal(7, -122.340, 47.602),
            terminal(4, -122.623, 47.561),
            terminal(1, -122.677, 48.507),
        ]
        .map(|mut t| {
            t.name = match t.id {
                9 => "Fauntleroy",
                22 => "Vashon Island",
                20 => "Southworth",
                7 => "Seattle",
                4 => "Bremerton",
                _ => "Anacortes",
            }
            .to_string();
            t
        });
        let services = [Service {
            name: "Fauntleroy / Vashon / Southworth".into(),
            terminals: vec![
                "Fauntleroy".into(),
                "vashon island".into(),
                "Southworth".into(),
            ],
        }];
        let legs = [
            leg(9, 20, 1.0),
            leg(7, 4, 1.0),
            leg(9, 22, 1.0),
            leg(22, 20, 1.0),
            leg(20, 20, 1.0),
            leg(1, 1, 1.0),
            Leg {
                ends: None,
                length_m: 1.0,
            },
        ];
        let routes = group(&legs, &services, &terminals);
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].name, "Fauntleroy / Vashon / Southworth");
        assert_eq!(routes[0].legs, [0, 2, 3, 4]);
        assert_eq!(routes[0].terminals, BTreeSet::from([9, 20, 22]));
        assert_eq!(routes[1].name, "Seattle / Bremerton");
        assert_eq!(routes[1].legs, [1]);
    }
}
