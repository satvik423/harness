//! The [`SessionStore`] port: the catalogue of sessions and tasks (FR-SES-01),
//! and task control requests between processes.

use std::path::PathBuf;

use async_trait::async_trait;
use harness_protocol::TaskState;
use harness_types::{Seq, SessionId, TaskId};
use serde::{Deserialize, Serialize};

use crate::StoreError;

/// Whether a session is still in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Can be resumed.
    Open,
    /// Closed by the user.
    Closed,
}

/// One session, as listed by `harness session list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// The session.
    pub id: SessionId,
    /// Creation time, Unix milliseconds.
    pub created_at: i64,
    /// A short title (the start of the first prompt), once known.
    pub title: Option<String>,
    /// The persona the session runs.
    pub persona: String,
    /// The project directory.
    pub cwd: PathBuf,
    /// Open or closed.
    pub status: SessionStatus,
    /// The session this one was forked from (Phase 5).
    pub parent: Option<SessionId>,
    /// The parent's sequence number at the fork (Phase 5).
    pub forked_at: Option<Seq>,
    /// The segment new events go to.
    pub current_segment: u32,
}

/// One task, as listed by `harness task list`. Kept up to date from the
/// task's `TaskState` events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    /// The task.
    pub id: TaskId,
    /// Its session.
    pub session: SessionId,
    /// The persona it runs.
    pub persona: String,
    /// Its latest state.
    pub state: TaskState,
    /// Why it entered that state.
    pub reason: Option<String>,
    /// When it was created, Unix milliseconds.
    pub created_at: i64,
    /// When its state last changed, Unix milliseconds.
    pub updated_at: i64,
}

/// A control request for a task, possibly from another process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskControl {
    /// Pause at the next safe point.
    Pause,
    /// Resume a paused task.
    Resume,
    /// Cancel the task.
    Cancel,
}

/// What [`SessionStore::delete_session`] removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeletedSession {
    /// Events removed from the log.
    pub events: u64,
    /// Stored files (blobs) freed because nothing references them any more.
    pub blobs_freed: u64,
}

/// The session catalogue and the task-control mailbox.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Registers a new session (before its first event is appended).
    ///
    /// # Errors
    /// [`StoreError::Conflict`] if the id exists, or a backend error.
    async fn create_session(&self, record: SessionRecord) -> Result<(), StoreError>;

    /// One session, or `None` if unknown.
    ///
    /// # Errors
    /// Returns a backend error if the lookup fails.
    async fn session(&self, id: SessionId) -> Result<Option<SessionRecord>, StoreError>;

    /// All sessions, newest first.
    ///
    /// # Errors
    /// Returns a backend error if the lookup fails.
    async fn sessions(&self) -> Result<Vec<SessionRecord>, StoreError>;

    /// Tasks of one session (or of all sessions), oldest first.
    ///
    /// # Errors
    /// Returns a backend error if the lookup fails.
    async fn tasks(&self, session: Option<SessionId>) -> Result<Vec<TaskRecord>, StoreError>;

    /// Leaves a control request for whichever process runs the task.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] for an unknown task, or a backend error.
    async fn request_control(&self, task: TaskId, control: TaskControl) -> Result<(), StoreError>;

    /// Deletes a session and everything stored for it: events, tasks, usage,
    /// snapshots and control requests. Blobs that no other session references
    /// are freed too, when that is safe (the store decides).
    ///
    /// This is a user action (`harness session delete`); it is the only way
    /// events ever leave the log (ADR-041).
    ///
    /// # Errors
    /// [`StoreError::NotFound`] for an unknown session, [`StoreError::Conflict`]
    /// if the session is open in a harness process or other sessions were
    /// forked from it, or a backend error.
    async fn delete_session(&self, id: SessionId) -> Result<DeletedSession, StoreError>;

    /// Removes and returns the pending control requests for a session, oldest first.
    ///
    /// # Errors
    /// Returns a backend error if the lookup fails.
    async fn take_controls(
        &self,
        session: SessionId,
    ) -> Result<Vec<(TaskId, TaskControl)>, StoreError>;
}
