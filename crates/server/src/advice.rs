//! When to arrive by car: from how sailings like it filled before, the
//! latest arrival that's likely to make it without much longer a wait.
//!
//! A sailing's drive-up count falls as cars check in (once the boat before
//! it has gone), so each past sailing says how many minutes before it left
//! it filled: or that it hadn't, as far as it was counted. From the
//! sailings like one (same terminal and destination, leaving within 15
//! minutes of the same time of day, on the same day of the week, pooled
//! toward weekdays or weekends while that day's own record is thin, the
//! recent ones weighing more), the chance of a space arriving so many
//! minutes early is a survival estimate (Kaplan-Meier, weighted), which
//! counts the sailings that stopped being counted early as having had room
//! that long and no further. The advice is the arrival that costs the
//! fewest expected minutes, waiting at the terminal or for the next boat
//! when this one's missed, its chance kept from 70% to 97%.
//!
//! Counts are of standard car spaces, so a sailing counts as full with a
//! few left (`FULL_SPACES`): a truck may not fit in them. Demand is cars,
//! not the share of a deck: a smaller boat fills sooner on the same day,
//! and a day that's busier so far than usual (`pace`) fills sooner too.
//! On routes that take reservations the drive-up count is the space left
//! for drive-ups, which rises as well as falls as reservations are held
//! and released, so it's taken as it is.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use ferry_model::api::{
    Advice, AdviceCheck, GuidanceReason, ReservationAdvice, SailingAdvice, TerminalAdvice,
    TerminalCheck,
};
use sqlx::{PgPool, Row};

use serde::{Deserialize, Serialize};

use crate::calendar;
use crate::http::{AppState, Doc};

/// A sailing counts as full with this many spaces left, or fewer.
pub const FULL_SPACES: i32 = 3;
/// How far apart (min, time of day) sailings may leave to be alike.
pub const ALIKE_MIN: i64 = 15;
/// Older sailings count for half every this many days.
pub const HALF_LIFE_DAYS: f64 = 28.0;
/// A day of the week's own sailings outweigh weekdays' (or weekends') as
/// a whole once there are this many: its weight is n / (n + `POOL_K`).
pub const POOL_K: f64 = 5.0;
/// Fewer sailings than this (weighted) aren't enough to go on.
pub const LEAST_SAILINGS: f64 = 3.0;
/// The chance an advised arrival gets on stays within these.
pub const LEAST_LIKELY: f64 = 0.70;
pub const MOST_LIKELY: f64 = 0.97;
/// Usually has room: this likely arriving `LATEST_LEAD_MIN` early.
pub const ROOM_LIKELY: f64 = 0.95;
/// Advised arrivals are this many minutes early, in steps of `STEP_MIN`.
pub const LATEST_LEAD_MIN: u32 = 10;
pub const EARLIEST_LEAD_MIN: u32 = 180;
pub const STEP_MIN: u32 = 5;
/// Missing the last sailing of the day costs as much as this (min).
pub const LAST_SAILING_GAP_MIN: f64 = 600.0;
/// On holidays and special days, this much more time at least (min).
pub const HOLIDAY_EXTRA_MIN: u32 = 15;

/// A count of the drive-up space a sailing had left: this many minutes
/// before it was scheduled to leave, below zero by as many cars as were
/// waiting beyond it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Count {
    pub lead_min: f64,
    pub spaces: i32,
}

/// A sailing's counts, earliest (most minutes before it left) first; the
/// boat's car spaces; and how close to leaving it was counted (min).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Past {
    pub day: i64,
    pub capacity: i32,
    pub counts: Vec<Count>,
    pub counted_to_min: f64,
}

/// How a past sailing went, for a boat like today's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// Full from this many minutes before it left.
    Filled(f64),
    /// Not full as far as it was counted: to this many minutes before.
    Open(f64),
}

impl Past {
    /// How it would have gone for a boat of `capacity` car spaces with
    /// `pace` times the cars (both only where `scale`: see the module's
    /// notes on reservations).
    pub fn outcome(&self, capacity: i32, pace: f64, scale: bool) -> Outcome {
        for c in &self.counts {
            let spaces = if scale {
                f64::from(capacity) - pace * f64::from(self.capacity - c.spaces)
            } else {
                f64::from(c.spaces)
            };
            if spaces <= f64::from(FULL_SPACES) {
                return Outcome::Filled(c.lead_min);
            }
        }
        Outcome::Open(self.counted_to_min)
    }

    /// Cars checked in by `lead_min` before it left: the count then in
    /// effect, or none checked in if it wasn't counted yet.
    pub fn cars_by(&self, lead_min: f64) -> f64 {
        let spaces = self
            .counts
            .iter()
            .take_while(|c| c.lead_min >= lead_min)
            .last()
            .map_or(self.capacity, |c| c.spaces);
        f64::from(self.capacity - spaces)
    }

    /// Whether it had space `lead_min` before it left, if it was counted
    /// then.
    pub fn had_space(&self, lead_min: f64) -> Option<bool> {
        if lead_min < self.counted_to_min {
            return None;
        }
        let spaces = self
            .counts
            .iter()
            .take_while(|c| c.lead_min >= lead_min)
            .last()
            .map_or(self.capacity, |c| c.spaces);
        Some(spaces > FULL_SPACES)
    }
}

/// The chance of a space against how early one arrives: a weighted
/// Kaplan-Meier estimate over outcomes, time running toward departure.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Curve {
    /// Each time sailings filled (min before leaving, earliest first) and
    /// the chance of a space arriving then or later.
    steps: Vec<(f64, f64)>,
    /// The sailings it's from, weighted, as an effective number.
    pub sailings: f64,
}

impl Curve {
    pub fn new(outcomes: &[(Outcome, f64)]) -> Self {
        let total: f64 = outcomes.iter().map(|(_, w)| w).sum();
        let squares: f64 = outcomes.iter().map(|(_, w)| w * w).sum();
        let sailings = if squares > 0.0 {
            total * total / squares
        } else {
            0.0
        };
        // Leads at which sailings filled, earliest first.
        let mut fills: Vec<f64> = outcomes
            .iter()
            .filter_map(|(o, _)| match o {
                Outcome::Filled(l) => Some(*l),
                Outcome::Open(_) => None,
            })
            .collect();
        fills.sort_by(|a, b| b.total_cmp(a));
        fills.dedup();
        let mut steps = Vec::with_capacity(fills.len());
        let mut chance = 1.0;
        for lead in fills {
            // At risk: not yet full or uncounted by then.
            let (mut at_risk, mut filled) = (0.0, 0.0);
            for (o, w) in outcomes {
                match *o {
                    Outcome::Filled(l) if l == lead => {
                        at_risk += w;
                        filled += w;
                    }
                    Outcome::Filled(l) | Outcome::Open(l) if l <= lead => at_risk += w,
                    _ => {}
                }
            }
            if at_risk > 0.0 {
                chance *= 1.0 - filled / at_risk;
            }
            steps.push((lead, chance));
        }
        Self { steps, sailings }
    }

    /// The chance of a space arriving `lead_min` before it leaves: after
    /// every fill at that lead or earlier.
    pub fn chance(&self, lead_min: f64) -> f64 {
        self.steps
            .iter()
            .take_while(|(l, _)| *l >= lead_min)
            .last()
            .map_or(1.0, |(_, c)| *c)
    }
}

/// A day of the week's curve, pooled toward its kind of day's.
pub struct Pooled {
    pub day: Curve,
    pub kind: Curve,
}

impl Pooled {
    /// How much the day's own sailings count.
    pub fn day_weight(&self) -> f64 {
        self.day.sailings / (self.day.sailings + POOL_K)
    }

    pub fn chance(&self, lead_min: f64) -> f64 {
        let w = self.day_weight();
        w * self.day.chance(lead_min) + (1.0 - w) * self.kind.chance(lead_min)
    }

    pub fn sailings(&self) -> f64 {
        self.day.sailings.max(self.kind.sailings)
    }
}

/// What to do: arrive this many minutes early, likely to get on with this
/// chance; or don't worry, it usually has room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    Room,
    ArriveBy { lead_min: u32, likely: f64 },
}

/// The advice given the chance of a space by lead, and how long until the
/// next sailing if this one's missed (min).
pub fn decide(chance: impl Fn(f64) -> f64, next_gap_min: f64) -> Decision {
    let leads: Vec<u32> = (LATEST_LEAD_MIN..=EARLIEST_LEAD_MIN)
        .step_by(STEP_MIN as usize)
        .collect();
    let at = |l: u32| chance(f64::from(l));
    if at(LATEST_LEAD_MIN) >= ROOM_LIKELY {
        return Decision::Room;
    }
    // Expected minutes: waiting at the terminal, and for the next boat if
    // there's no space.
    let cost = |l: u32| f64::from(l) + (1.0 - at(l)) * next_gap_min;
    let best = leads
        .iter()
        .copied()
        .min_by(|&a, &b| cost(a).total_cmp(&cost(b)).then(a.cmp(&b)))
        .unwrap_or(LATEST_LEAD_MIN);
    let target = at(best).clamp(LEAST_LIKELY, MOST_LIKELY);
    // The latest arrival that likely; failing any, the likeliest.
    let lead_min = leads
        .iter()
        .copied()
        .find(|&l| at(l) >= target)
        .unwrap_or_else(|| {
            leads
                .iter()
                .copied()
                .max_by(|&a, &b| at(a).total_cmp(&at(b)).then(b.cmp(&a)))
                .unwrap_or(EARLIEST_LEAD_MIN)
        });
    Decision::ArriveBy {
        lead_min,
        likely: at(lead_min),
    }
}

/// How much busier today's sailing is than usual so far: cars checked in
/// by `now_lead_min` before it leaves, against the (weighted) median of
/// past sailings' then. None too early to tell, or with too few cars.
pub fn pace(pasts: &[(&Past, f64)], today_cars: f64, now_lead_min: f64) -> Option<f64> {
    let mut usual: Vec<(f64, f64)> = pasts
        .iter()
        .map(|(p, w)| (p.cars_by(now_lead_min), *w))
        .collect();
    usual.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = usual.iter().map(|(_, w)| w).sum();
    let mut seen = 0.0;
    let median = usual.iter().find_map(|&(cars, w)| {
        seen += w;
        (seen >= total / 2.0).then_some(cars)
    })?;
    (median >= 10.0 && today_cars >= 10.0).then(|| (today_cars / median).clamp(0.5, 2.0))
}

/// A past sailing's weight, `age_days` before the one advised on.
pub fn recency_weight(age_days: f64) -> f64 {
    0.5f64.powf(age_days.max(0.0) / HALF_LIFE_DAYS)
}

// The advice from the record: each of a terminal's sailings today.

/// How far back sailings count (days).
const LOOKBACK_DAYS: i64 = 84;
/// The self-check looks back at most this far (days).
pub const CHECK_MAX_DAYS: u32 = 60;

/// WSF's guidance for drivers with a reservation, from the terminals on
/// routes that take them (Anacortes and the San Juan Islands, Port
/// Townsend / Coupeville): arrive 30 to 45 minutes early, and check in at
/// least 30 minutes before, or the reservation goes and the car waits on
/// standby. As the user gave it (2026-10-30), over WSF's reservations
/// page then, which asked 45 to 60 minutes for the San Juans.
fn reservation_advice(terminal: u32) -> Option<ReservationAdvice> {
    matches!(terminal, 1 | 10 | 11 | 13 | 15 | 17 | 18).then_some(ReservationAdvice {
        arrive_min: [30, 45],
        check_in_min: 30,
    })
}

/// WSF's general guidance on arriving (to the tollbooth), where there's
/// too little record to go on: at least 20 minutes early off-peak; 30 to
/// 60 at busy times (taken as 45, the middle); 30 to 45 on routes that
/// take reservations (taken as 45, the most). More on holidays.
pub const GUIDANCE_OFF_PEAK_MIN: u32 = 20;
pub const GUIDANCE_BUSY_MIN: u32 = 45;
pub const GUIDANCE_RESERVATIONS_MIN: u32 = 45;

fn guidance(departs_ms: i64, reservations: bool, holiday: bool, counted: bool) -> Advice {
    let busy = holiday
        || calendar::busy(
            calendar::pacific_day(departs_ms),
            calendar::pacific_minute(departs_ms),
        );
    let (reason, lead) = if reservations {
        (GuidanceReason::Reservations, GUIDANCE_RESERVATIONS_MIN)
    } else if busy {
        (GuidanceReason::Busy, GUIDANCE_BUSY_MIN)
    } else {
        (GuidanceReason::OffPeak, GUIDANCE_OFF_PEAK_MIN)
    };
    let lead_min = lead + if holiday { HOLIDAY_EXTRA_MIN } else { 0 };
    Advice::Guidance {
        arrive_by_ms: departs_ms - i64::from(lead_min) * 60_000,
        lead_min,
        reason,
        counted,
    }
}

/// A sailing's counts as recorded: when it was to leave, to where, and
/// how it went.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Recorded {
    departs_ms: i64,
    minute: i64,
    destination: u32,
    past: Past,
    reservations: bool,
}

/// A terminal's counts for sailings scheduled from `from_ms` to `to_ms`,
/// cancelled sailings left out.
async fn recorded(pool: &PgPool, terminal: u32, from_ms: i64, to_ms: i64) -> Result<Vec<Recorded>> {
    let rows = sqlx::query(
        "SELECT (extract(epoch FROM departs_at) * 1000)::bigint AS departs_ms, destination_id,
             (extract(epoch FROM observed_at) * 1000)::bigint AS observed_ms,
             (extract(epoch FROM COALESCE(seen_until, observed_at)) * 1000)::bigint AS until_ms,
             drive_up, oversold_by, capacity, cancelled, reservable IS NOT NULL AS reservable
         FROM sailing_space
         WHERE terminal_id = $1
           AND departs_at >= to_timestamp($2::double precision / 1000)
           AND departs_at < to_timestamp($3::double precision / 1000)
         ORDER BY departs_at, destination_id, observed_at",
    )
    .bind(terminal as i32)
    .bind(from_ms)
    .bind(to_ms)
    .fetch_all(pool)
    .await?;
    let mut out: Vec<Recorded> = Vec::new();
    // One sailing to one destination at a time.
    let mut current: Option<(Recorded, bool, i64)> = None;
    let finish = |c: Option<(Recorded, bool, i64)>, out: &mut Vec<Recorded>| {
        if let Some((mut r, cancelled, until_ms)) = c
            && !cancelled
            && r.past.capacity > 0
            && !r.past.counts.is_empty()
        {
            r.past.counted_to_min = ((r.departs_ms - until_ms) as f64 / 60_000.0).max(0.0);
            out.push(r);
        }
    };
    for row in &rows {
        let departs_ms: i64 = row.get("departs_ms");
        let destination = row.get::<i32, _>("destination_id") as u32;
        let same = current
            .as_ref()
            .is_some_and(|(r, ..)| r.departs_ms == departs_ms && r.destination == destination);
        if !same {
            finish(current.take(), &mut out);
            let day = calendar::pacific_day(departs_ms);
            current = Some((
                Recorded {
                    departs_ms,
                    minute: calendar::pacific_minute(departs_ms),
                    destination,
                    past: Past {
                        day,
                        capacity: 0,
                        counts: Vec::new(),
                        counted_to_min: 0.0,
                    },
                    reservations: false,
                },
                false,
                departs_ms,
            ));
        }
        let Some((r, cancelled, until_ms)) = current.as_mut() else {
            continue;
        };
        *cancelled |= row.get::<bool, _>("cancelled");
        r.reservations |= row.get::<bool, _>("reservable");
        if let Some(capacity) = row.get::<Option<i32>, _>("capacity") {
            r.past.capacity = capacity;
        }
        *until_ms = (*until_ms).max(row.get("until_ms"));
        let spaces = match (
            row.get::<Option<i32>, _>("oversold_by"),
            row.get::<Option<i32>, _>("drive_up"),
        ) {
            (Some(over), _) => Some(-over),
            (None, drive_up) => drive_up,
        };
        if let Some(spaces) = spaces {
            let observed_ms: i64 = row.get("observed_ms");
            r.past.counts.push(Count {
                lead_min: (departs_ms - observed_ms) as f64 / 60_000.0,
                spaces,
            });
        }
    }
    finish(current, &mut out);
    Ok(out)
}

/// Special days (special_days) by day number, with their names.
async fn special_days(pool: &PgPool) -> Result<HashMap<i64, String>> {
    let rows = sqlx::query("SELECT (day - DATE '1970-01-01')::int8 AS n, name FROM special_days")
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(|r| (r.get("n"), r.get("name"))).collect())
}

/// What a day is, for advice: a holiday or special day by name.
fn unusual(day: i64, special: &HashMap<i64, String>) -> Option<String> {
    special
        .get(&day)
        .cloned()
        .or_else(|| calendar::holiday(day).map(str::to_string))
}

/// Advice for a sailing to `destination` at `departs_ms` from the record
/// of the sailings like it: on usual days other than `day`'s (and, live,
/// before it), for a boat of `capacity` with demand at `pace`, missing it
/// costing `gap_min`. None with too few sailings like it.
struct Advised {
    decision: Decision,
    sailings: f64,
    day_weight: f64,
    /// Where most alike sailings filled, the usual time (min before).
    usual_fill_min: Option<f64>,
    /// How busy today's sailing is against them, where known (live).
    pace: Option<f64>,
}

#[allow(clippy::too_many_arguments)]
fn advise(
    records: &[Recorded],
    destination: u32,
    departs_ms: i64,
    earlier_only: bool,
    special: &HashMap<i64, String>,
    capacity: i32,
    pace: f64,
    gap_min: f64,
) -> Option<Advised> {
    let day = calendar::pacific_day(departs_ms);
    let minute = calendar::pacific_minute(departs_ms);
    let weekday = calendar::weekday(day);
    let weekend = weekday >= 5;
    let scale = !records.iter().any(|r| r.reservations);
    let alike: Vec<(&Past, f64)> = records
        .iter()
        .filter(|r| {
            r.destination == destination
                && (r.minute - minute).abs() <= ALIKE_MIN
                && r.past.day != day
                && (!earlier_only || r.past.day < day)
                && unusual(r.past.day, special).is_none()
        })
        .map(|r| (&r.past, recency_weight((day - r.past.day) as f64)))
        .collect();
    let outcomes = |keep: &dyn Fn(&Past) -> bool| -> Vec<(Outcome, f64)> {
        alike
            .iter()
            .filter(|(p, _)| keep(p))
            .map(|(p, w)| (p.outcome(capacity, pace, scale), *w))
            .collect()
    };
    let same_day = outcomes(&|p| calendar::weekday(p.day) == weekday);
    let same_kind = outcomes(&|p| (calendar::weekday(p.day) >= 5) == weekend);
    let pooled = Pooled {
        day: Curve::new(&same_day),
        kind: Curve::new(&same_kind),
    };
    if pooled.sailings() < LEAST_SAILINGS {
        return None;
    }
    // The usual fill time, where most filled: their weighted median.
    let pool = if pooled.day_weight() >= 0.5 {
        &same_day
    } else {
        &same_kind
    };
    let total: f64 = pool.iter().map(|(_, w)| w).sum();
    let mut fills: Vec<(f64, f64)> = pool
        .iter()
        .filter_map(|(o, w)| match o {
            Outcome::Filled(l) => Some((*l, *w)),
            Outcome::Open(_) => None,
        })
        .collect();
    let filled: f64 = fills.iter().map(|(_, w)| w).sum();
    fills.sort_by(|a, b| b.0.total_cmp(&a.0));
    let usual_fill_min = (filled >= total / 2.0)
        .then(|| {
            let mut seen = 0.0;
            fills.iter().find_map(|&(l, w)| {
                seen += w;
                (seen >= filled / 2.0).then_some(l)
            })
        })
        .flatten();
    Some(Advised {
        decision: decide(|l| pooled.chance(l), gap_min),
        sailings: pooled.sailings(),
        day_weight: pooled.day_weight(),
        usual_fill_min,
        pace: None,
    })
}

/// "Fridays", or "weekdays" while Fridays' own record is thin.
fn basis(day: i64, day_weight: f64) -> String {
    const DAYS: [&str; 7] = [
        "Mondays",
        "Tuesdays",
        "Wednesdays",
        "Thursdays",
        "Fridays",
        "Saturdays",
        "Sundays",
    ];
    let weekday = calendar::weekday(day);
    if day_weight >= 0.5 {
        DAYS[weekday as usize].to_string()
    } else if weekday >= 5 {
        "weekends".to_string()
    } else {
        "weekdays".to_string()
    }
}

/// How long (min) until the next sailing to `destination` after
/// `departs_ms` in `departures`, or as costly as missing the last.
fn gap_after(departures: &[(i64, Vec<u32>)], destination: u32, departs_ms: i64) -> f64 {
    departures
        .iter()
        .filter(|(ms, stops)| *ms > departs_ms && stops.contains(&destination))
        .map(|(ms, _)| (ms - departs_ms) as f64 / 60_000.0)
        .fold(LAST_SAILING_GAP_MIN, f64::min)
}

/// `terminal`'s sailings in the schedules, trip day by trip day: each
/// sailing's time and stops, in order. Days with none are left out.
fn departures_by_day(
    schedules: &HashMap<String, ferry_model::api::RouteSchedule>,
    terminal: u32,
) -> Vec<Vec<(i64, Vec<u32>)>> {
    let mut days: BTreeMap<&str, Vec<(i64, Vec<u32>)>> = BTreeMap::new();
    for day in schedules.values().flat_map(|s| &s.days) {
        for t in day.terminals.iter().filter(|t| t.terminal == terminal) {
            days.entry(&day.trip_date).or_default().extend(
                t.departures
                    .iter()
                    .map(|d| (d.departs_ms, d.stops.iter().map(|s| s.terminal).collect())),
            );
        }
    }
    days.into_values()
        .map(|mut departures| {
            departures.sort_by_key(|(ms, _)| *ms);
            departures.dedup_by_key(|(ms, _)| *ms);
            departures
        })
        .collect()
}

/// Advice for each of `terminal`'s sailings, `days` being its departures
/// trip day by trip day, from the record of its sailings (`records`), as of
/// `now_ms`. Each day apart: its last sailing is the last one, however soon
/// the next day's first.
fn advise_terminal(
    terminal: u32,
    days: &[Vec<(i64, Vec<u32>)>],
    records: &[Recorded],
    special: &HashMap<i64, String>,
    now_ms: i64,
) -> TerminalAdvice {
    let reservations = reservation_advice(terminal).is_some();
    let mut sailings = Vec::new();
    for departures in days {
        // A later day learns only from sailings that have left, not from
        // today's still filling.
        let first_ms = departures.first().map_or(i64::MAX, |(ms, _)| *ms);
        let settled: Vec<Recorded> = records
            .iter()
            .filter(|r| r.departs_ms < now_ms || r.departs_ms >= first_ms)
            .cloned()
            .collect();
        sailings.extend(departures.iter().map(|(departs_ms, stops)| {
            let holiday = unusual(calendar::pacific_day(*departs_ms), special);
            let advice = if settled.is_empty() {
                guidance(*departs_ms, reservations, holiday.is_some(), false)
            } else {
                advise_sailing(
                    &settled,
                    departures,
                    *departs_ms,
                    stops,
                    special,
                    holiday.is_some(),
                    now_ms,
                )
                .unwrap_or_else(|| guidance(*departs_ms, reservations, holiday.is_some(), true))
            };
            SailingAdvice {
                departs_ms: *departs_ms,
                advice,
                holiday,
            }
        }));
    }
    TerminalAdvice {
        terminal,
        reservation: reservation_advice(terminal),
        sailings,
    }
}

/// The start of the Pacific day at `utc_ms`, in UTC ms.
fn day_start_ms(utc_ms: i64) -> i64 {
    let local = calendar::pacific_day(utc_ms) * calendar::DAY_MS;
    local - calendar::pacific_offset_ms(local)
}

/// Works out each terminal's record of past sailings (those scheduled
/// before today, back `LOOKBACK_DAYS`) and stores it for the server to
/// load (`advice_history`). Run daily (`ferry-server compute-advice`).
/// The number of terminals stored.
pub async fn compute_history(pool: &PgPool, now_ms: i64) -> Result<usize> {
    let through = day_start_ms(now_ms);
    let terminals: Vec<i32> =
        sqlx::query_scalar("SELECT DISTINCT terminal_id FROM sailing_space ORDER BY 1")
            .fetch_all(pool)
            .await?;
    for &terminal in &terminals {
        let from = through - LOOKBACK_DAYS * calendar::DAY_MS;
        let records = recorded(pool, terminal as u32, from, through).await?;
        sqlx::query(
            "INSERT INTO advice_history (terminal_id, computed_at, through_ms, records)
             VALUES ($1, now(), $2, $3)
             ON CONFLICT (terminal_id) DO UPDATE SET computed_at = now(),
                 through_ms = EXCLUDED.through_ms, records = EXCLUDED.records",
        )
        .bind(terminal)
        .bind(through)
        .bind(sqlx::types::Json(&records))
        .execute(pool)
        .await?;
    }
    Ok(terminals.len())
}

/// The stored records of past sailings, as loaded: each terminal's, and
/// the sailings they run to (scheduled before that instant); and when
/// they were worked out, to know when there are newer.
#[derive(Default)]
pub struct Book {
    computed_ms: Option<i64>,
    history: HashMap<u32, (i64, Vec<Recorded>)>,
}

impl Book {
    /// Loads the stored records if they're newer than those held.
    pub async fn refresh(&mut self, pool: &PgPool) -> Result<()> {
        let latest: Option<i64> = sqlx::query_scalar(
            "SELECT (extract(epoch FROM max(computed_at)) * 1000)::bigint FROM advice_history",
        )
        .fetch_one(pool)
        .await?;
        if latest.is_none() || latest == self.computed_ms {
            return Ok(());
        }
        let rows = sqlx::query("SELECT terminal_id, through_ms, records FROM advice_history")
            .fetch_all(pool)
            .await?;
        self.history = rows
            .iter()
            .map(|r| {
                let sqlx::types::Json(records): sqlx::types::Json<Vec<Recorded>> = r.get("records");
                (
                    r.get::<i32, _>("terminal_id") as u32,
                    (r.get("through_ms"), records),
                )
            })
            .collect();
        self.computed_ms = latest;
        tracing::info!(terminals = self.history.len(), "advice history loaded");
        Ok(())
    }

    /// `terminal`'s records as of `now_ms`: those stored (still within
    /// `LOOKBACK_DAYS`), and those since from the database (today's, and
    /// any day the job didn't get to; everything, without any stored).
    async fn records(&self, pool: &PgPool, terminal: u32, now_ms: i64) -> Result<Vec<Recorded>> {
        let from = now_ms - LOOKBACK_DAYS * calendar::DAY_MS;
        let (through, stored) = match self.history.get(&terminal) {
            Some((through, records)) => (*through, records.as_slice()),
            None => (from, &[][..]),
        };
        let mut records: Vec<Recorded> = stored
            .iter()
            .filter(|r| r.departs_ms >= from)
            .cloned()
            .collect();
        records
            .extend(recorded(pool, terminal, through.max(from), now_ms + calendar::DAY_MS).await?);
        Ok(records)
    }
}

/// How often the advice is worked out again: as often as WSF's car space
/// counts come.
pub const PUBLISH_EVERY_MS: i64 = 60_000;

/// Works out every terminal's advice for the published schedule as of
/// `now_ms`, publishes it, and records what changed (`advice_given`). False,
/// doing nothing, before the schedules have loaded.
pub async fn publish(
    pool: &PgPool,
    state: &AppState,
    book: &mut Book,
    now_ms: i64,
) -> Result<bool> {
    let Some(schedules) = state.route_schedules() else {
        return Ok(false);
    };
    book.refresh(pool).await?;
    let special = special_days(pool).await?;
    let mut terminals: Vec<u32> = schedules
        .values()
        .flat_map(|s| &s.days)
        .flat_map(|d| d.terminals.iter().map(|t| t.terminal))
        .collect();
    terminals.sort_unstable();
    terminals.dedup();
    let mut docs = HashMap::new();
    for terminal in terminals {
        let days = departures_by_day(&schedules, terminal);
        let records = book.records(pool, terminal, now_ms).await?;
        let advice = advise_terminal(terminal, &days, &records, &special, now_ms);
        record_given(pool, &advice, now_ms).await?;
        docs.insert(terminal, Doc::json(&advice));
    }
    state.set_advice(docs, now_ms + PUBLISH_EVERY_MS);
    Ok(true)
}

/// Records each sailing's advice where it changed since last recorded.
async fn record_given(pool: &PgPool, advice: &TerminalAdvice, now_ms: i64) -> Result<()> {
    for s in &advice.sailings {
        sqlx::query(
            "INSERT INTO advice_given (terminal_id, departs_at, published_at, advice)
             SELECT $1, to_timestamp($2::double precision / 1000),
                 to_timestamp($3::double precision / 1000), $4
             WHERE NOT EXISTS (
                 SELECT 1 FROM (
                     SELECT advice FROM advice_given
                     WHERE terminal_id = $1 AND departs_at = to_timestamp($2::double precision / 1000)
                     ORDER BY published_at DESC LIMIT 1) AS last
                 WHERE last.advice = $4)
             ON CONFLICT DO NOTHING",
        )
        .bind(advice.terminal as i32)
        .bind(s.departs_ms)
        .bind(now_ms)
        .bind(sqlx::types::Json(&s.advice))
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Until the schedules load, look again this often (ms).
const SCHEDULES_WAIT_MS: u64 = 5_000;

/// Publishes the advice every `PUBLISH_EVERY_MS` (as soon as the trip
/// day's schedules load, at first), failures logged.
pub fn spawn(pool: PgPool, state: std::sync::Arc<AppState>) {
    tokio::spawn(async move {
        let mut book = Book::default();
        loop {
            let wait = match publish(&pool, &state, &mut book, crate::wsf::now_ms()).await {
                Ok(true) => PUBLISH_EVERY_MS as u64,
                Ok(false) => SCHEDULES_WAIT_MS,
                Err(e) => {
                    tracing::warn!("advice not published: {e:#}");
                    PUBLISH_EVERY_MS as u64
                }
            };
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
        }
    });
}

/// One sailing's advice: the most cautious of its stops'; None with too
/// few sailings like it recorded.
fn advise_sailing(
    records: &[Recorded],
    departures: &[(i64, Vec<u32>)],
    departs_ms: i64,
    stops: &[u32],
    special: &HashMap<i64, String>,
    holiday: bool,
    now_ms: i64,
) -> Option<Advice> {
    let mut best: Option<(Advised, f64)> = None;
    for &to in stops {
        // Today's record of it, if any: the boat on it, and how busy.
        let today = records
            .iter()
            .find(|r| r.departs_ms == departs_ms && r.destination == to);
        let alike = || {
            records.iter().filter(|r| {
                r.destination == to
                    && r.past.day < calendar::pacific_day(departs_ms)
                    && (r.minute - calendar::pacific_minute(departs_ms)).abs() <= ALIKE_MIN
            })
        };
        let capacity = today.map(|r| r.past.capacity).or_else(|| {
            alike()
                .max_by_key(|r| r.departs_ms)
                .map(|r| r.past.capacity)
        });
        let Some(capacity) = capacity else {
            continue;
        };
        let now_lead = (departs_ms - now_ms) as f64 / 60_000.0;
        let pace = today
            .filter(|_| now_lead > 0.0 && !records.iter().any(|r| r.reservations))
            .and_then(|r| {
                let usual: Vec<(&Past, f64)> = alike().map(|a| (&a.past, 1.0)).collect();
                pace(&usual, r.past.cars_by(now_lead), now_lead)
            });
        let gap = gap_after(departures, to, departs_ms);
        let Some(mut advised) = advise(
            records,
            to,
            departs_ms,
            true,
            special,
            capacity,
            pace.unwrap_or(1.0),
            gap,
        ) else {
            continue;
        };
        advised.pace = pace;
        let lead = match advised.decision {
            Decision::Room => 0.0,
            Decision::ArriveBy { lead_min, .. } => f64::from(lead_min),
        };
        if best.as_ref().is_none_or(|(_, l)| lead > *l) {
            best = Some((advised, lead));
        }
    }
    let (advised, _) = best?;
    let pace = advised.pace;
    let day = calendar::pacific_day(departs_ms);
    let sailings = advised.sailings.round() as u32;
    let basis = basis(day, advised.day_weight);
    Some(match advised.decision {
        Decision::Room if !holiday => Advice::Room { sailings, basis },
        Decision::Room => Advice::ArriveBy {
            arrive_by_ms: departs_ms - i64::from(LATEST_LEAD_MIN + HOLIDAY_EXTRA_MIN) * 60_000,
            likely: ROOM_LIKELY,
            usually_full_ms: None,
            sailings,
            basis,
            pace,
        },
        Decision::ArriveBy { lead_min, likely } => {
            let lead = lead_min + if holiday { HOLIDAY_EXTRA_MIN } else { 0 };
            Advice::ArriveBy {
                arrive_by_ms: departs_ms - i64::from(lead) * 60_000,
                likely,
                usually_full_ms: advised
                    .usual_fill_min
                    .map(|l| departs_ms - (l * 60_000.0) as i64),
                sailings,
                basis,
                pace,
            }
        }
    })
}

/// How the advice riders saw held over the last `days`: for each sailing
/// gone, the advice published as it stood at the time it advised
/// arriving by (or, where it usually had room, `LATEST_LEAD_MIN` before
/// it left), and whether each of its destinations had space then. WSF's
/// general guidance isn't an estimate, so isn't scored.
pub async fn check(pool: &PgPool, days: u32, now_ms: i64) -> Result<AdviceCheck> {
    let since = now_ms - i64::from(days) * calendar::DAY_MS;
    let rows = sqlx::query(
        "SELECT terminal_id, (extract(epoch FROM departs_at) * 1000)::bigint AS departs_ms,
             (extract(epoch FROM published_at) * 1000)::bigint AS published_ms, advice
         FROM advice_given
         WHERE departs_at >= to_timestamp($1::double precision / 1000)
           AND departs_at < to_timestamp($2::double precision / 1000)
         ORDER BY terminal_id, departs_at, published_at",
    )
    .bind(since)
    .bind(now_ms)
    .fetch_all(pool)
    .await?;
    // The advice in force at the time it named: per sailing, the last
    // published by then.
    let mut shown: Vec<(u32, i64, Advice)> = Vec::new();
    for r in &rows {
        let terminal = r.get::<i32, _>("terminal_id") as u32;
        let departs_ms: i64 = r.get("departs_ms");
        let published_ms: i64 = r.get("published_ms");
        let sqlx::types::Json(advice): sqlx::types::Json<Advice> = r.get("advice");
        let by = match &advice {
            Advice::ArriveBy { arrive_by_ms, .. } => *arrive_by_ms,
            Advice::Room { .. } => departs_ms - i64::from(LATEST_LEAD_MIN) * 60_000,
            Advice::Guidance { .. } => continue,
        };
        if published_ms > by {
            continue;
        }
        match shown.last_mut() {
            Some(last) if last.0 == terminal && last.1 == departs_ms => last.2 = advice,
            _ => shown.push((terminal, departs_ms, advice)),
        }
    }
    let mut out: Vec<TerminalCheck> = Vec::new();
    let mut by_terminal: HashMap<u32, Vec<(i64, Advice)>> = HashMap::new();
    for (terminal, departs_ms, advice) in shown {
        by_terminal
            .entry(terminal)
            .or_default()
            .push((departs_ms, advice));
    }
    let mut terminals: Vec<u32> = by_terminal.keys().copied().collect();
    terminals.sort_unstable();
    for terminal in terminals {
        let records = recorded(pool, terminal, since, now_ms).await?;
        let (mut n, mut held, mut predicted) = (0u32, 0.0, 0.0);
        for (departs_ms, advice) in &by_terminal[&terminal] {
            let (lead, likely) = match advice {
                Advice::ArriveBy {
                    arrive_by_ms,
                    likely,
                    ..
                } => ((departs_ms - arrive_by_ms) as f64 / 60_000.0, *likely),
                _ => (f64::from(LATEST_LEAD_MIN), ROOM_LIKELY),
            };
            let outcomes: Vec<bool> = records
                .iter()
                .filter(|r| r.departs_ms == *departs_ms)
                .filter_map(|r| r.past.had_space(lead))
                .collect();
            if outcomes.is_empty() {
                continue;
            }
            n += 1;
            held += f64::from(u8::from(outcomes.iter().all(|&space| space)));
            predicted += likely;
        }
        if n > 0 {
            out.push(TerminalCheck {
                terminal,
                sailings: n,
                held: held / f64::from(n),
                predicted: predicted / f64::from(n),
            });
        }
    }
    Ok(AdviceCheck {
        days,
        terminals: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sailing of `capacity` spaces counted from 60 minutes out, losing
    /// `per_min` spaces a minute from `starts` minutes before it leaves,
    /// counted to `counted_to` minutes before.
    fn sailing(day: i64, capacity: i32, starts: f64, per_min: f64, counted_to: f64) -> Past {
        let mut counts = vec![Count {
            lead_min: 60.0,
            spaces: capacity,
        }];
        let mut lead = starts;
        while lead >= counted_to {
            let spaces = f64::from(capacity) - per_min * (starts - lead);
            counts.push(Count {
                lead_min: lead,
                spaces: spaces.round() as i32,
            });
            lead -= 1.0;
        }
        Past {
            day,
            capacity,
            counts,
            counted_to_min: counted_to,
        }
    }

    #[test]
    fn finds_when_sailings_fill_for_today_s_boat() {
        // 100 spaces, 4 cars a minute from 40 minutes out: full (3 or
        // fewer left) 15 minutes before it leaves.
        let p = sailing(0, 100, 40.0, 4.0, 0.0);
        assert_eq!(p.outcome(100, 1.0, true), Outcome::Filled(15.0));
        // A 60-space boat on such a day fills 10 minutes sooner...
        assert_eq!(p.outcome(60, 1.0, true), Outcome::Filled(25.0));
        // ...and on a day twice as busy, sooner still.
        assert_eq!(p.outcome(100, 2.0, true), Outcome::Filled(27.0));
        // A quiet sailing never fills; one counted only to 8 minutes
        // before is open that far.
        let quiet = sailing(0, 100, 40.0, 1.0, 0.0);
        assert_eq!(quiet.outcome(100, 1.0, true), Outcome::Open(0.0));
        let cut = sailing(0, 100, 40.0, 1.0, 8.0);
        assert_eq!(cut.outcome(100, 1.0, true), Outcome::Open(8.0));
        assert_eq!(p.cars_by(30.0), 40.0);
        assert_eq!(p.cars_by(50.0), 0.0);
        assert_eq!(p.had_space(30.0), Some(true));
        assert_eq!(p.had_space(10.0), Some(false));
    }

    #[test]
    fn estimates_the_chance_of_a_space_with_censoring() {
        // Four sailings: full at 20 and 30 minutes out, one open counted
        // to 25 minutes out (gone from the risk set after), one never full.
        let outcomes = [
            (Outcome::Filled(20.0), 1.0),
            (Outcome::Filled(30.0), 1.0),
            (Outcome::Open(25.0), 1.0),
            (Outcome::Open(0.0), 1.0),
        ];
        let curve = Curve::new(&outcomes);
        assert_eq!(curve.chance(40.0), 1.0);
        // At 30: all four at risk, one fills: 3/4.
        assert_eq!(curve.chance(30.0), 0.75);
        // At 20: two at risk (the one open to 25 has left), one fills.
        assert_eq!(curve.chance(20.0), 0.375);
        assert_eq!(curve.sailings, 4.0);
        let heavy: Vec<(Outcome, f64)> = outcomes.iter().map(|&(o, _)| (o, 2.0)).collect();
        assert_eq!(Curve::new(&heavy).chance(20.0), 0.375);
    }

    #[test]
    fn pools_a_day_toward_its_kind_while_its_record_is_thin() {
        let full = Curve::new(&[(Outcome::Filled(30.0), 1.0)]);
        let room = Curve::new(&[(Outcome::Open(0.0), 1.0); 10]);
        let pooled = Pooled {
            day: full,
            kind: room,
        };
        // One Friday against ten weekdays: mostly the weekdays.
        assert!((pooled.day_weight() - 1.0 / 6.0).abs() < 1e-9);
        assert!((pooled.chance(20.0) - 5.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn advises_the_arrival_costing_the_least() {
        // Fills 30 minutes out half the time, 50 minutes out the rest.
        let chance = |lead: f64| {
            if lead > 50.0 {
                1.0
            } else if lead > 30.0 {
                0.5
            } else {
                0.0
            }
        };
        assert_eq!(
            decide(chance, 20.0),
            Decision::ArriveBy {
                lead_min: 55,
                likely: 1.0
            }
        );
        // A chance rising slowly: frequent boats accept more risk than
        // the last of the day.
        let slow = |lead: f64| (lead / 100.0).min(1.0);
        let frequent = decide(slow, 20.0);
        let last = decide(slow, LAST_SAILING_GAP_MIN);
        let (
            Decision::ArriveBy { lead_min: f, .. },
            Decision::ArriveBy {
                lead_min: l,
                likely,
            },
        ) = (frequent, last)
        else {
            panic!("{frequent:?} {last:?}");
        };
        assert_eq!(f, 70); // the 70% floor
        assert_eq!((l, likely), (100, 1.0));
        assert_eq!(decide(|_| 1.0, 20.0), Decision::Room);
        assert_eq!(
            decide(|lead| if lead >= 180.0 { 0.4 } else { 0.0 }, 30.0),
            Decision::ArriveBy {
                lead_min: 180,
                likely: 0.4
            }
        );
    }

    #[tokio::test]
    async fn advises_from_the_record() {
        let Some(db) = crate::db::tests::Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping");
            return;
        };
        let pool = &db.pool;
        // Friday, Oct 30, 2026: the 5:10 PM and 5:55 PM Seattle (7) to
        // Bainbridge (3), asked at noon.
        let day = calendar::days_from_civil(2026, 10, 30);
        let at = |d: i64, h: i64, m: i64| {
            let local = d * calendar::DAY_MS + (h * 60 + m) * 60_000;
            local - calendar::pacific_offset_ms(local)
        };
        let (five_ten, five_55) = (at(day, 17, 10), at(day, 17, 55));
        let departure = |ms: i64| ferry_model::api::Departure {
            departs_ms: ms,
            vessel_id: None,
            vessel: String::new(),
            stops: vec![ferry_model::api::Stop {
                terminal: 3,
                arrives_ms: None,
            }],
            loading: ferry_model::api::Loading::Both,
            notes: vec![],
        };
        let state = AppState::default();
        state.set_schedules(HashMap::from([(
            "seattle-bainbridge-island".to_string(),
            ferry_model::api::RouteSchedule {
                route: "seattle-bainbridge-island".into(),
                days: vec![ferry_model::api::ScheduleDay {
                    trip_date: "2026-10-30".into(),
                    season: "Fall 2026".into(),
                    pdf_url: None,
                    terminals: vec![ferry_model::api::TerminalDepartures {
                        terminal: 7,
                        departures: vec![departure(five_ten), departure(five_55)],
                        notes: vec![],
                    }],
                }],
            },
        )]));
        // Six Fridays before: the 5:10 counted from an hour out, full 20
        // or 25 minutes out (197 spaces, 6 or 7 cars a minute from 55
        // minutes out, counted every 5 minutes).
        for week in 1..=6 {
            let departs = at(day - 7 * week, 17, 10);
            let per_min = if week % 2 == 0 { 7.0 } else { 6.0 };
            for lead in (0..=60).rev().step_by(5) {
                let spaces = if lead > 55 {
                    197.0
                } else {
                    (197.0 - per_min * (55 - lead) as f64).max(0.0)
                };
                sqlx::query(
                    "INSERT INTO sailing_space (terminal_id, departs_at, destination_id,
                         observed_at, seen_until, vessel_id, drive_up, capacity, oversold,
                         cancelled)
                     VALUES (7, to_timestamp($1::double precision / 1000), 3,
                         to_timestamp($2::double precision / 1000),
                         to_timestamp($2::double precision / 1000 + 299), 1, $3, 197,
                         false, false)",
                )
                .bind(departs)
                .bind(departs - i64::from(lead) * 60_000)
                .bind(spaces as i32)
                .execute(pool)
                .await
                .unwrap();
            }
        }
        // The daily job stores the record; publishing at noon adds today's
        // (none yet) and works the advice out.
        let noon = at(day, 12, 0);
        assert_eq!(compute_history(pool, noon).await.unwrap(), 1);
        let mut book = Book::default();
        publish(pool, &state, &mut book, noon).await.unwrap();
        assert_eq!(book.history[&7].1.len(), 6);
        let schedules = state.route_schedules().unwrap();
        let days = departures_by_day(&schedules, 7);
        let records = book.records(pool, 7, noon).await.unwrap();
        let advice = advise_terminal(7, &days, &records, &HashMap::new(), noon);
        assert_eq!(advice.reservation, None);
        assert_eq!(advice.sailings.len(), 2);
        let Advice::ArriveBy {
            arrive_by_ms,
            likely,
            usually_full_ms,
            sailings,
            basis,
            pace,
        } = advice.sailings[0].advice.clone()
        else {
            panic!("{:?}", advice.sailings[0]);
        };
        // Full by 20 or 25 minutes out every time: arrive by 25 or more
        // minutes before, and it's from six Fridays.
        let lead = (five_ten - arrive_by_ms) / 60_000;
        assert!((25..=35).contains(&lead), "{lead} min early");
        assert!(likely >= LEAST_LIKELY, "{likely}");
        let usual = (five_ten - usually_full_ms.unwrap()) / 60_000;
        assert!((20..=25).contains(&usual), "usually full {usual} min out");
        assert_eq!((sailings, basis.as_str(), pace), (6, "Fridays", None));
        // The 5:55 has no record: WSF's guidance, 45 minutes early on a
        // Friday evening (busy).
        assert_eq!(
            advice.sailings[1].advice,
            Advice::Guidance {
                arrive_by_ms: five_55 - 45 * 60_000,
                lead_min: 45,
                reason: GuidanceReason::Busy,
                counted: true
            }
        );
        // What was published was recorded, once while it stays the same.
        publish(pool, &state, &mut book, noon + PUBLISH_EVERY_MS)
            .await
            .unwrap();
        let given: i64 =
            sqlx::query_scalar("SELECT count(*) FROM advice_given WHERE terminal_id = 7")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(given, 2);
        // Today's 5:10 then fills 20 minutes out: the advice held.
        for (lead, spaces) in [(60, 197), (30, 60), (20, 2)] {
            sqlx::query(
                "INSERT INTO sailing_space (terminal_id, departs_at, destination_id, observed_at,
                     seen_until, vessel_id, drive_up, capacity, oversold, cancelled)
                 VALUES (7, to_timestamp($1::double precision / 1000), 3,
                     to_timestamp($2::double precision / 1000),
                     to_timestamp($2::double precision / 1000 + 299), 1, $3, 197, false, false)",
            )
            .bind(five_ten)
            .bind(five_ten - lead * 60_000)
            .bind(spaces)
            .execute(pool)
            .await
            .unwrap();
        }
        let check = check(pool, 1, at(day, 18, 0)).await.unwrap();
        assert_eq!(check.terminals.len(), 1);
        let t = &check.terminals[0];
        assert_eq!((t.terminal, t.sailings, t.held), (7, 1, 1.0));
        db.finish().await;
    }

    #[test]
    fn measures_today_s_pace_against_the_usual() {
        let usual: Vec<Past> = (0..3).map(|d| sailing(d, 100, 40.0, 2.0, 0.0)).collect();
        let pasts: Vec<(&Past, f64)> = usual.iter().map(|p| (p, 1.0)).collect();
        // 20 minutes out they've 40 cars; today 60: half again as busy.
        assert_eq!(pace(&pasts, 60.0, 20.0), Some(1.5));
        assert_eq!(pace(&pasts, 2.0, 38.0), None);
        assert!((recency_weight(28.0) - 0.5).abs() < 1e-12);
    }

    /// A route's schedule with `terminal`'s day of sailings at these times.
    fn day_of(trip_date: &str, terminal: u32, times: &[i64]) -> ferry_model::api::ScheduleDay {
        ferry_model::api::ScheduleDay {
            trip_date: trip_date.into(),
            season: "Fall 2026".into(),
            pdf_url: None,
            terminals: vec![ferry_model::api::TerminalDepartures {
                terminal,
                departures: times
                    .iter()
                    .map(|&departs_ms| ferry_model::api::Departure {
                        departs_ms,
                        vessel_id: Some(1),
                        vessel: "Sealth".into(),
                        stops: vec![ferry_model::api::Stop {
                            terminal: 3,
                            arrives_ms: None,
                        }],
                        loading: ferry_model::api::Loading::Both,
                        notes: vec![],
                    })
                    .collect(),
                notes: vec![],
            }],
        }
    }

    #[test]
    fn a_days_last_sailing_stays_the_last_whatever_the_next_day_holds() {
        const MIN: i64 = 60_000;
        let schedules = HashMap::from([(
            "r".to_string(),
            ferry_model::api::RouteSchedule {
                route: "r".into(),
                days: vec![
                    day_of("2026-10-30", 7, &[100 * MIN, 200 * MIN]),
                    day_of("2026-10-31", 7, &[230 * MIN]),
                ],
            },
        )]);
        let days = departures_by_day(&schedules, 7);
        assert_eq!(days.len(), 2);
        // Only 30 minutes before the next day's first, but tonight's last.
        assert_eq!(gap_after(&days[0], 3, 200 * MIN), LAST_SAILING_GAP_MIN);
        assert_eq!(gap_after(&days[0], 3, 100 * MIN), 100.0);
        // A terminal with nothing published has no days.
        assert!(departures_by_day(&schedules, 99).is_empty());
    }

    #[test]
    fn the_next_days_advice_ignores_sailings_still_filling() {
        // It's noon. Today's 5:00 hasn't left and is near empty so far; it's
        // all the record there is. Today's own advice may use it, but
        // tomorrow's has nothing to go on, so gives WSF's guidance.
        let noon = 1_793_386_800_000; // 2026-10-30, 12:00 PDT
        let later = noon + 5 * calendar::HOUR_MS;
        let tomorrow = later + calendar::DAY_MS;
        let records = [Recorded {
            departs_ms: later,
            minute: calendar::pacific_minute(later),
            destination: 3,
            past: Past {
                day: calendar::pacific_day(later),
                capacity: 200,
                counts: vec![],
                counted_to_min: 300.0,
            },
            reservations: false,
        }];
        let days = [vec![(tomorrow, vec![3])]];
        let advice = advise_terminal(7, &days, &records, &HashMap::new(), noon);
        let Advice::Guidance { counted, .. } = &advice.sailings[0].advice else {
            panic!("{:?}", advice.sailings[0].advice);
        };
        assert!(!counted, "nothing that's left should have been considered");
    }
}
