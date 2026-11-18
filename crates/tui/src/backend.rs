//! [`Backend`]: what the TUI needs from the rest of harness. The `cli` crate
//! implements it over the runtime and the store; tests use a fake.

use std::path::PathBuf;

use async_trait::async_trait;
use harness_ports::{Effort, TaskRecord};
use harness_protocol::{Envelope, Op, TaskState};
use harness_types::{Mode, ModelRef, SessionId};
use tokio::sync::broadcast;

/// What the TUI shows about the current session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    /// The session.
    pub id: SessionId,
    /// Persona name.
    pub persona: String,
    /// The session's model.
    pub model: ModelRef,
    /// The session's effort, if set.
    pub effort: Option<Effort>,
    /// The permission mode the session starts in; `ModeChanged` events in its
    /// history and live update it (ADR-042).
    pub mode: Mode,
    /// Whether `full` mode may be chosen (`--dangerously-full-access`).
    pub full_access: bool,
    /// Input tokens at which the context counts as full (`context.compact_at_tokens`).
    pub context_limit: u64,
    /// Whether the model has a price in the config (else costs show `$?`).
    pub priced: bool,
    /// The project directory.
    pub cwd: PathBuf,
}

/// A session the TUI is attached to.
#[derive(Debug)]
pub struct Attached {
    /// What to show about it.
    pub view: SessionView,
    /// Live events (subscribed before any event the TUI must see).
    pub events: broadcast::Receiver<Envelope>,
    /// Events stored before attaching (a resumed session's history).
    pub history: Vec<Envelope>,
    /// The user's earlier prompts in this project, oldest first (↑ recalls them).
    pub prompts: Vec<String>,
}

/// One file the session changed: its contents before the session's first
/// change, and now (`None`: did not exist).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// The file.
    pub path: PathBuf,
    /// Contents before the session changed it.
    pub before: Option<String>,
    /// Contents now.
    pub after: Option<String>,
}

/// A stored session, as listed by `/sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// The session.
    pub id: SessionId,
    /// Its title (the start of its first prompt).
    pub title: Option<String>,
    /// When it was last active (its latest task change, else its creation), Unix ms.
    pub last_active: i64,
    /// The state of its latest task.
    pub state: Option<TaskState>,
}

/// The TUI's view of harness.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Sends an op to the current session.
    ///
    /// # Errors
    /// A message if the session has stopped.
    async fn send(&self, op: Op) -> Result<(), String>;

    /// The current session's tasks.
    ///
    /// # Errors
    /// A message if the store cannot be read.
    async fn tasks(&self) -> Result<Vec<TaskRecord>, String>;

    /// Files the current session changed (from its snapshots).
    ///
    /// # Errors
    /// A message if the store cannot be read.
    async fn changes(&self) -> Result<Vec<ChangedFile>, String>;

    /// Starts a new session with `model` and `effort` and makes it current.
    ///
    /// # Errors
    /// A message if it cannot start (for example a missing API key).
    async fn new_session(
        &self,
        model: ModelRef,
        effort: Option<Effort>,
    ) -> Result<Attached, String>;

    /// This project's sessions that have a prompt, most recently active first.
    ///
    /// # Errors
    /// A message if the store cannot be read.
    async fn sessions(&self) -> Result<Vec<SessionSummary>, String>;

    /// Opens a stored session with the model it was started with and makes it current.
    ///
    /// # Errors
    /// A message if it cannot be opened (for example it is open in another
    /// harness process, or its provider has no API key).
    async fn open_session(&self, id: SessionId) -> Result<Attached, String>;

    /// Models offered by `/model` (the configured default and priced models).
    fn models(&self) -> Vec<ModelRef>;
}
