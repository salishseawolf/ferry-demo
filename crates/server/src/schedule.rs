//! The published schedule: each route's sailings for the trip day and the
//! next, from WSF's schedule service, served as `/schedule?route=…`.
//! Fetched for each new trip day, and again when WSF says its schedules
//! changed: at every look for the trip day, where boats get swapped within
//! minutes and the changes are what `published` is for, and at most hourly
//! for the next. Days further ahead are fetched once a trip day
//! only to be recorded, so that a day's history (see `published`) starts
//! near when WSF first published it.
//!
//! A route with nothing published for a day, whether for good or for now,
//! gets that day with no departures. A day that couldn't be fetched isn't
//! taken for one with no sailings: the last copy stands, or it's left out.
//!
//! WSF lists a route's sailings per pair of terminals, so a boat calling
//! at two more terminals appears under both pairs; they're merged here
//! into one departure with its stops in order.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use ferry_model::api::{Departure, Loading, RouteSchedule, ScheduleDay, Stop, TerminalDepartures};
use sqlx::PgPool;

use crate::calendar::{trip_date, trip_date_after};
use crate::db;
use crate::http::AppState;
use crate::published::{self, routes_path, schedule_path};
use crate::wsf::{self, DaySchedule, Wsf};

/// How often to look for a new trip day or a changed schedule.
const CHECK: Duration = Duration::from_secs(600);
/// After a failed load, try again this soon.
const RETRY: Duration = Duration::from_secs(60);
/// The soonest a moved flush date reloads the next day's schedules again:
/// it moves every few minutes, mostly without the schedules changing, and
/// each load is a request per route.
const NEXT_DAY_RELOAD_MS: i64 = 3_600_000;
/// How many trip days ahead are recorded (see the top of this file).
const LOOK_AHEAD_DAYS: i64 = 14;
/// A pause between days when looking ahead, to go easy on WSF.
const LOOK_AHEAD_PAUSE: Duration = Duration::from_secs(1);
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

/// A route's day from WSF's schedules for it (one or more of WSF's
/// routes): each terminal's departures, a sailing listed under several
/// pairs made one, with its stops in the order it reaches them.
pub fn build(trip_date: &str, days: &[DaySchedule]) -> ScheduleDay {
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
    ScheduleDay {
        trip_date: trip_date.to_string(),
        season: first(|d| Some(&d.season).filter(|s| !s.is_empty())).unwrap_or_default(),
        pdf_url: first(|d| d.pdf_url.as_ref()),
        terminals,
    }
}

/// What a day's last load was of: the trip day, and WSF's schedule flush
/// date then.
#[derive(Clone, Debug, PartialEq)]
struct Loaded {
    trip_date: String,
    flushed_ms: Option<i64>,
    /// When it was loaded.
    at_ms: i64,
    /// Whether all its routes came through.
    complete: bool,
}

/// Whether to load the schedules for `trip_date`, WSF's flush date being
/// `flushed_ms`, at `now_ms`: the first time, once that's another day, after
/// a load with parts missing, and when the flush date has moved, but not
/// within `reload_ms` of the last load.
fn due(
    loaded: Option<&Loaded>,
    trip_date: &str,
    flushed_ms: Option<i64>,
    now_ms: i64,
    reload_ms: i64,
) -> bool {
    match loaded {
        None => true,
        Some(l) if l.trip_date != trip_date || !l.complete => true,
        Some(l) => l.flushed_ms != flushed_ms && now_ms - l.at_ms >= reload_ms,
    }
}

/// WSF's answers for a trip day: its routes' schedules, and whether all
/// came through (if not, what's missing isn't known to be absent).
struct DayAnswers {
    schedules: Vec<(u32, DaySchedule)>,
    complete: bool,
}

impl DayAnswers {
    /// For a day not fetched this time: what's held of it stands.
    fn not_fetched() -> Self {
        Self {
            schedules: Vec::new(),
            complete: false,
        }
    }
}

/// Fetches a trip day's schedules and records them in the history. WSF
/// listing no routes at all is taken for a fault rather than the whole
/// system stopping, so it says nothing about the routes it had.
async fn fetch_day(pool: &PgPool, wsf: &Wsf, trip_date: &str) -> DayAnswers {
    let path = routes_path(trip_date);
    let listed = wsf.get(&path).await.and_then(|f| {
        let ids = wsf::parse_schedule_routes(&f.body)?;
        Ok((ids, f.fetched_ms))
    });
    let ids = match listed {
        Ok((ids, _)) if ids.is_empty() => {
            tracing::warn!(trip_date, "WSF lists no routes for this day");
            Vec::new()
        }
        Ok((ids, seen_ms)) => {
            if let Err(e) = published::record_routes(pool, trip_date, &ids, seen_ms).await {
                tracing::warn!(trip_date, "couldn't record the routes: {e:#}");
            }
            ids
        }
        Err(e) => {
            tracing::warn!(path, "couldn't fetch the routes: {e:#}");
            Vec::new()
        }
    };
    let mut answers = DayAnswers {
        schedules: Vec::new(),
        complete: !ids.is_empty(),
    };
    for id in ids {
        let path = schedule_path(trip_date, id);
        let schedule = wsf.get(&path).await.and_then(|f| {
            let day = wsf::parse_schedule(&f.body)?;
            Ok((day, f.fetched_ms))
        });
        match schedule {
            Ok((day, seen_ms)) => {
                if let Err(e) = published::record_schedule(pool, trip_date, id, &day, seen_ms).await
                {
                    tracing::warn!(path, "couldn't record the schedule: {e:#}");
                }
                answers.schedules.push((id, day));
            }
            Err(e) => {
                tracing::warn!(path, "couldn't fetch the schedule: {e:#}");
                answers.complete = false;
            }
        }
    }
    answers
}

/// Each of our routes' days, from WSF's answers for each trip day. Where a
/// day's answers are incomplete, a route with none of its own keeps the
/// copy in `held` if there is one, or goes without that day.
fn assemble(
    ours: &[(String, BTreeSet<u32>)],
    loaded: Vec<(String, DayAnswers)>,
    held: Option<&HashMap<String, RouteSchedule>>,
) -> HashMap<String, RouteSchedule> {
    let mut schedules: HashMap<String, RouteSchedule> = ours
        .iter()
        .map(|(route, _)| {
            let schedule = RouteSchedule {
                route: route.clone(),
                days: Vec::new(),
            };
            (route.clone(), schedule)
        })
        .collect();
    for (trip_date, answers) in loaded {
        let terminals: Vec<(u32, BTreeSet<u32>)> = answers
            .schedules
            .iter()
            .map(|(id, day)| {
                let ts = day.pairs.iter().flat_map(|p| [p.from, p.to]).collect();
                (*id, ts)
            })
            .collect();
        let assigned: HashMap<u32, &str> = assign(&terminals, ours).into_iter().collect();
        let mut by_route: HashMap<&str, Vec<DaySchedule>> = HashMap::new();
        for (id, day) in answers.schedules {
            match assigned.get(&id) {
                Some(route) => by_route.entry(route).or_default().push(day),
                None => tracing::debug!(wsf_route = id, "no route of ours for this schedule"),
            }
        }
        for (route, _) in ours {
            let fetched = by_route.remove(route.as_str());
            let copy = held
                .and_then(|h| h.get(route))
                .and_then(|s| s.days.iter().find(|d| d.trip_date == trip_date));
            let day = match (answers.complete, fetched, copy) {
                (true, Some(days), _) | (false, Some(days), None) => Some(build(&trip_date, &days)),
                (true, None, _) => Some(build(&trip_date, &[])),
                (false, _, Some(copy)) => Some(copy.clone()),
                (false, None, None) => None,
            };
            if let (Some(day), Some(schedule)) = (day, schedules.get_mut(route)) {
                schedule.days.push(day);
            }
        }
    }
    schedules
}

/// Fetches the trip day's schedules, the next day's, or both as asked, and
/// publishes them with what's held of the other. Whether each fetched came
/// through whole: (the trip day, the next).
async fn load(
    pool: &PgPool,
    wsf: &Wsf,
    state: &AppState,
    today: &str,
    fetch_today: bool,
    tomorrow: &str,
    fetch_tomorrow: bool,
) -> Result<(bool, bool)> {
    let ours = db::route_terminals(pool).await?;
    if ours.is_empty() {
        bail!("the route network hasn't loaded yet");
    }
    let fetch = async |date, wanted| {
        if wanted {
            fetch_day(pool, wsf, date).await
        } else {
            DayAnswers::not_fetched()
        }
    };
    let first = fetch(today, fetch_today).await;
    let second = fetch(tomorrow, fetch_tomorrow).await;
    let whole = (
        !fetch_today || first.complete,
        !fetch_tomorrow || second.complete,
    );
    let loaded = vec![(today.to_string(), first), (tomorrow.to_string(), second)];
    let held = state.route_schedules();
    let schedules = assemble(&ours, loaded, held.as_deref());
    if schedules.values().all(|s| s.days.is_empty()) {
        bail!("no schedule could be fetched");
    }
    if held.as_deref() == Some(&schedules) {
        tracing::debug!(today, "schedules unchanged");
    } else {
        tracing::info!(today, routes = schedules.len(), "schedules loaded");
        state.set_schedules(schedules);
    }
    Ok(whole)
}

/// Records the days beyond the next, for their history.
async fn look_ahead(pool: &PgPool, wsf: &Wsf, now_ms: i64) {
    for days in 2..=LOOK_AHEAD_DAYS {
        fetch_day(pool, wsf, &trip_date_after(now_ms, days)).await;
        tokio::time::sleep(LOOK_AHEAD_PAUSE).await;
    }
}

/// Keeps the schedules current: loads each day's for each new trip day,
/// and again when WSF's schedule flush date moves (see `due`).
pub fn spawn(pool: PgPool, wsf: Arc<Wsf>, state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut today_loaded: Option<Loaded> = None;
        let mut tomorrow_loaded: Option<Loaded> = None;
        let mut looked_ahead: Option<String> = None;
        loop {
            let flushed_ms = match wsf.get("schedule/rest/cacheflushdate").await {
                Ok(f) => wsf::parse_flush_date(&f.body),
                Err(e) => {
                    tracing::warn!("couldn't check the schedules: {e:#}");
                    None
                }
            };
            let now_ms = wsf::now_ms();
            let today = trip_date(now_ms);
            let tomorrow = trip_date_after(now_ms, 1);
            // Unknown (the check failed): as last time, so only a new day
            // reloads.
            let flushed_ms = flushed_ms.or(today_loaded.as_ref().and_then(|l| l.flushed_ms));
            let fetch_today = due(today_loaded.as_ref(), &today, flushed_ms, now_ms, 0);
            let fetch_tomorrow = due(
                tomorrow_loaded.as_ref(),
                &tomorrow,
                flushed_ms,
                now_ms,
                NEXT_DAY_RELOAD_MS,
            );
            let wait = if !fetch_today && !fetch_tomorrow {
                CHECK
            } else {
                match load(
                    &pool,
                    &wsf,
                    &state,
                    &today,
                    fetch_today,
                    &tomorrow,
                    fetch_tomorrow,
                )
                .await
                {
                    Ok((today_whole, tomorrow_whole)) => {
                        let mark = |trip_date: &str, complete: bool| Loaded {
                            trip_date: trip_date.to_string(),
                            flushed_ms,
                            at_ms: now_ms,
                            complete,
                        };
                        if fetch_today {
                            today_loaded = Some(mark(&today, today_whole));
                        }
                        if fetch_tomorrow {
                            tomorrow_loaded = Some(mark(&tomorrow, tomorrow_whole));
                        }
                        if today_whole { CHECK } else { RETRY }
                    }
                    Err(e) => {
                        tracing::warn!("schedules not loaded: {e:#}; retrying in {RETRY:?}");
                        RETRY
                    }
                }
            };
            if wait == CHECK && !wsf.is_replay() && looked_ahead.as_ref() != Some(&today) {
                look_ahead(&pool, &wsf, now_ms).await;
                looked_ahead = Some(today);
            }
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
    fn reloads_for_a_new_day_and_when_the_flush_date_moves() {
        let loaded = Loaded {
            trip_date: "2026-10-30".to_string(),
            flushed_ms: Some(1_000),
            at_ms: 0,
            complete: true,
        };
        let at = |min: i64| min * MIN_MS;
        let due_now = |l: Option<&Loaded>, date: &str, flushed: i64, min: i64, reload: i64| {
            due(l, date, Some(flushed), at(min), reload)
        };
        const DAY: &str = "2026-10-30";
        assert!(due(None, DAY, None, 0, 0));
        // Nothing moved: not however long it's been, with or without a limit.
        assert!(!due_now(Some(&loaded), DAY, 1_000, 600, 0));
        assert!(!due_now(Some(&loaded), DAY, 1_000, 600, NEXT_DAY_RELOAD_MS));
        // The flush date moved: the trip day's at the next look...
        assert!(due_now(Some(&loaded), DAY, 2_000, 10, 0));
        // ... the next day's not within the hour, then yes.
        assert!(!due_now(Some(&loaded), DAY, 2_000, 10, NEXT_DAY_RELOAD_MS));
        assert!(due_now(Some(&loaded), DAY, 2_000, 60, NEXT_DAY_RELOAD_MS));
        // Another day at once, whatever the flush date or the limit.
        assert!(due_now(
            Some(&loaded),
            "2026-10-31",
            1_000,
            10,
            NEXT_DAY_RELOAD_MS
        ));
        // Parts missing: at the next look, whatever else.
        let partial = Loaded {
            complete: false,
            ..loaded
        };
        assert!(due_now(Some(&partial), DAY, 1_000, 10, NEXT_DAY_RELOAD_MS));
    }

    /// One WSF route's schedule, 8 to 12.
    fn answer(id: u32, vessel: &str) -> (u32, DaySchedule) {
        let day = DaySchedule {
            season: "Fall 2026".into(),
            pdf_url: None,
            pairs: vec![PairSailings {
                from: 8,
                to: 12,
                notes: vec![],
                sailings: vec![sailing(600, 630, vessel, vec![])],
            }],
        };
        (id, day)
    }

    #[test]
    fn tells_no_sailings_from_not_knowing() {
        const TODAY: &str = "2026-10-30";
        const NEXT: &str = "2026-10-31";
        let set = |ids: &[u32]| ids.iter().copied().collect::<BTreeSet<u32>>();
        let ours = vec![
            ("edmonds-kingston".to_string(), set(&[8, 12])),
            ("seattle-bremerton".to_string(), set(&[7, 4])),
        ];
        let answers = |schedules, complete| DayAnswers {
            schedules,
            complete,
        };
        let days = |s: &HashMap<String, RouteSchedule>, route: &str| -> Vec<(String, usize)> {
            s[route]
                .days
                .iter()
                .map(|d| (d.trip_date.clone(), d.terminals.len()))
                .collect()
        };
        // WSF answered in full and lists nothing for Bremerton: it has the
        // days, with no sailings. The next day's fetch failed, and nothing's
        // held: that day is left out for both.
        let first = assemble(
            &ours,
            vec![
                (TODAY.to_string(), answers(vec![answer(6, "Spokane")], true)),
                (NEXT.to_string(), answers(vec![], false)),
            ],
            None,
        );
        assert_eq!(days(&first, "edmonds-kingston"), [(TODAY.to_string(), 1)]);
        assert_eq!(days(&first, "seattle-bremerton"), [(TODAY.to_string(), 0)]);
        // Today's fetch fails later on: the copy stands rather than
        // reading as no sailings.
        let second = assemble(
            &ours,
            vec![(TODAY.to_string(), answers(vec![], false))],
            Some(&first),
        );
        assert_eq!(
            second["edmonds-kingston"].days,
            first["edmonds-kingston"].days
        );
        // Only today fetched: a day held stays, in its place after today.
        let both = assemble(
            &ours,
            vec![
                (TODAY.to_string(), answers(vec![answer(6, "Spokane")], true)),
                (NEXT.to_string(), answers(vec![answer(6, "Sealth")], true)),
            ],
            None,
        );
        let today_only = assemble(
            &ours,
            vec![
                (
                    TODAY.to_string(),
                    answers(vec![answer(6, "Walla Walla")], true),
                ),
                (NEXT.to_string(), DayAnswers::not_fetched()),
            ],
            Some(&both),
        );
        let held = &today_only["edmonds-kingston"].days;
        assert_eq!(held[0].terminals[0].departures[0].vessel, "Walla Walla");
        assert_eq!(held[1], both["edmonds-kingston"].days[1]);
        // WSF answers in full without Edmonds / Kingston: now it has none.
        let gone = assemble(
            &ours,
            vec![(TODAY.to_string(), answers(vec![], true))],
            Some(&first),
        );
        assert_eq!(gone["edmonds-kingston"].days[0].terminals.len(), 0);
    }

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
        let s = build("2026-10-30", &[day]);
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
