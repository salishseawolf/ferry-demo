//! The API under `/api/v1/`, and the health checks Kubernetes probes.
//!
//! Every document is serialized once when it changes and served as is,
//! with a weak ETag (the body may be compressed on the way out), so
//! clients revalidating an unchanged document get an empty 304.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ferry_model::api::{ApiError, ErrorBody, RouteSchedule};
use serde::Serialize;
use tower_http::compression::CompressionLayer;

use crate::advice;
use crate::live::{Feed, Published};
use crate::stats;
use crate::wsf::now_ms;

/// Reference documents change rarely; revalidating hourly is plenty.
const REFERENCE_MAX_AGE_S: u32 = 3600;
/// Statistics move by a trip at a time: a few minutes' caching is fine.
const STATS_MAX_AGE_S: u32 = 300;
/// The advice self-check's window by default (days).
const ADVICE_CHECK_DAYS: u32 = 14;
/// A boat's latest loads change as it leaves a dock.
const LOADS_MAX_AGE_S: u32 = 60;
/// Sailings a boat's loads list by default, and at most.
const LOADS_LIMIT: u32 = 5;
const LOADS_MAX_LIMIT: u32 = 50;
/// Schedules are checked for changes every 10 minutes.
const SCHEDULE_MAX_AGE_S: u32 = 600;

/// A document ready to serve: its JSON and its ETag.
pub struct Doc {
    body: Bytes,
    etag: HeaderValue,
}

impl Doc {
    pub fn json<T: Serialize>(value: &T) -> Arc<Self> {
        Self::raw(serde_json::to_vec(value).expect("API documents serialize"))
    }

    /// A document from JSON already written.
    fn raw(body: Vec<u8>) -> Arc<Self> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        body.hash(&mut hasher);
        let etag = HeaderValue::from_str(&format!("W/\"{:016x}\"", hasher.finish()))
            .expect("hex is a valid header");
        Arc::new(Self {
            body: body.into(),
            etag,
        })
    }
}

/// Each route's schedule document, by route id.
type Schedules = HashMap<String, Arc<Doc>>;
/// Each terminal's advice document, by WSF id.
type AdviceDocs = HashMap<u32, Arc<Doc>>;

/// What the server serves, as it stands.
#[derive(Default)]
pub struct AppState {
    network: RwLock<Option<Arc<Doc>>>,
    vessels: RwLock<Option<Arc<Doc>>>,
    /// Each route's schedule for the trip day, by route id, once loaded:
    /// as served, and as data (for advice).
    schedules: RwLock<Option<Arc<Schedules>>>,
    schedule_data: RwLock<Option<Arc<HashMap<String, RouteSchedule>>>>,
    /// Each terminal's arrival advice as last published, by WSF id, and
    /// when it's next due (epoch ms).
    advice: RwLock<Option<(Arc<AdviceDocs>, i64)>>,
    /// Each live feed's latest document, by `Feed::index`.
    live: [RwLock<Option<Arc<Published>>>; 3],
    /// Whether the database answered at the last check.
    pub db_ok: AtomicBool,
    /// The database, for what's computed on request (statistics).
    pub db: std::sync::OnceLock<sqlx::PgPool>,
    /// Something the route network carries (a slip's direction) has been
    /// learned: it's worth rebuilding before the daily reload.
    pub network_dirty: tokio::sync::Notify,
}

impl AppState {
    pub fn set_network(&self, doc: Arc<Doc>) {
        *self.network.write().unwrap() = Some(doc);
    }

    pub fn set_vessels(&self, doc: Arc<Doc>) {
        *self.vessels.write().unwrap() = Some(doc);
    }

    pub fn set_schedules(&self, schedules: HashMap<String, RouteSchedule>) {
        let docs = schedules
            .iter()
            .map(|(route, s)| (route.clone(), Doc::json(s)))
            .collect();
        *self.schedules.write().unwrap() = Some(Arc::new(docs));
        *self.schedule_data.write().unwrap() = Some(Arc::new(schedules));
    }

    pub fn set_advice(&self, docs: HashMap<u32, Arc<Doc>>, next_ms: i64) {
        *self.advice.write().unwrap() = Some((Arc::new(docs), next_ms));
    }

    /// The trip day's schedules, by route id, once loaded.
    pub fn schedules_today(&self) -> Option<Arc<HashMap<String, RouteSchedule>>> {
        self.schedule_data.read().unwrap().clone()
    }

    pub fn set_live(&self, feed: Feed, published: Published) {
        *self.live[feed.index()].write().unwrap() = Some(Arc::new(published));
    }

    fn live(&self, feed: Feed) -> Option<Arc<Published>> {
        self.live[feed.index()].read().unwrap().clone()
    }

    fn network(&self) -> Option<Arc<Doc>> {
        self.network.read().unwrap().clone()
    }

    fn vessels(&self) -> Option<Arc<Doc>> {
        self.vessels.read().unwrap().clone()
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/network", get(network))
        .route("/vessels", get(vessels))
        .route("/stats/crossings", get(crossings))
        .route("/schedule", get(schedule))
        .route("/loads", get(loads))
        .route("/advice", get(advice))
        .route("/stats/advice", get(advice_check))
        .route("/stats/loads", get(load_stats))
        .route("/live", get(live_all))
        .route("/live/vessels", get(|s, h| live(Feed::Vessels, s, h)))
        .route("/live/sailings", get(|s, h| live(Feed::Sailings, s, h)))
        .route("/live/notices", get(|s, h| live(Feed::Notices, s, h)))
        .fallback(not_found);
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .nest("/api/v1", api)
        .layer(CompressionLayer::new())
        .with_state(state)
}

async fn network(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    serve(state.network(), REFERENCE_MAX_AGE_S, &headers)
}

async fn vessels(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    serve(state.vessels(), REFERENCE_MAX_AGE_S, &headers)
}

/// A live document, cached by clients until its next update is due.
async fn live(feed: Feed, State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let published = state.live(feed);
    let max_age = published
        .as_ref()
        .map_or(1, |p| seconds_until(p.next_update_ms));
    serve(published.map(|p| p.doc.clone()), max_age, &headers)
}

/// All three live documents in one, for a client's first load: ready once
/// each is, and cached until the first of them is due.
async fn live_all(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let parts: Option<Vec<Arc<Published>>> = Feed::ALL.iter().map(|&f| state.live(f)).collect();
    let Some(parts) = parts else {
        return serve(None, 1, &headers);
    };
    let mut body = Vec::new();
    for (i, (name, part)) in ["vessels", "sailings", "notices"]
        .iter()
        .zip(&parts)
        .enumerate()
    {
        body.extend_from_slice(if i == 0 { b"{\"" } else { b",\"" });
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(b"\":");
        body.extend_from_slice(&part.doc.body);
    }
    body.push(b'}');
    let due = parts.iter().map(|p| p.next_update_ms).min().unwrap_or(0);
    serve(Some(Doc::raw(body)), seconds_until(due), &headers)
}

/// `/stats/crossings?route=…&days=…`: crossing times on a route.
async fn crossings(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(route) = params.get("route").filter(|r| !r.is_empty()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "Give a route, as ?route=…",
        );
    };
    let Some(days) = stats::parse_days(params.get("days").map(String::as_str)) else {
        let message = format!("days is a whole number from 1 to {}", stats::MAX_DAYS);
        return error(StatusCode::BAD_REQUEST, "bad_request", &message);
    };
    let Some(pool) = state.db.get() else {
        return serve(None, 1, &headers);
    };
    match stats::crossings(pool, route, days).await {
        Ok(Some(found)) => serve(Some(Doc::json(&found)), STATS_MAX_AGE_S, &headers),
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("No route {route:?}"),
        ),
        Err(e) => {
            tracing::warn!("crossing stats for {route}: {e:#}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Couldn't compute these statistics",
            )
        }
    }
}

/// `/advice?terminal=…`: when to arrive by car for a terminal's sailings
/// today, as last published (advice::publish), fresh until the next.
async fn advice(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(terminal) = params.get("terminal").and_then(|t| t.parse::<u32>().ok()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "Give a terminal by its WSF id, as ?terminal=…",
        );
    };
    let Some((docs, next_ms)) = state.advice.read().unwrap().clone() else {
        return serve(None, 1, &headers);
    };
    match docs.get(&terminal) {
        Some(doc) => serve(Some(doc.clone()), seconds_until(next_ms), &headers),
        None => error(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("No sailings from terminal {terminal} today"),
        ),
    }
}

/// `/stats/advice?days=…`: how the advice held lately, per terminal.
async fn advice_check(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let days = match params.get("days").map(|d| d.parse::<u32>()) {
        None => ADVICE_CHECK_DAYS,
        Some(Ok(d)) if (1..=advice::CHECK_MAX_DAYS).contains(&d) => d,
        Some(_) => {
            let message = format!(
                "days is a whole number from 1 to {}",
                advice::CHECK_MAX_DAYS
            );
            return error(StatusCode::BAD_REQUEST, "bad_request", &message);
        }
    };
    let Some(pool) = state.db.get() else {
        return serve(None, 1, &headers);
    };
    match advice::check(pool, days, now_ms()).await {
        Ok(found) => serve(Some(Doc::json(&found)), STATS_MAX_AGE_S, &headers),
        Err(e) => {
            tracing::warn!("advice check: {e:#}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Couldn't check the advice",
            )
        }
    }
}

/// `/loads?vessel=…&limit=…`: how full a boat's latest sailings left.
async fn loads(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(vessel) = params.get("vessel").and_then(|v| v.parse::<u32>().ok()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "Give a boat by its WSF id, as ?vessel=…",
        );
    };
    let limit = match params.get("limit").map(|l| l.parse::<u32>()) {
        None => LOADS_LIMIT,
        Some(Ok(l)) if (1..=LOADS_MAX_LIMIT).contains(&l) => l,
        Some(_) => {
            let message = format!("limit is a whole number from 1 to {LOADS_MAX_LIMIT}");
            return error(StatusCode::BAD_REQUEST, "bad_request", &message);
        }
    };
    let Some(pool) = state.db.get() else {
        return serve(None, 1, &headers);
    };
    match stats::vessel_loads(pool, vessel, limit).await {
        Ok(found) => serve(Some(Doc::json(&found)), LOADS_MAX_AGE_S, &headers),
        Err(e) => {
            tracing::warn!("loads for boat {vessel}: {e:#}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Couldn't look up these loads",
            )
        }
    }
}

/// `/stats/loads?route=…` or `?vessel=…`, with `&days=…`: how full car
/// decks leave, by direction and hour.
async fn load_stats(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let route = params.get("route").filter(|r| !r.is_empty());
    let vessel = params.get("vessel");
    let scope = match (route, vessel) {
        (Some(route), None) => stats::LoadScope::Route(route),
        (None, Some(v)) => match v.parse() {
            Ok(id) => stats::LoadScope::Vessel(id),
            Err(_) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "vessel is a boat's WSF id",
                );
            }
        },
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "Give a route or a boat, as ?route=… or ?vessel=…",
            );
        }
    };
    let Some(days) = stats::parse_days(params.get("days").map(String::as_str)) else {
        let message = format!("days is a whole number from 1 to {}", stats::MAX_DAYS);
        return error(StatusCode::BAD_REQUEST, "bad_request", &message);
    };
    let Some(pool) = state.db.get() else {
        return serve(None, 1, &headers);
    };
    match stats::load_stats(pool, scope, days).await {
        Ok(Some(found)) => serve(Some(Doc::json(&found)), STATS_MAX_AGE_S, &headers),
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("No route {:?}", route.map_or("", |r| r.as_str())),
        ),
        Err(e) => {
            tracing::warn!("load stats: {e:#}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Couldn't compute these statistics",
            )
        }
    }
}

/// `/schedule?route=…`: a route's sailings for the trip day.
async fn schedule(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(route) = params.get("route").filter(|r| !r.is_empty()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "Give a route, as ?route=…",
        );
    };
    let Some(all) = state.schedules.read().unwrap().clone() else {
        return serve(None, 1, &headers);
    };
    match all.get(route) {
        Some(doc) => serve(Some(doc.clone()), SCHEDULE_MAX_AGE_S, &headers),
        None => error(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("No schedule for route {route:?} today"),
        ),
    }
}

/// Whole seconds from now until `ms` (epoch), at least 1: a document
/// that's due is worth a second's caching while the poll lands.
fn seconds_until(ms: i64) -> u32 {
    let left = ms - now_ms();
    (left.max(0) as u64).div_ceil(1000).max(1) as u32
}

/// Ready once the database answers and the route network has loaded.
async fn ready(State(state): State<Arc<AppState>>) -> Response {
    let db = state.db_ok.load(Ordering::Relaxed);
    let network = state.network().is_some();
    if db && network {
        (StatusCode::OK, "ready").into_response()
    } else {
        let why = match (db, network) {
            (false, false) => "database unreachable; route network not loaded",
            (false, true) => "database unreachable",
            _ => "route network not loaded",
        };
        (StatusCode::SERVICE_UNAVAILABLE, why).into_response()
    }
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not_found", "No such endpoint")
}

/// `doc`, or a 304 if the client already has it, or a 503 if it hasn't
/// loaded yet.
fn serve(doc: Option<Arc<Doc>>, max_age_s: u32, headers: &HeaderMap) -> Response {
    let Some(doc) = doc else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
            "Still loading; try again shortly",
        );
    };
    let cache =
        HeaderValue::from_str(&format!("public, max-age={max_age_s}")).expect("a valid header");
    let cached = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|tags| {
            tags.split(',')
                .map(str::trim)
                .any(|t| t == "*" || weak_eq(t, doc.etag.to_str().unwrap_or("")))
        });
    let common = [
        (header::ETAG, doc.etag.clone()),
        (header::CACHE_CONTROL, cache),
    ];
    if cached {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    (
        StatusCode::OK,
        common,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        doc.body.clone(),
    )
        .into_response()
}

/// ETags compared as If-None-Match does: weakly, ignoring `W/`.
fn weak_eq(a: &str, b: &str) -> bool {
    a.trim_start_matches("W/") == b.trim_start_matches("W/")
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = ApiError {
        error: ErrorBody {
            code: code.to_string(),
            message: message.to_string(),
        },
    };
    (status, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn get(
        state: &Arc<AppState>,
        path: &str,
        etag: Option<&str>,
    ) -> (StatusCode, HeaderMap, String) {
        let mut request = Request::get(path);
        if let Some(tag) = etag {
            request = request.header(header::IF_NONE_MATCH, tag);
        }
        let response = router(state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        (
            parts.status,
            parts.headers,
            String::from_utf8(bytes.to_vec()).unwrap(),
        )
    }

    #[tokio::test]
    async fn serves_documents_with_etags_and_304s() {
        let state = Arc::new(AppState::default());
        let (status, _, body) = get(&state, "/api/v1/network", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("\"not_ready\""));

        state.set_network(Doc::json(&serde_json::json!({"terminals": []})));
        let (status, headers, body) = get(&state, "/api/v1/network", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, r#"{"terminals":[]}"#);
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=3600");
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        let etag = headers[header::ETAG].to_str().unwrap().to_string();
        assert!(etag.starts_with("W/\""));

        // The same version, by its tag (weak or not), or any version.
        for tag in [etag.as_str(), etag.trim_start_matches("W/"), "\"x\", *"] {
            let (status, headers, body) = get(&state, "/api/v1/network", Some(tag)).await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{tag}");
            assert_eq!(headers[header::ETAG], etag.as_str());
            assert!(body.is_empty());
        }
        // A new version gets a new tag.
        state.set_network(Doc::json(&serde_json::json!({"terminals": [1]})));
        let (status, headers, _) = get(&state, "/api/v1/network", Some(&etag)).await;
        assert_eq!(status, StatusCode::OK);
        assert_ne!(headers[header::ETAG], etag.as_str());
    }

    #[tokio::test]
    async fn serves_live_documents_until_their_next_update() {
        let state = Arc::new(AppState::default());
        assert_eq!(
            get(&state, "/api/v1/live/vessels", None).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let publish = |feed, json: serde_json::Value, due_in_ms: i64| {
            state.set_live(
                feed,
                Published {
                    doc: Doc::json(&json),
                    next_update_ms: now_ms() + due_in_ms,
                },
            );
        };
        publish(Feed::Vessels, serde_json::json!({"vessels": [1]}), 9_400);
        let (status, headers, body) = get(&state, "/api/v1/live/vessels", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, r#"{"vessels":[1]}"#);
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=10");
        // Overdue: a second, while the poll lands.
        publish(Feed::Vessels, serde_json::json!({"vessels": [2]}), -5_000);
        let (_, headers, _) = get(&state, "/api/v1/live/vessels", None).await;
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=1");

        // All three together once each has loaded, due with the soonest.
        assert_eq!(
            get(&state, "/api/v1/live", None).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        publish(Feed::Vessels, serde_json::json!({"vessels": [3]}), 4_000);
        publish(Feed::Sailings, serde_json::json!({"sailings": []}), 50_000);
        publish(Feed::Notices, serde_json::json!({"notices": []}), 290_000);
        let (status, headers, body) = get(&state, "/api/v1/live", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=4");
        let all: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(all["vessels"]["vessels"][0], 3);
        assert_eq!(all["notices"]["notices"], serde_json::json!([]));
        let etag = headers[header::ETAG].to_str().unwrap().to_string();
        assert_eq!(
            get(&state, "/api/v1/live", Some(&etag)).await.0,
            StatusCode::NOT_MODIFIED
        );
    }

    #[tokio::test]
    async fn checks_what_crossing_stats_are_asked_for() {
        let state = Arc::new(AppState::default());
        for (path, status, code) in [
            (
                "/api/v1/stats/crossings",
                StatusCode::BAD_REQUEST,
                "bad_request",
            ),
            (
                "/api/v1/stats/crossings?route=",
                StatusCode::BAD_REQUEST,
                "bad_request",
            ),
            (
                "/api/v1/stats/crossings?route=x&days=0",
                StatusCode::BAD_REQUEST,
                "bad_request",
            ),
            (
                "/api/v1/stats/crossings?route=x",
                StatusCode::SERVICE_UNAVAILABLE,
                "not_ready",
            ),
        ] {
            let (got, _, body) = get(&state, path, None).await;
            assert_eq!(got, status, "{path}");
            let error: ApiError = serde_json::from_str(&body).unwrap();
            assert_eq!(error.error.code, code, "{path}");
        }

        let Some(db) = crate::db::tests::Scratch::new().await else {
            eprintln!("DATABASE_URL isn't set: skipping the rest");
            return;
        };
        crate::stats::tests::seed(&db.pool).await;
        let _ = state.db.set(db.pool.clone());
        let (status, headers, body) = get(
            &state,
            "/api/v1/stats/crossings?route=seattle-bremerton&days=7",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=300");
        let stats: ferry_model::api::CrossingStats = serde_json::from_str(&body).unwrap();
        assert_eq!((stats.route.as_str(), stats.days), ("seattle-bremerton", 7));
        assert_eq!(stats.pairs[0].trips, 4);
        let etag = headers[header::ETAG].to_str().unwrap().to_string();
        assert_eq!(
            get(
                &state,
                "/api/v1/stats/crossings?route=seattle-bremerton&days=7",
                Some(&etag)
            )
            .await
            .0,
            StatusCode::NOT_MODIFIED
        );
        let (status, _, body) = get(&state, "/api/v1/stats/crossings?route=nowhere", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("nowhere"));
        db.finish().await;
    }

    #[tokio::test]
    async fn serves_schedules_by_route() {
        let state = Arc::new(AppState::default());
        let path = "/api/v1/schedule?route=seattle-bremerton";
        let (status, _, _) = get(&state, path, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let schedule = RouteSchedule {
            route: "seattle-bremerton".into(),
            trip_date: "2026-10-30".into(),
            season: "Fall 2026".into(),
            pdf_url: None,
            terminals: vec![],
        };
        state.set_schedules(HashMap::from([("seattle-bremerton".to_string(), schedule)]));
        let (status, headers, body) = get(&state, path, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=600");
        assert!(body.contains("seattle-bremerton"));
        for (path, status) in [
            ("/api/v1/schedule?route=nowhere", StatusCode::NOT_FOUND),
            ("/api/v1/schedule", StatusCode::BAD_REQUEST),
        ] {
            assert_eq!(get(&state, path, None).await.0, status, "{path}");
        }
    }

    #[tokio::test]
    async fn serves_published_advice() {
        let state = Arc::new(AppState::default());
        let path = "/api/v1/advice?terminal=7";
        assert_eq!(
            get(&state, path, None).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let doc = Doc::json(&serde_json::json!({"terminal": 7}));
        state.set_advice(HashMap::from([(7, doc)]), now_ms() + 30_000);
        let (status, headers, body) = get(&state, path, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"terminal\":7"));
        // Fresh until the next publish, about 30 s away.
        let age = headers[header::CACHE_CONTROL].to_str().unwrap().to_string();
        assert!(
            age == "public, max-age=30" || age == "public, max-age=29",
            "{age}"
        );
        for (path, status) in [
            ("/api/v1/advice?terminal=9", StatusCode::NOT_FOUND),
            ("/api/v1/advice", StatusCode::BAD_REQUEST),
        ] {
            assert_eq!(get(&state, path, None).await.0, status, "{path}");
        }
    }

    #[tokio::test]
    async fn answers_health_and_unknown_paths() {
        let state = Arc::new(AppState::default());
        assert_eq!(get(&state, "/healthz", None).await.0, StatusCode::OK);
        let (status, _, body) = get(&state, "/readyz", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("database unreachable"));
        state.db_ok.store(true, Ordering::Relaxed);
        state.set_network(Doc::json(&serde_json::json!({})));
        assert_eq!(get(&state, "/readyz", None).await.0, StatusCode::OK);

        let (status, headers, body) = get(&state, "/api/v1/nope", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        let error: ApiError = serde_json::from_str(&body).unwrap();
        assert_eq!(error.error.code, "not_found");
    }
}
