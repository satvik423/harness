//! [`AskForChanges`]: a minimal [`Policy`] for runtime tests.

use std::sync::{Mutex, PoisonError};

use harness_ports::{GrantScope, Policy, PolicyCall, PolicyError, Verdict};
use harness_types::{Mode, Risk};

/// Allows read-only calls and asks about everything else, like the Phase 1
/// runtime did. Calls whose input contains `"deny": true` are denied, so tests
/// can exercise policy refusals. Remembered grants (any scope) allow later
/// calls with the same tool and risk. The mode of every judged call is kept,
/// so tests can check which mode the runtime passed.
///
/// # Examples
/// ```
/// use harness_ports::{GrantScope, Policy, PolicyCall, Verdict};
/// use harness_testkit::AskForChanges;
/// use harness_types::{Mode, Risk};
/// let policy = AskForChanges::default();
/// let call = PolicyCall {
///     tool: "exec".into(),
///     input: serde_json::json!({}),
///     risk: Risk::Exec { command: "ls".into() },
///     cwd: "/p".into(),
///     writable_roots: vec![],
///     mode: Mode::Auto,
/// };
/// assert!(matches!(policy.evaluate(&call), Verdict::Ask { .. }));
/// policy.remember(&call, GrantScope::Session).unwrap();
/// assert!(matches!(policy.evaluate(&call), Verdict::Allow { .. }));
/// ```
#[derive(Debug, Default)]
pub struct AskForChanges {
    grants: Mutex<Vec<(String, Risk, GrantScope)>>,
    modes: Mutex<Vec<Mode>>,
}

impl AskForChanges {
    /// Every grant remembered so far, in order.
    #[must_use]
    pub fn grants(&self) -> Vec<(String, Risk, GrantScope)> {
        self.grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The mode of every call judged so far, in order.
    #[must_use]
    pub fn modes(&self) -> Vec<Mode> {
        self.modes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Policy for AskForChanges {
    fn evaluate(&self, call: &PolicyCall) -> Verdict {
        self.modes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(call.mode);
        if call.input.get("deny").and_then(serde_json::Value::as_bool) == Some(true) {
            return Verdict::Deny {
                reason: "Denied by policy: test rule".into(),
                approval_required: false,
            };
        }
        if call.risk.is_read_only() {
            return Verdict::Allow {
                reason: "read-only".into(),
            };
        }
        let granted = self
            .grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(tool, risk, _)| *tool == call.tool && *risk == call.risk);
        if granted {
            Verdict::Allow {
                reason: "remembered".into(),
            }
        } else {
            Verdict::Ask {
                reason: format!("{} needs approval", call.tool),
            }
        }
    }

    fn remember(&self, call: &PolicyCall, scope: GrantScope) -> Result<String, PolicyError> {
        self.grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((call.tool.clone(), call.risk.clone(), scope));
        Ok(format!("{} for {scope:?}", call.tool))
    }
}
