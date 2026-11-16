//! [`Risk`]: what a tool call would touch. Policy decides allow/ask/deny from it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What a tool call would read, change, run or contact.
///
/// Tools compute this from their input before running; the policy engine
/// (Phase 2) turns it into allow, ask or deny.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Risk {
    /// Only reads the given paths (may be empty for pure computations).
    ReadOnly {
        /// Paths that would be read.
        paths: Vec<PathBuf>,
    },
    /// Creates, changes or deletes the given paths.
    Write {
        /// Paths that would be written.
        paths: Vec<PathBuf>,
    },
    /// Runs a command.
    Exec {
        /// The command line as given by the model.
        command: String,
    },
    /// Contacts a network host.
    Network {
        /// Host name, for example `docs.rs`.
        host: String,
    },
    /// Starts something outside the harness's control (a subagent, a delegate CLI).
    External {
        /// Short description of what is started.
        what: String,
    },
}

impl Risk {
    /// `true` only for [`Risk::ReadOnly`].
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        matches!(self, Risk::ReadOnly { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_read_only_is_read_only() {
        assert!(Risk::ReadOnly { paths: vec![] }.is_read_only());
        assert!(
            !Risk::Write {
                paths: vec!["a".into()]
            }
            .is_read_only()
        );
        assert!(
            !Risk::Exec {
                command: "ls".into()
            }
            .is_read_only()
        );
        assert!(!Risk::Network { host: "x".into() }.is_read_only());
        assert!(
            !Risk::External {
                what: "agent".into()
            }
            .is_read_only()
        );
    }

    #[test]
    fn risk_serialises_with_kind_tag() {
        let v = serde_json::to_value(Risk::Exec {
            command: "cargo test".into(),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"kind": "exec", "command": "cargo test"})
        );
    }
}
