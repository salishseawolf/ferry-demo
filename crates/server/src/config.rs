//! Settings, from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

pub struct Config {
    /// Postgres, as `postgres://user:password@host:port/database`.
    pub database_url: String,
    /// The WSDOT access code. Without one the server serves what's in the
    /// database already, and fetches nothing.
    pub wsdot_api_key: Option<String>,
    /// WSF's API root, ending in a slash.
    pub wsf_base: String,
    /// A replay file to play WSF's answers back from instead (WSF_SOURCE
    /// set to `replay:<file>`; see `ferry-server export-replay`).
    pub replay: Option<PathBuf>,
    pub bind: SocketAddr,
    /// The ferry routes the pipeline fitted (tools/routes/osm_routes.py).
    pub routes_file: PathBuf,
    /// How often to check whether WSF's terminals and boats have changed.
    pub reference_refresh: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let database_url = var("DATABASE_URL").context("DATABASE_URL isn't set")?;
        let wsdot_api_key = var("WSDOT_API_KEY");
        let refresh_s: u64 = var("REFERENCE_REFRESH_S")
            .map(|s| s.parse())
            .transpose()
            .context("REFERENCE_REFRESH_S isn't a number of seconds")?
            .unwrap_or(24 * 3600);
        let replay = match var("WSF_SOURCE").as_deref() {
            None | Some("wsf") => None,
            Some(s) => Some(
                s.strip_prefix("replay:")
                    .context("WSF_SOURCE is `wsf` or `replay:<file>`")?
                    .into(),
            ),
        };
        Ok(Self {
            database_url,
            replay,
            wsdot_api_key,
            wsf_base: var("WSF_BASE")
                .unwrap_or_else(|| "https://www.wsdot.wa.gov/ferries/api/".to_string()),
            bind: var("BIND")
                .unwrap_or_else(|| "127.0.0.1:8090".to_string())
                .parse()
                .context("BIND isn't an address and port")?,
            routes_file: var("ROUTES_FILE")
                .unwrap_or_else(|| "tiles/routes/ferry-routes.json".to_string())
                .into(),
            reference_refresh: Duration::from_secs(refresh_s),
        })
    }
}
