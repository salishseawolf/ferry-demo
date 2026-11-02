//! What shows in the map's place when the browser can't draw it (no
//! WebGPU, no usable GPU, or the GPU lost): the routes riders know, each
//! with the boats on it now, every one a button that brings it up in the
//! finder as a click on the map would.

use crate::fleet::{Fleet, Vessel};
use crate::legend::escape;
use crate::routes::ServiceRoute;

/// The list's HTML at `now_ms`, with times written by `clock`. Each
/// in-service boat goes under the first route serving both its ends (or
/// its one end, before it has a destination); boats no route claims come
/// after, and boats out of service last, by name only.
pub fn html(
    fleet: &Fleet,
    routes: &[ServiceRoute],
    now_ms: f64,
    clock: impl Fn(f64) -> String,
) -> String {
    let mut boats: Vec<&Vessel> = fleet.vessels.iter().collect();
    boats.sort_by(|a, b| a.name.cmp(&b.name));
    let (serving, laid_up): (Vec<&Vessel>, Vec<&Vessel>) =
        boats.into_iter().partition(|v| v.in_service);
    let route_of = |v: &Vessel| {
        let from = v.departing_id?;
        routes.iter().position(|r| {
            r.terminals.contains(&from) && v.arriving_id.is_none_or(|to| r.terminals.contains(&to))
        })
    };
    let boat = |v: &Vessel| {
        let lines = v.describe(now_ms, &clock);
        let (first, rest) = lines
            .split_first()
            .map_or(("", &[][..]), |(f, r)| (f.as_str(), r));
        let rest: String = rest
            .iter()
            .map(|l| format!("<span>{}</span>", escape(l)))
            .collect();
        format!(
            "<li><button type=\"button\" data-vessel=\"{}\"><b>{}</b>{rest}</button></li>",
            v.id,
            escape(first)
        )
    };
    let mut out = String::new();
    let mut claimed = vec![false; serving.len()];
    for (r, route) in routes.iter().enumerate() {
        let on: String = (serving.iter().enumerate())
            .filter(|(_, v)| route_of(v) == Some(r))
            .map(|(i, v)| {
                claimed[i] = true;
                boat(v)
            })
            .collect();
        let on = if on.is_empty() {
            "<li class=\"none\">No boat on this route now.</li>".to_string()
        } else {
            on
        };
        out += &format!(
            "<section class=\"paper\"><h3><button type=\"button\" data-route=\"{r}\">{}</button></h3><ul>{on}</ul></section>",
            escape(&route.name)
        );
    }
    let others: String = (serving.iter().zip(&claimed))
        .filter(|(_, claimed)| !**claimed)
        .map(|(v, _)| boat(v))
        .collect();
    if !others.is_empty() {
        out += &format!("<section class=\"paper\"><h3>Other boats</h3><ul>{others}</ul></section>");
    }
    if !laid_up.is_empty() {
        let names: Vec<String> = laid_up.iter().map(|v| escape(&v.name)).collect();
        out += &format!(
            "<p class=\"laid-up\">Out of service: {}.</p>",
            names.join(", ")
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::geo::LonLat;

    fn boat(id: u32, name: &str, ends: (Option<u32>, Option<u32>), in_service: bool) -> Vessel {
        Vessel {
            id,
            name: name.into(),
            position: LonLat::new(-122.4, 47.6),
            heading_deg: 0.0,
            speed_kn: 0.0,
            at_dock: true,
            in_service,
            departing: ends.0.map(|t| format!("T{t}")),
            arriving: ends.1.map(|t| format!("T{t}")),
            departing_id: ends.0,
            arriving_id: ends.1,
            scheduled_departure_ms: None,
            left_dock_ms: None,
            eta_ms: None,
            reported_ms: 0.0,
            note: None,
            wake: Vec::new(),
        }
    }

    fn route(name: &str, terminals: &[u32]) -> ServiceRoute {
        ServiceRoute {
            name: name.into(),
            legs: Vec::new(),
            terminals: terminals.iter().copied().collect::<BTreeSet<_>>(),
        }
    }

    #[test]
    fn lists_boats_under_their_routes() {
        let fleet = Fleet {
            vessels: vec![
                boat(1, "Wenatchee", (Some(3), Some(7)), true),
                boat(2, "Tacoma", (Some(7), None), true),
                boat(3, "Issaquah", (None, None), false),
                boat(4, "Chelan", (Some(9), Some(10)), true),
            ],
            ..Default::default()
        };
        let routes = [
            route("Seattle / Bainbridge", &[3, 7]),
            route("Fauntleroy / Vashon", &[20, 21]),
        ];
        let html = html(&fleet, &routes, 0.0, |_| "12:00".into());
        let bainbridge = html.find("Seattle / Bainbridge").unwrap();
        let vashon = html.find("Fauntleroy / Vashon").unwrap();
        // Both boats under their route, by name; the other route empty.
        let (tacoma, wenatchee) = (
            html.find("Tacoma").unwrap(),
            html.find("Wenatchee").unwrap(),
        );
        assert!(bainbridge < tacoma && tacoma < wenatchee && wenatchee < vashon);
        assert!(html[vashon..].contains("No boat on this route now."));
        // A boat no route claims, then the ones out of service.
        let others = html.find("Other boats").unwrap();
        assert!(others < html.find("Chelan").unwrap());
        assert!(html.ends_with("<p class=\"laid-up\">Out of service: Issaquah.</p>"));
        assert!(html.contains("data-vessel=\"1\"") && html.contains("data-route=\"1\""));
    }

    #[test]
    fn escapes_names() {
        let fleet = Fleet {
            vessels: vec![boat(1, "<b>", (Some(3), None), true)],
            ..Default::default()
        };
        let html = html(&fleet, &[route("A & B", &[3])], 0.0, |_| String::new());
        assert!(html.contains("A &amp; B") && html.contains("&lt;b&gt;"));
        assert!(!html.contains("<b><b>"));
    }
}
