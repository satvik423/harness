//! Approval handling for tool calls that are not read-only.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use harness_protocol::Decision;
use harness_types::ApprovalId;
use tokio::sync::oneshot;

/// How the session answers calls that need approval (Phase 2 replaces this with the policy engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Ask the user for every call that is not read-only.
    Ask,
    /// Deny every such call without asking (headless runs, `--approval never`).
    Never,
}

/// Pending approval requests waiting for the user's answer.
#[derive(Debug, Default)]
pub(crate) struct Approvals {
    pending: Mutex<HashMap<ApprovalId, oneshot::Sender<Decision>>>,
}

impl Approvals {
    /// Registers a request; the receiver yields the user's decision.
    pub(crate) fn register(&self, id: ApprovalId) -> oneshot::Receiver<Decision> {
        let (tx, rx) = oneshot::channel();
        self.lock().insert(id, tx);
        rx
    }

    /// Delivers a decision. Returns `false` if no such request is pending.
    pub(crate) fn resolve(&self, id: ApprovalId, decision: Decision) -> bool {
        self.lock()
            .remove(&id)
            .is_some_and(|tx| tx.send(decision).is_ok())
    }

    /// Drops a request that will not be answered (for example after an interrupt).
    pub(crate) fn forget(&self, id: ApprovalId) {
        self.lock().remove(&id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ApprovalId, oneshot::Sender<Decision>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolve_delivers_the_decision() {
        let approvals = Approvals::default();
        let id = ApprovalId::new();
        let rx = approvals.register(id);
        assert!(approvals.resolve(id, Decision::Once));
        assert_eq!(rx.await.unwrap(), Decision::Once);
    }

    #[test]
    fn unknown_or_forgotten_requests_are_not_resolved() {
        let approvals = Approvals::default();
        assert!(!approvals.resolve(ApprovalId::new(), Decision::Once));
        let id = ApprovalId::new();
        let _rx = approvals.register(id);
        approvals.forget(id);
        assert!(!approvals.resolve(id, Decision::Once));
    }
}
