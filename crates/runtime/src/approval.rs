//! Requests that wait for the user: approvals of tool calls, and `ask_user` questions.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Mutex, PoisonError};

use harness_protocol::Decision;
use harness_types::{ApprovalId, QuestionId};
use tokio::sync::oneshot;

/// How the session answers calls that need approval (Phase 2 replaces this with the policy engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Ask the user for every call that is not read-only.
    Ask,
    /// Deny every such call without asking (headless runs, `--approval never`).
    Never,
}

/// Requests waiting for the user's answer, keyed by request id: approvals
/// ([`Approvals`]) and `ask_user` questions ([`Questions`]).
#[derive(Debug)]
pub(crate) struct Pending<K, V> {
    pending: Mutex<HashMap<K, oneshot::Sender<V>>>,
}

impl<K, V> Default for Pending<K, V> {
    fn default() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }
}

/// Pending approval requests.
pub(crate) type Approvals = Pending<ApprovalId, Decision>;
/// Pending `ask_user` questions.
pub(crate) type Questions = Pending<QuestionId, String>;

impl<K: Eq + Hash, V> Pending<K, V> {
    /// Registers a request; the receiver yields the user's answer.
    pub(crate) fn register(&self, id: K) -> oneshot::Receiver<V> {
        let (tx, rx) = oneshot::channel();
        self.lock().insert(id, tx);
        rx
    }

    /// Delivers an answer. Returns `false` if no such request is pending.
    pub(crate) fn resolve(&self, id: &K, answer: V) -> bool {
        self.lock()
            .remove(id)
            .is_some_and(|tx| tx.send(answer).is_ok())
    }

    /// Drops a request that will not be answered (for example after an interrupt).
    pub(crate) fn forget(&self, id: &K) {
        self.lock().remove(id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<K, oneshot::Sender<V>>> {
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
        assert!(approvals.resolve(&id, Decision::Once));
        assert_eq!(rx.await.unwrap(), Decision::Once);
    }

    #[test]
    fn unknown_or_forgotten_requests_are_not_resolved() {
        let approvals = Approvals::default();
        assert!(!approvals.resolve(&ApprovalId::new(), Decision::Once));
        let id = ApprovalId::new();
        let _rx = approvals.register(id);
        approvals.forget(&id);
        assert!(!approvals.resolve(&id, Decision::Once));
    }
}
