//! Token accounting ([`Usage`]) and why a model reply ended ([`StopReason`]).

use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};

/// Token counts reported by a provider for one model call (or a sum of calls).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Uncached input tokens.
    pub input: u64,
    /// Output tokens (including thinking, where the provider bills it as output).
    pub output: u64,
    /// Input tokens served from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write: u64,
}

impl Usage {
    /// All input tokens: uncached + cache reads + cache writes.
    #[must_use]
    pub fn total_input(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// Share of input tokens served from cache, or `None` if there was no input.
    /// NFR-05 targets at least 0.70 on long sessions.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // token counts stay far below 2^52
    pub fn cache_read_ratio(&self) -> Option<f64> {
        let total = self.total_input();
        (total > 0).then(|| self.cache_read as f64 / total as f64)
    }
}

impl Add for Usage {
    type Output = Usage;

    fn add(self, rhs: Usage) -> Usage {
        Usage {
            input: self.input.saturating_add(rhs.input),
            output: self.output.saturating_add(rhs.output),
            cache_read: self.cache_read.saturating_add(rhs.cache_read),
            cache_write: self.cache_write.saturating_add(rhs.cache_write),
        }
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Usage) {
        *self = *self + rhs;
    }
}

/// Why the model stopped generating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its turn.
    EndTurn,
    /// The model wants tool results before continuing.
    ToolUse,
    /// The output limit was hit; a tool input may be truncated.
    MaxTokens,
    /// A server-side tool loop paused; resend to continue.
    PauseTurn,
    /// The provider declined the request.
    Refusal {
        /// Provider-supplied category, if any (for example `cyber`).
        category: Option<String>,
    },
    /// Any other provider value, kept verbatim.
    Other {
        /// The raw stop reason string.
        raw: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
        Usage {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    #[test]
    fn total_input_includes_cache_reads_and_writes() {
        assert_eq!(u(10, 99, 70, 20).total_input(), 100);
    }

    #[test]
    fn cache_read_ratio_is_none_without_input() {
        assert_eq!(Usage::default().cache_read_ratio(), None);
    }

    #[test]
    fn cache_read_ratio_divides_reads_by_total_input() {
        assert_eq!(u(10, 0, 70, 20).cache_read_ratio(), Some(0.7));
    }

    #[test]
    fn usage_adds_field_by_field() {
        let mut total = u(1, 2, 3, 4);
        total += u(10, 20, 30, 40);
        assert_eq!(total, u(11, 22, 33, 44));
    }

    #[test]
    fn stop_reason_serialises_with_kind_tag() {
        let v = serde_json::to_value(StopReason::Refusal {
            category: Some("cyber".into()),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"kind": "refusal", "category": "cyber"})
        );
        let back: StopReason = serde_json::from_value(v).unwrap();
        assert_eq!(
            back,
            StopReason::Refusal {
                category: Some("cyber".into())
            }
        );
    }
}
