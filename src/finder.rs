//! The finder at the top of the page: a fuzzy search over the ferries,
//! terminals and routes, showing the best few matches with the selected
//! one's details opened beneath it.

use std::collections::BTreeSet;

use crate::fleet::{Fleet, Vessel};
use crate::geo::compass_point;
use crate::routes::{Leg, ServiceRoute};
use crate::search;
use crate::terminals::{Bulletin, Terminal, Terminals};
use ferry_model::api::{
    Advice, CrossingStats, GuidanceReason, LoadStats, RouteSailings, RouteSchedule, SailingOutcome,
    SailingRecord, ScheduleDay, TerminalAdvice, VesselLoads,
};

/// How many matches show.
pub const SHOWN: usize = 3;
/// A timetable shows this many of the sailings already gone, before the
/// next.
const GONE_SHOWN: usize = 2;
/// Said of a day's boats before it starts: WSF often reassigns them by
/// morning (29 of 112 sailings, the first day this was measured).
const BOATS_MAY_CHANGE: &str = "boats may change";
/// A sailing leaving within this many minutes of its time is on time.
const ON_TIME_MIN: i32 = 5;
/// A timetable runs on into the next trip day's sailings this far from now
/// (ms), so it doesn't thin out as the day ends.
const AHEAD_MS: f64 = 12.0 * 3_600_000.0;
/// A terminal lists this many upcoming sailings.
const SAILINGS_LISTED: usize = 4;

/// Something the finder can find: a boat or terminal by WSF id, or a
/// route by index into the routes riders know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Vessel(u32),
    Terminal(u32),
    Route(usize),
}

/// A match as listed.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub target: Target,
    pub name: String,
    pub kind: &'static str,
}

/// What's in scope to find, by name.
pub fn entries(fleet: &Fleet, terminals: &Terminals, routes: &[ServiceRoute]) -> Vec<Entry> {
    let boats = fleet.vessels.iter().map(|v| Entry {
        target: Target::Vessel(v.id),
        name: v.name.clone(),
        kind: "Ferry",
    });
    let docks = terminals.list.iter().map(|t| Entry {
        target: Target::Terminal(t.id),
        name: t.name.clone(),
        kind: "Terminal",
    });
    let lines = routes.iter().enumerate().map(|(i, r)| Entry {
        target: Target::Route(i),
        name: r.name.clone(),
        kind: "Route",
    });
    boats.chain(docks).chain(lines).collect()
}

/// The best `SHOWN` of `all` for `query`, best first.
pub fn matches(query: &str, all: &[Entry]) -> Vec<Entry> {
    search::best(query, all.iter().map(|e| e.name.as_str()), SHOWN)
        .into_iter()
        .map(|i| all[i].clone())
        .collect()
}

/// A match's details: label and value pairs, short lists under a heading,
/// and notices posted for it.
#[derive(Debug, Default, PartialEq)]
pub struct Info {
    pub facts: Vec<(String, String)>,
    pub lists: Vec<(String, Vec<String>)>,
    pub notices: Vec<Notice>,
    /// The scheduled departures coming up, for a terminal or route.
    pub timetables: Vec<Timetable>,
    /// How full car decks leave, by hour, for a route.
    pub load_charts: Vec<LoadChart>,
    /// Where to read more, opened in a new tab.
    pub wikipedia: Option<String>,
}

/// One terminal's departures coming up, from the published schedule.
#[derive(Debug, PartialEq)]
pub struct Timetable {
    pub heading: String,
    /// Which schedule and day, or why there are no times.
    pub caption: String,
    pub rows: Vec<TimetableRow>,
    /// Footnotes, numbered from 1 in order.
    pub notes: Vec<String>,
    /// Where to see the sailings before these, if there are any.
    pub older: Option<OlderLink>,
}

/// The way to a leg's older sailings, from a timetable.
#[derive(Clone, Debug, PartialEq)]
pub struct OlderLink {
    /// By index into `routes`.
    pub route: usize,
    pub terminal: u32,
    /// Before this sailing.
    pub before_ms: i64,
    /// What the sailings are, as the timetable's heading says.
    pub title: String,
}

/// A page of a leg's older sailings, in place of a match's details.
#[derive(Debug, PartialEq)]
pub struct OlderPage {
    pub title: String,
    /// Newest first, each trip day under its date.
    pub rows: Vec<TimetableRow>,
    /// How it stands: still loading, or nothing to show.
    pub status: String,
    pub more: bool,
}

/// How a sailing went, in the timetable's words.
#[derive(Clone, Debug, PartialEq)]
pub struct RowOutcome {
    pub tag: String,
    pub kind: OutcomeKind,
    /// The whole of it, in a sentence.
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OutcomeKind {
    Fine,
    Late,
    Cancelled,
    Quiet,
}

#[derive(Debug, PartialEq)]
pub struct TimetableRow {
    pub time: String,
    /// Where it goes, when the timetable's sailings don't all go to the
    /// one place (named in the heading).
    pub to: String,
    pub vessel: String,
    /// Its footnotes' numbers, "1" or "1,2".
    pub marks: String,
    pub when: When,
    /// Cancelled, as the live sailings say.
    pub cancelled: bool,
    /// When to arrive by car, for sailings still to come.
    pub advice: Option<RowAdvice>,
    /// The next trip day starts at this row: what to call it, as WSF's
    /// printed schedules divide them (sailings after midnight go with the
    /// day before).
    pub day_break: Option<String>,
    /// How it went, for a sailing gone or cancelled.
    pub outcome: Option<RowOutcome>,
    /// The boat is only WSF's plan so far: the sailing's on a later day.
    pub tentative: bool,
}

/// A timetable row's arrival advice: what its Arrive by column says (a
/// time, "4:25 PM"; or "room" where there's usually room),
/// what it means in full (shown on resting the pointer on the row), and
/// how pressing it is.
#[derive(Debug, PartialEq)]
pub struct RowAdvice {
    pub tag: String,
    pub sentence: String,
    pub urgency: Urgency,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Urgency {
    /// WSF's general guidance, there being no estimate.
    Guidance,
    /// Usually has room.
    Room,
    /// Arrive by a time.
    ArriveBy,
    /// Filling faster than usual today, or often full even early.
    Rush,
}

/// How full car decks leave one way on a route, by hour of the day: a
/// row each for weekdays and weekends, over `hours`.
#[derive(Debug, PartialEq)]
pub struct LoadChart {
    pub heading: String,
    pub caption: String,
    pub hours: Vec<u8>,
    pub rows: Vec<LoadRow>,
}

#[derive(Debug, PartialEq)]
pub struct LoadRow {
    pub label: &'static str,
    /// Per hour: the median share of the deck taken (0 to 1), and what a
    /// pointer resting on it says; None where nothing sailed.
    pub cells: Vec<Option<(f64, String)>>,
}

/// A departure against the time now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    Gone,
    Next,
    Later,
}

/// Something fetched when wanted: not back yet, unavailable, or here.
pub enum Loaded<'a, T> {
    Pending,
    Missing,
    Here(&'a T),
}

/// A notice as listed: its title, when it was updated, and its text.
#[derive(Debug, PartialEq)]
pub struct Notice {
    pub title: String,
    pub when: String,
    pub text: String,
}

/// What the details need: the live data, the route network, the time, and
/// how to write a time ("5:03 PM") and a date and time ("Oct 29, 4:31 PM").
pub struct Scope<'a> {
    pub fleet: &'a Fleet,
    pub terminals: &'a Terminals,
    pub routes: &'a [ServiceRoute],
    pub legs: &'a [Leg],
    /// Crossing times on each route (by index into `routes`), once
    /// they've come from the server.
    pub crossings: &'a dyn Fn(usize) -> Option<&'a CrossingStats>,
    /// The day's schedule on each route (by index into `routes`).
    pub schedules: &'a dyn Fn(usize) -> Loaded<'a, RouteSchedule>,
    /// How each route's sailings from each terminal went (by index into
    /// `routes` and WSF id).
    pub sailings: &'a dyn Fn(usize, u32) -> Loaded<'a, RouteSailings>,
    /// How full each boat's latest sailings left (by WSF id), and car
    /// deck use on each route (by index into `routes`).
    pub vessel_loads: &'a dyn Fn(u32) -> Loaded<'a, VesselLoads>,
    pub route_loads: &'a dyn Fn(usize) -> Loaded<'a, LoadStats>,
    /// When to arrive by car for each terminal's sailings (by WSF
    /// id).
    pub advice: &'a dyn Fn(u32) -> Loaded<'a, TerminalAdvice>,
    pub now_ms: f64,
    pub clock: &'a dyn Fn(f64) -> String,
    pub date_clock: &'a dyn Fn(f64) -> String,
}

impl Scope<'_> {
    pub fn info(&self, target: Target) -> Info {
        match target {
            Target::Vessel(id) => self
                .fleet
                .vessels
                .iter()
                .find(|v| v.id == id)
                .map(|v| self.vessel(v)),
            Target::Terminal(id) => self
                .terminals
                .list
                .iter()
                .find(|t| t.id == id)
                .map(|t| self.terminal(t)),
            Target::Route(i) => self.routes.get(i).map(|r| self.route(i, r)),
        }
        .unwrap_or_default()
    }

    /// "5:03 PM (in 6 min)", or "(6 min ago)".
    fn when(&self, ms: f64) -> String {
        let minutes = ((ms - self.now_ms) / 60_000.0).round();
        let rel = if minutes.abs() < 1.0 {
            "now".to_string()
        } else if minutes > 0.0 {
            format!("in {minutes:.0} min")
        } else {
            format!("{:.0} min ago", -minutes)
        };
        format!("{} ({rel})", (self.clock)(ms))
    }

    fn notices(&self, bulletins: &[Bulletin]) -> Vec<Notice> {
        bulletins
            .iter()
            .map(|b| Notice {
                title: b.title.clone(),
                when: if b.updated_ms > 0.0 {
                    format!("Updated {}", (self.date_clock)(b.updated_ms))
                } else {
                    String::new()
                },
                text: b.text.clone(),
            })
            .collect()
    }

    fn terminal_name(&self, id: u32) -> String {
        let t = self.terminals.list.iter().find(|t| t.id == id);
        t.map_or_else(|| format!("Terminal {id}"), |t| t.name.clone())
    }

    fn vessel(&self, v: &Vessel) -> Info {
        let mut facts = Vec::new();
        let mut fact = |label: &str, value: String| facts.push((label.to_string(), value));
        let status = if !v.in_service {
            "Out of service"
        } else if v.at_dock {
            "At the dock"
        } else {
            "Under way"
        };
        fact("Status", status.to_string());
        fact("Speed", format!("{:.1} kn", v.speed_kn));
        fact(
            "Heading",
            format!("{:.0}° ({})", v.heading_deg, compass_point(v.heading_deg)),
        );
        if let Some(from) = &v.departing {
            fact("From", from.clone());
        }
        if let Some(to) = &v.arriving {
            fact("To", to.clone());
        }
        match (v.left_dock_ms, v.scheduled_departure_ms) {
            (Some(left), scheduled) => {
                let late = scheduled.map(|s| (left - s) / 60_000.0);
                let timing = match late {
                    Some(m) if m.abs() < 1.0 => ", on time".to_string(),
                    Some(m) if m > 0.0 => format!(", {m:.0} min late"),
                    Some(m) => format!(", {:.0} min early", -m),
                    None => String::new(),
                };
                fact("Left", format!("{}{timing}", (self.clock)(left)));
            }
            (None, Some(scheduled)) => fact("Leaves", self.when(scheduled)),
            _ => {}
        }
        if let Some(eta) = v.eta_ms {
            fact("ETA", self.when(eta));
        }
        if let Some(&(length, beam)) = self.fleet.dimensions.get(&v.id) {
            fact("Size", format!("{length:.0} m long, {beam:.0} m wide"));
        }
        fact("Position", format!("as of {}", self.when(v.reported_ms)));
        if let Some(note) = &v.note {
            fact("Notice", note.clone());
        }
        // How full it left lately, from WSF's counts of car space.
        let mut lists = Vec::new();
        match (self.vessel_loads)(v.id) {
            Loaded::Pending => fact("Car deck", "Loading…".to_string()),
            Loaded::Missing => {}
            Loaded::Here(loads) => match loads.sailings.first() {
                None => fact("Car deck", "Not counted on its recent sailings".to_string()),
                Some(last) => {
                    let mut line = format!(
                        "{} leaving {} at {}",
                        about_full(last.load),
                        self.terminal_name(last.from),
                        (self.clock)(last.left_ms as f64)
                    );
                    if last.reading_age_min >= 5.0 {
                        line +=
                            &format!(" (counted {:.0} min before it left)", last.reading_age_min);
                    }
                    fact("Car deck", line);
                    let recent = loads
                        .sailings
                        .iter()
                        .map(|s| {
                            let to = s.to.map_or_else(String::new, |t| {
                                format!(" → {}", self.terminal_name(t))
                            });
                            format!(
                                "{} {}{to} · {}",
                                (self.clock)(s.scheduled_ms as f64),
                                self.terminal_name(s.from),
                                about_full(s.load)
                            )
                        })
                        .collect();
                    lists.push(("Recent sailings, cars".to_string(), recent));
                }
            },
        }
        Info {
            facts,
            lists,
            wikipedia: Some(wikipedia_url(&v.name)),
            ..Info::default()
        }
    }

    /// How full a route's car decks leave, by direction and hour.
    fn load_charts(&self, stats: Loaded<'_, LoadStats>) -> Vec<LoadChart> {
        let note = |caption: &str| {
            vec![LoadChart {
                heading: "Car deck use".to_string(),
                caption: caption.to_string(),
                hours: Vec::new(),
                rows: Vec::new(),
            }]
        };
        let stats = match stats {
            Loaded::Pending => return note("Loading…"),
            Loaded::Missing => return Vec::new(),
            Loaded::Here(s) => s,
        };
        if stats.directions.is_empty() {
            return note(&format!(
                "No car counts in the last {} days (WSF doesn't count car space at every terminal)",
                stats.days
            ));
        }
        stats
            .directions
            .iter()
            .map(|d| {
                let first = d.hours.iter().map(|h| h.hour).min().unwrap_or(0);
                let last = d.hours.iter().map(|h| h.hour).max().unwrap_or(0);
                let hours: Vec<u8> = (first..=last).collect();
                let rows = [(false, "Weekdays"), (true, "Weekends")]
                    .into_iter()
                    .filter(|&(weekend, _)| d.hours.iter().any(|h| h.weekend == weekend))
                    .map(|(weekend, label)| LoadRow {
                        label,
                        cells: hours
                            .iter()
                            .map(|&hour| {
                                let h = d.hours.iter().find(|h| h.weekend == weekend && h.hour == hour)?;
                                let full = (h.full_share * h.sailings as f64).round() as u32;
                                Some((
                                    h.median_load,
                                    format!(
                                        "{} {}: {:.0}% full (median of {} sailing{}), {full} left full",
                                        hour_label(hour),
                                        label.to_lowercase(),
                                        100.0 * h.median_load,
                                        h.sailings,
                                        if h.sailings == 1 { "" } else { "s" }
                                    ),
                                ))
                            })
                            .collect(),
                    })
                    .collect();
                LoadChart {
                    heading: format!(
                        "Cars, {} → {}",
                        self.terminal_name(d.from),
                        self.terminal_name(d.to)
                    ),
                    caption: format!(
                        "Median {:.0}% full over {} sailings, last {} days; darker is fuller (cars only)",
                        100.0 * d.median_load,
                        d.sailings,
                        stats.days
                    ),
                    hours,
                    rows,
                }
            })
            .collect()
    }

    fn terminal(&self, t: &Terminal) -> Info {
        let mut facts = Vec::new();
        let vessels = &self.fleet.vessels;
        let docked: Vec<String> = vessels
            .iter()
            .filter(|v| v.in_service && v.at_dock && v.departing_id == Some(t.id))
            .map(|v| match (&v.arriving, v.scheduled_departure_ms) {
                (Some(to), Some(leaves)) => {
                    format!("{} (to {to}, leaves {})", v.name, (self.clock)(leaves))
                }
                (Some(to), None) => format!("{} (to {to})", v.name),
                _ => v.name.clone(),
            })
            .collect();
        facts.push((
            "At the dock".to_string(),
            if docked.is_empty() {
                "No boat".to_string()
            } else {
                docked.join(", ")
            },
        ));
        let upcoming = self.terminals.upcoming(t, self.now_ms);
        if let Some(next) = self.terminals.next_sailing(t, self.now_ms) {
            facts.push((
                "Next sailing".to_string(),
                format!(
                    "{} to {} · {}",
                    self.when(next.departs_ms),
                    next.to,
                    next.vessel
                ),
            ));
            facts.push(("Open car spaces".to_string(), spaces(next)));
        }
        let arriving = vessels
            .iter()
            .filter(|v| v.arriving_id == Some(t.id) && !v.at_dock)
            .filter_map(|v| v.eta_ms.map(|eta| (v, eta)))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((v, eta)) = arriving {
            let from = v
                .departing
                .as_ref()
                .map_or_else(String::new, |f| format!(" from {f}"));
            facts.push((
                "Next arrival".to_string(),
                format!("{}{from}, {}", v.name, self.when(eta)),
            ));
        }
        let sailings: Vec<String> = upcoming
            .iter()
            .take(SAILINGS_LISTED)
            .map(|s| {
                let space = if s.cancelled {
                    "cancelled".to_string()
                } else {
                    spaces(s)
                };
                format!(
                    "{} to {} · {} · {space}",
                    (self.clock)(s.departs_ms),
                    s.to,
                    s.vessel
                )
            })
            .collect();
        let mut lists = Vec::new();
        if !sailings.is_empty() {
            lists.push(("Upcoming sailings".to_string(), sailings));
        }
        // With a reservation, when to arrive and check in (WSF's rules for
        // the route). Each sailing's advice without one is in its row.
        if let Loaded::Here(advice) = (self.advice)(t.id)
            && let Some(r) = &advice.reservation
        {
            facts.push((
                "With a reservation".to_string(),
                format!(
                    "Arrive {} to {} min early; check in at least {} min before, or the space goes",
                    r.arrive_min[0], r.arrive_min[1], r.check_in_min
                ),
            ));
        }
        // Its routes' schedules, departures from here.
        let timetables = (self.routes.iter().enumerate())
            .filter(|(_, r)| r.terminals.contains(&t.id))
            .map(|(i, r)| {
                let several = self.routes.iter().filter(|r| r.terminals.contains(&t.id));
                let heading = if several.count() > 1 {
                    r.name.clone()
                } else {
                    "Departures".to_string()
                };
                self.timetable(heading, i, t.id)
            })
            .collect();
        Info {
            facts,
            lists,
            notices: self.notices(self.terminals.bulletins(t)),
            timetables,
            ..Info::default()
        }
    }

    /// Terminal `from`'s departures on a route's `schedule` for the day,
    /// under `heading` (which names the destination, if they all go to
    /// one).
    fn timetable(&self, heading: String, route: usize, from: u32) -> Timetable {
        let records = match (self.sailings)(route, from) {
            Loaded::Here(r) => Some(r),
            _ => None,
        };
        let older_link = |title: &str, before_ms: i64, hidden: bool| {
            (hidden || records.is_some_and(|r| r.more)).then(|| OlderLink {
                route,
                terminal: from,
                before_ms,
                title: title.to_string(),
            })
        };
        let now = self.now_ms as i64;
        let empty = |heading: String, caption: &str| Timetable {
            older: older_link(&heading, now, false),
            heading,
            caption: caption.to_string(),
            rows: Vec::new(),
            notes: Vec::new(),
        };
        let schedule = match (self.schedules)(route) {
            Loaded::Pending => return empty(heading, "Loading the schedule…"),
            Loaded::Missing => return empty(heading, "The schedule isn't available right now"),
            Loaded::Here(s) => s,
        };
        let Some(last_day) = schedule.days.last() else {
            return empty(heading, "The schedule isn't available right now");
        };
        let record_of = |departs_ms: i64| {
            records.and_then(|r| r.sailings.iter().find(|s| s.departs_ms == departs_ms))
        };
        // Sailings withdrawn stay where they were, marked, not gone without
        // a word.
        let withdrawn: Vec<(usize, ferry_model::api::Departure)> = records
            .iter()
            .flat_map(|r| &r.sailings)
            .filter(|s| matches!(s.outcome, SailingOutcome::Cancelled { .. }))
            .filter_map(|s| {
                let day = schedule
                    .days
                    .iter()
                    .position(|d| d.trip_date == s.trip_date)?;
                Some((day, departure_of(s)))
            })
            .collect();
        let mut cancelled: Vec<f64> = (self.terminals.list.iter())
            .find(|t| t.id == from)
            .map(|t| self.terminals.upcoming(t, self.now_ms))
            .into_iter()
            .flatten()
            .filter(|s| s.cancelled)
            .map(|s| s.departs_ms)
            .collect();
        cancelled.extend(withdrawn.iter().map(|(_, d)| d.departs_ms as f64));
        // What to show: all of the first day's, then the next day's as far
        // as `AHEAD_MS` from now, or to its first if none's come up by then.
        let mut shown = Vec::new();
        let mut coming = false;
        'days: for (i, day) in schedule.days.iter().enumerate() {
            let here = day.terminals.iter().find(|t| t.terminal == from);
            let notes = here.map_or(&[][..], |h| &h.notes[..]);
            let listed = here.iter().flat_map(|h| &h.departures);
            let again = (withdrawn.iter())
                .filter(|(day, d)| {
                    *day == i
                        && !here.is_some_and(|h| {
                            h.departures.iter().any(|s| s.departs_ms == d.departs_ms)
                        })
                })
                .map(|(_, d)| d);
            let mut sailings: Vec<&ferry_model::api::Departure> = listed.chain(again).collect();
            sailings.sort_by_key(|d| d.departs_ms);
            for d in sailings {
                let departs = d.departs_ms as f64;
                if i > 0 && coming && departs > self.now_ms + AHEAD_MS {
                    break 'days;
                }
                shown.push((i, d, notes));
                coming |= departs >= self.now_ms && !cancelled.contains(&departs);
            }
        }
        // WSF's schedules can only say what's published, not why or for how
        // long; any notice about it is shown with the terminal or route.
        let through = trip_day(&last_day.trip_date).unwrap_or_default();
        let unknown_after = if schedule.days.len() < 2 {
            "; the next day's schedule isn't available"
        } else {
            ""
        };
        if shown.is_empty() {
            let caption = format!("No sailings scheduled through {through}{unknown_after}");
            return empty(heading, &caption);
        }
        // Of those gone, only the last few, for context.
        let gone = (shown.iter())
            .filter(|(_, d, _)| (d.departs_ms as f64) < self.now_ms)
            .count();
        let hidden = gone > GONE_SHOWN;
        shown.drain(..gone.saturating_sub(GONE_SHOWN));
        let places = |d: &ferry_model::api::Departure| {
            (d.stops.iter())
                .map(|s| self.terminal_name(s.terminal))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let first = places(shown[0].1);
        let one_place = shown.iter().all(|(_, d, _)| places(d) == first);
        let heading = if one_place {
            format!("{heading} to {first}")
        } else {
            heading
        };
        let first_day = &schedule.days[shown[0].0];
        let caption = if coming {
            let caption = trip_day(&first_day.trip_date).map_or_else(
                || first_day.season.clone(),
                |day| match first_day.season.as_str() {
                    "" => format!("Schedule for {day}"),
                    season => format!("{season} schedule, {day}"),
                },
            );
            if shown[0].0 > 0 {
                format!("{caption} · {BOATS_MAY_CHANGE}")
            } else {
                caption
            }
        } else {
            format!("No more sailings scheduled through {through}{unknown_after}")
        };
        let mut next_found = false;
        let advice = match (self.advice)(from) {
            Loaded::Here(a) => Some(a),
            _ => None,
        };
        // Footnotes are per day: one list for all those shown, each once.
        let mut notes: Vec<String> = Vec::new();
        let mut day_shown = shown[0].0;
        let rows = (shown.iter())
            .map(|&(i, d, day_notes)| {
                let departs = d.departs_ms as f64;
                let is_cancelled = cancelled.contains(&departs);
                let when = if departs < self.now_ms {
                    When::Gone
                } else if !next_found && !is_cancelled {
                    next_found = true;
                    When::Next
                } else {
                    When::Later
                };
                let marks: Vec<String> = (d.notes.iter())
                    .filter_map(|&n| day_notes.get(usize::from(n)))
                    .map(|text| match notes.iter().position(|n| n == text) {
                        Some(at) => (at + 1).to_string(),
                        None => {
                            notes.push(text.clone());
                            notes.len().to_string()
                        }
                    })
                    .collect();
                let day_break = (i != day_shown).then(|| {
                    day_shown = i;
                    format!(
                        "{} · {BOATS_MAY_CHANGE}",
                        day_heading(&schedule.days[i], first_day)
                    )
                });
                let record = record_of(d.departs_ms);
                let outcome = match record {
                    Some(r) if when == When::Gone || is_cancelled => self.outcome_of(r),
                    _ if is_cancelled => Some(RowOutcome {
                        tag: "cancelled".to_string(),
                        kind: OutcomeKind::Cancelled,
                        detail: "WSF has cancelled this sailing.".to_string(),
                    }),
                    _ => None,
                };
                TimetableRow {
                    time: (self.clock)(departs),
                    to: if one_place { String::new() } else { places(d) },
                    vessel: record
                        .filter(|_| when == When::Gone)
                        .and_then(sailed_by)
                        .unwrap_or(&d.vessel)
                        .to_string(),
                    marks: marks.join(","),
                    when,
                    cancelled: is_cancelled,
                    advice: (when != When::Gone && !is_cancelled)
                        .then_some(advice)
                        .flatten()
                        .and_then(|a| a.sailings.iter().find(|s| s.departs_ms == d.departs_ms))
                        .and_then(|s| self.row_advice(s)),
                    day_break,
                    outcome,
                    tentative: i > 0,
                }
            })
            .collect();
        Timetable {
            older: older_link(&heading, shown[0].1.departs_ms, hidden),
            heading,
            caption,
            rows,
            notes,
        }
    }

    /// How a sailing went, in words; None if there's no telling.
    fn outcome_of(&self, r: &SailingRecord) -> Option<RowOutcome> {
        let clock = |ms: i64| (self.clock)(ms as f64);
        let first_scheduled = |boat: &str| {
            if boat == r.first_vessel {
                String::new()
            } else {
                format!(" First scheduled for {}.", r.first_vessel)
            }
        };
        let (tag, kind, detail) = match &r.outcome {
            SailingOutcome::Ran {
                left_ms,
                vessel,
                late_min,
            } => {
                let left = clock(*left_ms);
                let how = match *late_min {
                    0 => "on time".to_string(),
                    m if m > 0 => format!("{m} min after its time"),
                    m => format!("{} min before its time", -m),
                };
                let (tag, kind) = if *late_min > ON_TIME_MIN {
                    (format!("{late_min} min late"), OutcomeKind::Late)
                } else {
                    ("on time".to_string(), OutcomeKind::Fine)
                };
                let detail = format!("{vessel} left at {left}, {how}.{}", first_scheduled(vessel));
                (tag, kind, detail)
            }
            SailingOutcome::OffSchedule { left_ms, vessel } => (
                "off schedule".to_string(),
                OutcomeKind::Late,
                format!(
                    "Boats were off the schedule. {vessel} left around then, at {}.{}",
                    clock(*left_ms),
                    first_scheduled(vessel)
                ),
            ),
            SailingOutcome::Cancelled { withdrawn_ms } => (
                "cancelled".to_string(),
                OutcomeKind::Cancelled,
                format!("WSF withdrew this sailing by {}.", clock(*withdrawn_ms)),
            ),
            SailingOutcome::Due => (
                "not left yet".to_string(),
                OutcomeKind::Quiet,
                "It's past its time, and no boat has left for it yet.".to_string(),
            ),
            SailingOutcome::NoRecord => (
                "no record".to_string(),
                OutcomeKind::Quiet,
                "It's still on the schedule, but no boat was seen leaving for it.".to_string(),
            ),
            SailingOutcome::Unknown => return None,
        };
        Some(RowOutcome { tag, kind, detail })
    }

    /// A page of older sailings from `pages` of them in turn (each newest
    /// first), `loading` while there are more to come.
    pub fn older_page(&self, title: String, pages: &[&RouteSailings], loading: bool) -> OlderPage {
        let mut rows = Vec::new();
        let mut day: Option<&str> = None;
        for r in pages.iter().flat_map(|p| &p.sailings) {
            let day_break = (day != Some(r.trip_date.as_str())).then(|| {
                day = Some(r.trip_date.as_str());
                let date = trip_day(&r.trip_date).unwrap_or_else(|| r.trip_date.clone());
                weekday(&r.trip_date).map_or(date.clone(), |w| format!("{w}, {date}"))
            });
            let outcome = self.outcome_of(r);
            let to = (r.stops.iter())
                .map(|&t| self.terminal_name(t))
                .collect::<Vec<_>>()
                .join(", ");
            rows.push(TimetableRow {
                time: (self.clock)(r.departs_ms as f64),
                to,
                vessel: sailed_by(r).unwrap_or(&r.vessel).to_string(),
                marks: String::new(),
                when: When::Gone,
                cancelled: matches!(r.outcome, SailingOutcome::Cancelled { .. }),
                advice: None,
                day_break,
                outcome,
                tentative: false,
            });
        }
        let status = if loading {
            "Loading…"
        } else if rows.is_empty() {
            "No older sailings on record."
        } else {
            ""
        };
        OlderPage {
            title,
            rows,
            status: status.to_string(),
            more: !loading && pages.last().is_some_and(|p| p.more),
        }
    }

    /// A sailing's advice in words: a sentence, and what its timetable
    /// row says.
    fn row_advice(&self, s: &ferry_model::api::SailingAdvice) -> Option<RowAdvice> {
        let holiday = s
            .holiday
            .as_ref()
            .map_or_else(String::new, |h| format!("{h}: allowing extra time. "));
        match &s.advice {
            Advice::Guidance {
                arrive_by_ms,
                lead_min,
                reason,
                counted,
            } => {
                // WSF's ranges as it gives them; the time shown is `lead_min`
                // early (more on holidays).
                let rule = match reason {
                    GuidanceReason::OffPeak => "at least 20 minutes early off-peak".to_string(),
                    GuidanceReason::Busy => {
                        format!("30 to 60 minutes early at busy times; {lead_min} shown")
                    }
                    GuidanceReason::Reservations => format!(
                        "30 to 45 minutes early on this route, without a reservation; {lead_min} shown"
                    ),
                };
                // Where WSF counts car space, the estimate will come with
                // history; where it doesn't, say why there's none.
                let why = if *counted {
                    ""
                } else {
                    " (WSF doesn't count car space here)"
                };
                let by = (self.clock)(*arrive_by_ms as f64);
                Some(RowAdvice {
                    tag: by,
                    sentence: format!("{holiday}WSF suggests arriving {rule}{why}"),
                    urgency: Urgency::Guidance,
                })
            }
            Advice::Room { sailings, basis } => Some(RowAdvice {
                tag: "room".to_string(),
                sentence: format!(
                    "{holiday}Usually has room for cars (based on {sailings} recent {basis})"
                ),
                urgency: Urgency::Room,
            }),
            Advice::ArriveBy {
                arrive_by_ms,
                likely,
                usually_full_ms,
                sailings,
                basis,
                pace,
            } => {
                let pace_note = match pace {
                    Some(p) if *p >= 1.15 => "Filling faster than usual today. ",
                    Some(p) if *p <= 0.85 => "Filling slower than usual today. ",
                    _ => "",
                };
                let sailing = (self.clock)(s.departs_ms as f64);
                let by = (self.clock)(*arrive_by_ms as f64);
                let ask = if *likely >= 0.7 {
                    format!("Arrive by {by} to likely make the {sailing}")
                } else {
                    format!("Often full: arrive by {by} for a fair chance at the {sailing}")
                };
                let usual = usually_full_ms.map_or_else(String::new, |ms| {
                    format!("usually full by {}, ", (self.clock)(ms as f64))
                });
                Some(RowAdvice {
                    tag: by.clone(),
                    sentence: format!(
                        "{holiday}{pace_note}{ask} ({usual}based on {sailings} recent {basis})"
                    ),
                    urgency: if *likely < 0.7 || pace.is_some_and(|p| p >= 1.15) {
                        Urgency::Rush
                    } else {
                        Urgency::ArriveBy
                    },
                })
            }
        }
    }

    /// Route `r`, `routes[i]`.
    fn route(&self, i: usize, r: &ServiceRoute) -> Info {
        let crossings = (self.crossings)(i);
        let ids: Vec<u32> = r.terminals.iter().copied().collect();
        let names: Vec<String> = ids.iter().map(|&id| self.terminal_name(id)).collect();
        let mut facts = vec![("Terminals".to_string(), names.join(", "))];
        // The server's crossing times, between a pair of terminals either way.
        let pair = |a: u32, b: u32| {
            crossings?
                .pairs
                .iter()
                .find(|p| p.terminals == [a.min(b), a.max(b)])
        };
        let typical = match crossings {
            None => "Loading…".to_string(),
            Some(c) => match c.pairs.as_slice() {
                [] => format!("No trips recorded in {} days", c.days),
                [p] => format!("{:.0} min (median, last {} days)", p.median_min, c.days),
                many => {
                    let (lo, hi) = many.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
                        (lo.min(p.median_min), hi.max(p.median_min))
                    });
                    format!("{lo:.0} to {hi:.0} min, by leg")
                }
            },
        };
        facts.push(("Typical crossing".to_string(), typical));
        let mut lists = Vec::new();
        let joins: Vec<(u32, u32)> = r
            .legs
            .iter()
            .filter_map(|&i| self.legs.get(i).and_then(Leg::joins))
            .collect();
        if joins.len() > 1 {
            let legs = joins
                .iter()
                .map(|&(a, b)| {
                    let time = pair(a, b).map_or_else(
                        || "no trips recorded".to_string(),
                        |p| format!("{:.0} min", p.median_min),
                    );
                    format!(
                        "{} ↔ {} · {time}",
                        self.terminal_name(a),
                        self.terminal_name(b)
                    )
                })
                .collect();
            lists.push(("Legs".to_string(), legs));
        }
        let serves = |id: Option<u32>| id.is_some_and(|id| r.terminals.contains(&id));
        let mut ferries: Vec<String> = self
            .fleet
            .vessels
            .iter()
            .filter(|v| serves(v.departing_id) && serves(v.arriving_id))
            .map(|v| {
                let to = v.arriving.as_deref().unwrap_or("?");
                match (v.at_dock, v.eta_ms) {
                    (true, _) => format!(
                        "{} · at {}, next to {to}",
                        v.name,
                        v.departing.as_deref().unwrap_or("dock")
                    ),
                    (false, Some(eta)) => {
                        format!("{} · to {to}, ETA {}", v.name, (self.clock)(eta))
                    }
                    (false, None) => format!("{} · to {to}", v.name),
                }
            })
            .collect();
        ferries.sort();
        lists.push((
            "Ferries".to_string(),
            if ferries.is_empty() {
                vec!["None on it now".to_string()]
            } else {
                ferries
            },
        ));
        // Its terminals' notices, each once (a route's notice is posted
        // at each of its terminals).
        let mut seen = BTreeSet::new();
        let mut bulletins: Vec<Bulletin> = self
            .terminals
            .list
            .iter()
            .filter(|t| r.terminals.contains(&t.id))
            .flat_map(|t| self.terminals.bulletins(t))
            .filter(|b| seen.insert(b.title.clone()))
            .cloned()
            .collect();
        bulletins.sort_by(|a, b| b.updated_ms.total_cmp(&a.updated_ms));
        let timetables = ids
            .iter()
            .map(|&id| {
                let heading = format!("From {}", self.terminal_name(id));
                self.timetable(heading, i, id)
            })
            .collect();
        Info {
            facts,
            lists,
            notices: self.notices(&bulletins),
            timetables,
            load_charts: self.load_charts((self.route_loads)(i)),
            wikipedia: None,
        }
    }
}

/// Wikipedia's article on WSF boat `name`, which it titles "MV Kitsap"
/// and the like: through its search, which goes straight to a page with
/// that title, or lists the closest matches if there's none.
pub fn wikipedia_url(name: &str) -> String {
    let mut query = String::new();
    for byte in format!("MV {name}").bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                query.push(byte as char)
            }
            b' ' => query.push('+'),
            _ => query.push_str(&format!("%{byte:02X}")),
        }
    }
    format!("https://en.wikipedia.org/wiki/Special:Search?search={query}&go=Go")
}

/// A share of a car deck taken, as people say it: "about 80% full", to
/// the nearest 5%, "full" from 95%.
fn about_full(load: f64) -> String {
    if load >= 0.95 {
        "full".to_string()
    } else {
        format!("about {:.0}% full", (load * 20.0).round() * 5.0)
    }
}

/// An hour of the day, as "7 AM".
fn hour_label(hour: u8) -> String {
    match hour {
        0 => "12 AM".to_string(),
        12 => "12 PM".to_string(),
        h if h < 12 => format!("{h} AM"),
        h => format!("{} PM", h - 12),
    }
}

/// A trip day, `2026-10-30`, as "Oct 30".
fn trip_day(date: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut parts = date.split('-').skip(1);
    let month: usize = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    Some(format!("{} {day}", MONTHS.get(month.checked_sub(1)?)?))
}

/// The boat that ran a sailing, if one did.
fn sailed_by(r: &SailingRecord) -> Option<&str> {
    match &r.outcome {
        SailingOutcome::Ran { vessel, .. } | SailingOutcome::OffSchedule { vessel, .. } => {
            Some(vessel)
        }
        _ => None,
    }
}

/// A sailing on record as a schedule's departure.
fn departure_of(r: &SailingRecord) -> ferry_model::api::Departure {
    ferry_model::api::Departure {
        departs_ms: r.departs_ms,
        vessel_id: None,
        vessel: r.vessel.clone(),
        stops: (r.stops.iter())
            .map(|&terminal| ferry_model::api::Stop {
                terminal,
                arrives_ms: None,
            })
            .collect(),
        loading: ferry_model::api::Loading::Both,
        notes: Vec::new(),
    }
}

/// The name of a date's day of the week (Sakamoto's method).
fn weekday(date: &str) -> Option<&'static str> {
    const DAYS: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    const MONTH_OFFSETS: [usize; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let mut parts = date.split('-');
    let year: usize = parts.next()?.parse().ok()?;
    let month: usize = parts.next()?.parse().ok()?;
    let day: usize = parts.next()?.parse().ok()?;
    let y = if month < 3 { year - 1 } else { year };
    let offset = MONTH_OFFSETS.get(month.checked_sub(1)?)?;
    DAYS.get((y + y / 4 - y / 100 + y / 400 + offset + day) % 7)
        .copied()
}

/// A divider's words for `day` in a timetable that began on `first`: its
/// date, and its schedule if that's changed (a new season).
fn day_heading(day: &ScheduleDay, first: &ScheduleDay) -> String {
    let date = trip_day(&day.trip_date).unwrap_or_else(|| day.trip_date.clone());
    let name = weekday(&day.trip_date).map_or(date.clone(), |w| format!("{w}, {date}"));
    if day.season == first.season || day.season.is_empty() {
        name
    } else {
        format!("{name} · {} schedule", day.season)
    }
}

/// A sailing's drive-up space, as "38 of 202 drive-up".
fn spaces(s: &crate::terminals::Sailing) -> String {
    match (s.drive_up, s.capacity) {
        (Some(left), Some(max)) => format!("{left} of {max} drive-up"),
        (Some(left), None) => format!("{left} drive-up"),
        _ => "not shown".to_string(),
    }
}

#[cfg(target_arch = "wasm32")]
pub use dom::{Finder, Sheet};

#[cfg(target_arch = "wasm32")]
mod dom {
    use std::collections::HashMap;

    use wasm_bindgen::JsCast;
    use web_sys::{Document, Element, HtmlInputElement};

    use super::{Entry, Info, OlderPage, OutcomeKind, TimetableRow, Urgency, When};

    /// How much of the screen the matches take on a phone, where they're a
    /// sheet at the bottom (index.html): a peek at the selected one, half
    /// the screen, or all of it below the search bar. Elsewhere the CSS
    /// ignores it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Sheet {
        Peek,
        Half,
        Full,
    }

    impl Sheet {
        fn class(self) -> &'static str {
            match self {
                Self::Peek => "sheet-peek",
                Self::Half => "sheet-half",
                Self::Full => "sheet-full",
            }
        }
    }

    /// The finder's elements (#finder in index.html) and state: the
    /// matches shown, which is selected (its details open), and which the
    /// pointer has rested on since when.
    pub struct Finder {
        pub input: HtmlInputElement,
        list: Element,
        pub entries: Vec<Entry>,
        pub selected: Option<usize>,
        resting: Option<(usize, f64)>,
        /// The details last built, to leave them be when unchanged.
        built: String,
        sheet: Sheet,
        /// The page of older sailings over the details (#finder-page), what
        /// it last showed, and where the details were scrolled to when it
        /// opened (the list on a desk, the sheet on a phone).
        page: Element,
        page_built: Option<String>,
        behind_page: (i32, i32),
    }

    /// How long (ms) the pointer rests on a match before it opens.
    const REST_MS: f64 = 1000.0;

    impl Finder {
        pub fn new(input: HtmlInputElement, list: Element, page: Element) -> Self {
            Self {
                input,
                list,
                page,
                page_built: None,
                behind_page: (0, 0),
                entries: Vec::new(),
                selected: None,
                resting: None,
                built: String::new(),
                sheet: Sheet::Half,
            }
        }

        fn document(&self) -> Document {
            self.list.owner_document().expect("document")
        }

        /// Shows a page of older sailings over the details, or goes back
        /// to them just as they were.
        pub fn set_older(&mut self, page: Option<&OlderPage>) {
            let fold = self.list.parent_element();
            let Some(page) = page else {
                if self.page_built.take().is_some() {
                    let _ = self.page.set_attribute("hidden", "");
                    if let Some(fold) = &fold {
                        let _ = fold.class_list().remove_1("paged");
                        fold.set_scroll_top(self.behind_page.1);
                    }
                    self.list.set_scroll_top(self.behind_page.0);
                }
                return;
            };
            let content = format!("{page:?}");
            if self.page_built.as_ref() == Some(&content) {
                return;
            }
            let opening = self.page_built.is_none();
            let select = |id: &str| self.page.query_selector(id).ok().flatten();
            if let Some(el) = select("#finder-page-title") {
                el.set_text_content(Some(&page.title));
            }
            if let Some(el) = select("#finder-page-status") {
                el.set_text_content(Some(&page.status));
            }
            if let Some(button) = select("#finder-page-more") {
                if page.more {
                    let _ = button.remove_attribute("hidden");
                } else {
                    let _ = button.set_attribute("hidden", "");
                }
            }
            if let Some(list) = select("#finder-page-rows") {
                let document = self.document();
                list.set_text_content(None);
                let head = add(&document, &list, "li", "head", "");
                let _ = head.set_attribute("aria-hidden", "true");
                for label in ["Departs", "How it went", "Vessel"] {
                    add(&document, &head, "span", "", label);
                }
                for row in &page.rows {
                    add_row(&document, &list, row, true);
                }
            }
            self.page_built = Some(content);
            if opening {
                let fold_scroll = fold.as_ref().map_or(0, |f| f.scroll_top());
                self.behind_page = (self.list.scroll_top(), fold_scroll);
                let _ = self.page.remove_attribute("hidden");
                if let Some(fold) = &fold {
                    let _ = fold.class_list().add_1("paged");
                    fold.set_scroll_top(0);
                }
                self.page.set_scroll_top(0);
                if let Some(back) = self
                    .page
                    .query_selector("#finder-page-back")
                    .ok()
                    .flatten()
                    .and_then(|b| b.dyn_into::<web_sys::HtmlElement>().ok())
                {
                    let _ = back.focus();
                }
            }
        }

        /// Lists `entries`, the first selected.
        pub fn show(&mut self, entries: Vec<Entry>) {
            self.list.set_text_content(None);
            let document = self.document();
            for (i, entry) in entries.iter().enumerate() {
                let item = add(&document, &self.list, "li", "hit", "");
                let _ = item.set_attribute("role", "option");
                let _ = item.set_attribute("id", &format!("finder-option-{i}"));
                let _ = item.set_attribute("data-index", &i.to_string());
                let row = add(&document, &item, "div", "row", "");
                add(&document, &row, "span", "name", &entry.name);
                add(&document, &row, "span", "kind", entry.kind);
                let more = add(&document, &item, "div", "more", "");
                add(&document, &more, "div", "inner", "");
            }
            self.selected = (!entries.is_empty()).then_some(0);
            self.entries = entries;
            self.resting = None;
            self.built.clear();
            self.mark();
            self.set_open(!self.entries.is_empty());
            if let Some(root) = self.root() {
                let _ = root
                    .class_list()
                    .toggle_with_force("matched", !self.entries.is_empty());
            }
        }

        fn root(&self) -> Option<Element> {
            self.list.closest("#finder").ok().flatten()
        }

        /// Sets how much of a phone's screen the sheet takes.
        pub fn set_sheet(&mut self, sheet: Sheet) {
            self.sheet = sheet;
            if let Some(root) = self.root() {
                for s in [Sheet::Peek, Sheet::Half, Sheet::Full] {
                    let _ = root.class_list().toggle_with_force(s.class(), s == sheet);
                }
            }
        }

        pub fn sheet(&self) -> Sheet {
            self.sheet
        }

        /// Whether there are matches showing (`.matched` on #finder).
        pub fn matched(&self) -> bool {
            !self.entries.is_empty()
        }

        /// The sheet's handle: peek, half, full, and round again.
        pub fn cycle_sheet(&mut self) {
            self.set_sheet(match self.sheet {
                Sheet::Peek => Sheet::Half,
                Sheet::Half => Sheet::Full,
                Sheet::Full => Sheet::Peek,
            });
        }

        /// Opens or folds up the list of matches (`.open` on #finder).
        pub fn set_open(&self, open: bool) {
            let open = open && !self.entries.is_empty();
            if let Some(root) = self.root() {
                let _ = root.class_list().toggle_with_force("open", open);
            }
            let _ = self
                .input
                .set_attribute("aria-expanded", if open { "true" } else { "false" });
        }

        /// Selects match `i`; says whether that changed anything.
        pub fn select(&mut self, i: usize) -> bool {
            if i >= self.entries.len() || self.selected == Some(i) {
                return false;
            }
            self.selected = Some(i);
            self.built.clear();
            self.mark();
            true
        }

        /// Moves the selection `step` matches along, wrapping around.
        pub fn cycle(&mut self, step: isize) -> bool {
            let n = self.entries.len() as isize;
            match self.selected {
                Some(i) if n > 1 => self.select((i as isize + step).rem_euclid(n) as usize),
                _ => false,
            }
        }

        pub fn entry(&self) -> Option<&Entry> {
            self.entries.get(self.selected?)
        }

        /// The pointer is on match `i` (or off them all) at `now_ms`.
        pub fn rest(&mut self, i: Option<usize>, now_ms: f64) {
            self.resting = match (i, self.resting) {
                (Some(i), Some((j, since))) if i == j => Some((j, since)),
                (Some(i), _) => Some((i, now_ms)),
                (None, _) => None,
            };
        }

        /// The match the pointer has rested on long enough to open, once.
        pub fn rested(&mut self, now_ms: f64) -> Option<usize> {
            let (i, since) = self.resting?;
            if now_ms - since < REST_MS || self.selected == Some(i) {
                return None;
            }
            self.resting = None;
            Some(i)
        }

        /// Marks the selected match, which opens its details (CSS).
        fn mark(&self) {
            let items = self.list.children();
            for i in 0..items.length() {
                if let Some(item) = items.item(i) {
                    let on = self.selected == Some(i as usize);
                    let _ = item.class_list().toggle_with_force("selected", on);
                    let _ = item.set_attribute("aria-selected", if on { "true" } else { "false" });
                }
            }
            match self.selected {
                Some(i) => {
                    let _ = self
                        .input
                        .set_attribute("aria-activedescendant", &format!("finder-option-{i}"));
                }
                None => {
                    let _ = self.input.remove_attribute("aria-activedescendant");
                }
            }
        }

        /// Fills the selected match's details with `info`, if they've
        /// changed, keeping the notices scrolled where they were.
        pub fn set_info(&mut self, info: &Info) {
            let content = format!("{info:?}");
            if content == self.built {
                return;
            }
            let Some(i) = self.selected else { return };
            let Some(inner) = self
                .list
                .children()
                .item(i as u32)
                .and_then(|item| item.query_selector(".inner").ok().flatten())
            else {
                return;
            };
            let scrolled = inner
                .query_selector(".bulletins")
                .ok()
                .flatten()
                .map_or(0, |b| b.scroll_top());
            // Timetables the reader has scrolled, by heading, and where to:
            // the rest open (and stay) at the top, where the last sailings
            // gone lead into the next.
            // Each list notes where it was put (data-auto), so moving
            // from there is the reader's doing.
            let mut scrolled_by_reader: HashMap<String, i32> = HashMap::new();
            if let Ok(old) = inner.query_selector_all(".timetable") {
                for k in 0..old.length() {
                    let Some(el) = old.item(k).and_then(|n| n.dyn_into::<Element>().ok()) else {
                        continue;
                    };
                    let put = el
                        .get_attribute("data-auto")
                        .and_then(|a| a.parse::<i32>().ok());
                    if put != Some(el.scroll_top())
                        && let Some(heading) = el.get_attribute("data-heading")
                    {
                        scrolled_by_reader.insert(heading, el.scroll_top());
                    }
                }
            }
            // Other matches' details are closed; clear them.
            if let Ok(all) = self.list.query_selector_all(".inner") {
                for k in 0..all.length() {
                    if let Some(node) = all.item(k) {
                        node.set_text_content(None);
                    }
                }
            }
            let document = self.document();
            if !info.facts.is_empty() {
                let dl = add(&document, &inner, "dl", "", "");
                for (label, value) in &info.facts {
                    add(&document, &dl, "dt", "", label);
                    add(&document, &dl, "dd", "", value);
                }
            }
            if let Some(url) = &info.wikipedia {
                let link = add(&document, &inner, "a", "wikipedia", "");
                let _ = link.set_attribute("href", url);
                let _ = link.set_attribute("target", "_blank");
                let _ = link.set_attribute("rel", "noopener noreferrer");
                let mark = add(&document, &link, "span", "mark", "W");
                let _ = mark.set_attribute("aria-hidden", "true");
                add(&document, &link, "span", "", "Wikipedia");
            }
            for (heading, lines) in &info.lists {
                let section = add(&document, &inner, "section", "", "");
                add(&document, &section, "h3", "", heading);
                let ul = add(&document, &section, "ul", "", "");
                for line in lines {
                    add(&document, &ul, "li", "", line);
                }
            }
            // All the schedules together, set off from the rest.
            let schedules = (!info.timetables.is_empty())
                .then(|| add(&document, &inner, "div", "schedules", ""));
            for table in &info.timetables {
                let Some(schedules) = &schedules else { break };
                let section = add(&document, schedules, "section", "leg", "");
                add(&document, &section, "h3", "", &table.heading);
                add(&document, &section, "div", "caption", &table.caption);
                if table.rows.is_empty() {
                    if let Some(link) = &table.older {
                        add_older_button(&document, &section, link);
                    }
                    continue;
                }
                let list = add(&document, &section, "ol", "timetable", "");
                // A header that stays put as the rows scroll beneath it.
                let head = add(&document, &list, "li", "head", "");
                let _ = head.set_attribute("aria-hidden", "true");
                for label in ["Departs", "Arrive by", "Vessel"] {
                    add(&document, &head, "span", "", label);
                }
                if let Some(link) = &table.older {
                    let item = add(&document, &list, "li", "older", "");
                    add_older_button(&document, &item, link);
                }
                for row in &table.rows {
                    add_row(&document, &list, row, false);
                }
                let _ = list.set_attribute("data-heading", &table.heading);
                match scrolled_by_reader.get(&table.heading) {
                    Some(&top) => {
                        list.set_scroll_top(top);
                        let _ = list.set_attribute("data-auto", "-1");
                    }
                    None => {
                        // From the top: the last sailings gone, then the
                        // next.
                        list.set_scroll_top(0);
                        // As the browser took it (it stops at the end).
                        let _ = list.set_attribute("data-auto", &list.scroll_top().to_string());
                    }
                }
                if !table.notes.is_empty() {
                    let notes = add(&document, &section, "ol", "footnotes", "");
                    for note in &table.notes {
                        add(&document, &notes, "li", "", note);
                    }
                }
            }
            for chart in &info.load_charts {
                let section = add(&document, &inner, "section", "", "");
                add(&document, &section, "h3", "", &chart.heading);
                add(&document, &section, "div", "caption", &chart.caption);
                if chart.rows.is_empty() {
                    continue;
                }
                // A row of hour cells each for weekdays and weekends,
                // shaded by how full, with the hours marked beneath.
                let grid = add(&document, &section, "div", "loadchart", "");
                let _ = grid.set_attribute(
                    "style",
                    &format!(
                        "grid-template-columns: auto repeat({}, 1fr)",
                        chart.hours.len()
                    ),
                );
                for row in &chart.rows {
                    add(&document, &grid, "span", "label", row.label);
                    for cell in &row.cells {
                        let el = add(&document, &grid, "i", "", "");
                        match cell {
                            Some((load, title)) => {
                                let _ = el.set_attribute("title", title);
                                let _ = el.set_attribute(
                                    "style",
                                    &format!("--load: {:.0}%", 100.0 * load.clamp(0.0, 1.0)),
                                );
                            }
                            None => el.set_class_name("none"),
                        }
                    }
                }
                add(&document, &grid, "span", "", "");
                for &hour in &chart.hours {
                    let label = if hour % 3 == 0 {
                        super::hour_label(hour)
                    } else {
                        String::new()
                    };
                    add(
                        &document,
                        &grid,
                        "span",
                        "hour",
                        &label.replace(" AM", "a").replace(" PM", "p"),
                    );
                }
            }
            if !info.notices.is_empty() {
                let section = add(&document, &inner, "section", "", "");
                add(&document, &section, "h3", "", "Notices");
                let list = add(&document, &section, "div", "bulletins", "");
                for b in &info.notices {
                    let article = add(&document, &list, "article", "", "");
                    add(&document, &article, "h4", "", &b.title);
                    if !b.when.is_empty() {
                        add(&document, &article, "div", "when", &b.when);
                    }
                    if !b.text.is_empty() {
                        add(&document, &article, "p", "", &b.text);
                    }
                }
                list.set_scroll_top(scrolled);
            }
            self.built = content;
        }
    }

    /// A new `tag` element in `parent`, with a class and text (WSF's text
    /// only ever as text).
    /// A "See older" button, saying where it leads (read by web/setup.rs).
    fn add_older_button(document: &Document, parent: &Element, link: &super::OlderLink) {
        let button = add(document, parent, "button", "older-link", "See older");
        let _ = button.set_attribute("type", "button");
        let _ = button.set_attribute("data-older-route", &link.route.to_string());
        let _ = button.set_attribute("data-older-terminal", &link.terminal.to_string());
        let _ = button.set_attribute("data-older-before", &link.before_ms.to_string());
        let _ = button.set_attribute("data-older-title", &link.title);
    }

    /// A timetable's row, under its trip day if one starts here; with how
    /// it went spelled out if `detailed`, or else on resting the pointer.
    fn add_row(document: &Document, list: &Element, row: &TimetableRow, detailed: bool) {
        if let Some(day) = &row.day_break {
            add(document, list, "li", "day", day);
        }
        let class = match row.when {
            When::Gone => "gone",
            When::Next => "next",
            When::Later => "",
        };
        let item = add(document, list, "li", class, "");
        if row.cancelled {
            let _ = item.class_list().add_1("cancelled");
        }
        // Departs: the time, and where to if the timetable goes to several
        // places.
        let departs = add(document, &item, "span", "departs", "");
        add(document, &departs, "span", "time", &row.time);
        if !row.to.is_empty() {
            add(document, &departs, "span", "to", &row.to);
        }
        // Arrive by: the recommended time, its sentence on resting the
        // pointer on the row. For a sailing gone, how it went instead.
        let arrive = add(document, &item, "span", "advice", "");
        if let Some(advice) = &row.advice {
            let _ = item.set_attribute("title", &advice.sentence);
            let class = match advice.urgency {
                Urgency::Rush => "advice rush",
                Urgency::Guidance => "advice guidance",
                Urgency::Room => "advice room",
                Urgency::ArriveBy => "advice",
            };
            arrive.set_class_name(class);
            arrive.set_text_content(Some(&advice.tag));
        }
        if let Some(outcome) = &row.outcome {
            let _ = item.set_attribute("title", &outcome.detail);
            arrive.set_class_name(match outcome.kind {
                OutcomeKind::Fine => "advice outcome fine",
                OutcomeKind::Late => "advice outcome late",
                OutcomeKind::Cancelled => "advice outcome cancelled",
                OutcomeKind::Quiet => "advice outcome quiet",
            });
            arrive.set_text_content(Some(&outcome.tag));
        }
        // Vessel, with its footnotes' numbers.
        let boat = add(document, &item, "span", "vessel", &row.vessel);
        if row.tentative {
            let _ = boat.class_list().add_1("tentative");
            let _ = boat.set_attribute(
                "title",
                "WSF's plan so far: boats are often reassigned by morning",
            );
        }
        add(document, &boat, "sup", "marks", &row.marks);
        if detailed && let Some(outcome) = &row.outcome {
            add(document, &item, "span", "detail", &outcome.detail);
        }
    }

    fn add(document: &Document, parent: &Element, tag: &str, class: &str, text: &str) -> Element {
        let el = document.create_element(tag).expect("element");
        if !class.is_empty() {
            el.set_class_name(class);
        }
        if !text.is_empty() {
            el.set_text_content(Some(text));
        }
        let _ = parent.append_child(&el);
        el
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet;
    use crate::geo::LonLat;

    fn terminal(id: u32, name: &str) -> Terminal {
        Terminal {
            id,
            name: name.to_string(),
            position: LonLat::new(-122.4, 47.6),
        }
    }

    #[test]
    fn finds_boats_terminals_and_routes_by_name() {
        let mut fleet = Fleet::default();
        let doc: ferry_model::api::LiveVessels = serde_json::from_str(
            r#"{"source":{"fetched_ms":0,"next_update_ms":0,"stale":false,"error":null},
                "vessels":[{"id":6,"lon":-122.4,"lat":47.5,"speed_kn":0,"heading_deg":0,
                  "at_dock":true,"in_service":true,"departing":9,"arriving":null,
                  "scheduled_departure_ms":null,"left_dock_ms":null,"eta_ms":null,
                  "reported_ms":1793297887000,"note":null,"wake":[]}]}"#,
        )
        .unwrap();
        let names = std::collections::HashMap::from([(6, "Kitsap".to_string())]);
        fleet.vessels = fleet::vessels_from(&doc, &names, &Default::default());
        let mut terminals = Terminals::default();
        terminals.list = vec![terminal(12, "Kingston"), terminal(9, "Fauntleroy")];
        let routes = [ServiceRoute {
            name: "Edmonds / Kingston".into(),
            legs: vec![],
            terminals: BTreeSet::from([8, 12]),
        }];
        let all = entries(&fleet, &terminals, &routes);
        let found = matches("ki", &all);
        let names: Vec<(&str, &str)> = found.iter().map(|e| (e.name.as_str(), e.kind)).collect();
        assert_eq!(
            names,
            [
                ("Kitsap", "Ferry"),
                ("Kingston", "Terminal"),
                ("Edmonds / Kingston", "Route")
            ]
        );
        assert_eq!(found[2].target, Target::Route(0));

        let clock = |ms: f64| format!("t{}", (ms / 60_000.0) as i64 % 1000);
        let stats: CrossingStats = serde_json::from_str(
            r#"{"route":"edmonds-kingston","days":30,
                "pairs":[{"terminals":[8,12],"trips":41,"median_min":31.2,"p90_min":36}]}"#,
        )
        .unwrap();
        let loaded = |_: usize| Some(&stats);
        // Kingston's sailings, one gone and two to come (now is
        // 1_793_297_887_000), the last with a footnote.
        let schedule: RouteSchedule = serde_json::from_str(
            r#"{"route":"edmonds-kingston","days":[{"trip_date":"2026-10-29","season":"Fall 2026",
                "pdf_url":null,"terminals":[{"terminal":12,"notes":["Walk-on only"],
                "departures":[
                  {"departs_ms":1793290000000,"vessel_id":2,"vessel":"Walla Walla",
                   "stops":[{"terminal":8,"arrives_ms":null}],"loading":"both","notes":[]},
                  {"departs_ms":1793293000000,"vessel_id":2,"vessel":"Walla Walla",
                   "stops":[{"terminal":8,"arrives_ms":null}],"loading":"both","notes":[]},
                  {"departs_ms":1793297000000,"vessel_id":1,"vessel":"Spokane",
                   "stops":[{"terminal":8,"arrives_ms":null}],"loading":"both","notes":[]},
                  {"departs_ms":1793299000000,"vessel_id":1,"vessel":"Spokane",
                   "stops":[{"terminal":8,"arrives_ms":null}],"loading":"both","notes":[]},
                  {"departs_ms":1793301000000,"vessel_id":2,"vessel":"Walla Walla",
                   "stops":[{"terminal":8,"arrives_ms":null}],"loading":"both","notes":[0]}]}]}]}"#,
        )
        .unwrap();
        let scheduled = |_: usize| Loaded::Here(&schedule);
        // Kitsap left Kingston (12) 82% full; Kingston to Edmonds 7 AM
        // weekday sailings run full.
        let loads: VesselLoads = serde_json::from_str(
            r#"{"vessel":6,"sailings":[{"from":12,"to":8,"scheduled_ms":1793297000000,
                "left_ms":1793297060000,"load":0.82,"capacity":200,"free":36,
                "reading_age_min":7.4}]}"#,
        )
        .unwrap();
        let use_stats: LoadStats = serde_json::from_str(
            r#"{"route":"edmonds-kingston","vessel":null,"days":30,"directions":[
                {"from":12,"to":8,"sailings":3,"median_load":0.9,"hours":[
                  {"weekend":false,"hour":7,"sailings":2,"median_load":0.97,"full_share":1.0},
                  {"weekend":false,"hour":9,"sailings":1,"median_load":0.4,"full_share":0.0}]}]}"#,
        )
        .unwrap();
        let vessel_loads = |_: u32| Loaded::Here(&loads);
        let route_loads = |_: usize| Loaded::Here(&use_stats);
        // Kingston's next sailing usually fills: arrive 30 minutes early;
        // the one after usually has room.
        let kingston_advice: TerminalAdvice = serde_json::from_str(
            r#"{"terminal":12,"reservation":null,"sailings":[
                {"departs_ms":1793299000000,"holiday":null,"advice":{"kind":"arrive_by",
                  "arrive_by_ms":1793297200000,"likely":0.9,"usually_full_ms":1793297800000,
                  "sailings":6,"basis":"Fridays","pace":null}},
                {"departs_ms":1793301000000,"holiday":null,
                  "advice":{"kind":"room","sailings":9,"basis":"weekdays"}}]}"#,
        )
        .unwrap();
        let advice = |id: u32| {
            if id == 12 {
                Loaded::Here(&kingston_advice)
            } else {
                Loaded::Missing
            }
        };
        let scope = Scope {
            fleet: &fleet,
            terminals: &terminals,
            routes: &routes,
            legs: &[],
            crossings: &loaded,
            schedules: &scheduled,
            sailings: &|_, _| Loaded::Missing,
            vessel_loads: &vessel_loads,
            route_loads: &route_loads,
            advice: &advice,
            now_ms: 1_793_297_887_000.0,
            clock: &clock,
            date_clock: &clock,
        };
        let kitsap = scope.info(Target::Vessel(6));
        assert_eq!(
            kitsap.wikipedia.as_deref(),
            Some("https://en.wikipedia.org/wiki/Special:Search?search=MV+Kitsap&go=Go")
        );
        assert_eq!(
            wikipedia_url("Walla Walla"),
            "https://en.wikipedia.org/wiki/Special:Search?search=MV+Walla+Walla&go=Go"
        );
        assert!(wikipedia_url("Tokitae & co").contains("search=MV+Tokitae+%26+co&"));
        assert_eq!(kitsap.facts[0], ("Status".into(), "At the dock".into()));
        assert!(
            kitsap
                .facts
                .iter()
                .any(|(l, v)| l == "Heading" && v == "0° (N)")
        );
        assert!(kitsap.facts.iter().any(|(l, v)| l == "Car deck"
            && v == "about 80% full leaving Kingston at t284 (counted 7 min before it left)"));
        assert_eq!(
            kitsap.lists[0].1,
            ["t283 Kingston → Terminal 8 · about 80% full"]
        );
        let fauntleroy = scope.info(Target::Terminal(9));
        assert_eq!(fauntleroy.facts[0], ("At the dock".into(), "Kitsap".into()));
        let route = scope.info(Target::Route(0));
        assert_eq!(route.facts[1].1, "31 min (median, last 30 days)");
        // One timetable per terminal; Kingston's sailings all go to
        // Edmonds (named "Terminal 8" here), so the heading says so.
        let headings: Vec<&str> = route
            .timetables
            .iter()
            .map(|t| t.heading.as_str())
            .collect();
        assert_eq!(headings, ["From Terminal 8", "From Kingston to Terminal 8"]);
        assert_eq!(
            route.timetables[0].caption,
            "No sailings scheduled through Oct 29; the next day's schedule isn't available"
        );
        let kingston = &route.timetables[1];
        assert_eq!(kingston.caption, "Fall 2026 schedule, Oct 29");
        let rows: Vec<(&str, When, &str)> = (kingston.rows.iter())
            .map(|r| (r.vessel.as_str(), r.when, r.marks.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                // Three gone; the last two shown.
                ("Walla Walla", When::Gone, ""),
                ("Spokane", When::Gone, ""),
                ("Spokane", When::Next, ""),
                ("Walla Walla", When::Later, "1")
            ]
        );
        assert_eq!(kingston.rows[0].to, "");
        assert_eq!(kingston.notes, ["Walk-on only"]);
        // Arrival advice: none for the sailing gone, a time for the next,
        // room for the one after.
        let tags: Vec<Option<(&str, Urgency)>> = (kingston.rows.iter())
            .map(|r| r.advice.as_ref().map(|a| (a.tag.as_str(), a.urgency)))
            .collect();
        assert_eq!(
            tags,
            [
                None,
                None,
                Some(("t286", Urgency::ArriveBy)),
                Some(("room", Urgency::Room))
            ]
        );
        let sentence = "Arrive by t286 to likely make the t316 \
                        (usually full by t296, based on 6 recent Fridays)";
        assert_eq!(kingston.rows[2].advice.as_ref().unwrap().sentence, sentence);
        let at_kingston = scope.info(Target::Terminal(12));
        assert!(!at_kingston.facts.iter().any(|(l, _)| l == "Arrive by car"));
        assert_eq!(
            at_kingston.timetables[0].heading,
            "Departures to Terminal 8"
        );
        // Car deck use: 7 to 9 AM, nothing at 8.
        let chart = &route.load_charts[0];
        assert_eq!(chart.heading, "Cars, Kingston → Terminal 8");
        assert_eq!(
            chart.caption,
            "Median 90% full over 3 sailings, last 30 days; darker is fuller (cars only)"
        );
        assert_eq!(chart.hours, [7, 8, 9]);
        let row = &chart.rows[0];
        assert_eq!((row.label, row.cells.len()), ("Weekdays", 3));
        assert_eq!(row.cells[1], None);
        let (load, title) = row.cells[0].as_ref().unwrap();
        assert_eq!(*load, 0.97);
        assert_eq!(
            title,
            "7 AM weekdays: 97% full (median of 2 sailings), 2 left full"
        );
        assert_eq!(
            (about_full(0.96), about_full(0.42)),
            ("full".into(), "about 40% full".into())
        );
        assert_eq!(
            (hour_label(0), hour_label(12), hour_label(15)),
            ("12 AM".into(), "12 PM".into(), "3 PM".into())
        );
        assert_eq!(scope.info(Target::Vessel(99)), Info::default());
        let loading = |_: usize| None;
        let pending = |_: usize| Loaded::Pending;
        let pending_stats = |_: usize| Loaded::Pending;
        let scope = Scope {
            crossings: &loading,
            schedules: &pending,
            sailings: &|_, _| Loaded::Missing,
            route_loads: &pending_stats,
            ..scope
        };
        let route = scope.info(Target::Route(0));
        assert_eq!(route.facts[1].1, "Loading…");
        assert_eq!(route.timetables[1].caption, "Loading the schedule…");
        assert_eq!(route.load_charts[0].caption, "Loading…");
        assert_eq!(trip_day("2026-01-05").as_deref(), Some("Jan 5"));
        assert_eq!(trip_day("soon"), None);
    }

    /// A trip day, its season, and the ms each sailing leaves, and whether
    /// it has the day's footnote.
    type TestDay<'a> = (&'a str, &'a str, &'a [(f64, bool)]);

    /// A timetable from terminal 12 (to 8) at `now`, of two days' sailings.
    fn timetable_of(days: &[TestDay<'_>], now: f64) -> Timetable {
        timetable_with(days, None, now)
    }

    /// The same, with how sailings went.
    fn timetable_with(days: &[TestDay<'_>], record: Option<&RouteSailings>, now: f64) -> Timetable {
        let days: Vec<serde_json::Value> = days
            .iter()
            .map(|(date, season, sailings)| {
                let departures: Vec<serde_json::Value> = sailings
                    .iter()
                    .map(|(ms, noted)| {
                        serde_json::json!({
                            "departs_ms": *ms as i64, "vessel_id": 1, "vessel": "Spokane",
                            "stops": [{"terminal": 8, "arrives_ms": null}],
                            "loading": "both", "notes": if *noted { vec![0] } else { vec![] },
                        })
                    })
                    .collect();
                let terminals = if sailings.is_empty() {
                    serde_json::json!([])
                } else {
                    serde_json::json!([{
                        "terminal": 12, "notes": [format!("Note for {date}")],
                        "departures": departures,
                    }])
                };
                serde_json::json!({
                    "trip_date": date, "season": season, "pdf_url": null, "terminals": terminals,
                })
            })
            .collect();
        let schedule: RouteSchedule = serde_json::from_value(serde_json::json!({
            "route": "edmonds-kingston", "days": days,
        }))
        .unwrap();
        let schedules = |_: usize| Loaded::Here(&schedule);
        let sailings = |_: usize, _: u32| record.map_or(Loaded::Missing, Loaded::Here);
        with_scope(now, &schedules, &sailings, |scope| {
            scope.timetable("Departures".to_string(), 0, 12)
        })
    }

    /// Calls `f` with a scope holding only the schedules and sailings.
    fn with_scope<'a, R>(
        now: f64,
        schedules: &'a dyn Fn(usize) -> Loaded<'a, RouteSchedule>,
        sailings: &'a dyn Fn(usize, u32) -> Loaded<'a, RouteSailings>,
        f: impl Fn(&Scope<'_>) -> R,
    ) -> R {
        fn clock(ms: f64) -> String {
            format!("t{}", (ms / HOUR) as i64)
        }
        // Scope's parts share one lifetime, which these must outlast.
        let fleet: &'static Fleet = Box::leak(Box::default());
        let terminals: &'static Terminals = Box::leak(Box::default());
        let scope = Scope {
            fleet,
            terminals,
            routes: &[],
            legs: &[],
            crossings: &|_| None,
            schedules,
            sailings,
            vessel_loads: &|_| Loaded::Missing,
            route_loads: &|_| Loaded::Missing,
            advice: &|_| Loaded::Missing,
            now_ms: now,
            clock: &clock,
            date_clock: &clock,
        };
        f(&scope)
    }

    /// Sailings on record, at these hours, each with how it went.
    fn records(sailings: Vec<(f64, serde_json::Value)>, more: bool) -> RouteSailings {
        let sailings: Vec<serde_json::Value> = sailings
            .into_iter()
            .map(|(hour, outcome)| {
                serde_json::json!({
                    "trip_date": "2026-10-30", "departs_ms": (hour * HOUR) as i64, "stops": [8],
                    "vessel": "Spokane", "first_vessel": "Spokane", "outcome": outcome,
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "route": "edmonds-kingston", "terminal": 12, "sailings": sailings, "more": more,
        }))
        .unwrap()
    }

    fn ran(hour: f64, vessel: &str, late_min: i32) -> serde_json::Value {
        let left = (hour * HOUR) as i64 + i64::from(late_min) * 60_000;
        serde_json::json!({"kind": "ran", "left_ms": left, "vessel": vessel, "late_min": late_min})
    }

    /// (time, outcome's tag, vessel) of each row.
    fn outcomes_of(table: &Timetable) -> Vec<(&str, Option<&str>, &str)> {
        (table.rows.iter())
            .map(|r| {
                (
                    r.time.as_str(),
                    r.outcome.as_ref().map(|o| o.tag.as_str()),
                    r.vessel.as_str(),
                )
            })
            .collect()
    }

    #[test]
    fn says_how_sailings_went_and_who_ran_them() {
        const DAY: TestDay<'_> = (
            "2026-10-30",
            "Fall 2026",
            &[
                (6.0 * HOUR, false),
                (7.0 * HOUR, false),
                (8.0 * HOUR, false),
                (11.0 * HOUR, false),
            ],
        );
        let doc = records(
            vec![
                (6.0, ran(6.0, "Spokane", 1)),
                // Salish ran what WSF has as Spokane's, 12 minutes late.
                (7.0, ran(7.0, "Salish", 12)),
                (8.0, serde_json::json!({"kind": "no_record"})),
            ],
            false,
        );
        let table = timetable_with(&[DAY], Some(&doc), 10.0 * HOUR);
        // The last two gone; the first of them behind "See older".
        assert_eq!(
            outcomes_of(&table),
            [
                ("t7", Some("12 min late"), "Salish"),
                ("t8", Some("no record"), "Spokane"),
                ("t11", None, "Spokane"),
            ]
        );
        assert_eq!(
            table.older,
            Some(OlderLink {
                route: 0,
                terminal: 12,
                before_ms: (7.0 * HOUR) as i64,
                title: table.heading.clone(),
            })
        );
    }

    #[test]
    fn a_withdrawn_sailing_stays_marked_and_isnt_next() {
        const DAY: TestDay<'_> = ("2026-10-30", "Fall 2026", &[(12.0 * HOUR, false)]);
        let withdrawn = serde_json::json!({"kind": "cancelled", "withdrawn_ms": 0});
        let doc = records(vec![(11.0, withdrawn)], false);
        let table = timetable_with(&[DAY], Some(&doc), 10.0 * HOUR);
        assert_eq!(
            outcomes_of(&table),
            [
                ("t11", Some("cancelled"), "Spokane"),
                ("t12", None, "Spokane")
            ]
        );
        assert!(table.rows[0].cancelled);
        assert_eq!(table.rows[0].when, When::Later);
        assert_eq!(table.rows[1].when, When::Next);
        // No older sailings, so no way to them.
        assert_eq!(table.older, None);
    }

    #[test]
    fn offers_older_sailings_even_with_none_scheduled() {
        const DAY: TestDay<'_> = ("2026-10-30", "", &[]);
        let table = timetable_with(
            &[DAY, ("2026-10-31", "", &[])],
            Some(&records(vec![], true)),
            10.0 * HOUR,
        );
        assert!(table.rows.is_empty());
        assert_eq!(table.older.map(|o| o.before_ms), Some((10.0 * HOUR) as i64));
    }

    #[test]
    fn pages_older_sailings_by_trip_day() {
        let mut older = records(
            vec![
                (9.0, ran(9.0, "Spokane", 0)),
                (8.0, serde_json::json!({"kind": "unknown"})),
            ],
            true,
        );
        older.sailings[1].trip_date = "2026-10-29".to_string();
        older.sailings[1].first_vessel = "Puyallup".to_string();
        let schedules = |_: usize| Loaded::Missing;
        let sailings = |_: usize, _: u32| Loaded::Missing;
        let page = with_scope(0.0, &schedules, &sailings, |scope| {
            scope.older_page("Departures".to_string(), &[&older], false)
        });
        assert_eq!(page.title, "Departures");
        assert!(page.more);
        assert_eq!(page.status, "");
        let days: Vec<Option<&str>> = page.rows.iter().map(|r| r.day_break.as_deref()).collect();
        assert_eq!(days, [Some("Friday, Oct 30"), Some("Thursday, Oct 29")]);
        // No telling how it went: nothing claimed.
        assert_eq!(page.rows[1].outcome, None);
        assert!(
            page.rows[0]
                .outcome
                .as_ref()
                .unwrap()
                .detail
                .contains("Spokane left at")
        );
        // While loading it says so, and offers no more; empty, it says that.
        let page = with_scope(0.0, &schedules, &sailings, |scope| {
            scope.older_page("Departures".to_string(), &[&older], true)
        });
        assert_eq!((page.status.as_str(), page.more), ("Loading…", false));
        let page = with_scope(0.0, &schedules, &sailings, |scope| {
            scope.older_page("Departures".to_string(), &[], false)
        });
        assert_eq!(page.status, "No older sailings on record.");
    }

    const HOUR: f64 = 3_600_000.0;

    /// (time, day break) of each row.
    fn rows_of(table: &Timetable) -> Vec<(&str, Option<&str>)> {
        (table.rows.iter())
            .map(|r| (r.time.as_str(), r.day_break.as_deref()))
            .collect()
    }

    #[test]
    fn runs_on_into_the_next_day_with_a_divider() {
        let now = 22.0 * HOUR;
        let table = timetable_of(
            &[
                ("2026-10-30", "Fall 2026", &[(23.0 * HOUR, false)]),
                (
                    "2026-10-31",
                    "Fall 2026",
                    &[
                        (29.0 * HOUR, false),
                        (34.0 * HOUR, false),
                        (35.0 * HOUR, false),
                    ],
                ),
            ],
            now,
        );
        // To 12 hours from now: the 35th hour falls just outside.
        assert_eq!(
            rows_of(&table),
            [
                ("t23", None),
                ("t29", Some("Saturday, Oct 31 · boats may change")),
                ("t34", None)
            ]
        );
        assert_eq!(table.caption, "Fall 2026 schedule, Oct 30");
        assert_eq!(table.rows[0].when, When::Next);
        // Today's boat stands; the next day's are only the plan so far.
        let tentative: Vec<bool> = table.rows.iter().map(|r| r.tentative).collect();
        assert_eq!(tentative, [false, true, true]);
    }

    #[test]
    fn says_boats_may_change_when_only_a_later_day_is_shown() {
        // Today's sailings are all behind the window's start: with none on
        // record today, only the next day's show.
        let table = timetable_of(
            &[
                ("2026-10-30", "", &[]),
                ("2026-10-31", "Fall 2026", &[(29.0 * HOUR, false)]),
            ],
            22.0 * HOUR,
        );
        assert_eq!(
            table.caption,
            "Fall 2026 schedule, Oct 31 · boats may change"
        );
        assert_eq!(table.rows[0].day_break, None);
        assert!(table.rows[0].tentative);
    }

    #[test]
    fn shows_all_of_the_day_however_far_off_and_nothing_of_the_next_unless_near() {
        let table = timetable_of(
            &[
                (
                    "2026-10-30",
                    "Fall 2026",
                    &[(9.0 * HOUR, false), (22.0 * HOUR, false)],
                ),
                ("2026-10-31", "Fall 2026", &[(29.0 * HOUR, false)]),
            ],
            8.0 * HOUR,
        );
        assert_eq!(rows_of(&table), [("t9", None), ("t22", None)]);
    }

    #[test]
    fn reaches_past_the_window_for_the_next_sailing_with_nothing_sooner() {
        let table = timetable_of(
            &[
                ("2026-10-30", "Fall 2026", &[(20.0 * HOUR, false)]),
                (
                    "2026-10-31",
                    "Winter 2027",
                    &[(40.0 * HOUR, false), (41.0 * HOUR, false)],
                ),
            ],
            21.0 * HOUR,
        );
        // The one gone, then only the next, however far, naming the new
        // season's schedule; that's still the caption's day.
        assert_eq!(
            rows_of(&table),
            [
                ("t20", None),
                (
                    "t40",
                    Some("Saturday, Oct 31 · Winter 2027 schedule · boats may change")
                )
            ]
        );
        assert_eq!(table.caption, "Fall 2026 schedule, Oct 30");
        assert_eq!(table.rows[1].when, When::Next);
    }

    #[test]
    fn numbers_footnotes_across_days() {
        let table = timetable_of(
            &[
                ("2026-10-30", "Fall 2026", &[(23.0 * HOUR, true)]),
                ("2026-10-31", "Fall 2026", &[(29.0 * HOUR, true)]),
            ],
            22.0 * HOUR,
        );
        let marks: Vec<&str> = table.rows.iter().map(|r| r.marks.as_str()).collect();
        assert_eq!(marks, ["1", "2"]);
        assert_eq!(table.notes, ["Note for 2026-10-30", "Note for 2026-10-31"]);
    }

    #[test]
    fn says_what_is_known_when_there_are_no_sailings() {
        let none: &[(f64, bool)] = &[];
        // Nothing published for either day.
        let table = timetable_of(
            &[("2026-10-30", "", none), ("2026-10-31", "", none)],
            10.0 * HOUR,
        );
        assert!(table.rows.is_empty());
        assert_eq!(table.caption, "No sailings scheduled through Oct 31");
        // The last has gone, and nothing's published for the next day.
        let table = timetable_of(
            &[
                ("2026-10-30", "Fall 2026", &[(9.0 * HOUR, false)]),
                ("2026-10-31", "", none),
            ],
            10.0 * HOUR,
        );
        assert_eq!(rows_of(&table), [("t9", None)]);
        assert_eq!(table.rows[0].when, When::Gone);
        assert_eq!(table.caption, "No more sailings scheduled through Oct 31");
        // The next day couldn't be loaded: that's not the same as none.
        let table = timetable_of(&[("2026-10-30", "", none)], 10.0 * HOUR);
        assert_eq!(
            table.caption,
            "No sailings scheduled through Oct 30; the next day's schedule isn't available"
        );
        // Nothing could be loaded at all.
        let table = timetable_of(&[], 10.0 * HOUR);
        assert_eq!(table.caption, "The schedule isn't available right now");
    }

    #[test]
    fn names_days_of_the_week() {
        assert_eq!(weekday("2026-10-31"), Some("Saturday"));
        assert_eq!(weekday("2027-01-01"), Some("Friday"));
        assert_eq!(weekday("2028-02-29"), Some("Tuesday"));
        assert_eq!(weekday("soon"), None);
    }
}
