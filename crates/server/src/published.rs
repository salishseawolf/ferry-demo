//! The published schedule through time (`published_sailings`), so that
//! what ran can later be compared with what was promised, and when. Derived
//! from WSF's schedule answers as they arrive, or all at once by `rebuild`.

use std::collections::HashSet;

use anyhow::Result;
use ferry_model::api::Loading;
use sqlx::{PgPool, Row};

use crate::db;
use crate::wsf::{self, DaySchedule};

/// Paths of the answers this derives from.
pub fn routes_path(trip_date: &str) -> String {
    format!("schedule/rest/routes/{trip_date}")
}

pub fn schedule_path(trip_date: &str, wsf_route: u32) -> String {
    format!("schedule/rest/schedule/{trip_date}/{wsf_route}")
}

/// One version of a sailing: all that's compared between answers.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Version {
    departing: i32,
    arriving: i32,
    departs_ms: i64,
    arrives_ms: Option<i64>,
    vessel_id: Option<i32>,
    vessel: String,
    loading: String,
    notes: Vec<String>,
}

fn versions(day: &DaySchedule) -> HashSet<Version> {
    let mut all = HashSet::new();
    for pair in &day.pairs {
        for s in &pair.sailings {
            all.insert(Version {
                departing: pair.from as i32,
                arriving: pair.to as i32,
                departs_ms: s.departs_ms,
                arrives_ms: s.arrives_ms,
                vessel_id: s.vessel_id.map(|id| id as i32),
                vessel: s.vessel.clone(),
                loading: match s.loading {
                    Loading::Passengers => "passengers",
                    Loading::Vehicles => "vehicles",
                    Loading::Both => "both",
                }
                .to_string(),
                notes: (s.notes.iter())
                    .filter_map(|&i| pair.notes.get(i).cloned())
                    .collect(),
            });
        }
    }
    all
}

/// Takes in a route's schedule for a trip day, as WSF gave it at `seen_ms`:
/// versions no longer listed are withdrawn, new ones added, and the rest
/// marked as still seen.
pub async fn record_schedule(
    pool: &PgPool,
    trip_date: &str,
    wsf_route: u32,
    day: &DaySchedule,
    seen_ms: i64,
) -> Result<()> {
    let listed = versions(day);
    let mut tx = pool.begin().await?;
    let rows = sqlx::query(
        "SELECT departing, arriving, first_seen,
             (extract(epoch FROM departs_at) * 1000)::bigint AS departs_ms,
             (extract(epoch FROM arrives_at) * 1000)::bigint AS arrives_ms,
             vessel_id, vessel, loading, notes
         FROM published_sailings
         WHERE trip_date = $1::date AND wsf_route = $2 AND withdrawn_at IS NULL",
    )
    .bind(trip_date)
    .bind(wsf_route as i32)
    .fetch_all(&mut *tx)
    .await?;
    let mut open = HashSet::new();
    for row in &rows {
        let sqlx::types::Json(notes): sqlx::types::Json<Vec<String>> = row.get("notes");
        let version = Version {
            departing: row.get("departing"),
            arriving: row.get("arriving"),
            departs_ms: row.get("departs_ms"),
            arrives_ms: row.get("arrives_ms"),
            vessel_id: row.get("vessel_id"),
            vessel: row.get("vessel"),
            loading: row.get("loading"),
            notes,
        };
        if !listed.contains(&version) {
            sqlx::query(
                "UPDATE published_sailings SET withdrawn_at = to_timestamp($1::double precision / 1000)
                 WHERE trip_date = $2::date AND wsf_route = $3 AND departing = $4 AND arriving = $5
                     AND departs_at = to_timestamp($6::double precision / 1000)
                     AND vessel = $7 AND withdrawn_at IS NULL",
            )
            .bind(seen_ms)
            .bind(trip_date)
            .bind(wsf_route as i32)
            .bind(version.departing)
            .bind(version.arriving)
            .bind(version.departs_ms)
            .bind(&version.vessel)
            .execute(&mut *tx)
            .await?;
        }
        open.insert(version);
    }
    sqlx::query(
        "UPDATE published_sailings SET last_seen = to_timestamp($1::double precision / 1000)
         WHERE trip_date = $2::date AND wsf_route = $3 AND withdrawn_at IS NULL",
    )
    .bind(seen_ms)
    .bind(trip_date)
    .bind(wsf_route as i32)
    .execute(&mut *tx)
    .await?;
    for v in listed.difference(&open) {
        sqlx::query(
            "INSERT INTO published_sailings (trip_date, wsf_route, departing, arriving, departs_at,
                 arrives_at, vessel_id, vessel, loading, notes, first_seen, last_seen)
             VALUES ($1::date, $2, $3, $4, to_timestamp($5::double precision / 1000),
                 to_timestamp($6::double precision / 1000), $7, $8, $9, $10,
                 to_timestamp($11::double precision / 1000),
                 to_timestamp($11::double precision / 1000))
             ON CONFLICT DO NOTHING",
        )
        .bind(trip_date)
        .bind(wsf_route as i32)
        .bind(v.departing)
        .bind(v.arriving)
        .bind(v.departs_ms)
        .bind(v.arrives_ms)
        .bind(v.vessel_id)
        .bind(&v.vessel)
        .bind(&v.loading)
        .bind(sqlx::types::Json(&v.notes))
        .bind(seen_ms)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Takes in WSF's list of routes with schedules for a trip day: a route no
/// longer on it has all its sailings withdrawn. An empty list says nothing
/// (see `schedule::fetch_day`).
pub async fn record_routes(
    pool: &PgPool,
    trip_date: &str,
    wsf_routes: &[u32],
    seen_ms: i64,
) -> Result<()> {
    if wsf_routes.is_empty() {
        return Ok(());
    }
    let ids: Vec<i32> = wsf_routes.iter().map(|&id| id as i32).collect();
    sqlx::query(
        "UPDATE published_sailings SET withdrawn_at = to_timestamp($1::double precision / 1000)
         WHERE trip_date = $2::date AND withdrawn_at IS NULL AND NOT (wsf_route = ANY($3))",
    )
    .bind(seen_ms)
    .bind(trip_date)
    .bind(&ids)
    .execute(pool)
    .await?;
    Ok(())
}

/// A path's trip day and, for a schedule, WSF's route.
fn parse_path(path: &str) -> Option<(&str, Option<u32>)> {
    if let Some(date) = path.strip_prefix("schedule/rest/routes/") {
        return Some((date, None));
    }
    let (date, route) = path
        .strip_prefix("schedule/rest/schedule/")?
        .split_once('/')?;
    Some((date, Some(route.parse().ok()?)))
}

/// Derives it all again from the raw poll log, oldest first: (answers
/// applied, answers unreadable). The caller empties the table.
pub async fn rebuild(pool: &PgPool, page: i64) -> Result<(u64, u64)> {
    let (mut answers, mut unreadable) = (0, 0);
    let mut after: i64 = 0;
    loop {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT id, path, {} AS fetched_ms, body::text AS body FROM raw_polls
             WHERE (path LIKE 'schedule/rest/routes/%' OR path LIKE 'schedule/rest/schedule/%')
                 AND status = 200 AND body IS NOT NULL AND id > $1
             ORDER BY id LIMIT $2",
            db::FETCHED_MS
        )))
        .bind(after)
        .bind(page)
        .fetch_all(pool)
        .await?;
        let Some(last) = rows.last() else { break };
        after = last.get("id");
        for row in &rows {
            let path: String = row.get("path");
            let body: String = row.get("body");
            let seen_ms: i64 = row.get("fetched_ms");
            let applied = match parse_path(&path) {
                Some((date, None)) => match wsf::parse_schedule_routes(&body) {
                    Ok(routes) => {
                        record_routes(pool, date, &routes, seen_ms).await?;
                        true
                    }
                    Err(_) => false,
                },
                Some((date, Some(route))) => match wsf::parse_schedule(&body) {
                    Ok(day) => {
                        record_schedule(pool, date, route, &day, seen_ms).await?;
                        true
                    }
                    Err(_) => false,
                },
                None => false,
            };
            if applied {
                answers += 1;
            } else {
                unreadable += 1;
            }
        }
    }
    Ok((answers, unreadable))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tests::Scratch;
    use crate::wsf::{PairSailing, PairSailings};

    const DATE: &str = "2026-10-30";

    fn sailing(departs_ms: i64, vessel: &str) -> PairSailing {
        PairSailing {
            departs_ms,
            arrives_ms: Some(departs_ms + 1_800_000),
            vessel_id: Some(vessel.len() as u32),
            vessel: vessel.to_string(),
            loading: Loading::Both,
            notes: vec![],
        }
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

    /// (departs, vessel, first seen, last seen, withdrawn), oldest first.
    async fn rows(pool: &PgPool) -> Vec<(i64, String, i64, i64, Option<i64>)> {
        let ms = |c: &str| format!("(extract(epoch FROM {c}) * 1000)::bigint");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {} AS departs, vessel, {} AS first, {} AS last, {} AS gone
             FROM published_sailings ORDER BY first_seen, departs_at",
            ms("departs_at"),
            ms("first_seen"),
            ms("last_seen"),
            ms("withdrawn_at")
        )))
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.get("departs"),
                r.get("vessel"),
                r.get("first"),
                r.get("last"),
                r.get("gone"),
            )
        })
        .collect()
    }

    #[tokio::test]
    async fn keeps_each_version_of_a_sailing() {
        let Some(db) = Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping");
            return;
        };
        let pool = &db.pool;
        let (a, b, c) = (1_000_000, 2_000_000, 3_000_000);
        // First published.
        let first = day(vec![sailing(a, "Spokane"), sailing(b, "Spokane")]);
        record_schedule(pool, DATE, 6, &first, 10_000)
            .await
            .unwrap();
        // The same again: only seen later.
        record_schedule(pool, DATE, 6, &first, 20_000)
            .await
            .unwrap();
        assert_eq!(
            rows(pool).await,
            [
                (a, "Spokane".to_string(), 10_000, 20_000, None),
                (b, "Spokane".to_string(), 10_000, 20_000, None),
            ]
        );
        // The first sailing given to another boat, the second retimed.
        let changed = day(vec![sailing(a, "Walla Walla"), sailing(c, "Spokane")]);
        record_schedule(pool, DATE, 6, &changed, 30_000)
            .await
            .unwrap();
        assert_eq!(
            rows(pool).await,
            [
                (a, "Spokane".to_string(), 10_000, 20_000, Some(30_000)),
                (b, "Spokane".to_string(), 10_000, 20_000, Some(30_000)),
                (a, "Walla Walla".to_string(), 30_000, 30_000, None),
                (c, "Spokane".to_string(), 30_000, 30_000, None),
            ]
        );
        // WSF stops listing the route: everything's withdrawn. An empty
        // list isn't believed, and doesn't.
        record_routes(pool, DATE, &[], 35_000).await.unwrap();
        record_routes(pool, DATE, &[6], 36_000).await.unwrap();
        assert!(rows(pool).await[2].4.is_none());
        record_routes(pool, DATE, &[13], 40_000).await.unwrap();
        assert!(rows(pool).await.iter().all(|r| r.4.is_some()));
        // It comes back: a new row, the old history kept.
        record_schedule(pool, DATE, 6, &changed, 50_000)
            .await
            .unwrap();
        let all = rows(pool).await;
        assert_eq!(all.len(), 6);
        assert_eq!(all[4].2, 50_000);
        assert_eq!(all[4].4, None);
        db.finish().await;
    }

    #[tokio::test]
    async fn rebuilds_from_the_recorded_answers() {
        let Some(db) = Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping");
            return;
        };
        let pool = &db.pool;
        // A route's schedule as WSF writes it, one sailing on `vessel`.
        let answer = |vessel: &str| {
            serde_json::json!({
                "ScheduleName": "Fall 2026",
                "TerminalCombos": [{
                    "DepartingTerminalID": 8, "ArrivingTerminalID": 12, "Annotations": [],
                    "Times": [{
                        "DepartingTime": "/Date(1000000-0700)/", "VesselID": 1,
                        "VesselName": vessel, "LoadingRule": 3, "AnnotationIndexes": [],
                    }],
                }],
            })
        };
        let schedule = schedule_path(DATE, 6);
        let routes = routes_path(DATE);
        let polls = [
            (&schedule, 10, Some(answer("Spokane"))),
            // A failed fetch says nothing.
            (&schedule, 15, None),
            (&schedule, 20, Some(answer("Walla Walla"))),
            (&routes, 30, Some(serde_json::json!([{"RouteID": 13}]))),
        ];
        for (path, at_s, body) in &polls {
            let status: i16 = if body.is_some() { 200 } else { 500 };
            sqlx::query(
                "INSERT INTO raw_polls (path, fetched_at, status, body)
                 VALUES ($1, to_timestamp($2::double precision), $3, $4)",
            )
            .bind(path.as_str())
            .bind(*at_s)
            .bind(status)
            .bind(body.as_ref().map(sqlx::types::Json))
            .execute(pool)
            .await
            .unwrap();
        }
        // Whatever was derived before is replaced.
        record_schedule(pool, "2026-01-01", 1, &day(vec![]), 1)
            .await
            .unwrap();
        crate::history::rebuild(pool).await.unwrap();
        assert_eq!(
            rows(pool).await,
            [
                (
                    1_000_000,
                    "Spokane".to_string(),
                    10_000,
                    10_000,
                    Some(20_000)
                ),
                (
                    1_000_000,
                    "Walla Walla".to_string(),
                    20_000,
                    20_000,
                    Some(30_000)
                ),
            ]
        );
        db.finish().await;
    }

    #[test]
    fn reads_answer_paths() {
        assert_eq!(parse_path(&routes_path(DATE)), Some((DATE, None)));
        assert_eq!(parse_path(&schedule_path(DATE, 6)), Some((DATE, Some(6))));
        assert_eq!(parse_path("schedule/rest/cacheflushdate"), None);
    }
}
