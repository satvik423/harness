//! Storage ports: the [`EventLog`] (source of truth) and the [`BlobStore`].

use async_trait::async_trait;
use harness_protocol::{Envelope, Event};
use harness_types::{BlobRef, Seq, SessionId, TaskId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors from storage adapters.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreError {
    /// The store (or its writer thread) has shut down.
    #[error("store is closed")]
    Closed,
    /// The event is a streaming delta and must not be stored.
    #[error("event kind {0:?} is not persisted")]
    NotPersistable(&'static str),
    /// No such session, event or blob.
    #[error("not found: {0}")]
    NotFound(String),
    /// Stored data failed to decode.
    #[error("corrupt data: {0}")]
    Corrupt(String),
    /// Underlying I/O or database error.
    #[error("storage backend error: {0}")]
    Backend(String),
}

/// A request to append one event.
#[derive(Debug, Clone, PartialEq)]
pub struct AppendRequest {
    /// The session.
    pub session: SessionId,
    /// The task the event belongs to, if any.
    pub task: Option<TaskId>,
    /// The segment the event belongs to.
    pub segment: u32,
    /// The event; must satisfy [`Event::is_persisted`].
    pub event: Event,
}

/// An event as stored: its envelope plus routing data.
#[derive(Debug, Clone, PartialEq)]
pub struct LoggedEvent {
    /// The envelope with the assigned sequence number and timestamp.
    pub envelope: Envelope,
    /// The task, if any.
    pub task: Option<TaskId>,
    /// The segment.
    pub segment: u32,
}

/// One full-text search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// The matching event's sequence number.
    pub seq: Seq,
    /// The matching event's segment.
    pub segment: u32,
    /// Relevance score (higher is better).
    pub score: f64,
    /// A short excerpt around the match.
    pub snippet: String,
}

/// The append-only event log: the single source of truth (ADR-002).
///
/// Implementations MUST commit an event durably before `append` returns, so
/// callers can perform side effects that depend on it (NFR-03).
#[async_trait]
pub trait EventLog: Send + Sync {
    /// Appends an event and returns it with its assigned [`Seq`] and timestamp.
    ///
    /// # Errors
    /// [`StoreError::NotPersistable`] for streaming deltas, [`StoreError::Closed`]
    /// after shutdown, or [`StoreError::Backend`] on write failure.
    async fn append(&self, request: AppendRequest) -> Result<LoggedEvent, StoreError>;

    /// Returns events `from..=to` of a session, in order.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] for an unknown session, or a backend error.
    async fn range(
        &self,
        session: SessionId,
        from: Seq,
        to: Seq,
    ) -> Result<Vec<LoggedEvent>, StoreError>;

    /// Full-text search over a session's events, best matches first.
    ///
    /// # Errors
    /// Returns a backend error if the search fails.
    async fn search(
        &self,
        session: SessionId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StoreError>;
}

/// Metadata about a stored blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobMeta {
    /// The blob.
    pub blob: BlobRef,
    /// MIME type.
    pub media_type: String,
    /// Size in bytes.
    pub bytes: u64,
    /// The original file name, if known.
    pub original_name: Option<String>,
}

/// Content-addressed storage: identical bytes are stored once (FR-IN-03).
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Stores bytes and returns their content address.
    ///
    /// # Errors
    /// Returns a backend error on write failure.
    async fn put(
        &self,
        bytes: Vec<u8>,
        media_type: &str,
        original_name: Option<&str>,
    ) -> Result<BlobRef, StoreError>;

    /// Reads a blob's bytes.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if the blob does not exist.
    async fn get(&self, blob: &BlobRef) -> Result<Vec<u8>, StoreError>;

    /// Returns a blob's metadata, or `None` if it does not exist.
    ///
    /// # Errors
    /// Returns a backend error if the lookup fails.
    async fn meta(&self, blob: &BlobRef) -> Result<Option<BlobMeta>, StoreError>;
}
