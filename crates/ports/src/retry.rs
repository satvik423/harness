//! [`backoff_delay`]: how long a provider adapter waits before retrying (FR-PRV-04).

use std::time::Duration;

/// Longest server-requested wait that is honoured before retrying.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Cap for exponential backoff.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Delay before retry number `attempt` (1-based).
///
/// Uses the server's `retry-after` when given (capped at [`MAX_RETRY_AFTER`]);
/// otherwise backs off exponentially from 1 s, capped at [`MAX_BACKOFF`], plus up
/// to 25% jitter derived from `jitter_seed` so many clients don't retry in step.
///
/// # Examples
/// ```
/// use std::time::Duration;
/// use harness_ports::backoff_delay;
/// assert_eq!(backoff_delay(1, None, 0), Duration::from_secs(1));
/// assert_eq!(backoff_delay(3, None, 0), Duration::from_secs(4));
/// ```
#[must_use]
pub fn backoff_delay(attempt: u32, retry_after: Option<Duration>, jitter_seed: u64) -> Duration {
    if let Some(wait) = retry_after {
        return wait.min(MAX_RETRY_AFTER);
    }
    let exponent = attempt.saturating_sub(1).min(16);
    let base = Duration::from_secs(1u64 << exponent).min(MAX_BACKOFF);
    if base == MAX_BACKOFF {
        return MAX_BACKOFF;
    }
    let permille = u32::try_from(jitter_seed % 250).unwrap_or(0);
    base + base * permille / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_exponentially_with_bounded_jitter() {
        assert_eq!(backoff_delay(1, None, 0), Duration::from_secs(1));
        assert_eq!(backoff_delay(3, None, 0), Duration::from_secs(4));
        let jittered = backoff_delay(1, None, 249);
        assert!(
            jittered >= Duration::from_secs(1) && jittered <= Duration::from_millis(1250),
            "{jittered:?}"
        );
        assert_eq!(backoff_delay(20, None, 0), MAX_BACKOFF);
    }

    #[test]
    fn server_wait_wins_but_is_capped() {
        assert_eq!(
            backoff_delay(1, Some(Duration::from_millis(10)), 999),
            Duration::from_millis(10)
        );
        assert_eq!(
            backoff_delay(1, Some(Duration::from_secs(600)), 0),
            MAX_RETRY_AFTER
        );
    }
}
