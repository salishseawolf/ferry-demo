//! The published schedule: each route's sailings for the trip day, from
//! WSF's schedule service, served as `/schedule?route=…`. Fetched for each
//! new trip day and again when WSF says its schedules changed.
//!
//! WSF lists a route's sailings per pair of terminals, so a boat calling
//! at two more terminals appears under both pairs; they're merged here
//! into one departure with its stops in order.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use ferry_model::api::{Departure, Loading, RouteSchedule, Stop, TerminalDepartures};
use sqlx::PgPool;

use crate::calendar::trip_date;
use crate::db;
use crate::http::AppState;
use crate::wsf::{self, DaySchedule, Wsf};

/// How often to look for a new trip day or a changed schedule.
const CHECK: Duration = Duration::from_secs(600);
/// After a failed load, try again this soon.
const RETRY: Duration = Duration::from_secs(60);
/// Which of our routes each of WSF's goes with: the smallest that calls
/// at all its terminals. WSF's without one are left out.
pub fn assign<'a>(
    wsf_routes: &[(u32, BTreeSet<u32>)],
    ours: &'a [(String, BTreeSet<u32>)],
) -> Vec<(u32, &'a str)> {
    wsf_routes
        .iter()
        .filter_map(|(id, terminals)| {
            ours.iter()
                .filter(|(_, ts)| !terminals.is_empty() && terminals.is_subset(ts))
                .min_by_key(|(_, ts)| ts.len())
                .map(|(route, _)| (*id, route.as_str()))
        })
        .collect()
}

/// A departure being merged from WSF's pairs.
struct Draft {
    departure: Departure,
    notes: Vec<String>,
}

/// Route `route`'s schedule from WSF's for it (one or more of WSF's
/// routes): each terminal's departures, a sailing listed under several
/// pairs made one, with its stops in the order it reaches them.
pub fn build(route: &str, trip_date: &str, days: &[DaySchedule]) -> RouteSchedule {
    let mut from: BTreeMap<u32, Vec<Draft>> = BTreeMap::new();
    for pair in days.iter().flat_map(|d| &d.pairs) {
        let drafts = from.entry(pair.from).or_default();
        for s in &pair.sailings {
            let notes = s.notes.iter().filter_map(|&i| pair.notes.get(i).cloned());
            let same = |d: &&mut Draft| {
                d.departure.departs_ms == s.departs_ms
                    && d.departure.vessel_id == s.vessel_id
                    && d.departure.vessel == s.vessel
            };
            let stop = Stop {
                terminal: pair.to,
                arrives_ms: s.arrives_ms,
            };
            match drafts.iter_mut().find(same) {
                Some(d) => {
                    if !d.departure.stops.iter().any(|t| t.terminal == pair.to) {
                        d.departure.stops.push(stop);
                    }
                    if d.departure.loading != s.loading {
                        d.departure.loading = Loading::Both;
                    }
                    d.notes.extend(notes);
                }
                None => drafts.push(Draft {
                    departure: Departure {
                        departs_ms: s.departs_ms,
                        vessel_id: s.vessel_id,
                        vessel: s.vessel.clone(),
                        stops: vec![stop],
                        loading: s.loading,
                        notes: Vec::new(),
                    },
                    notes: notes.collect(),
                }),
            }
        }
    }
    let terminals = from
        .into_iter()
        .map(|(terminal, mut drafts)| {
            drafts.sort_by_key(|d| d.departure.departs_ms);
            let mut notes: Vec<String> = Vec::new();
            let departures = drafts
                .into_iter()
                .map(|mut d| {
                    d.departure
                        .stops
                        .sort_by_key(|s| (s.arrives_ms.unwrap_or(i64::MAX), s.terminal));
                    let mut marks: Vec<u16> = d
                        .notes
                        .iter()
                        .map(|text| match notes.iter().position(|n| n == text) {
                            Some(i) => i as u16,
                            None => {
                                notes.push(text.clone());
                                (notes.len() - 1) as u16
                            }
                        })
                        .collect();
                    marks.sort_unstable();
                    marks.dedup();
                    d.departure.notes = marks;
                    d.departure
                })
                .collect();
            TerminalDepartures {
                terminal,
                departures,
                notes,
            }
        })
        .collect();
    let first = |f: fn(&DaySchedule) -> Option<&String>| days.iter().find_map(f).cloned();
    RouteSchedule {
        route: route.to_string(),
        trip_date: trip_date.to_string(),
        season: first(|d| Some(&d.season).filter(|s| !s.is_empty())).unwrap_or_default(),
        pdf_url: first(|d| d.pdf_url.as_ref()),
        terminals,
    }
}

/// What a load was of: the trip day, and WSF's schedule flush date then.
#[derive(Clone, PartialEq)]
struct Loaded {
    trip_date: String,
    flushed_ms: Option<i64>,
}

/// Fetches every route's schedule for `trip_date` and publishes them.
async fn load(pool: &PgPool, wsf: &Wsf, state: &AppState, trip_date: &str) -> Result<()> {
    let ours = db::route_terminals(pool).await?;
    if ours.is_empty() {
        bail!("the route network hasn't loaded yet");
    }
    let path = format!("schedule/rest/routes/{trip_date}");
    let ids = wsf::parse_schedule_routes(&wsf.get(&path).await?.body)?;
    let mut fetched = Vec::new();
    for id in ids {
        let path = format!("schedule/rest/schedule/{trip_date}/{id}");
        match wsf::parse_schedule(&wsf.get(&path).await?.body) {
            Ok(day) => fetched.push((id, day)),
            Err(e) => tracing::warn!(path, "couldn't read the schedule: {e:#}"),
        }
    }
    let terminals: Vec<(u32, BTreeSet<u32>)> = fetched
        .iter()
        .map(|(id, day)| {
            let ts = day.pairs.iter().flat_map(|p| [p.from, p.to]).collect();
            (*id, ts)
        })
        .collect();
    let mut by_route: HashMap<&str, Vec<DaySchedule>> = HashMap::new();
    let assigned: HashMap<u32, &str> = assign(&terminals, &ours).into_iter().collect();
    for (id, day) in fetched {
        match assigned.get(&id) {
            Some(route) => by_route.entry(route).or_default().push(day),
            None => tracing::debug!(wsf_route = id, "no route of ours for this schedule"),
        }
    }
    let schedules: HashMap<String, RouteSchedule> = by_route
        .into_iter()
        .map(|(route, days)| (route.to_string(), build(route, trip_date, &days)))
        .collect();
    tracing::info!(trip_date, routes = schedules.len(), "schedules loaded");
    state.set_schedules(schedules);
    Ok(())
}

/// Keeps the schedules current: loads them for each new trip day, and
/// again when WSF's schedule flush date moves.
pub fn spawn(pool: PgPool, wsf: Arc<Wsf>, state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut loaded: Option<Loaded> = None;
        loop {
            let flushed_ms = match wsf.get("schedule/rest/cacheflushdate").await {
                Ok(f) => wsf::parse_flush_date(&f.body),
                Err(e) => {
                    tracing::warn!("couldn't check the schedules: {e:#}");
                    None
                }
            };
            let want = Loaded {
                trip_date: trip_date(wsf::now_ms()),
                // Unknown (the check failed): as last time, so only a new
                // day reloads.
                flushed_ms: flushed_ms.or(loaded.as_ref().and_then(|l| l.flushed_ms)),
            };
            let wait = if loaded.as_ref() == Some(&want) {
                CHECK
            } else {
                match load(&pool, &wsf, &state, &want.trip_date).await {
                    Ok(()) => {
                        loaded = Some(want);
                        CHECK
                    }
                    Err(e) => {
                        tracing::warn!("schedules not loaded: {e:#}; retrying in {RETRY:?}");
                        RETRY
                    }
                }
            };
            tokio::time::sleep(wait).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wsf::{PairSailing, PairSailings};

    const MIN_MS: i64 = 60_000;

    #[test]
    fn matches_wsf_routes_to_ours() {
        let set = |ids: &[u32]| ids.iter().copied().collect::<BTreeSet<u32>>();
        let ours = vec![
            (
                "anacortes-san-juan-islands".to_string(),
                set(&[1, 10, 13, 15, 18]),
            ),
            (
                "fauntleroy-vashon-southworth".to_string(),
                set(&[9, 20, 22]),
            ),
            ("seattle-bremerton".to_string(), set(&[7, 4])),
        ];
        let wsf = [
            (9, set(&[1, 10, 13, 15, 18])),
            (13, set(&[9, 22, 20])),
            // The inter-island route goes with the islands' route.
            (20, set(&[10, 13, 15, 18])),
            (99, set(&[7, 3])),
        ];
        assert_eq!(
            assign(&wsf, &ours),
            [
                (9, "anacortes-san-juan-islands"),
                (13, "fauntleroy-vashon-southworth"),
                (20, "anacortes-san-juan-islands")
            ]
        );
    }

    fn sailing(departs_min: i64, arrives_min: i64, vessel: &str, notes: Vec<usize>) -> PairSailing {
        PairSailing {
            departs_ms: departs_min * MIN_MS,
            arrives_ms: Some(arrives_min * MIN_MS),
            vessel_id: Some(vessel.len() as u32),
            vessel: vessel.to_string(),
            loading: Loading::Both,
            notes,
        }
    }

    #[test]
    fn merges_a_sailing_listed_under_several_pairs() {
        // Fauntleroy (9) to Vashon (22) and to Southworth (20): Kitsap's
        // 3:50 calls at Southworth first, as its Vashon footnote says.
        let via = "Via Southworth, crossing time 45 minutes".to_string();
        let day = DaySchedule {
            season: "Fall 2026".into(),
            pdf_url: None,
            pairs: vec![
                PairSailings {
                    from: 9,
                    to: 22,
                    notes: vec![via.clone()],
                    sailings: vec![
                        sailing(950, 995, "Kitsap", vec![0]),
                        sailing(905, 925, "Cathlamet", vec![]),
                    ],
                },
                PairSailings {
                    from: 9,
                    to: 20,
                    notes: vec![],
                    sailings: vec![sailing(950, 975, "Kitsap", vec![])],
                },
                PairSailings {
                    from: 22,
                    to: 9,
                    notes: vec![],
                    sailings: vec![sailing(930, 950, "Cathlamet", vec![])],
                },
            ],
        };
        let s = build("fauntleroy-vashon-southworth", "2026-10-30", &[day]);
        assert_eq!(s.season, "Fall 2026");
        let fauntleroy = &s.terminals[0];
        assert_eq!(fauntleroy.terminal, 9);
        assert_eq!(fauntleroy.notes, [via]);
        let times: Vec<(i64, &str, Vec<u32>, Vec<u16>)> = fauntleroy
            .departures
            .iter()
            .map(|d| {
                let stops = d.stops.iter().map(|s| s.terminal).collect();
                (
                    d.departs_ms / MIN_MS,
                    d.vessel.as_str(),
                    stops,
                    d.notes.clone(),
                )
            })
            .collect();
        assert_eq!(
            times,
            [
                (905, "Cathlamet", vec![22], vec![]),
                (950, "Kitsap", vec![20, 22], vec![0])
            ]
        );
        assert_eq!(s.terminals[1].terminal, 22);
    }
}
