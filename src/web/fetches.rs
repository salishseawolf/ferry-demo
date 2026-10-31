//! Fetching the documents the finder shows, each kept a while once fetched.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::finder::Loaded;
use crate::live::{Update, get_json};
use serde::de::DeserializeOwned;

use super::dom::now_ms;

/// A fetch that failed is tried again, when next wanted, after this long.
const FAILED_KEEP_MS: f64 = 30_000.0;

/// Answers fetched in the background, by what they were for, waiting to be
/// taken in by the next frame.
type Arrivals<T> = Rc<RefCell<Vec<(String, Update<T>)>>>;

/// Documents fetched when they're wanted, by key (a route id): each
/// asked for once at a time, and kept `keep_ms` before it's asked for
/// again.
pub(super) struct Fetches<T> {
    /// What's come, or why it didn't, with when (ms). A failure is kept
    /// `FAILED_KEEP_MS`.
    got: HashMap<String, (Update<T>, f64)>,
    pending: HashSet<String>,
    arrived: Arrivals<T>,
    keep_ms: f64,
}

impl<T: DeserializeOwned + 'static> Fetches<T> {
    pub(super) fn new(keep_ms: f64) -> Self {
        Self {
            got: HashMap::new(),
            pending: HashSet::new(),
            arrived: Rc::default(),
            keep_ms,
        }
    }

    /// Asks for `key`'s document at `url`, unless it's here and recent,
    /// or already asked for.
    pub(super) fn want(&mut self, key: &str, url: String) {
        let recent = self.got.get(key).is_some_and(|(got, at)| {
            now_ms() - at
                < if got.is_ok() {
                    self.keep_ms
                } else {
                    FAILED_KEEP_MS
                }
        });
        if recent || !self.pending.insert(key.to_string()) {
            return;
        }
        let (key, arrived) = (key.to_string(), self.arrived.clone());
        wasm_bindgen_futures::spawn_local(async move {
            let got = get_json::<T>(&url).await;
            arrived.borrow_mut().push((key, got));
        });
    }

    /// Takes in what's come back; says whether anything new did. Failures
    /// are logged as `what` (asked for again when next wanted).
    pub(super) fn receive(&mut self, what: &str) -> bool {
        let arrived: Vec<_> = self.arrived.borrow_mut().drain(..).collect();
        let mut fresh = false;
        for (key, got) in arrived {
            self.pending.remove(&key);
            if let Err(e) = &got {
                log::warn!("{what} for {key}: {e}");
            }
            self.got.insert(key, (got, now_ms()));
            fresh = true;
        }
        fresh
    }

    pub(super) fn get(&self, key: &str) -> Option<&T> {
        self.got.get(key).and_then(|(got, _)| got.as_ref().ok())
    }

    pub(super) fn loaded(&self, key: &str) -> Loaded<'_, T> {
        match self.got.get(key) {
            None => Loaded::Pending,
            Some((Ok(doc), _)) => Loaded::Here(doc),
            Some((Err(_), _)) => Loaded::Missing,
        }
    }
}
