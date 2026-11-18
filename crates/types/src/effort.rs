//! [`Effort`]: how much a model thinks (moved here from `harness-ports` so the
//! protocol can name it, Phase 5).

use serde::{Deserialize, Serialize};

/// Reasoning effort level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    /// Least thinking; fastest and cheapest.
    Low,
    /// Moderate thinking.
    Medium,
    /// Thorough thinking.
    High,
    /// Very thorough thinking.
    Xhigh,
    /// Maximum thinking.
    Max,
}

impl Effort {
    /// Every level, lowest first.
    pub const ALL: [Self; 5] = [Self::Low, Self::Medium, Self::High, Self::Xhigh, Self::Max];

    /// The level's name (`low` … `max`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_the_serialized_form() {
        for e in Effort::ALL {
            assert_eq!(
                serde_json::to_value(e).unwrap(),
                serde_json::json!(e.name())
            );
        }
    }
}
