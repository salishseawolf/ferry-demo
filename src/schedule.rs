//! When to ask the server again (aligned polling): each live document is
//! cached until the server's next update is due, so a client waits out the
//! response's freshness (`Cache-Control: max-age` less `Age`) and a little
//! more, spread out so clients don't all arrive at once. If the document
//! hasn't changed, the server's own poll is running late: try again soon.
//! After a failure, back off.

/// After an unchanged live document, try again this soon (ms).
pub const UNCHANGED_RETRY_MS: f64 = 2000.0;
/// Up to this long (ms) is added to each wait, at random.
pub const JITTER_MS: f64 = 1000.0;
/// The first wait after a failure (ms), doubling for each one in a row,
/// up to `MAX_BACKOFF_MS`.
pub const FIRST_BACKOFF_MS: f64 = 2000.0;
pub const MAX_BACKOFF_MS: f64 = 60_000.0;

/// How a request went.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// New data.
    Fresh,
    /// The same document as last time.
    Unchanged,
    /// The `n`th failure in a row (from 1).
    Failed(u32),
}

/// How long (s) a response stays fresh: its `max-age` less its `Age` (how
/// long a cache on the way has held it). None without a `max-age`.
pub fn fresh_for_s(cache_control: Option<&str>, age: Option<&str>) -> Option<f64> {
    let max_age: f64 = cache_control?
        .split(',')
        .filter_map(|d| d.trim().strip_prefix("max-age="))
        .find_map(|v| v.trim().parse().ok())?;
    let age: f64 = age.and_then(|a| a.trim().parse().ok()).unwrap_or(0.0);
    Some((max_age - age).max(0.0))
}

/// How long (ms) to wait before asking again. `fresh_s` is how long the
/// response stays fresh (`fallback_ms` if it didn't say); `retry_unchanged`
/// is for live documents, where an unchanged one means the server is
/// behind; `jitter` is a random number from 0 to 1.
pub fn next_delay_ms(
    outcome: Outcome,
    fresh_s: Option<f64>,
    fallback_ms: f64,
    retry_unchanged: bool,
    jitter: f64,
) -> f64 {
    let spread = jitter.clamp(0.0, 1.0) * JITTER_MS;
    match outcome {
        Outcome::Unchanged if retry_unchanged => UNCHANGED_RETRY_MS,
        Outcome::Fresh | Outcome::Unchanged => fresh_s.map_or(fallback_ms, |s| s * 1000.0) + spread,
        Outcome::Failed(n) => {
            let doublings = n.saturating_sub(1).min(16) as i32;
            (FIRST_BACKOFF_MS * 2f64.powi(doublings)).min(MAX_BACKOFF_MS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_freshness_from_headers() {
        assert_eq!(fresh_for_s(Some("public, max-age=10"), None), Some(10.0));
        assert_eq!(
            fresh_for_s(Some("max-age=10, public"), Some("3")),
            Some(7.0)
        );
        // A cache that held it past its freshness: due now.
        assert_eq!(fresh_for_s(Some("max-age=10"), Some("12")), Some(0.0));
        assert_eq!(fresh_for_s(Some("no-store"), None), None);
        assert_eq!(fresh_for_s(None, Some("3")), None);
        assert_eq!(fresh_for_s(Some("max-age=soon"), None), None);
    }

    #[test]
    fn waits_for_the_next_update_then_retries_or_backs_off() {
        // Fresh: out the freshness, plus up to a second.
        assert_eq!(
            next_delay_ms(Outcome::Fresh, Some(9.0), 10_000.0, true, 0.0),
            9000.0
        );
        assert_eq!(
            next_delay_ms(Outcome::Fresh, Some(9.0), 10_000.0, true, 1.0),
            10_000.0
        );
        // No freshness given: the fallback.
        assert_eq!(
            next_delay_ms(Outcome::Fresh, None, 60_000.0, true, 0.5),
            60_500.0
        );
        // A live document not yet updated: soon. Reference data: as usual.
        assert_eq!(
            next_delay_ms(Outcome::Unchanged, Some(1.0), 10_000.0, true, 0.9),
            2000.0
        );
        assert_eq!(
            next_delay_ms(Outcome::Unchanged, Some(3600.0), 0.0, false, 0.0),
            3_600_000.0
        );
        // Failures: 2, 4, 8 s and on, to a minute at most.
        let backoff: Vec<f64> = (1..=7)
            .map(|n| next_delay_ms(Outcome::Failed(n), Some(9.0), 10_000.0, true, 0.5))
            .collect();
        assert_eq!(
            backoff,
            [
                2000.0, 4000.0, 8000.0, 16_000.0, 32_000.0, 60_000.0, 60_000.0
            ]
        );
        assert_eq!(
            next_delay_ms(Outcome::Failed(99), None, 0.0, true, 0.0),
            60_000.0
        );
    }
}
