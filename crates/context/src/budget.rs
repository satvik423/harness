//! The token budget (P5-T1, Tech spec 3.13 "Budgets and triggers"): when a
//! segment is full enough to compact.
//!
//! `compact_at = min(ratio × window, compact_at_tokens)`. Before every model
//! call the runtime estimates the request's input from the last measured usage
//! plus the estimated size of what was appended since
//! ([`crate::SegmentContext::estimated_input`]), and asks the provider to count
//! exactly when the estimate is within [`NEAR_SHARE`] of the threshold.

/// Share of the threshold from which an estimate is checked with `count_tokens`.
pub const NEAR_SHARE: f64 = 0.9;

/// How full a request is, compared with the compaction threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    /// Well below the threshold: no action.
    Under,
    /// Within 10% of the threshold: count exactly before deciding.
    Near,
    /// At or above the threshold: compact.
    Over,
}

/// The compaction threshold of one segment.
///
/// # Examples
/// ```
/// use harness_context::{Budget, Pressure};
/// let budget = Budget::new(1_000_000, 0.7, 250_000);
/// assert_eq!(budget.compact_at(), 250_000);
/// assert_eq!(budget.pressure(100_000), Pressure::Under);
/// assert_eq!(budget.pressure(230_000), Pressure::Near);
/// assert_eq!(budget.pressure(250_000), Pressure::Over);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    compact_at: u64,
}

impl Budget {
    /// The threshold for a model with a `window`-token context: the smaller of
    /// `ratio × window` and `cap`. An unknown window (0) uses `cap` alone.
    #[must_use]
    pub fn new(window: u64, ratio: f64, cap: u64) -> Self {
        let share = if window == 0 {
            cap
        } else {
            // A ratio outside (0, 1] would make the threshold meaningless.
            let ratio = if ratio > 0.0 && ratio <= 1.0 {
                ratio
            } else {
                0.7
            };
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let scaled = (window as f64 * ratio) as u64;
            scaled
        };
        Self {
            compact_at: share.min(cap).max(1),
        }
    }

    /// Input tokens at which the segment is compacted.
    #[must_use]
    pub fn compact_at(&self) -> u64 {
        self.compact_at
    }

    /// How `tokens` of input compare with the threshold.
    #[must_use]
    pub fn pressure(&self, tokens: u64) -> Pressure {
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let near = (self.compact_at as f64 * NEAR_SHARE) as u64;
        if tokens >= self.compact_at {
            Pressure::Over
        } else if tokens >= near {
            Pressure::Near
        } else {
            Pressure::Under
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_threshold_is_the_smaller_of_the_window_share_and_the_cap() {
        assert_eq!(Budget::new(200_000, 0.7, 250_000).compact_at(), 140_000);
        assert_eq!(Budget::new(1_000_000, 0.7, 250_000).compact_at(), 250_000);
        assert_eq!(Budget::new(0, 0.7, 30_000).compact_at(), 30_000);
        // A bad ratio falls back to the default share.
        assert_eq!(Budget::new(100_000, 7.0, 250_000).compact_at(), 70_000);
    }

    #[test]
    fn pressure_has_a_near_band_below_the_threshold() {
        let b = Budget::new(0, 0.7, 1_000);
        assert_eq!(b.pressure(899), Pressure::Under);
        assert_eq!(b.pressure(900), Pressure::Near);
        assert_eq!(b.pressure(999), Pressure::Near);
        assert_eq!(b.pressure(1_000), Pressure::Over);
    }
}
