//! The server's documents, fetched as they change (see `schedule`): the
//! route network and boat details, and the live boats, sailing space and
//! notices.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ferry_model::api::{LiveNotices, LiveSailings, LiveVessels, Network, Vessels};
use serde::de::DeserializeOwned;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::Response;

use crate::schedule::{Outcome, fresh_for_s, next_delay_ms};

/// How often each live document updates, in case a response doesn't say.
pub const VESSELS_MS: f64 = 10_000.0;
const SAILINGS_MS: f64 = 60_000.0;
const NOTICES_MS: f64 = 300_000.0;
/// Reference documents (the network, boat details), likewise.
const REFERENCE_MS: f64 = 3_600_000.0;

/// What a fetch brought: the data, or why there isn't any.
pub type Update<T> = Result<T, String>;

/// One of the server's documents, fetched when it's due and kept until
/// taken. A new document is only passed on when it differs from the last.
pub struct Feed<T> {
    url: &'static str,
    fallback_ms: f64,
    /// Live documents: unchanged means the server is behind, so ask again
    /// soon.
    retry_unchanged: bool,
    /// When it's next due (performance.now ms; 0: at once).
    due_ms: Rc<Cell<f64>>,
    failures: Rc<Cell<u32>>,
    in_flight: Rc<Cell<bool>>,
    last_body: Rc<RefCell<String>>,
    arrived: Rc<RefCell<Option<Update<T>>>>,
}

impl<T: DeserializeOwned + 'static> Feed<T> {
    fn new(url: &'static str, fallback_ms: f64, retry_unchanged: bool) -> Self {
        Self {
            url,
            fallback_ms,
            retry_unchanged,
            due_ms: Rc::new(Cell::new(0.0)),
            failures: Rc::default(),
            in_flight: Rc::default(),
            last_body: Rc::default(),
            arrived: Rc::new(RefCell::new(None)),
        }
    }

    /// Starts a fetch if one is due at `now_ms` (performance.now) and none
    /// is under way.
    pub fn poll(&mut self, now_ms: f64) {
        if now_ms < self.due_ms.get() || self.in_flight.get() {
            return;
        }
        self.in_flight.set(true);
        let (url, fallback_ms, retry_unchanged) =
            (self.url, self.fallback_ms, self.retry_unchanged);
        let (due, failures, in_flight, last, arrived) = (
            self.due_ms.clone(),
            self.failures.clone(),
            self.in_flight.clone(),
            self.last_body.clone(),
            self.arrived.clone(),
        );
        wasm_bindgen_futures::spawn_local(async move {
            let (outcome, fresh_s) = match fetch_text(url).await {
                Ok((body, fresh_s)) if *last.borrow() == body => (Outcome::Unchanged, fresh_s),
                Ok((body, fresh_s)) => match serde_json::from_str::<T>(&body) {
                    Ok(doc) => {
                        *last.borrow_mut() = body;
                        *arrived.borrow_mut() = Some(Ok(doc));
                        (Outcome::Fresh, fresh_s)
                    }
                    Err(e) => fail(&failures, &arrived, format!("{url}: {e}")),
                },
                Err(e) => fail(&failures, &arrived, e),
            };
            if !matches!(outcome, Outcome::Failed(_)) {
                failures.set(0);
            }
            let delay = next_delay_ms(
                outcome,
                fresh_s,
                fallback_ms,
                retry_unchanged,
                js_sys::Math::random(),
            );
            due.set(performance_now() + delay);
            in_flight.set(false);
        });
    }

    /// The latest new document, or failure, once.
    pub fn take(&mut self) -> Option<Update<T>> {
        self.arrived.borrow_mut().take()
    }
}

/// Counts a failure and passes it on; gives the outcome to schedule by.
fn fail<T>(
    failures: &Cell<u32>,
    arrived: &RefCell<Option<Update<T>>>,
    why: String,
) -> (Outcome, Option<f64>) {
    log::warn!("{why}");
    failures.set(failures.get() + 1);
    *arrived.borrow_mut() = Some(Err(why));
    (Outcome::Failed(failures.get()), None)
}

pub fn network_feed() -> Feed<Network> {
    Feed::new("api/v1/network", REFERENCE_MS, false)
}

pub fn vessel_info_feed() -> Feed<Vessels> {
    Feed::new("api/v1/vessels", REFERENCE_MS, false)
}

pub fn vessel_feed() -> Feed<LiveVessels> {
    Feed::new("api/v1/live/vessels", VESSELS_MS, true)
}

pub fn sailings_feed() -> Feed<LiveSailings> {
    Feed::new("api/v1/live/sailings", SAILINGS_MS, true)
}

pub fn notices_feed() -> Feed<LiveNotices> {
    Feed::new("api/v1/live/notices", NOTICES_MS, true)
}

/// A document fetched once, such as crossing statistics.
pub async fn get_json<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    let (body, _) = fetch_text(url).await?;
    serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))
}

/// The body of `url`, and how long it stays fresh (s), if it says. A
/// response other than 200 is an error, carrying the server's message.
async fn fetch_text(url: &str) -> Result<(String, Option<f64>), String> {
    let window = web_sys::window().ok_or("no window")?;
    let response: Response = JsFuture::from(window.fetch_with_str(url))
        .await
        .and_then(|r| r.dyn_into())
        .map_err(message)?;
    let header = |name: &str| response.headers().get(name).ok().flatten();
    let fresh_s = fresh_for_s(header("cache-control").as_deref(), header("age").as_deref());
    let status = response.status();
    let body = JsFuture::from(response.text().map_err(message)?)
        .await
        .map_err(message)?
        .as_string()
        .ok_or_else(|| format!("{url}: not text"))?;
    if status != 200 {
        let why = serde_json::from_str::<ferry_model::api::ApiError>(&body)
            .map_or_else(|_| format!("HTTP {status}"), |e| e.error.message);
        return Err(format!("{url}: {why}"));
    }
    Ok((body, fresh_s))
}

fn performance_now() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map_or(0.0, |p| p.now())
}

fn message(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}
