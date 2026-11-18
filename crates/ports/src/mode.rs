//! The [`ModeControl`] port: reading and switching the session's permission
//! [`Mode`] (ADR-042).
//!
//! The runtime implements it for each session: a switch is recorded as
//! `Event::ModeChanged` and the model is told in a system note. Tools get it
//! through [`crate::ToolCtx::mode`], so `propose_plan` can leave plan mode once
//! the user approves the plan.

use async_trait::async_trait;
use harness_types::Mode;
use thiserror::Error;

/// Why the mode was not switched.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModeError {
    /// `full` was asked for, but the process was not started with
    /// `--dangerously-full-access`.
    #[error("full mode needs harness to be started with --dangerously-full-access")]
    FullAccessNotAllowed,
    /// Nothing here can switch modes (a tool run outside a session).
    #[error("the mode cannot be changed here")]
    Unavailable,
    /// The switch could not be recorded; the mode is unchanged.
    #[error("could not record the mode change: {0}")]
    Store(String),
}

/// Reads and switches the permission mode of the session a call belongs to.
#[async_trait]
pub trait ModeControl: Send + Sync {
    /// The mode in force now.
    fn current(&self) -> Mode;

    /// Switches to `mode`. Switching to the current mode does nothing.
    ///
    /// # Errors
    /// [`ModeError::FullAccessNotAllowed`] for `full` without the flag,
    /// [`ModeError::Unavailable`] outside a session, [`ModeError::Store`] if
    /// the change cannot be recorded.
    async fn set(&self, mode: Mode) -> Result<(), ModeError>;
}

/// A [`ModeControl`] that never changes (tests, tools run outside a session).
///
/// # Examples
/// ```
/// use harness_ports::{FixedMode, ModeControl};
/// use harness_types::Mode;
/// assert_eq!(FixedMode(Mode::Plan).current(), Mode::Plan);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct FixedMode(pub Mode);

#[async_trait]
impl ModeControl for FixedMode {
    fn current(&self) -> Mode {
        self.0
    }

    async fn set(&self, _: Mode) -> Result<(), ModeError> {
        Err(ModeError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fixed_mode_cannot_be_switched() {
        let fixed = FixedMode(Mode::Auto);
        assert_eq!(fixed.set(Mode::Plan).await, Err(ModeError::Unavailable));
        assert_eq!(fixed.current(), Mode::Auto);
    }
}
