//! Reference data: the terminals and boats (from WSF, refreshed when WSF
//! says they've changed), and the route network built from the routes
//! file and the terminals. Kept in Postgres, and served as `/network` and
//! `/vessels`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use ferry_model::api::{Network, NetworkLeg, NetworkRoute, NetworkTerminal, Vessels};
use ferry_model::routes::{self, Leg, RouteFile};
use ferry_model::terminal::Terminal;
use sqlx::PgPool;

use crate::db;
use crate::http::{AppState, Doc};
use crate::wsf::Wsf;

/// The route network as served: terminals with their learned slip
/// directions, each leg with the terminals its ends reach, and the routes
/// riders know (ids made from their names).
pub fn network(file: &RouteFile, terminals: &[Terminal], facing: &HashMap<u32, f64>) -> Network {
    let legs: Vec<Leg> = file
        .routes
        .iter()
        .map(|r| Leg::new(&r.path(), terminals))
        .collect();
    let grouped = routes::group(&legs, &file.services, terminals);
    Network {
        terminals: terminals
            .iter()
            .map(|t| NetworkTerminal {
                id: t.id,
                name: t.name.clone(),
                lon: t.position.lon,
                lat: t.position.lat,
                slip_facing_deg: facing.get(&t.id).copied(),
            })
            .collect(),
        legs: file
            .routes
            .iter()
            .zip(&legs)
            .map(|(r, leg)| NetworkLeg {
                id: r.way,
                ends: leg.ends.map(|(a, b)| [a, b]),
                length_m: leg.length_m.round(),
                curves: r.curves.clone(),
            })
            .collect(),
        routes: grouped
            .iter()
            .map(|g| NetworkRoute {
                id: slug(&g.name),
                name: g.name.clone(),
                terminals: g.terminals.iter().copied().collect(),
                legs: g.legs.iter().map(|&i| file.routes[i].way).collect(),
            })
            .collect(),
        attribution: vec![file.attribution.clone()],
    }
}

/// An id from a name: "Fauntleroy / Vashon / Southworth" becomes
/// "fauntleroy-vashon-southworth".
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for word in name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if !out.is_empty() {
            out.push('-');
        }
        out.extend(word.chars().flat_map(char::to_lowercase));
    }
    out
}

/// Loads the reference data and publishes it to `state`: from the
/// database, after refreshing it from WSF where WSF says it changed (or
/// the database has none yet). Without `wsf`, only what's stored.
pub async fn load(
    pool: &PgPool,
    wsf: Option<&Wsf>,
    routes_file: &Path,
    state: &AppState,
) -> Result<()> {
    if let Some(wsf) = wsf {
        refresh(pool, wsf, "terminals").await;
        refresh(pool, wsf, "vessels").await;
    }
    let terminals = db::terminals(pool).await?;
    let vessels = db::vessels(pool).await?;
    let facing = db::facing(pool).await?;
    let text = tokio::fs::read_to_string(routes_file)
        .await
        .with_context(|| format!("reading {}", routes_file.display()))?;
    let file = routes::parse(&text).map_err(anyhow::Error::msg)?;
    let network = network(&file, &terminals, &facing);
    db::replace_network(pool, &network).await?;
    tracing::info!(
        terminals = network.terminals.len(),
        legs = network.legs.len(),
        routes = network.routes.len(),
        vessels = vessels.len(),
        "reference data loaded"
    );
    state.set_network(Doc::json(&network));
    state.set_vessels(Doc::json(&Vessels { vessels }));
    Ok(())
}

/// Refetches `service`'s data (`terminals` or `vessels`) if WSF's flush
/// date has moved since the last fetch, or nothing's stored. Failures are
/// logged, leaving what's stored.
async fn refresh(pool: &PgPool, wsf: &Wsf, service: &str) {
    let result: Result<()> = async {
        let flushed = wsf.flushed_ms(service).await?;
        let stored = db::flushed_ms(pool, service).await?;
        let empty = match service {
            "terminals" => db::terminals(pool).await?.is_empty(),
            _ => db::vessels(pool).await?.is_empty(),
        };
        if stored == Some(flushed) && !empty {
            return Ok(());
        }
        match service {
            "terminals" => db::upsert_terminals(pool, &wsf.terminals().await?).await?,
            _ => db::upsert_vessels(pool, &wsf.vessels().await?).await?,
        }
        db::set_flushed_ms(pool, service, flushed).await?;
        tracing::info!(service, "refreshed from WSF");
        Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::warn!(service, "couldn't refresh from WSF: {e:#}");
    }
}

/// After a slip's direction is first learned, how long to wait for others
/// before rebuilding the network.
const NETWORK_SETTLE: std::time::Duration = std::time::Duration::from_secs(30);

/// Loads the reference data, retrying until it works, then reloads it
/// every `every` (picking up WSF's changes), or soon after a slip's
/// direction is first learned.
pub fn spawn(
    pool: PgPool,
    wsf: Option<Arc<Wsf>>,
    routes_file: std::path::PathBuf,
    state: Arc<AppState>,
    every: std::time::Duration,
) {
    tokio::spawn(async move {
        loop {
            let mut wait = std::time::Duration::from_secs(5);
            while let Err(e) = load(&pool, wsf.as_deref(), &routes_file, &state).await {
                tracing::warn!("reference data not loaded: {e:#}; retrying in {wait:?}");
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(std::time::Duration::from_secs(300));
            }
            tokio::select! {
                () = tokio::time::sleep(every) => {}
                () = state.network_dirty.notified() => {
                    // Others are often learned close behind: take them too.
                    tokio::time::sleep(NETWORK_SETTLE).await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_model::geo::LonLat;

    fn terminal(id: u32, name: &str, lon: f64, lat: f64) -> Terminal {
        Terminal {
            id,
            name: name.to_string(),
            position: LonLat::new(lon, lat),
        }
    }

    #[test]
    fn makes_ids_from_names() {
        assert_eq!(
            slug("Fauntleroy / Vashon / Southworth"),
            "fauntleroy-vashon-southworth"
        );
        assert_eq!(slug("Pt. Defiance / Tahlequah"), "pt-defiance-tahlequah");
        assert_eq!(slug("Sidney B.C. / Anacortes"), "sidney-b-c-anacortes");
    }

    #[test]
    fn builds_the_network_from_the_routes_file() {
        let file = routes::parse(
            r#"{"attribution":"Ferry routes © OpenStreetMap contributors (ODbL)",
              "services":[{"name":"Fauntleroy / Vashon / Southworth",
                "terminals":["Fauntleroy","Vashon Island","Southworth"]}],
              "routes":[
                {"way":11,"name":"Fauntleroy - Vashon","curves":[
                  [[-122.3965,47.5232],[-122.42,47.52],[-122.44,47.515],[-122.4638,47.5109]]]},
                {"way":12,"name":"Vashon - Southworth","curves":[
                  [[-122.4638,47.5109],[-122.47,47.512],[-122.48,47.513],[-122.4959,47.5130]]]},
                {"way":13,"name":"Seattle - Bremerton","curves":[
                  [[-122.3399,47.6020],[-122.45,47.59],[-122.55,47.57],[-122.6228,47.5602]]]},
                {"way":14,"name":"Somewhere","curves":[
                  [[-123.5,48.0],[-123.4,48.0],[-123.3,48.0],[-123.2,48.0]]]}]}"#,
        )
        .unwrap();
        let terminals = [
            terminal(9, "Fauntleroy", -122.3965, 47.5232),
            terminal(22, "Vashon Island", -122.4638, 47.5109),
            terminal(20, "Southworth", -122.4959, 47.5130),
            terminal(7, "Seattle", -122.3399, 47.6020),
            terminal(4, "Bremerton", -122.6228, 47.5602),
        ];
        let facing = HashMap::from([(7, 265.0)]);
        let n = network(&file, &terminals, &facing);
        assert_eq!(n.terminals.len(), 5);
        assert_eq!(n.terminals[3].slip_facing_deg, Some(265.0));
        assert_eq!(n.terminals[0].slip_facing_deg, None);
        assert_eq!(n.legs[0].ends, Some([9, 22]));
        assert_eq!(n.legs[3].ends, None);
        assert!(n.legs[2].length_m > 20_000.0);
        let ids: Vec<&str> = n.routes.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["fauntleroy-vashon-southworth", "seattle-bremerton"]);
        assert_eq!(n.routes[0].legs, [11, 12]);
        assert_eq!(n.routes[0].terminals, [9, 20, 22]);
        assert_eq!(n.attribution.len(), 1);
    }
}
