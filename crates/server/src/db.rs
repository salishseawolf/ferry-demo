//! Postgres: connecting, migrating, and the reference tables.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use anyhow::{Context, Result};
use ferry_model::api::{Network, VesselInfo};
use ferry_model::geo::LonLat;
use ferry_model::terminal::Terminal;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

/// The schema, from crates/server/migrations.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Connects, waiting for the database to come up (it may start alongside
/// the server), then brings the schema up to date.
pub async fn connect(url: &str) -> Result<PgPool> {
    let mut wait = Duration::from_secs(1);
    let pool = loop {
        match PgPoolOptions::new().max_connections(8).connect(url).await {
            Ok(pool) => break pool,
            Err(e) if wait < Duration::from_secs(30) => {
                tracing::info!("waiting for the database ({e}); retrying in {wait:?}");
                tokio::time::sleep(wait).await;
                wait *= 2;
            }
            Err(e) => return Err(e).context("connecting to the database"),
        }
    };
    MIGRATOR
        .run(&pool)
        .await
        .context("migrating the database")?;
    Ok(pool)
}

pub async fn terminals(pool: &PgPool) -> Result<Vec<Terminal>> {
    let rows = sqlx::query("SELECT id, name, lon, lat FROM terminals ORDER BY id")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .iter()
        .map(|r| Terminal {
            id: r.get::<i32, _>("id") as u32,
            name: r.get("name"),
            position: LonLat::new(r.get("lon"), r.get("lat")),
        })
        .collect())
}

/// Each route of the network (by id) with the terminals it calls at.
pub async fn route_terminals(pool: &PgPool) -> Result<Vec<(String, BTreeSet<u32>)>> {
    let rows = sqlx::query("SELECT route_id, terminal_id FROM route_terminals ORDER BY route_id")
        .fetch_all(pool)
        .await?;
    let mut routes: Vec<(String, BTreeSet<u32>)> = Vec::new();
    for r in &rows {
        let (route, terminal): (String, i32) = (r.get("route_id"), r.get("terminal_id"));
        match routes.last_mut() {
            Some((id, ts)) if *id == route => {
                ts.insert(terminal as u32);
            }
            _ => routes.push((route, BTreeSet::from([terminal as u32]))),
        }
    }
    Ok(routes)
}

/// Adds or updates `terminals`. Terminals WSF stops listing are kept: legs
/// and routes may still refer to them.
pub async fn upsert_terminals(pool: &PgPool, terminals: &[Terminal]) -> Result<()> {
    let mut tx = pool.begin().await?;
    for t in terminals {
        sqlx::query(
            "INSERT INTO terminals (id, name, lon, lat) VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE
             SET name = $2, lon = $3, lat = $4, updated_at = now()",
        )
        .bind(t.id as i32)
        .bind(&t.name)
        .bind(t.position.lon)
        .bind(t.position.lat)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn vessels(pool: &PgPool) -> Result<Vec<VesselInfo>> {
    let rows = sqlx::query(
        "SELECT id, name, class, length_m, beam_m, car_capacity FROM vessels ORDER BY name",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| VesselInfo {
            id: r.get::<i32, _>("id") as u32,
            name: r.get("name"),
            class: r.get("class"),
            length_m: r.get("length_m"),
            beam_m: r.get("beam_m"),
            car_capacity: r.get::<Option<i32>, _>("car_capacity").map(|c| c as u32),
        })
        .collect())
}

pub async fn upsert_vessels(pool: &PgPool, vessels: &[VesselInfo]) -> Result<()> {
    let mut tx = pool.begin().await?;
    for v in vessels {
        sqlx::query(
            "INSERT INTO vessels (id, name, class, length_m, beam_m, car_capacity)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (id) DO UPDATE
             SET name = $2, class = $3, length_m = $4, beam_m = $5, car_capacity = $6,
                 updated_at = now()",
        )
        .bind(v.id as i32)
        .bind(&v.name)
        .bind(&v.class)
        .bind(v.length_m)
        .bind(v.beam_m)
        .bind(v.car_capacity.map(|c| c as i32))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Each slip's learned direction (degrees from north), by terminal id.
pub async fn facing(pool: &PgPool) -> Result<HashMap<u32, f64>> {
    let rows = sqlx::query("SELECT terminal_id, heading_deg FROM terminal_facing")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get::<i32, _>("terminal_id") as u32, r.get("heading_deg")))
        .collect())
}

/// Replaces the stored legs and routes with `network`'s, in one
/// transaction.
pub async fn replace_network(pool: &PgPool, network: &Network) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM routes").execute(&mut *tx).await?;
    sqlx::query("DELETE FROM legs").execute(&mut *tx).await?;
    for (leg, name) in network.legs.iter().zip(leg_names(network)) {
        sqlx::query(
            "INSERT INTO legs (id, name, terminal_a, terminal_b, length_m, curves)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(leg.id as i64)
        .bind(name)
        .bind(leg.ends.map(|e| e[0] as i32))
        .bind(leg.ends.map(|e| e[1] as i32))
        .bind(leg.length_m)
        .bind(sqlx::types::Json(&leg.curves))
        .execute(&mut *tx)
        .await?;
    }
    for route in &network.routes {
        sqlx::query("INSERT INTO routes (id, name) VALUES ($1, $2)")
            .bind(&route.id)
            .bind(&route.name)
            .execute(&mut *tx)
            .await?;
        for &leg in &route.legs {
            sqlx::query("INSERT INTO route_legs (route_id, leg_id) VALUES ($1, $2)")
                .bind(&route.id)
                .bind(leg as i64)
                .execute(&mut *tx)
                .await?;
        }
        for &terminal in &route.terminals {
            sqlx::query("INSERT INTO route_terminals (route_id, terminal_id) VALUES ($1, $2)")
                .bind(&route.id)
                .bind(terminal as i32)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// A name for each leg, for people reading the table: its terminals'
/// names, or its id where it reaches none.
fn leg_names(network: &Network) -> Vec<String> {
    let name = |id: u32| {
        network
            .terminals
            .iter()
            .find(|t| t.id == id)
            .map_or_else(|| id.to_string(), |t| t.name.clone())
    };
    network
        .legs
        .iter()
        .map(|leg| match leg.ends {
            Some([a, b]) if a == b => format!("{} (slip approach)", name(a)),
            Some([a, b]) => format!("{} ↔ {}", name(a), name(b)),
            None => format!("way {}", leg.id),
        })
        .collect()
}

/// When `source`'s data last changed, as WSF said at our last fetch.
pub async fn flushed_ms(pool: &PgPool, source: &str) -> Result<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT flushed_ms FROM source_state WHERE source = $1")
            .bind(source)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn set_flushed_ms(pool: &PgPool, source: &str, flushed_ms: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO source_state (source, flushed_ms) VALUES ($1, $2)
         ON CONFLICT (source) DO UPDATE SET flushed_ms = $2, fetched_at = now()",
    )
    .bind(source)
    .bind(flushed_ms)
    .execute(pool)
    .await?;
    Ok(())
}

/// Adds an answer to the raw poll log: its body when it was JSON, or why
/// there isn't one.
pub async fn insert_raw(
    pool: &PgPool,
    path: &str,
    fetched_ms: i64,
    status: Option<u16>,
    body: Option<&serde_json::Value>,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO raw_polls (path, fetched_at, status, body, error)
         VALUES ($1, to_timestamp($2::double precision / 1000), $3, $4, $5)",
    )
    .bind(path)
    .bind(fetched_ms)
    .bind(status.map(|s| s as i16))
    .bind(body.map(sqlx::types::Json))
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

/// The epoch ms of a fetched_at, in SQL.
pub(crate) const FETCHED_MS: &str = "(extract(epoch FROM fetched_at) * 1000)::bigint";

/// `path`'s answers since `since_ms` that came back whole, oldest first:
/// when each was fetched (epoch ms) and its body.
pub async fn raw_since(pool: &PgPool, path: &str, since_ms: i64) -> Result<Vec<(i64, String)>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {FETCHED_MS} AS fetched_ms, body::text AS body FROM raw_polls
         WHERE path = $1 AND body IS NOT NULL AND status = 200
           AND fetched_at >= to_timestamp($2::double precision / 1000)
         ORDER BY fetched_at"
    )))
    .bind(path)
    .bind(since_ms)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get("fetched_ms"), r.get("body")))
        .collect())
}

/// `path`'s latest answer that came back whole, if any.
pub async fn latest_raw(pool: &PgPool, path: &str) -> Result<Option<(i64, String)>> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {FETCHED_MS} AS fetched_ms, body::text AS body FROM raw_polls
         WHERE path = $1 AND body IS NOT NULL AND status = 200
         ORDER BY fetched_at DESC LIMIT 1"
    )))
    .bind(path)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.get("fetched_ms"), r.get("body"))))
}

/// Answers for a replay file: every whole answer from the last `minutes`,
/// plus each path's latest from before then if it has none since (so
/// rarely fetched data, such as the terminal list, comes along), oldest
/// first.
pub async fn export_replay(pool: &PgPool, minutes: u32) -> Result<Vec<crate::wsf::Recorded>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "WITH whole AS (
             SELECT path, fetched_at, body FROM raw_polls
             WHERE body IS NOT NULL AND status = 200
         ),
         recent AS (
             SELECT * FROM whole WHERE fetched_at >= now() - make_interval(mins => $1)
         ),
         older AS (
             SELECT DISTINCT ON (path) * FROM whole
             WHERE path NOT IN (SELECT path FROM recent)
             ORDER BY path, fetched_at DESC
         )
         SELECT path, {FETCHED_MS} AS fetched_ms, body
         FROM (SELECT * FROM recent UNION ALL SELECT * FROM older) AS chosen
         ORDER BY fetched_at"
    )))
    .bind(minutes as i32)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| crate::wsf::Recorded {
            path: r.get("path"),
            fetched_ms: r.get("fetched_ms"),
            body: r.get::<sqlx::types::Json<serde_json::Value>, _>("body").0,
        })
        .collect())
}

/// Whether the database answers.
pub async fn ping(pool: &PgPool) -> bool {
    sqlx::query("SELECT 1").execute(pool).await.is_ok()
}

/// Against a real Postgres: set DATABASE_URL (the compose database works)
/// to run these; without it they pass without checking anything.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ferry_model::api::{NetworkLeg, NetworkRoute, NetworkTerminal};
    use sqlx::AssertSqlSafe;

    /// A fresh, migrated database for one test, dropped by `finish`; None
    /// without DATABASE_URL.
    pub(crate) struct Scratch {
        admin: PgPool,
        name: String,
        pub(crate) pool: PgPool,
    }

    impl Scratch {
        pub(crate) async fn new() -> Option<Self> {
            let url = std::env::var("DATABASE_URL").ok()?;
            let admin = PgPool::connect(&url).await.expect("DATABASE_URL connects");
            let name = format!("ferry_test_{}_{}", std::process::id(), rand_suffix());
            // The name is made here, not taken from input.
            sqlx::query(AssertSqlSafe(format!("CREATE DATABASE {name}")))
                .execute(&admin)
                .await
                .unwrap();
            let (base, _) = url.rsplit_once('/').unwrap();
            let pool = connect(&format!("{base}/{name}")).await.unwrap();
            Some(Self { admin, name, pool })
        }

        pub(crate) async fn finish(self) {
            self.pool.close().await;
            sqlx::query(AssertSqlSafe(format!("DROP DATABASE {}", self.name)))
                .execute(&self.admin)
                .await
                .unwrap();
        }
    }

    fn rand_suffix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64
    }

    fn terminal(id: u32, name: &str) -> Terminal {
        Terminal {
            id,
            name: name.to_string(),
            position: LonLat::new(-122.4, 47.6),
        }
    }

    #[tokio::test]
    async fn stores_reference_data() {
        let Some(db) = Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping");
            return;
        };
        let pool = &db.pool;
        upsert_terminals(pool, &[terminal(7, "Seattle"), terminal(4, "Bremerton")])
            .await
            .unwrap();
        // A rename updates rather than duplicates.
        upsert_terminals(pool, &[terminal(4, "Bremerton (renamed)")])
            .await
            .unwrap();
        let stored = terminals(pool).await.unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].name, "Bremerton (renamed)");

        let boat = VesselInfo {
            id: 37,
            name: "Wenatchee".into(),
            class: Some("Jumbo Mark II".into()),
            length_m: Some(140.2),
            beam_m: None,
            car_capacity: Some(202),
        };
        upsert_vessels(pool, std::slice::from_ref(&boat))
            .await
            .unwrap();
        assert_eq!(vessels(pool).await.unwrap(), [boat]);

        let network = Network {
            terminals: vec![NetworkTerminal {
                id: 7,
                name: "Seattle".into(),
                lon: -122.4,
                lat: 47.6,
                slip_facing_deg: None,
            }],
            legs: vec![
                NetworkLeg {
                    id: 41,
                    ends: Some([7, 4]),
                    length_m: 24_000.0,
                    curves: vec![[
                        [-122.3, 47.6],
                        [-122.4, 47.6],
                        [-122.5, 47.6],
                        [-122.6, 47.56],
                    ]],
                },
                NetworkLeg {
                    id: 42,
                    ends: None,
                    length_m: 100.0,
                    curves: vec![],
                },
            ],
            routes: vec![NetworkRoute {
                id: "seattle-bremerton".into(),
                name: "Seattle / Bremerton".into(),
                terminals: vec![4, 7],
                legs: vec![41],
            }],
            attribution: vec![],
        };
        // Replacing twice leaves one copy.
        replace_network(pool, &network).await.unwrap();
        replace_network(pool, &network).await.unwrap();
        let legs: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM legs ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
        assert_eq!(legs, [(41, "Seattle ↔ 4".into()), (42, "way 42".into())]);
        let joins: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM route_legs) + (SELECT count(*) FROM route_terminals)",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(joins, 3);

        assert_eq!(flushed_ms(pool, "vessels").await.unwrap(), None);
        set_flushed_ms(pool, "vessels", 5).await.unwrap();
        set_flushed_ms(pool, "vessels", 6).await.unwrap();
        assert_eq!(flushed_ms(pool, "vessels").await.unwrap(), Some(6));
        assert!(facing(pool).await.unwrap().is_empty());
        assert!(ping(pool).await);

        // The raw poll log: whole answers come back, failures don't.
        let path = "vessels/rest/vessellocations";
        let now = crate::wsf::now_ms();
        for (at, n) in [(now - 20_000, 1), (now - 10_000, 2)] {
            let body = serde_json::json!([n]);
            insert_raw(pool, path, at, Some(200), Some(&body), None)
                .await
                .unwrap();
        }
        insert_raw(pool, path, now, None, None, Some("timed out"))
            .await
            .unwrap();
        insert_raw(
            pool,
            "terminals/rest/terminallocations",
            now - 7_200_000,
            Some(200),
            Some(&serde_json::json!({"t": 1})),
            None,
        )
        .await
        .unwrap();
        let (at, body) = latest_raw(pool, path).await.unwrap().unwrap();
        assert_eq!((at, body.as_str()), (now - 10_000, "[2]"));
        let since: Vec<String> = raw_since(pool, path, now - 15_000)
            .await
            .unwrap()
            .into_iter()
            .map(|(_, b)| b)
            .collect();
        assert_eq!(since, ["[2]"]);
        // A replay: the last minute's answers, plus the terminal list
        // from two hours ago, as the latest it has.
        let replay = export_replay(pool, 1).await.unwrap();
        let paths: Vec<&str> = replay.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["terminals/rest/terminallocations", path, path]);
        assert_eq!(replay[2].body, serde_json::json!([2]));
        db.finish().await;
    }
}
