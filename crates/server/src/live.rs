//! The live data: WSF polled on a steady beat for each feed, every answer
//! recorded (see `wsf`), parsed, and published as the `/live` documents,
//! each saying when the next one is due so clients can time their
//! requests (aligned polling).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ferry_model::api::{LiveNotices, LiveSailings, LiveVessel, LiveVessels, Source, WakePoint};
use ferry_model::terminal::Terminal;
use serde::Serialize;
use sqlx::PgPool;
use tokio::time::Instant;

use crate::http::{AppState, Doc};
use crate::wsf::{self, Wsf, now_ms};
use crate::{db, history};

/// A boat's wake: its reports over this long (ms).
const WAKE_MS: i64 = 3 * 60_000;
/// Added to when the next answer should be ready, so clients arrive just
/// after it rather than just before.
const SLACK_MS: i64 = 250;
/// How long a fetch takes, until one has been timed.
const TYPICAL_FETCH_MS: f64 = 1000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feed {
    Vessels,
    Sailings,
    Notices,
}

impl Feed {
    pub const ALL: [Feed; 3] = [Feed::Vessels, Feed::Sailings, Feed::Notices];

    pub fn path(self) -> &'static str {
        match self {
            Feed::Vessels => "vessels/rest/vessellocations",
            Feed::Sailings => "terminals/rest/terminalsailingspace",
            Feed::Notices => "terminals/rest/terminalbulletins",
        }
    }

    /// How often it's polled: positions change by the second, car space
    /// by the minute, notices a few times a day.
    pub fn every(self) -> Duration {
        Duration::from_secs(match self {
            Feed::Vessels => 10,
            Feed::Sailings => 60,
            Feed::Notices => 300,
        })
    }

    /// How soon a failed poll is tried again.
    fn retry(self) -> Duration {
        self.every().min(Duration::from_secs(30))
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// A feed's latest data, parsed.
pub(crate) enum Items {
    Vessels(Vec<LiveVessel>),
    Sailings(Vec<ferry_model::api::Sailing>),
    Notices(Vec<ferry_model::api::Notice>),
}

/// A document published for a feed, and when the next is due (epoch ms).
pub struct Published {
    pub doc: Arc<Doc>,
    pub next_update_ms: i64,
}

/// Each boat's recent reports, for its wake.
#[derive(Default)]
pub(crate) struct Tracks(HashMap<u32, VecDeque<WakePoint>>);

impl Tracks {
    /// Adds each boat's report (once per report) and gives each boat its
    /// wake: its reports over the last `WAKE_MS`, oldest first. Boats no
    /// longer reported are forgotten.
    fn add(&mut self, vessels: &mut [LiveVessel]) {
        for v in vessels.iter_mut() {
            let track = self.0.entry(v.id).or_default();
            if track.back().is_none_or(|p| v.reported_ms > p.0) {
                track.push_back(WakePoint(v.reported_ms, v.lon, v.lat));
            }
            while track.front().is_some_and(|p| v.reported_ms - p.0 > WAKE_MS) {
                track.pop_front();
            }
            v.wake = track.iter().copied().collect();
        }
        self.0.retain(|id, _| vessels.iter().any(|v| v.id == *id));
    }
}

/// Parses an answer for `feed`, adding boats' reports to `tracks`.
pub(crate) fn parse(
    feed: Feed,
    body: &str,
    terminals: &[Terminal],
    tracks: &mut Tracks,
) -> Result<Items> {
    Ok(match feed {
        Feed::Vessels => {
            let mut vessels = wsf::parse_positions(body)?;
            tracks.add(&mut vessels);
            Items::Vessels(vessels)
        }
        Feed::Sailings => Items::Sailings(wsf::parse_sailings(body, terminals)?),
        Feed::Notices => Items::Notices(wsf::parse_notices(body)?),
    })
}

fn document(items: &Items, source: Source) -> Arc<Doc> {
    fn doc<T: Serialize>(value: &T) -> Arc<Doc> {
        Doc::json(value)
    }
    match items {
        Items::Vessels(vessels) => doc(&LiveVessels {
            source,
            vessels: vessels.clone(),
        }),
        Items::Sailings(sailings) => doc(&LiveSailings {
            source,
            sailings: sailings.clone(),
        }),
        Items::Notices(notices) => doc(&LiveNotices {
            source,
            notices: notices.clone(),
        }),
    }
}

/// The last answer recorded for `feed`, so a restarted server serves
/// something at once (with boats' wakes rebuilt from the answers before).
async fn restore(feed: Feed, pool: &PgPool, tracks: &mut Tracks) -> Result<Option<(Items, i64)>> {
    let Some((fetched_ms, _)) = db::latest_raw(pool, feed.path()).await? else {
        return Ok(None);
    };
    let since = if feed == Feed::Vessels {
        fetched_ms - WAKE_MS
    } else {
        fetched_ms
    };
    let terminals = db::terminals(pool).await?;
    let mut last = None;
    for (at, body) in db::raw_since(pool, feed.path(), since).await? {
        last = Some((parse(feed, &body, &terminals, tracks)?, at));
    }
    Ok(last)
}

/// Starts a poller for each feed. With `wsf` None (no access code), the
/// last recorded answers are served, and nothing's fetched.
pub fn spawn(wsf: Option<Arc<Wsf>>, pool: PgPool, state: Arc<AppState>) {
    for feed in Feed::ALL {
        let (wsf, pool, state) = (wsf.clone(), pool.clone(), state.clone());
        tokio::spawn(async move { run(feed, wsf, pool, state).await });
    }
}

async fn run(feed: Feed, wsf: Option<Arc<Wsf>>, pool: PgPool, state: Arc<AppState>) {
    let every_ms = feed.every().as_millis() as i64;
    let mut tracks = Tracks::default();
    let publish = |items: &Items, fetched_ms: i64, next_update_ms: i64, error: Option<String>| {
        let source = Source {
            fetched_ms,
            next_update_ms,
            stale: now_ms() - fetched_ms > 2 * every_ms,
            error,
        };
        state.set_live(
            feed,
            Published {
                doc: document(items, source),
                next_update_ms,
            },
        );
    };

    // A replay isn't mixed with recorded live answers.
    let replaying = wsf.as_deref().is_some_and(Wsf::is_replay);
    let mut last = if replaying {
        None
    } else {
        restore(feed, &pool, &mut tracks).await.unwrap_or_else(|e| {
            tracing::warn!(feed = feed.path(), "couldn't restore: {e:#}");
            None
        })
    };
    let Some(wsf) = wsf else {
        if let Some((items, fetched_ms)) = &last {
            publish(items, *fetched_ms, now_ms() + every_ms, None);
        }
        return;
    };
    if let Some((items, fetched_ms)) = &last {
        // The first poll follows at once.
        publish(items, *fetched_ms, now_ms() + 2000, None);
    }

    let mut took_ms = TYPICAL_FETCH_MS;
    let mut next = Instant::now();
    loop {
        tokio::time::sleep_until(next).await;
        let started = Instant::now();
        let started_ms = now_ms();
        let result = async {
            let fetched = wsf.get(feed.path()).await?;
            let terminals = db::terminals(&pool).await?;
            let items = parse(feed, &fetched.body, &terminals, &mut tracks)?;
            // A replay's answers aren't history.
            if !replaying {
                match history::apply(&pool, &items, fetched.fetched_ms, &terminals).await {
                    Ok(applied) if applied.new_facing => state.network_dirty.notify_one(),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(feed = feed.path(), "history not recorded: {e:#}"),
                }
            }
            anyhow::Ok((items, fetched.fetched_ms))
        }
        .await;
        took_ms = 0.8 * took_ms + 0.2 * started.elapsed().as_millis() as f64;
        let error = match result {
            Ok(fresh) => {
                last = Some(fresh);
                None
            }
            Err(e) => {
                tracing::warn!(feed = feed.path(), "poll failed: {e:#}");
                Some(format!("{e:#}"))
            }
        };
        let wait = if error.is_none() {
            feed.every()
        } else {
            feed.retry()
        };
        next = started + wait;
        let next_update_ms = started_ms + wait.as_millis() as i64 + took_ms as i64 + SLACK_MS;
        if let Some((items, fetched_ms)) = &last {
            publish(items, *fetched_ms, next_update_ms, error);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn boat(id: u32, reported_ms: i64, lon: f64) -> LiveVessel {
        LiveVessel {
            id,
            lon,
            lat: 47.6,
            speed_kn: 15.0,
            heading_deg: 90.0,
            at_dock: false,
            in_service: true,
            departing: None,
            arriving: None,
            scheduled_departure_ms: None,
            left_dock_ms: None,
            eta_ms: None,
            reported_ms,
            note: None,
            wake: Vec::new(),
        }
    }

    #[test]
    fn wakes_keep_the_last_three_minutes_of_reports() {
        let mut tracks = Tracks::default();
        for (t, lon) in [
            (0, -122.50),
            (60_000, -122.49),
            (60_000, -122.49),
            (200_000, -122.47),
        ] {
            let mut boats = vec![boat(1, t, lon), boat(2, 0, -122.3)];
            tracks.add(&mut boats);
            if t == 200_000 {
                // The report at 0 is past three minutes; the repeat of the
                // one at 60 s is counted once.
                let wake: Vec<i64> = boats[0].wake.iter().map(|p| p.0).collect();
                assert_eq!(wake, [60_000, 200_000]);
                assert_eq!(boats[1].wake.len(), 1);
            }
        }
        // A boat no longer reported is forgotten.
        let mut boats = vec![boat(2, 1, -122.3)];
        tracks.add(&mut boats);
        assert!(!tracks.0.contains_key(&1));
    }

    #[test]
    fn feeds_have_their_own_beats() {
        assert_eq!(Feed::Vessels.every(), Duration::from_secs(10));
        assert_eq!(Feed::Notices.retry(), Duration::from_secs(30));
        assert_eq!(Feed::Sailings.retry(), Duration::from_secs(30));
        assert_eq!(Feed::Vessels.retry(), Duration::from_secs(10));
        let indexes: Vec<usize> = Feed::ALL.iter().map(|f| f.index()).collect();
        assert_eq!(indexes, [0, 1, 2]);
    }
}
