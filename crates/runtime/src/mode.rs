//! The session's permission mode (ADR-042): [`SessionMode`] holds it, records
//! every switch as `Event::ModeChanged` and queues the note that tells the model.
//!
//! The system prompt and tools are frozen for a segment, so a switch never
//! edits them. The model learns about it from a system note sent at the start
//! of the next user message, the same way replay rebuilds it: a note recorded
//! while a segment is open is queued for that segment, and a new segment's
//! environment states the mode instead, so notes queued before it are dropped.

use std::sync::{Arc, Mutex, PoisonError, RwLock};

use async_trait::async_trait;
use harness_ports::{ModeControl, ModeError};
use harness_protocol::Event;
use harness_types::Mode;

use crate::ApprovalMode;
use crate::journal::Journal;

/// What the model may do in `mode`, in words it can act on.
fn guidance(mode: Mode) -> &'static str {
    match mode {
        Mode::ReadOnly => "reads run freely; every edit and command needs the user's approval.",
        Mode::Auto => {
            "edits inside the project run without asking; commands need the user's approval."
        }
        Mode::Plan => {
            "do not change any files. Explore with read-only tools (commands need the user's approval), then call propose_plan with your plan and wait for the user's decision."
        }
        Mode::Full => "tool calls run without asking. Take extra care with destructive commands.",
    }
}

/// The system note sent after a switch to `mode`.
pub(crate) fn mode_note(mode: Mode) -> String {
    format!("The permission mode is now {mode}: {}", guidance(mode))
}

/// The environment's sentence about permissions for a new segment.
pub(crate) fn environment_note(mode: Mode, approval: ApprovalMode) -> String {
    let never = match approval {
        ApprovalMode::Ask => "",
        ApprovalMode::Never => {
            " This session never asks: tool calls that need approval are denied automatically."
        }
    };
    format!(
        "Commands run without a sandbox. Permission mode: {mode}: {}{never} The user can switch modes during the session; a system note will say so.",
        guidance(mode)
    )
}

/// The permission mode of one session.
pub(crate) struct SessionMode {
    current: RwLock<Mode>,
    full_access: bool,
    journal: Arc<Journal>,
    /// Notes for the model, sent before the next user message.
    pending: Mutex<Vec<String>>,
    /// Serializes switches, so `from` in each event is the mode it replaced.
    switching: tokio::sync::Mutex<()>,
}

impl SessionMode {
    pub(crate) fn new(start: Mode, full_access: bool, journal: Arc<Journal>) -> Self {
        Self {
            current: RwLock::new(start),
            full_access,
            journal,
            pending: Mutex::default(),
            switching: tokio::sync::Mutex::new(()),
        }
    }

    /// Takes the notes queued since the last user message.
    pub(crate) fn take_notes(&self) -> Vec<String> {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl std::fmt::Debug for SessionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionMode")
            .field("current", &self.current())
            .field("full_access", &self.full_access)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ModeControl for SessionMode {
    fn current(&self) -> Mode {
        *self.current.read().unwrap_or_else(PoisonError::into_inner)
    }

    async fn set(&self, to: Mode) -> Result<(), ModeError> {
        if to == Mode::Full && !self.full_access {
            return Err(ModeError::FullAccessNotAllowed);
        }
        let _one_at_a_time = self.switching.lock().await;
        let from = self.current();
        if from == to {
            return Ok(());
        }
        let note = mode_note(to);
        // Recorded first: the mode only changes once the log says so.
        self.journal
            .record(
                None,
                Event::ModeChanged {
                    from,
                    to,
                    note: note.clone(),
                },
            )
            .await
            .map_err(|e| ModeError::Store(e.to_string()))?;
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = to;
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(note);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_and_the_environment_tell_the_model_what_it_may_do() {
        assert!(mode_note(Mode::Plan).contains("propose_plan"));
        assert!(mode_note(Mode::Plan).starts_with("The permission mode is now plan"));
        let env = environment_note(Mode::Auto, ApprovalMode::Never);
        assert!(env.contains("Permission mode: auto"), "{env}");
        assert!(env.contains("denied automatically"), "{env}");
        assert!(!environment_note(Mode::Auto, ApprovalMode::Ask).contains("denied"));
    }
}
