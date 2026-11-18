//! Snapshots for `/undo` (FR-SES-05): file tools record each file's state
//! before they write it, and undo restores the last turn's files.
//!
//! - [`FileSnapshots`] is in every [`crate::ToolCtx`], bound to the current
//!   session and turn by the runtime.
//! - [`SnapshotStore`] keeps the snapshots and restores them.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use harness_types::{BlobRef, Seq, SessionId};

use crate::StoreError;

/// Records files before a tool changes them.
#[async_trait]
pub trait FileSnapshots: Send + Sync {
    /// Records `path`'s contents before a write; `current` is `None` when the
    /// file does not exist yet. Must be durable before it returns.
    ///
    /// # Errors
    /// A storage error; the tool must not write the file then.
    async fn before_write(&self, path: &Path, current: Option<&[u8]>) -> Result<(), StoreError>;
}

/// A [`FileSnapshots`] that records nothing (tests and tools run outside a session).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSnapshots;

#[async_trait]
impl FileSnapshots for NoSnapshots {
    async fn before_write(&self, _: &Path, _: Option<&[u8]>) -> Result<(), StoreError> {
        Ok(())
    }
}

/// One file's state before a turn changed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Absolute path.
    pub path: PathBuf,
    /// The contents before the turn, or `None` if the file did not exist.
    pub before: Option<BlobRef>,
}

/// What an undo restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undone {
    /// The `TurnStarted` sequence number of the undone turn.
    pub turn: Seq,
    /// The files restored (or removed, if the turn created them).
    pub paths: Vec<PathBuf>,
}

/// Keeps snapshots and restores them.
#[async_trait]
pub trait SnapshotStore: Send + Sync {
    /// Records a snapshot for `turn`. Only the first snapshot of a path in a
    /// turn is kept: it is the state before the turn.
    ///
    /// # Errors
    /// Returns a backend error if the write fails.
    async fn record_snapshot(
        &self,
        session: SessionId,
        turn: Seq,
        snapshot: Snapshot,
    ) -> Result<(), StoreError>;

    /// Restores every file of the latest turn that has snapshots, then forgets
    /// that turn's snapshots, so the next undo goes one turn further back.
    /// Returns `None` when there is nothing to undo.
    ///
    /// # Errors
    /// Returns a backend or I/O error; files restored before it stay restored,
    /// and a repeated undo finishes the job.
    async fn undo_last_turn(&self, session: SessionId) -> Result<Option<Undone>, StoreError>;

    /// For each file the session changed (and has not undone), its state
    /// before the session first changed it, sorted by path (for `/diff`).
    ///
    /// # Errors
    /// Returns a backend error if the lookup fails.
    async fn session_snapshots(&self, session: SessionId) -> Result<Vec<Snapshot>, StoreError>;
}
