//! [`Mode`]: the permission mode a session runs in, switchable at any time
//! (ADR-042).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// What the agent may do without asking (Tech spec 3.6).
///
/// The mode is the session's; the user switches it while the session runs
/// (Shift+Tab or `/mode` in the TUI) and the policy reads it on every call.
/// Rules and the approval policy still apply on top of it.
///
/// # Examples
/// ```
/// use harness_types::Mode;
/// let mode: Mode = "plan".parse().unwrap();
/// assert_eq!(mode.next(false), Mode::ReadOnly);
/// assert_eq!(mode.to_string(), "plan");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Reads run freely; every edit and command asks first.
    ReadOnly,
    /// Edits inside the project run; commands ask (until the Phase 13 sandbox).
    Auto,
    /// Reads only: edits are refused until the user approves a plan
    /// (`propose_plan`); commands ask.
    Plan,
    /// No questions. Only available when the process was started with
    /// `--dangerously-full-access`.
    Full,
}

impl Mode {
    /// Every mode, in Shift+Tab order.
    pub const ALL: [Self; 4] = [Self::ReadOnly, Self::Auto, Self::Plan, Self::Full];

    /// The mode's name, as typed in config, flags and `/mode`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Auto => "auto",
            Self::Plan => "plan",
            Self::Full => "full",
        }
    }

    /// One line for pickers and help: what the mode lets run without asking.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::ReadOnly => "asks before every edit and command",
            Self::Auto => "edits inside the project run; commands ask",
            Self::Plan => "reads only; the model proposes a plan for you to approve",
            Self::Full => "runs everything without asking",
        }
    }

    /// The mode after this one in the Shift+Tab cycle; `full` is skipped
    /// unless `full_allowed`.
    #[must_use]
    pub fn next(self, full_allowed: bool) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL
            .iter()
            .cycle()
            .skip(i + 1)
            .copied()
            .find(|m| *m != Self::Full || full_allowed)
            .unwrap_or(Self::ReadOnly)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A mode name that is not one of `read-only`, `auto`, `plan`, `full`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown mode {0:?}: choose read-only, auto, plan or full")]
pub struct ParseModeError(pub String);

impl FromStr for Mode {
    type Err = ParseModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|m| m.name() == s.trim())
            .ok_or_else(|| ParseModeError(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_tab_cycles_and_skips_full_unless_allowed() {
        let mut seen = vec![Mode::ReadOnly];
        for _ in 0..3 {
            seen.push(seen.last().unwrap().next(false));
        }
        assert_eq!(
            seen,
            [Mode::ReadOnly, Mode::Auto, Mode::Plan, Mode::ReadOnly]
        );
        assert_eq!(Mode::Plan.next(true), Mode::Full);
        assert_eq!(Mode::Full.next(true), Mode::ReadOnly);
        assert_eq!(Mode::Full.next(false), Mode::ReadOnly);
    }

    #[test]
    fn names_parse_back_and_match_the_serialized_form() {
        for mode in Mode::ALL {
            assert_eq!(mode.name().parse::<Mode>(), Ok(mode));
            assert_eq!(
                serde_json::to_value(mode).unwrap(),
                serde_json::json!(mode.name())
            );
        }
        assert!("yolo".parse::<Mode>().is_err());
    }
}
