//! How published sailings went (`/sailings`): the schedule's history
//! (`published_sailings`) held against the trips boats made. WSF rewrites
//! a day's schedule as it goes, boats and all, so what's published now can't
//! say who ran what; this can.

use std::collections::BTreeMap;

use anyhow::Result;
use ferry_model::api::{RouteSailings, SailingOutcome, SailingRecord};
use sqlx::{PgPool, Row};

use crate::calendar::trip_date;

/// Sailings to a page, by default and at most.
pub const PAGE: u32 = 50;
pub const PAGE_MAX: u32 = 200;
/// A sailing unaccounted for is still due for this long (ms) past its time.
const DUE_MS: i64 = 60 * 60_000;
/// A boat off the schedule can stand in for a sailing it leaves this long
/// (ms) before it, or up to `DUE_MS` after.
const STANDS_IN_EARLY_MS: i64 = 20 * 60_000;
/// A boat leaving this close (ms) to a sailing's time ran it, whatever WSF
/// called it: riders can't tell the difference.
const RAN_IT_MS: i64 = 5 * 60_000;

/// A sailing as published, however many pairs of terminals list it.
#[derive(Debug, Default)]
struct Published {
    trip_date: String,
    departs_ms: i64,
    stops: Vec<u32>,
    vessel: String,
    first_vessel: String,
    /// When its last listing went, if none is left.
    withdrawn_ms: Option<i64>,
}

/// A boat's departure from the terminal.
#[derive(Debug)]
struct Trip {
    vessel: String,
    arriving: Option<u32>,
    scheduled_ms: Option<i64>,
    left_ms: i64,
}

/// What became of `sailings` (oldest first) at `now_ms`, from `trips` from
/// the terminal. `recording(i)` is whether we were recording around sailing
/// `i`'s time. In order: a boat that left as that sailing, however WSF has
/// it since; then WSF having withdrawn it; then a boat off the schedule,
/// each standing in for the one sailing it left nearest to.
fn outcomes(
    sailings: &[Published],
    trips: &[Trip],
    recording: &[bool],
    now_ms: i64,
) -> Vec<SailingOutcome> {
    let mut found: Vec<Option<SailingOutcome>> = sailings
        .iter()
        .map(|s| {
            let ran = trips.iter().find(|t| t.scheduled_ms == Some(s.departs_ms));
            match (ran, s.withdrawn_ms) {
                (Some(t), _) => Some(SailingOutcome::Ran {
                    left_ms: t.left_ms,
                    vessel: t.vessel.clone(),
                    late_min: ((t.left_ms - s.departs_ms) as f64 / 60_000.0).round() as i32,
                }),
                (None, Some(withdrawn_ms)) => Some(SailingOutcome::Cancelled { withdrawn_ms }),
                (None, None) => None,
            }
        })
        .collect();
    // Who could stand in for whom, nearest in time first.
    let mut pairs: Vec<(i64, usize, usize)> = Vec::new();
    for (i, s) in sailings.iter().enumerate() {
        if found[i].is_some() {
            continue;
        }
        for (j, t) in trips.iter().enumerate() {
            let near = t.left_ms >= s.departs_ms - STANDS_IN_EARLY_MS
                && t.left_ms <= s.departs_ms + DUE_MS;
            let goes = t.arriving.is_some_and(|to| s.stops.contains(&to));
            if near && goes && t.scheduled_ms.is_none() {
                pairs.push(((t.left_ms - s.departs_ms).abs(), i, j));
            }
        }
    }
    pairs.sort_unstable();
    let mut taken = vec![false; trips.len()];
    for (_, i, j) in pairs {
        if found[i].is_none() && !taken[j] {
            taken[j] = true;
            let (left_ms, vessel) = (trips[j].left_ms, trips[j].vessel.clone());
            let off = left_ms - sailings[i].departs_ms;
            found[i] = Some(if off.abs() <= RAN_IT_MS {
                SailingOutcome::Ran {
                    left_ms,
                    vessel,
                    late_min: (off as f64 / 60_000.0).round() as i32,
                }
            } else {
                SailingOutcome::OffSchedule { left_ms, vessel }
            });
        }
    }
    found
        .into_iter()
        .enumerate()
        .map(|(i, outcome)| {
            outcome.unwrap_or(if now_ms < sailings[i].departs_ms + DUE_MS {
                SailingOutcome::Due
            } else if recording[i] {
                SailingOutcome::NoRecord
            } else {
                SailingOutcome::Unknown
            })
        })
        .collect()
}

/// How `route`'s sailings from `terminal` went (see `RouteSailings`). None
/// for a route there's no such thing as.
pub async fn for_terminal(
    pool: &PgPool,
    route: &str,
    terminal: u32,
    before_ms: Option<i64>,
    limit: u32,
    now_ms: i64,
) -> Result<Option<RouteSailings>> {
    let Some(terminals) = crate::stats::route_terminals(pool, route).await? else {
        return Ok(None);
    };
    let today = trip_date(now_ms);
    // Which sailings: a page before a time, newest first, or today's so far
    // and whatever's been withdrawn ahead.
    const LISTED: &str = "departing = $1 AND arriving = ANY($2)";
    let times: Vec<i64> = match before_ms {
        Some(before) => {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT (extract(epoch FROM departs_at) * 1000)::bigint FROM published_sailings
                 WHERE {LISTED} AND departs_at < to_timestamp($3::double precision / 1000)
                 GROUP BY departs_at ORDER BY departs_at DESC LIMIT $4"
            )))
            .bind(terminal as i32)
            .bind(&terminals)
            .bind(before.min(now_ms))
            .bind(i64::from(limit) + 1)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT (extract(epoch FROM departs_at) * 1000)::bigint FROM published_sailings
                 WHERE {LISTED} AND trip_date >= $3::date
                 GROUP BY departs_at
                 HAVING departs_at <= to_timestamp($4::double precision / 1000)
                     OR bool_and(withdrawn_at IS NOT NULL)
                 ORDER BY departs_at"
            )))
            .bind(terminal as i32)
            .bind(&terminals)
            .bind(&today)
            .bind(now_ms)
            .fetch_all(pool)
            .await?
        }
    };
    let (times, more) = match before_ms {
        Some(_) => {
            let more = times.len() > limit as usize;
            (times.into_iter().take(limit as usize).collect(), more)
        }
        None => {
            // Older than the trip day: than its first sailing listed ever,
            // or than now where it has none so far.
            let first: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT (extract(epoch FROM min(departs_at)) * 1000)::bigint
                 FROM published_sailings WHERE {LISTED} AND trip_date >= $3::date"
            )))
            .bind(terminal as i32)
            .bind(&terminals)
            .bind(&today)
            .fetch_one(pool)
            .await?;
            let more: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT EXISTS (SELECT 1 FROM published_sailings WHERE {LISTED}
                     AND departs_at < to_timestamp($3::double precision / 1000))"
            )))
            .bind(terminal as i32)
            .bind(&terminals)
            .bind(first.unwrap_or(now_ms))
            .fetch_one(pool)
            .await?;
            (times, more)
        }
    };
    let mut records = Vec::new();
    if let (Some(&oldest), Some(&newest)) = (times.iter().min(), times.iter().max()) {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT trip_date::text AS trip_date, arriving, vessel, withdrawn_at IS NULL AS open,
                 (extract(epoch FROM departs_at) * 1000)::bigint AS departs_ms,
                 (extract(epoch FROM first_seen) * 1000)::bigint AS first_ms,
                 (extract(epoch FROM withdrawn_at) * 1000)::bigint AS withdrawn_ms
             FROM published_sailings
             WHERE {LISTED} AND departs_at = ANY(
                 SELECT to_timestamp(t::double precision / 1000) FROM unnest($3::bigint[]) AS t)
             ORDER BY departs_at, first_seen"
        )))
        .bind(terminal as i32)
        .bind(&terminals)
        .bind(&times)
        .fetch_all(pool)
        .await?;
        let mut sailings: BTreeMap<i64, Published> = BTreeMap::new();
        // The latest listing's boat: rows come oldest first.
        let mut any_open: BTreeMap<i64, bool> = BTreeMap::new();
        for r in &rows {
            let departs_ms: i64 = r.get("departs_ms");
            let vessel: String = r.get("vessel");
            let open: bool = r.get("open");
            let s = sailings.entry(departs_ms).or_insert_with(|| Published {
                trip_date: r.get("trip_date"),
                departs_ms,
                first_vessel: vessel.clone(),
                ..Published::default()
            });
            let arriving = r.get::<i32, _>("arriving") as u32;
            if !s.stops.contains(&arriving) {
                s.stops.push(arriving);
            }
            s.vessel = vessel;
            s.withdrawn_ms = s.withdrawn_ms.max(r.get("withdrawn_ms"));
            *any_open.entry(departs_ms).or_default() |= open;
        }
        for (ms, open) in any_open {
            if open && let Some(s) = sailings.get_mut(&ms) {
                s.withdrawn_ms = None;
            }
        }
        let sailings: Vec<Published> = sailings.into_values().collect();
        let trips: Vec<Trip> = sqlx::query(
            "SELECT v.name, t.arriving,
                 (extract(epoch FROM t.scheduled_departure) * 1000)::bigint AS scheduled_ms,
                 (extract(epoch FROM t.left_dock) * 1000)::bigint AS left_ms
             FROM trips t JOIN vessels v ON v.id = t.vessel_id
             WHERE t.departing = $1
               AND t.left_dock >= to_timestamp($2::double precision / 1000)
               AND t.left_dock <= to_timestamp($3::double precision / 1000)
             ORDER BY t.left_dock",
        )
        .bind(terminal as i32)
        .bind(oldest - 2 * DUE_MS)
        .bind(newest + 2 * DUE_MS)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| Trip {
            vessel: r.get("name"),
            arriving: r.get::<Option<i32>, _>("arriving").map(|t| t as u32),
            scheduled_ms: r.get("scheduled_ms"),
            left_ms: r.get("left_ms"),
        })
        .collect();
        let recording: Vec<bool> = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM vessel_positions
                 WHERE reported_at >= to_timestamp(t::double precision / 1000)
                   AND reported_at < to_timestamp((t + $2)::double precision / 1000))
             FROM unnest($1::bigint[]) WITH ORDINALITY AS u(t, n) ORDER BY n",
        )
        .bind(sailings.iter().map(|s| s.departs_ms).collect::<Vec<_>>())
        .bind(DUE_MS)
        .fetch_all(pool)
        .await?;
        let found = outcomes(&sailings, &trips, &recording, now_ms);
        records = sailings
            .into_iter()
            .zip(found)
            .map(|(s, outcome)| SailingRecord {
                trip_date: s.trip_date,
                departs_ms: s.departs_ms,
                stops: s.stops,
                vessel: s.vessel,
                first_vessel: s.first_vessel,
                outcome,
            })
            .collect();
    }
    if before_ms.is_some() {
        records.reverse();
    }
    Ok(Some(RouteSailings {
        route: route.to_string(),
        terminal,
        sailings: records,
        more,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tests::Scratch;
    use crate::published::record_schedule;
    use crate::wsf::{DaySchedule, PairSailing, PairSailings};
    use ferry_model::api::Loading;

    const MIN: i64 = 60_000;

    fn sailing(min: i64, withdrawn: Option<i64>) -> Published {
        Published {
            trip_date: "2026-10-30".into(),
            departs_ms: min * MIN,
            stops: vec![22],
            vessel: "Kitsap".into(),
            first_vessel: "Kitsap".into(),
            withdrawn_ms: withdrawn.map(|m| m * MIN),
        }
    }

    fn trip(vessel: &str, arriving: u32, scheduled_min: Option<i64>, left_min: i64) -> Trip {
        Trip {
            vessel: vessel.into(),
            arriving: Some(arriving),
            scheduled_ms: scheduled_min.map(|m| m * MIN),
            left_ms: left_min * MIN,
        }
    }

    fn ran(left_min: i64, vessel: &str, late_min: i32) -> SailingOutcome {
        SailingOutcome::Ran {
            left_ms: left_min * MIN,
            vessel: vessel.into(),
            late_min,
        }
    }

    #[test]
    fn a_boat_on_a_sailing_ran_it_whatever_wsf_says_since() {
        // Salish really ran what WSF now has as Cathlamet's; and Mukilteo's
        // 6:00 went despite having been withdrawn.
        let found = outcomes(
            &[sailing(1000, None), sailing(1100, Some(1050))],
            &[
                trip("Salish", 22, Some(1000), 1003),
                trip("Suquamish", 22, Some(1100), 1101),
            ],
            &[true, true],
            2000 * MIN,
        );
        assert_eq!(found, [ran(1003, "Salish", 3), ran(1101, "Suquamish", 1)]);
    }

    #[test]
    fn a_withdrawn_sailing_nothing_ran_was_cancelled() {
        // A boat off the schedule leaving around then doesn't change that.
        let found = outcomes(
            &[sailing(1000, Some(990))],
            &[trip("Salish", 22, None, 1002)],
            &[true],
            2000 * MIN,
        );
        assert_eq!(
            found,
            [SailingOutcome::Cancelled {
                withdrawn_ms: 990 * MIN
            }]
        );
    }

    #[test]
    fn a_boat_off_the_schedule_stands_in_for_one_sailing() {
        // Two sailings and one boat, which left nearest the second. A trip
        // on another sailing, or to elsewhere, or too far off, doesn't count.
        let found = outcomes(
            &[
                sailing(1000, None),
                sailing(1030, None),
                sailing(1500, None),
            ],
            &[
                trip("Cathlamet", 22, None, 1040),
                trip("Kitsap", 22, Some(900), 1000),
                trip("Salish", 20, None, 1001),
                trip("Salish", 22, None, 1570),
            ],
            &[true, true, true],
            2000 * MIN,
        );
        assert_eq!(
            found,
            [
                SailingOutcome::NoRecord,
                SailingOutcome::OffSchedule {
                    left_ms: 1040 * MIN,
                    vessel: "Cathlamet".into()
                },
                SailingOutcome::NoRecord,
            ]
        );
    }

    #[test]
    fn a_boat_leaving_right_on_a_sailings_time_ran_it() {
        // WSF named no sailing for it, but riders saw a boat leave on time.
        let found = outcomes(
            &[sailing(1000, None)],
            &[trip("Cathlamet", 22, None, 1002)],
            &[true],
            2000 * MIN,
        );
        assert_eq!(found, [ran(1002, "Cathlamet", 2)]);
    }

    #[test]
    fn says_nothing_it_cant_know() {
        let found = outcomes(
            &[
                sailing(1000, None),
                sailing(1000, None),
                sailing(1950, None),
            ],
            &[],
            &[false, true, true],
            2000 * MIN,
        );
        assert_eq!(
            found,
            [
                // We weren't recording.
                SailingOutcome::Unknown,
                SailingOutcome::NoRecord,
                // It's only just past its time.
                SailingOutcome::Due,
            ]
        );
    }

    fn day(sailings: Vec<PairSailing>) -> DaySchedule {
        DaySchedule {
            season: "Fall 2026".into(),
            pdf_url: None,
            pairs: vec![PairSailings {
                from: 8,
                to: 12,
                notes: vec![],
                sailings,
            }],
        }
    }

    fn listed(departs_ms: i64, vessel: &str) -> PairSailing {
        PairSailing {
            departs_ms,
            arrives_ms: None,
            vessel_id: Some(1),
            vessel: vessel.into(),
            loading: Loading::Both,
            notes: vec![],
        }
    }

    #[tokio::test]
    async fn pages_through_sailings_from_the_database() {
        let Some(db) = Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping");
            return;
        };
        let pool = &db.pool;
        sqlx::query(
            "INSERT INTO routes (id, name) VALUES ('edmonds-kingston', 'Edmonds / Kingston')",
        )
        .execute(pool)
        .await
        .unwrap();
        for t in [8, 12] {
            sqlx::query("INSERT INTO terminals (id, name, lon, lat) VALUES ($1, 'T', 0, 0)")
                .bind(t)
                .execute(pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO route_terminals (route_id, terminal_id)
                 VALUES ('edmonds-kingston', $1)",
            )
            .bind(t)
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO vessels (id, name) VALUES (1, 'Spokane'), (2, 'Walla Walla')")
            .execute(pool)
            .await
            .unwrap();
        // Noon, Oct 30 Pacific; yesterday's 9 AM and 10 AM, this morning's
        // 9 AM and 10 AM, and one this afternoon withdrawn.
        let noon: i64 = 1_793_386_800_000;
        let hour = 60 * MIN;
        let day_ms = 24 * hour;
        let yesterday = day(vec![
            listed(noon - day_ms - 3 * hour, "Spokane"),
            listed(noon - day_ms - 2 * hour, "Spokane"),
        ]);
        record_schedule(pool, "2026-10-29", 6, &yesterday, noon - 2 * day_ms)
            .await
            .unwrap();
        let (nine, ten, two) = (noon - 3 * hour, noon - 2 * hour, noon + 2 * hour);
        let first = day(vec![
            listed(nine, "Spokane"),
            listed(ten, "Spokane"),
            listed(two, "Spokane"),
        ]);
        record_schedule(pool, "2026-10-30", 6, &first, noon - 6 * hour)
            .await
            .unwrap();
        // WSF gives the 10 AM to Walla Walla and withdraws this afternoon's.
        let later = day(vec![listed(nine, "Spokane"), listed(ten, "Walla Walla")]);
        record_schedule(pool, "2026-10-30", 6, &later, noon - 5 * hour)
            .await
            .unwrap();
        // Walla Walla left two minutes late at 10 AM; nothing left at 9.
        sqlx::query(
            "INSERT INTO trips (vessel_id, departing, arriving, scheduled_departure, left_dock)
             VALUES (2, 8, 12, to_timestamp($1::double precision / 1000),
                 to_timestamp($2::double precision / 1000))",
        )
        .bind(ten)
        .bind(ten + 2 * MIN)
        .execute(pool)
        .await
        .unwrap();
        // We were recording at 9 AM.
        sqlx::query(
            "INSERT INTO vessel_positions (vessel_id, reported_at, lon, lat, speed_kn,
                 heading_deg, at_dock, in_service)
             VALUES (1, to_timestamp($1::double precision / 1000), 0, 0, 0, 0, true, true)",
        )
        .bind(nine + 5 * MIN)
        .execute(pool)
        .await
        .unwrap();

        let today = for_terminal(pool, "edmonds-kingston", 8, None, PAGE, noon)
            .await
            .unwrap()
            .unwrap();
        assert!(today.more);
        let outcomes: Vec<(i64, &SailingOutcome)> = today
            .sailings
            .iter()
            .map(|s| (s.departs_ms, &s.outcome))
            .collect();
        assert_eq!(
            outcomes,
            [
                (nine, &SailingOutcome::NoRecord),
                (
                    ten,
                    &SailingOutcome::Ran {
                        left_ms: ten + 2 * MIN,
                        vessel: "Walla Walla".into(),
                        late_min: 2
                    }
                ),
                (
                    two,
                    &SailingOutcome::Cancelled {
                        withdrawn_ms: noon - 5 * hour
                    }
                ),
            ]
        );
        assert_eq!(today.sailings[1].first_vessel, "Spokane");
        assert_eq!(today.sailings[1].vessel, "Walla Walla");
        // A page, newest first, with more behind it.
        let page = for_terminal(pool, "edmonds-kingston", 8, Some(ten), 2, noon)
            .await
            .unwrap()
            .unwrap();
        let times: Vec<i64> = page.sailings.iter().map(|s| s.departs_ms).collect();
        assert_eq!(times, [nine, noon - day_ms - 2 * hour]);
        assert_eq!(page.sailings[1].outcome, SailingOutcome::Unknown);
        assert!(page.more);
        // The last page, and nothing past it.
        let last = for_terminal(
            pool,
            "edmonds-kingston",
            8,
            Some(noon - day_ms - 2 * hour),
            2,
            noon,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(last.sailings.len(), 1);
        assert!(!last.more);
        // Other terminals' are nobody's here, and there's no such route.
        let other = for_terminal(pool, "edmonds-kingston", 12, None, PAGE, noon)
            .await
            .unwrap()
            .unwrap();
        assert!(other.sailings.is_empty() && !other.more);
        assert_eq!(
            for_terminal(pool, "nowhere", 8, None, PAGE, noon)
                .await
                .unwrap(),
            None
        );
        db.finish().await;
    }
}
