//! History, derived from WSF's answers: boat positions, trips, changes in
//! drive-up space, notices, and which way each slip opens. Each live poll
//! adds its answer (`apply`); `rebuild` derives it all again from the raw
//! poll log. Applying answers in the order they came gives the same tables
//! either way.

use anyhow::Result;
use ferry_model::api::{LiveVessel, Notice, Sailing};
use ferry_model::geo::{EARTH_RADIUS_M, LonLat, distance_m};
use ferry_model::terminal::Terminal;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::db;
use crate::live::{Feed, Items, Tracks, parse};

/// A boat under way within this distance (m) of the terminal it's leaving
/// shows which way the slip opens.
const FACING_NEAR_M: f64 = 1500.0;
/// Under way at this speed (knots) or more.
const MOVING_KN: f64 = 1.0;
/// A slip's direction blends in each sighting, weighing what's been seen
/// as at most this many sightings, so it settles but can still change.
const FACING_MEMORY: f64 = 20.0;
/// WSF can move a boat's LeftDock more than once as it leaves (it eases
/// out of the slip, reports docked again for a moment, then goes): a new
/// LeftDock from the same terminal within this many minutes of an open
/// trip's is that trip, leaving later.
const RELEFT_MINUTES: i32 = 10;
/// Raw answers read at a time by `rebuild`.
const REBUILD_PAGE: i64 = 200;

type Tx<'a> = Transaction<'a, Postgres>;

/// What applying an answer learned that the rest of the server cares about.
#[derive(Debug, Default, PartialEq)]
pub struct Applied {
    /// A slip's direction was seen for the first time (the route network,
    /// which carries them, is worth rebuilding).
    pub new_facing: bool,
}

/// Adds an answer's history, fetched at `fetched_ms` (epoch), in one
/// transaction.
pub async fn apply(
    pool: &PgPool,
    items: &Items,
    fetched_ms: i64,
    terminals: &[Terminal],
) -> Result<Applied> {
    let mut tx = pool.begin().await?;
    let applied = match items {
        Items::Vessels(vessels) => apply_vessels(&mut tx, vessels, terminals).await?,
        Items::Sailings(sailings) => {
            apply_sailings(&mut tx, sailings, fetched_ms).await?;
            Applied::default()
        }
        Items::Notices(notices) => {
            apply_notices(&mut tx, notices, fetched_ms).await?;
            Applied::default()
        }
    };
    tx.commit().await?;
    Ok(applied)
}

/// A time in SQL from epoch milliseconds (a bound parameter).
const AT: &str = "to_timestamp($1::double precision / 1000)";

async fn apply_vessels(
    tx: &mut Tx<'_>,
    vessels: &[LiveVessel],
    terminals: &[Terminal],
) -> Result<Applied> {
    let mut applied = Applied::default();
    for v in vessels.iter().filter(|v| v.reported_ms > 0) {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO vessel_positions (reported_at, vessel_id, lon, lat, speed_kn, heading_deg,
                 at_dock, in_service, departing, arriving)
             VALUES ({AT}, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT DO NOTHING"
        )))
        .bind(v.reported_ms)
        .bind(v.id as i32)
        .bind(v.lon)
        .bind(v.lat)
        .bind(v.speed_kn as f32)
        .bind(v.heading_deg as f32)
        .bind(v.at_dock)
        .bind(v.in_service)
        .bind(v.departing.map(|t| t as i32))
        .bind(v.arriving.map(|t| t as i32))
        .execute(&mut **tx)
        .await?;

        match (v.at_dock, v.left_dock_ms, v.departing, v.arriving) {
            // Under way: the trip it's on (its open one from here, if it
            // left lately: see RELEFT_MINUTES), with WSF's latest estimate.
            // A boat off the schedule may name no destination: it's filled
            // in when the boat docks.
            (false, Some(left), Some(from), to) => {
                let moved = sqlx::query(
                    "UPDATE trips SET
                         left_dock = GREATEST(left_dock, to_timestamp($3::double precision / 1000)),
                         arriving = COALESCE($4, arriving),
                         scheduled_departure =
                             COALESCE(to_timestamp($5::double precision / 1000), scheduled_departure),
                         eta = COALESCE(to_timestamp($6::double precision / 1000), eta)
                     WHERE id = (
                         SELECT id FROM trips
                         WHERE vessel_id = $1 AND departing = $2 AND arrived_at IS NULL
                           AND abs(extract(epoch FROM left_dock) - $3::double precision / 1000)
                               < $7 * 60
                         ORDER BY left_dock DESC LIMIT 1)",
                )
                .bind(v.id as i32)
                .bind(from as i32)
                .bind(left)
                .bind(to.map(|t| t as i32))
                .bind(v.scheduled_departure_ms)
                .bind(v.eta_ms)
                .bind(RELEFT_MINUTES)
                .execute(&mut **tx)
                .await?
                .rows_affected();
                if moved == 0 {
                    sqlx::query(
                        "INSERT INTO trips (vessel_id, departing, arriving, scheduled_departure,
                             left_dock, eta)
                         VALUES ($1, $2, $3, to_timestamp($4::double precision / 1000),
                             to_timestamp($5::double precision / 1000),
                             to_timestamp($6::double precision / 1000))
                         ON CONFLICT (vessel_id, left_dock) DO UPDATE SET
                             arriving = COALESCE(EXCLUDED.arriving, trips.arriving),
                             scheduled_departure =
                                 COALESCE(EXCLUDED.scheduled_departure, trips.scheduled_departure),
                             eta = COALESCE(EXCLUDED.eta, trips.eta)",
                    )
                    .bind(v.id as i32)
                    .bind(from as i32)
                    .bind(to.map(|t| t as i32))
                    .bind(v.scheduled_departure_ms)
                    .bind(left)
                    .bind(v.eta_ms)
                    .execute(&mut **tx)
                    .await?;
                }
            }
            // Docked: the end of its latest open trip to here, or to
            // somewhere unnamed from elsewhere (WSF names the terminal it's
            // at as the one it leaves next). An unnamed one from here went
            // nowhere (a boat shifting berths): it's dropped.
            (true, _, Some(here), _) => {
                sqlx::query(
                    "DELETE FROM trips
                     WHERE vessel_id = $1 AND arrived_at IS NULL
                       AND arriving IS NULL AND departing = $2",
                )
                .bind(v.id as i32)
                .bind(here as i32)
                .execute(&mut **tx)
                .await?;
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE trips SET arrived_at = {AT}, arriving = $3
                     WHERE id = (
                         SELECT id FROM trips
                         WHERE vessel_id = $2 AND arrived_at IS NULL
                           AND (arriving = $3 OR (arriving IS NULL AND departing <> $3))
                           AND left_dock < {AT} AND left_dock > {AT} - interval '6 hours'
                         ORDER BY left_dock DESC LIMIT 1)"
                )))
                .bind(v.reported_ms)
                .bind(v.id as i32)
                .bind(here as i32)
                .execute(&mut **tx)
                .await?;
            }
            _ => {}
        }

        if let Some(t) = leaving_slip(v, terminals) {
            applied.new_facing |= sight_facing(tx, t, v.heading_deg, v.reported_ms).await?;
        }
    }
    Ok(applied)
}

/// The terminal whose slip `v` is leaving, if it's under way close by.
fn leaving_slip(v: &LiveVessel, terminals: &[Terminal]) -> Option<u32> {
    if v.at_dock || v.speed_kn < MOVING_KN {
        return None;
    }
    let t = terminals.iter().find(|t| Some(t.id) == v.departing)?;
    let here = LonLat::new(v.lon, v.lat);
    (distance_m(t.position, here, EARTH_RADIUS_M) < FACING_NEAR_M).then_some(t.id)
}

/// Blends a sighting into a slip's direction; says whether it's the
/// slip's first.
async fn sight_facing(
    tx: &mut Tx<'_>,
    terminal: u32,
    heading_deg: f64,
    at_ms: i64,
) -> Result<bool> {
    let known: Option<(f64, i32)> =
        sqlx::query_as("SELECT heading_deg, samples FROM terminal_facing WHERE terminal_id = $1")
            .bind(terminal as i32)
            .fetch_optional(&mut **tx)
            .await?;
    let (heading, samples) = blend(known, heading_deg);
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO terminal_facing (observed_at, terminal_id, heading_deg, samples)
         VALUES ({AT}, $2, $3, $4)
         ON CONFLICT (terminal_id) DO UPDATE SET
             heading_deg = $3, samples = $4, observed_at = EXCLUDED.observed_at"
    )))
    .bind(at_ms)
    .bind(terminal as i32)
    .bind(heading)
    .bind(samples)
    .execute(&mut **tx)
    .await?;
    Ok(known.is_none())
}

/// A direction (degrees) averaged with a new sighting, as unit vectors (so
/// 350° and 10° average to 0°, not 180°), the past weighing as many
/// sightings as it had, up to `FACING_MEMORY`.
fn blend(known: Option<(f64, i32)>, seen_deg: f64) -> (f64, i32) {
    let Some((deg, samples)) = known else {
        return (seen_deg.rem_euclid(360.0), 1);
    };
    let w = f64::from(samples).min(FACING_MEMORY);
    let (old, new) = (deg.to_radians(), seen_deg.to_radians());
    let east = w * old.sin() + new.sin();
    let north = w * old.cos() + new.cos();
    (
        east.atan2(north).to_degrees().rem_euclid(360.0),
        samples + 1,
    )
}

async fn apply_sailings(tx: &mut Tx<'_>, sailings: &[Sailing], fetched_ms: i64) -> Result<()> {
    for s in sailings {
        // Destination 0 where WSF named none.
        let none = [ferry_model::api::Destination {
            terminal: 0,
            drive_up: None,
            oversold: false,
            reservable: None,
            oversold_by: None,
        }];
        let destinations = if s.destinations.is_empty() {
            &none[..]
        } else {
            &s.destinations[..]
        };
        for d in destinations {
            // A new record when something changed since the last; else
            // the last one stands until now (seen_until).
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "WITH last AS (
                     SELECT observed_at, vessel_id, drive_up, reservable, capacity,
                         oversold, cancelled, oversold_by
                     FROM sailing_space
                     WHERE terminal_id = $2
                       AND departs_at = to_timestamp($3::double precision / 1000)
                       AND destination_id = $4
                     ORDER BY observed_at DESC LIMIT 1),
                 same AS (
                     UPDATE sailing_space s SET seen_until = {AT}
                     FROM last
                     WHERE s.terminal_id = $2
                       AND s.departs_at = to_timestamp($3::double precision / 1000)
                       AND s.destination_id = $4
                       AND s.observed_at = last.observed_at
                       AND last.vessel_id IS NOT DISTINCT FROM $5
                       AND last.drive_up IS NOT DISTINCT FROM $6
                       AND last.capacity IS NOT DISTINCT FROM $7
                       AND last.oversold = $8 AND last.cancelled = $9
                       AND last.reservable IS NOT DISTINCT FROM $10
                       AND last.oversold_by IS NOT DISTINCT FROM $11
                     RETURNING 1)
                 INSERT INTO sailing_space (observed_at, terminal_id, departs_at, destination_id,
                     vessel_id, drive_up, capacity, oversold, cancelled, reservable, oversold_by,
                     seen_until)
                 SELECT {AT}, $2, to_timestamp($3::double precision / 1000), $4, $5, $6, $7, $8,
                     $9, $10, $11, {AT}
                 WHERE NOT EXISTS (SELECT 1 FROM same)
                 ON CONFLICT DO NOTHING"
            )))
            .bind(fetched_ms)
            .bind(s.terminal as i32)
            .bind(s.departs_ms)
            .bind(d.terminal as i32)
            .bind(s.vessel.map(|v| v as i32))
            .bind(d.drive_up.map(|n| n as i32))
            .bind(s.capacity.map(|n| n as i32))
            .bind(d.oversold)
            .bind(s.cancelled)
            .bind(d.reservable.map(|n| n as i32))
            .bind(d.oversold_by.map(|n| n as i32))
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

async fn apply_notices(tx: &mut Tx<'_>, notices: &[Notice], fetched_ms: i64) -> Result<()> {
    for n in notices {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO notices (first_seen, last_seen, id, title, text, updated_at)
             VALUES ({AT}, {AT}, $2, $3, $4, to_timestamp($5::double precision / 1000))
             ON CONFLICT (id) DO UPDATE SET
                 title = EXCLUDED.title, text = EXCLUDED.text,
                 updated_at = EXCLUDED.updated_at,
                 last_seen = GREATEST(notices.last_seen, EXCLUDED.last_seen)"
        )))
        .bind(fetched_ms)
        .bind(&n.id)
        .bind(&n.title)
        .bind(&n.text)
        .bind(n.updated_ms)
        .execute(&mut **tx)
        .await?;
        for &t in &n.terminals {
            sqlx::query(
                "INSERT INTO notice_terminals (notice_id, terminal_id) VALUES ($1, $2)
                 ON CONFLICT DO NOTHING",
            )
            .bind(&n.id)
            .bind(t as i32)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

/// How much `rebuild` derived.
#[derive(Debug, Default)]
pub struct Rebuilt {
    pub answers: u64,
    pub unreadable: u64,
}

/// Derives all history again from the raw poll log: empties the history
/// tables, then applies every whole answer, oldest first. Run it with the
/// server stopped, so live polls don't interleave.
pub async fn rebuild(pool: &PgPool) -> Result<Rebuilt> {
    sqlx::query(
        "TRUNCATE vessel_positions, trips, sailing_space, notices, notice_terminals,
             terminal_facing",
    )
    .execute(pool)
    .await?;
    let terminals = db::terminals(pool).await?;
    let mut rebuilt = Rebuilt::default();
    for feed in Feed::ALL {
        let mut tracks = Tracks::default();
        let mut after: i64 = 0;
        loop {
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT id, {} AS fetched_ms, body::text AS body FROM raw_polls
                 WHERE path = $1 AND status = 200 AND body IS NOT NULL AND id > $2
                 ORDER BY id LIMIT $3",
                db::FETCHED_MS
            )))
            .bind(feed.path())
            .bind(after)
            .bind(REBUILD_PAGE)
            .fetch_all(pool)
            .await?;
            let Some(last) = rows.last() else { break };
            after = last.get("id");
            for row in &rows {
                let body: String = row.get("body");
                match parse(feed, &body, &terminals, &mut tracks) {
                    Ok(items) => {
                        apply(pool, &items, row.get("fetched_ms"), &terminals).await?;
                        rebuilt.answers += 1;
                    }
                    Err(_) => rebuilt.unreadable += 1,
                }
            }
        }
    }
    Ok(rebuilt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blends_directions_around_the_compass() {
        assert_eq!(blend(None, 365.0), (5.0, 1));
        // 350° and 10° meet at north, not south.
        let (deg, n) = blend(Some((350.0, 1)), 10.0);
        assert!(deg.min(360.0 - deg) < 1e-9 && n == 2);
        // A settled direction moves only a little for one odd sighting.
        let (deg, n) = blend(Some((90.0, 50)), 180.0);
        assert!((deg - 92.86).abs() < 0.01, "{deg}");
        assert_eq!(n, 51);
    }

    #[test]
    fn learns_slips_only_from_boats_leaving_close_by() {
        let seattle = Terminal {
            id: 7,
            name: "Seattle".into(),
            position: LonLat::new(-122.3405, 47.6025),
        };
        let mut v = crate::live::tests::boat(1, 1, -122.35);
        v.departing = Some(7);
        v.lat = 47.6025;
        assert_eq!(leaving_slip(&v, std::slice::from_ref(&seattle)), Some(7));
        v.lon = -122.40; // 4.5 km out
        assert_eq!(leaving_slip(&v, std::slice::from_ref(&seattle)), None);
        v.lon = -122.35;
        v.speed_kn = 0.5;
        assert_eq!(leaving_slip(&v, std::slice::from_ref(&seattle)), None);
        v.speed_kn = 12.0;
        v.departing = Some(3);
        assert_eq!(leaving_slip(&v, &[seattle]), None);
    }

    /// A vessellocations answer for one boat, as WSF writes it.
    fn position(
        at_s: i64,
        lon: f64,
        at_dock: bool,
        departing: u32,
        arriving: u32,
        eta_s: Option<i64>,
    ) -> serde_json::Value {
        let date = |s: i64| format!("/Date({}-0700)/", s * 1000);
        serde_json::json!([{
            "VesselID": 28, "VesselName": "Sealth", "Latitude": 47.6, "Longitude": lon,
            "Speed": if at_dock { 0.0 } else { 15.0 }, "Heading": 262, "InService": true,
            "AtDock": at_dock, "DepartingTerminalID": departing, "ArrivingTerminalID": arriving,
            "ScheduledDeparture": date(1000), "LeftDock": if at_dock { None } else { Some(date(1060)) },
            "Eta": eta_s.map(date), "TimeStamp": date(at_s),
        }])
    }

    #[tokio::test]
    async fn a_departure_reported_twice_is_one_trip() {
        let Some(db) = crate::db::tests::Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping");
            return;
        };
        let pool = &db.pool;
        let left_at = |mut answer: serde_json::Value, s: i64| {
            answer[0]["LeftDock"] = format!("/Date({}-0700)/", s * 1000).into();
            answer
        };
        // Sealth eases out at 1060, shows docked again, leaves for good at
        // 1180, and docks at Bremerton. Later it leaves Bremerton.
        let answers = [
            (1070, position(1070, -122.35, false, 7, 4, Some(4600))),
            (1100, position(1100, -122.35, true, 7, 4, None)),
            (
                1190,
                left_at(position(1190, -122.35, false, 7, 4, Some(4700)), 1180),
            ),
            // WSF going back to the first time doesn't move it back.
            (1200, position(1200, -122.36, false, 7, 4, None)),
            (4700, position(4700, -122.624, true, 4, 7, None)),
            (
                5000,
                left_at(position(5000, -122.62, false, 4, 7, Some(8000)), 4990),
            ),
        ];
        for (at_s, body) in &answers {
            let path = "vessels/rest/vessellocations";
            crate::db::insert_raw(pool, path, at_s * 1000, Some(200), Some(body), None)
                .await
                .unwrap();
        }
        rebuild(pool).await.unwrap();
        // From, to, left, ETA, arrived.
        type Trip = (i32, i32, f64, Option<f64>, Option<f64>);
        let trips: Vec<Trip> = sqlx::query_as(
            "SELECT departing, arriving, extract(epoch FROM left_dock)::float8,
                 extract(epoch FROM eta)::float8, extract(epoch FROM arrived_at)::float8
             FROM trips ORDER BY left_dock",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(
            trips,
            [
                (7, 4, 1180.0, Some(4700.0), Some(4700.0)),
                (4, 7, 4990.0, Some(8000.0), None)
            ]
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn an_unscheduled_trip_learns_where_it_went_at_the_dock() {
        let Some(db) = crate::db::tests::Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping");
            return;
        };
        let pool = &db.pool;
        // A bonus boat: WSF names no destination or scheduled departure.
        let unscheduled = |mut answer: serde_json::Value| {
            answer[0]["ArrivingTerminalID"] = serde_json::Value::Null;
            answer[0]["ScheduledDeparture"] = serde_json::Value::Null;
            answer
        };
        let left_at = |mut answer: serde_json::Value, s: i64| {
            answer[0]["LeftDock"] = format!("/Date({}-0700)/", s * 1000).into();
            answer
        };
        // It leaves Seattle, shows docked there again for a moment (not
        // an arrival), crosses, docks at Bremerton and shifts berths there
        // (no trip); then a scheduled trip back.
        let answers = [
            (
                1070,
                unscheduled(position(1070, -122.35, false, 7, 4, None)),
            ),
            (1080, unscheduled(position(1080, -122.35, true, 7, 4, None))),
            (
                2000,
                unscheduled(position(2000, -122.50, false, 7, 4, None)),
            ),
            (
                4700,
                unscheduled(position(4700, -122.624, true, 4, 7, None)),
            ),
            (
                4800,
                unscheduled(left_at(position(4800, -122.624, false, 4, 7, None), 4790)),
            ),
            (
                4900,
                unscheduled(position(4900, -122.624, true, 4, 7, None)),
            ),
            (
                5000,
                left_at(position(5000, -122.62, false, 4, 7, Some(8000)), 4990),
            ),
        ];
        for (at_s, body) in &answers {
            let path = "vessels/rest/vessellocations";
            crate::db::insert_raw(pool, path, at_s * 1000, Some(200), Some(body), None)
                .await
                .unwrap();
        }
        rebuild(pool).await.unwrap();
        // From, to, arrived, unscheduled.
        type Trip = (i32, Option<i32>, Option<f64>, bool);
        let trips: Vec<Trip> = sqlx::query_as(
            "SELECT departing, arriving, extract(epoch FROM arrived_at)::float8, unscheduled
             FROM trips ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(
            trips,
            [(7, Some(4), Some(4700.0), true), (4, Some(7), None, false)]
        );
        db.finish().await;
    }

    /// A terminalsailingspace answer: Seattle's 2:30 to Bremerton with
    /// `count` cars' space, and a cancelled sailing naming no destination.
    fn space(count: i32) -> serde_json::Value {
        serde_json::json!([{"TerminalID": 7, "DepartingSpaces": [
            {"Departure": "/Date(9000000000-0700)/", "IsCancelled": false,
             "VesselID": 28, "MaxSpaceCount": 188, "SpaceForArrivalTerminals": [
                {"TerminalID": 4, "TerminalName": "Seattle -> Bremerton",
                 "ArrivalTerminalIDs": [4], "DisplayDriveUpSpace": true,
                 "DriveUpSpaceCount": count}]},
            {"Departure": "/Date(9100000000-0700)/", "IsCancelled": true,
             "VesselID": null, "MaxSpaceCount": null, "SpaceForArrivalTerminals": []}]}])
    }

    #[tokio::test]
    async fn rebuilds_history_from_the_raw_log() {
        let Some(db) = crate::db::tests::Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping");
            return;
        };
        let pool = &db.pool;
        crate::db::upsert_terminals(
            pool,
            &[
                Terminal {
                    id: 7,
                    name: "Seattle".into(),
                    position: LonLat::new(-122.3405, 47.6025),
                },
                Terminal {
                    id: 4,
                    name: "Bremerton".into(),
                    position: LonLat::new(-122.6240, 47.5618),
                },
            ],
        )
        .await
        .unwrap();
        // Sealth leaves Seattle (close to its slip, then out in the Sound),
        // its ETA revised on the way, then docks at Bremerton, twice over.
        let vessels = "vessels/rest/vessellocations";
        let answers = [
            (
                vessels,
                1100,
                position(1100, -122.35, false, 7, 4, Some(4600)),
            ),
            (
                vessels,
                1110,
                position(1110, -122.35, false, 7, 4, Some(4600)),
            ),
            (
                vessels,
                2000,
                position(2000, -122.50, false, 7, 4, Some(4700)),
            ),
            (vessels, 4700, position(4700, -122.624, true, 4, 7, None)),
            (vessels, 4710, position(4710, -122.624, true, 4, 7, None)),
            // Car space: the same twice, then fewer.
            ("terminals/rest/terminalsailingspace", 1000, space(40)),
            ("terminals/rest/terminalsailingspace", 1060, space(40)),
            ("terminals/rest/terminalsailingspace", 1120, space(12)),
            // A notice, posted at Seattle, then at Bremerton too.
            (
                "terminals/rest/terminalbulletins",
                1000,
                serde_json::json!([
                {"TerminalID": 7, "Bulletins": [{"BulletinTitle": "Late boats",
                    "BulletinText": "<p>Running late.</p>", "BulletinLastUpdated": null}]}]),
            ),
            (
                "terminals/rest/terminalbulletins",
                1300,
                serde_json::json!([
                {"TerminalID": 7, "Bulletins": [{"BulletinTitle": "Late boats",
                    "BulletinText": "<p>Running later.</p>", "BulletinLastUpdated": null}]},
                {"TerminalID": 4, "Bulletins": [{"BulletinTitle": "Late boats",
                    "BulletinText": "<p>Running later.</p>", "BulletinLastUpdated": null}]}]),
            ),
        ];
        for (path, at_s, body) in &answers {
            crate::db::insert_raw(pool, path, at_s * 1000, Some(200), Some(body), None)
                .await
                .unwrap();
        }
        // An answer that failed is skipped.
        crate::db::insert_raw(pool, vessels, 5_000_000, None, None, Some("timed out"))
            .await
            .unwrap();

        for round in 0..2 {
            let rebuilt = rebuild(pool).await.unwrap();
            assert_eq!(
                (rebuilt.answers, rebuilt.unreadable),
                (10, 0),
                "round {round}"
            );

            let positions: i64 = sqlx::query_scalar("SELECT count(*) FROM vessel_positions")
                .fetch_one(pool)
                .await
                .unwrap();
            assert_eq!(positions, 5);
            let trips: Vec<(i32, i32, f64, f64, Option<f64>)> = sqlx::query_as(
                "SELECT departing, arriving, extract(epoch FROM left_dock)::float8,
                     extract(epoch FROM eta)::float8, extract(epoch FROM arrived_at)::float8
                 FROM trips",
            )
            .fetch_all(pool)
            .await
            .unwrap();
            // One trip: the revised ETA, and the first report at the dock.
            assert_eq!(trips, [(7, 4, 1060.0, 4700.0, Some(4700.0))]);

            let space: Vec<(i32, Option<i32>, bool)> = sqlx::query_as(
                "SELECT destination_id, drive_up, cancelled FROM sailing_space
                 ORDER BY departs_at, observed_at",
            )
            .fetch_all(pool)
            .await
            .unwrap();
            // 40, then 12 (the repeat of 40 not recorded), and the
            // cancelled sailing once, with no destination.
            assert_eq!(
                space,
                [(4, Some(40), false), (4, Some(12), false), (0, None, true)]
            );

            let notice: (String, f64, f64) = sqlx::query_as(
                "SELECT text, extract(epoch FROM first_seen)::float8,
                     extract(epoch FROM last_seen)::float8 FROM notices",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(notice, ("Running later.".into(), 1000.0, 1300.0));
            let posted: i64 = sqlx::query_scalar("SELECT count(*) FROM notice_terminals")
                .fetch_one(pool)
                .await
                .unwrap();
            assert_eq!(posted, 2);

            // Seattle's slip, from the two sightings near it, heading out
            // west (262°); none for Bremerton, which nothing left.
            let facing: Vec<(i32, f64, i32)> =
                sqlx::query_as("SELECT terminal_id, heading_deg, samples FROM terminal_facing")
                    .fetch_all(pool)
                    .await
                    .unwrap();
            assert_eq!(facing.len(), 1);
            assert_eq!((facing[0].0, facing[0].2), (7, 2));
            assert!((facing[0].1 - 262.0).abs() < 1e-6);
        }
        db.finish().await;
    }
}
