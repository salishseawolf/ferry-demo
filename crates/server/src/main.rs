//! The ferry-demo server: polls WSF, recording every answer in Postgres
//! along with the reference data (terminals, boats, the route network),
//! and serves the app's API. Configured from the environment (see
//! `config`).
//!
//!     ferry-server                              serve
//!     ferry-server export-replay FILE [MINUTES] write the last MINUTES
//!                                               (default 30) of recorded
//!                                               answers as a replay file
//!     ferry-server rebuild                      derive all history again
//!                                               from the recorded answers
//!                                               (with the server stopped)

mod advice;
mod calendar;
mod config;
mod db;
mod history;
mod http;
mod live;
mod reference;
mod schedule;
mod stats;
mod wsf;

use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

/// How often the database's health is checked, for /readyz.
const DB_CHECK: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        // Colors only for a person at a terminal, not in container logs.
        .with_ansi(std::io::stdout().is_terminal())
        .with_env_filter(
            EnvFilter::try_from_default_env()
                // Postgres's notices ("already exists, skipping") aren't news.
                .unwrap_or_else(|_| EnvFilter::new("info,sqlx::postgres::notice=warn")),
        )
        .init();
    let config = config::Config::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("export-replay") => return export_replay(&pool, &args[1..]).await,
        Some("rebuild") => {
            let rebuilt = history::rebuild(&pool).await?;
            tracing::info!(
                answers = rebuilt.answers,
                unreadable = rebuilt.unreadable,
                "history rebuilt"
            );
            return Ok(());
        }
        Some("compute-advice") => {
            let terminals = advice::compute_history(&pool, wsf::now_ms()).await?;
            tracing::info!(terminals, "advice history computed");
            return Ok(());
        }
        Some(other) => anyhow::bail!(
            "unknown command {other:?} (try export-replay, rebuild or compute-advice)"
        ),
    }
    let state = Arc::new(http::AppState::default());
    let _ = state.db.set(pool.clone());

    let wsf = match (config.replay, config.wsdot_api_key) {
        (Some(file), _) => {
            tracing::info!("playing back {}", file.display());
            Some(Arc::new(wsf::Wsf::replay(&file)?))
        }
        (None, Some(key)) if key.starts_with("op://") => {
            anyhow::bail!("WSDOT_API_KEY is still a 1Password reference: run under `op run`")
        }
        (None, Some(key)) => Some(Arc::new(wsf::Wsf::live(
            config.wsf_base,
            key,
            pool.clone(),
        )?)),
        (None, None) => {
            tracing::warn!("no WSDOT_API_KEY: serving stored data, fetching nothing from WSF");
            None
        }
    };
    live::spawn(wsf.clone(), pool.clone(), state.clone());
    advice::spawn(pool.clone(), state.clone());
    // Schedules are fetched by date, which a replay file can't answer.
    if let Some(wsf) = wsf.as_ref().filter(|w| !w.is_replay()) {
        schedule::spawn(pool.clone(), wsf.clone(), state.clone());
    }
    reference::spawn(
        pool.clone(),
        wsf,
        config.routes_file,
        state.clone(),
        config.reference_refresh,
    );
    let checked = state.clone();
    tokio::spawn(async move {
        loop {
            checked
                .db_ok
                .store(db::ping(&pool).await, Ordering::Relaxed);
            tokio::time::sleep(DB_CHECK).await;
        }
    });

    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    tracing::info!("listening on http://{}", config.bind);
    axum::serve(listener, http::router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Writes recorded answers as a replay file: `FILE [MINUTES]`.
async fn export_replay(pool: &sqlx::PgPool, args: &[String]) -> Result<()> {
    let file = args.first().context("export-replay FILE [MINUTES]")?;
    let minutes: u32 = match args.get(1) {
        Some(m) => m.parse().context("MINUTES isn't a whole number")?,
        None => 30,
    };
    let answers = db::export_replay(pool, minutes).await?;
    let mut out = String::new();
    for answer in &answers {
        out += &serde_json::to_string(answer)?;
        out.push('\n');
    }
    std::fs::write(file, out).with_context(|| format!("writing {file}"))?;
    tracing::info!("wrote {} answers to {file}", answers.len());
    Ok(())
}

/// Resolves on Ctrl-C or SIGTERM (how Kubernetes and compose stop a
/// container), letting requests in flight finish.
async fn shutdown() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}
