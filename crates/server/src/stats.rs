//! Statistics from history: how long crossings take, and how full car
//! decks are as boats leave (the `sailing_loads` view: see migration
//! 0005).

use anyhow::Result;
use ferry_model::api::{
    CrossingStats, DirectionLoads, HourLoads, LoadStats, PairStats, SailingLoad, VesselLoads,
};
use sqlx::{PgPool, Row};

/// The longest window asked for, and the default.
pub const MAX_DAYS: u32 = 365;
pub const DEFAULT_DAYS: u32 = 30;

/// Crossing times between each pair of `route`'s terminals, either way,
/// from trips completed in the last `days`: from leaving the dock to first
/// being seen docked (so to within a poll, about 10 s). None if there's no
/// such route.
pub async fn crossings(pool: &PgPool, route: &str, days: u32) -> Result<Option<CrossingStats>> {
    let Some(terminals) = route_terminals(pool, route).await? else {
        return Ok(None);
    };
    let rows: Vec<(i32, i32, i64, f64, f64)> = sqlx::query_as(
        "SELECT LEAST(departing, arriving), GREATEST(departing, arriving), count(*),
             percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM arrived_at - left_dock)),
             percentile_cont(0.9) WITHIN GROUP (ORDER BY extract(epoch FROM arrived_at - left_dock))
         FROM trips
         WHERE arrived_at > left_dock
           AND left_dock >= now() - make_interval(days => $2)
           AND departing = ANY($1) AND arriving = ANY($1)
         GROUP BY 1, 2
         ORDER BY 1, 2",
    )
    .bind(&terminals)
    .bind(days as i32)
    .fetch_all(pool)
    .await?;
    let minutes = |s: f64| (s / 6.0).round() / 10.0;
    Ok(Some(CrossingStats {
        route: route.to_string(),
        days,
        pairs: rows
            .into_iter()
            .map(|(a, b, trips, median_s, p90_s)| PairStats {
                terminals: [a as u32, b as u32],
                trips: trips as u32,
                median_min: minutes(median_s),
                p90_min: minutes(p90_s),
            })
            .collect(),
    }))
}

/// The terminals route `route` calls at; None if there's no such route.
async fn route_terminals(pool: &PgPool, route: &str) -> Result<Option<Vec<i32>>> {
    let terminals: Vec<i32> =
        sqlx::query_scalar("SELECT terminal_id FROM route_terminals WHERE route_id = $1")
            .bind(route)
            .fetch_all(pool)
            .await?;
    if terminals.is_empty() {
        let known: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM routes WHERE id = $1)")
            .bind(route)
            .fetch_one(pool)
            .await?;
        if !known {
            return Ok(None);
        }
    }
    Ok(Some(terminals))
}

/// A boat's latest `limit` sailings with how full each left, newest first.
pub async fn vessel_loads(pool: &PgPool, vessel: u32, limit: u32) -> Result<VesselLoads> {
    let rows = sqlx::query(
        "SELECT departing, arriving, capacity, free, load, reading_age_min,
             (extract(epoch FROM scheduled_departure) * 1000)::bigint AS scheduled_ms,
             (extract(epoch FROM left_dock) * 1000)::bigint AS left_ms
         FROM sailing_loads WHERE vessel_id = $1
         ORDER BY left_dock DESC LIMIT $2",
    )
    .bind(vessel as i32)
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;
    Ok(VesselLoads {
        vessel,
        sailings: rows
            .iter()
            .map(|r| SailingLoad {
                from: r.get::<i32, _>("departing") as u32,
                to: r.get::<Option<i32>, _>("arriving").map(|t| t as u32),
                scheduled_ms: r.get("scheduled_ms"),
                left_ms: r.get("left_ms"),
                load: r.get("load"),
                capacity: r.get::<i32, _>("capacity") as u32,
                free: r.get::<i32, _>("free") as u32,
                reading_age_min: r.get("reading_age_min"),
            })
            .collect(),
    })
}

/// A sailing counts as full with this share of its car deck taken.
pub const FULL_LOAD: f64 = 0.95;
/// Loads from counts older than this (min) as the boat left are left out
/// of statistics: too many cars may have boarded since.
const STALE_READING_MIN: f64 = 15.0;

/// What load statistics are for: a route's directions, or a boat's.
pub enum LoadScope<'a> {
    Route(&'a str),
    Vessel(u32),
}

/// How full car decks are leaving in each direction, overall and by hour
/// of the day (Pacific time) on weekdays and weekends, from sailings in
/// the last `days`. None for a route there's no such.
pub async fn load_stats(
    pool: &PgPool,
    scope: LoadScope<'_>,
    days: u32,
) -> Result<Option<LoadStats>> {
    let (route, vessel, terminals) = match scope {
        LoadScope::Route(route) => {
            let Some(terminals) = route_terminals(pool, route).await? else {
                return Ok(None);
            };
            (Some(route.to_string()), None, Some(terminals))
        }
        LoadScope::Vessel(id) => (None, Some(id), None),
    };
    // One row per direction (all hours), then per direction, day type and
    // hour.
    let rows = sqlx::query(
        "WITH l AS (
             SELECT departing, arriving, load,
                 extract(hour FROM scheduled_departure AT TIME ZONE 'America/Los_Angeles')::int
                     AS hour,
                 extract(isodow FROM scheduled_departure AT TIME ZONE 'America/Los_Angeles') >= 6
                     AS weekend
             FROM sailing_loads
             WHERE left_dock >= now() - make_interval(days => $1)
               AND arriving IS NOT NULL AND reading_age_min <= $2
               AND ($3::int[] IS NULL OR (departing = ANY($3) AND arriving = ANY($3)))
               AND ($4::int IS NULL OR vessel_id = $4))
         SELECT departing, arriving, weekend, hour, GROUPING(weekend, hour) = 0 AS by_hour,
             count(*) AS sailings,
             percentile_cont(0.5) WITHIN GROUP (ORDER BY load) AS median_load,
             avg((load >= $5)::int)::float8 AS full_share
         FROM l
         GROUP BY GROUPING SETS ((departing, arriving), (departing, arriving, weekend, hour))
         ORDER BY departing, arriving, by_hour, weekend, hour",
    )
    .bind(days as i32)
    .bind(STALE_READING_MIN)
    .bind(terminals)
    .bind(vessel.map(|v| v as i32))
    .bind(FULL_LOAD)
    .fetch_all(pool)
    .await?;
    let mut directions: Vec<DirectionLoads> = Vec::new();
    for r in &rows {
        let (from, to) = (
            r.get::<i32, _>("departing") as u32,
            r.get::<i32, _>("arriving") as u32,
        );
        let sailings = r.get::<i64, _>("sailings") as u32;
        let median_load: f64 = r.get("median_load");
        if !r.get::<bool, _>("by_hour") {
            directions.push(DirectionLoads {
                from,
                to,
                sailings,
                median_load,
                hours: Vec::new(),
            });
        } else if let Some(d) = directions
            .last_mut()
            .filter(|d| (d.from, d.to) == (from, to))
        {
            d.hours.push(HourLoads {
                weekend: r.get("weekend"),
                hour: r.get::<i32, _>("hour") as u8,
                sailings,
                median_load,
                full_share: r.get("full_share"),
            });
        }
    }
    Ok(Some(LoadStats {
        route,
        vessel,
        days,
        directions,
    }))
}

/// `days` as asked for (none: the default), if it's a whole number from 1
/// to `MAX_DAYS`.
pub fn parse_days(days: Option<&str>) -> Option<u32> {
    match days {
        None => Some(DEFAULT_DAYS),
        Some(d) => d.parse().ok().filter(|d| (1..=MAX_DAYS).contains(d)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ferry_model::api::{Network, NetworkRoute};
    use ferry_model::geo::LonLat;
    use ferry_model::terminal::Terminal;

    #[test]
    fn reads_days() {
        assert_eq!(parse_days(None), Some(30));
        assert_eq!(parse_days(Some("7")), Some(7));
        assert_eq!(parse_days(Some("365")), Some(365));
        for bad in ["0", "366", "-1", "1.5", "week", ""] {
            assert_eq!(parse_days(Some(bad)), None, "{bad}");
        }
    }

    /// Seattle (7), Bremerton (4) and Bainbridge (3), with a Seattle /
    /// Bremerton route and some trips.
    pub(crate) async fn seed(pool: &PgPool) {
        let terminal = |id: u32, name: &str| Terminal {
            id,
            name: name.into(),
            position: LonLat::new(-122.4, 47.6),
        };
        crate::db::upsert_terminals(
            pool,
            &[
                terminal(7, "Seattle"),
                terminal(4, "Bremerton"),
                terminal(3, "Bainbridge Island"),
            ],
        )
        .await
        .unwrap();
        let route = |id: &str, terminals: Vec<u32>| NetworkRoute {
            id: id.into(),
            name: id.into(),
            terminals,
            legs: vec![],
        };
        let network = Network {
            terminals: vec![],
            legs: vec![],
            routes: vec![
                route("seattle-bremerton", vec![4, 7]),
                route("empty", vec![]),
            ],
            attribution: vec![],
        };
        crate::db::replace_network(pool, &network).await.unwrap();
        // (vessel, from, to, left this many days ago, took this many
        // minutes, or still under way).
        let trips: [(i32, i32, i32, f64, Option<f64>); 7] = [
            (1, 7, 4, 1.0, Some(58.0)),
            (1, 4, 7, 1.0, Some(60.0)),
            (2, 7, 4, 2.0, Some(56.0)),
            (2, 4, 7, 2.0, Some(80.0)),
            (3, 7, 4, 0.1, None),         // under way: not counted
            (3, 7, 3, 1.0, Some(35.0)),   // another route
            (4, 7, 4, 40.0, Some(120.0)), // older than 30 days
        ];
        for (vessel, from, to, ago_days, took) in trips {
            sqlx::query(
                "INSERT INTO trips (vessel_id, departing, arriving, left_dock, arrived_at)
                 VALUES ($1, $2, $3, now() - make_interval(secs => $4),
                     now() - make_interval(secs => $4) + make_interval(secs => $5))",
            )
            .bind(vessel)
            .bind(from)
            .bind(to)
            .bind(ago_days * 86_400.0)
            .bind(took.map(|m| m * 60.0))
            .execute(pool)
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn estimates_how_full_boats_leave() {
        let Some(db) = crate::db::tests::Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping");
            return;
        };
        let pool = &db.pool;
        seed(pool).await;
        // Sailings scheduled yesterday at 7:00, 7:30 and 8:00 Pacific,
        // leaving a minute late: (vessel, from, to, minutes past 7:00).
        let day = "date_trunc('day', now() AT TIME ZONE 'America/Los_Angeles')
                   AT TIME ZONE 'America/Los_Angeles' - interval '1 day' + interval '7 hours'";
        for (vessel, from, to, past) in [(5, 7, 4, 0), (5, 4, 7, 60), (6, 7, 4, 30)] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO trips (vessel_id, departing, arriving, scheduled_departure,
                     left_dock, arrived_at)
                 VALUES ($1, $2, $3, {day} + make_interval(mins => $4),
                     {day} + make_interval(mins => $4 + 1), {day} + make_interval(mins => $4 + 60))"
            )))
            .bind(vessel)
            .bind(from)
            .bind(to)
            .bind(past)
            .execute(pool)
            .await
            .unwrap();
        }
        // Counts: (from, minutes past 7:00 it's scheduled, recorded and
        // last seen this many minutes from its departure, drive-up,
        // oversold, reservable). Capacity 200.
        let counts = [
            // 7:00: 100 left an hour before, 10 at the end; a count after
            // it left doesn't count.
            (7, 0, -60, -50, 100, false, None),
            (7, 0, -50, 2, 10, false, None),
            (7, 0, 11, 12, 190, false, None),
            // 8:00 back: last seen 20 minutes before it left (stale).
            (4, 60, -40, -20, 150, false, None),
            // 7:30: oversold, 5 reservable spaces left.
            (7, 30, -5, 3, 0, true, Some(5)),
        ];
        for (from, past, at, until, drive_up, oversold, reservable) in counts {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO sailing_space (terminal_id, departs_at, destination_id, observed_at,
                     seen_until, vessel_id, drive_up, capacity, oversold, cancelled, reservable)
                 VALUES ($1, {day} + make_interval(mins => $2), $3,
                     {day} + make_interval(mins => $2 + $4), {day} + make_interval(mins => $2 + $5),
                     NULL, $6, 200, $7, false, $8)"
            )))
            .bind(from)
            .bind(past)
            .bind(if from == 7 { 4 } else { 7 })
            .bind(at)
            .bind(until)
            .bind(drive_up)
            .bind(oversold)
            .bind(reservable)
            .execute(pool)
            .await
            .unwrap();
        }

        // Boat 5, newest first: 8:00 a quarter full (from a stale count),
        // 7:00 95% full.
        let five = vessel_loads(pool, 5, 5).await.unwrap();
        let got: Vec<(u32, f64, u32, f64)> = five
            .sailings
            .iter()
            .map(|s| {
                (
                    s.from,
                    s.load,
                    s.free,
                    (s.reading_age_min * 10.0).round() / 10.0,
                )
            })
            .collect();
        assert_eq!(got, [(4, 0.25, 150, 21.0), (7, 0.95, 10, 0.0)]);

        // Seattle to Bremerton: both full; the stale count back is left out.
        let stats = load_stats(pool, LoadScope::Route("seattle-bremerton"), 30)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stats.directions.len(), 1);
        let d = &stats.directions[0];
        assert_eq!((d.from, d.to, d.sailings), (7, 4, 2));
        assert!((d.median_load - 0.9625).abs() < 1e-9);
        assert_eq!(d.hours.len(), 1);
        let h = &d.hours[0];
        assert_eq!((h.hour, h.sailings, h.full_share), (7, 2, 1.0));
        // By boat; and no such route.
        let six = load_stats(pool, LoadScope::Vessel(6), 30)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(six.directions[0].sailings, 1);
        assert!(
            load_stats(pool, LoadScope::Route("nowhere"), 30)
                .await
                .unwrap()
                .is_none()
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn measures_crossings_on_a_route() {
        let Some(db) = crate::db::tests::Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping");
            return;
        };
        let pool = &db.pool;
        seed(pool).await;

        let stats = crossings(pool, "seattle-bremerton", 30)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stats.pairs.len(), 1);
        let pair = &stats.pairs[0];
        // Four trips either way: 56, 58, 60 and 80 minutes.
        assert_eq!((pair.terminals, pair.trips), ([4, 7], 4));
        assert_eq!(pair.median_min, 59.0);
        assert_eq!(pair.p90_min, 74.0);
        // A longer window takes in the older trip.
        let year = crossings(pool, "seattle-bremerton", 365)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(year.pairs[0].trips, 5);
        // A route with no terminals has no pairs; an unknown route is none.
        assert!(
            crossings(pool, "empty", 30)
                .await
                .unwrap()
                .unwrap()
                .pairs
                .is_empty()
        );
        assert_eq!(crossings(pool, "nowhere", 30).await.unwrap(), None);
        db.finish().await;
    }
}
