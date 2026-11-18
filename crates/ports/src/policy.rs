//! The [`Policy`] port (ADR-017, Phase 2): decides whether a tool call may run.
//!
//! The runtime asks the policy about every call before running it. The answer
//! is a [`Verdict`]: run it, ask the user, or refuse with a reason for the
//! model. When the user approves "for this session" or "always", the runtime
//! calls [`Policy::remember`] so the same kind of call is allowed next time.

use std::path::PathBuf;

use harness_types::{Mode, Risk};
use serde_json::Value;
use thiserror::Error;

/// A tool call to judge.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyCall {
    /// Tool name, for example `shell` or `mcp__github__get_issue`.
    pub tool: String,
    /// The call's input as sent by the model.
    pub input: Value,
    /// What the tool says the call would touch.
    pub risk: Risk,
    /// Working directory of the task.
    pub cwd: PathBuf,
    /// Roots the task may write to.
    pub writable_roots: Vec<PathBuf>,
    /// The session's permission mode when the call was made (ADR-042).
    pub mode: Mode,
}

/// The policy's answer for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run it.
    Allow {
        /// Why (for logs), for example `rule: prefix:cargo test (user rules)`.
        reason: String,
    },
    /// Ask the user first.
    Ask {
        /// What the user is asked about, for example ``run `git push` ``.
        reason: String,
    },
    /// Refuse; the model receives the reason as the tool result.
    Deny {
        /// Why, written for the model.
        reason: String,
        /// `true` when the call would have been asked about, but the approval
        /// policy is `never` (headless runs exit with code 4 then).
        approval_required: bool,
    },
}

/// How long a remembered approval lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantScope {
    /// Until the session ends.
    Session,
    /// Saved in the user's rules file.
    User,
    /// Saved in the project's rules file (only honoured for trusted projects).
    Project,
}

/// Errors from [`Policy::remember`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PolicyError {
    /// The call cannot be turned into a safe rule (for example a shell command
    /// with substitutions); it was approved once only.
    #[error("this call cannot be remembered: {0}")]
    NotRememberable(String),
    /// The rules file could not be written.
    #[error("cannot save the rule to {path}: {message}")]
    Save {
        /// The rules file.
        path: String,
        /// The underlying error.
        message: String,
    },
}

/// Decides allow, ask or deny for tool calls, and remembers approvals.
///
/// Implementations must be cheap to call: the runtime asks about every call.
pub trait Policy: Send + Sync {
    /// Judges one call.
    fn evaluate(&self, call: &PolicyCall) -> Verdict;

    /// Remembers that the user approved `call` for `scope`, and returns a short
    /// description of what was remembered.
    ///
    /// May write a file for [`GrantScope::User`] and [`GrantScope::Project`];
    /// the runtime calls it off the async worker threads.
    ///
    /// # Errors
    /// [`PolicyError::NotRememberable`] if no safe rule describes the call;
    /// [`PolicyError::Save`] if the rules file cannot be written.
    fn remember(&self, call: &PolicyCall, scope: GrantScope) -> Result<String, PolicyError>;
}
