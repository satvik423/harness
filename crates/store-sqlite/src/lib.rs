//! SQLite storage for the harness (P3-T1, P3-T2, Tech spec 3.12).
//!
//! [`SqliteStore`] keeps a project's state in `.harness/`:
//!
//! - `state.db`: sessions, tasks, the append-only event log with full-text
//!   search, segments, snapshots, usage and task control requests
//! - `blobs/ab/cdef…`: content-addressed bytes (BLAKE3)
//! - `.gitignore`: written on first use so local state is never committed
//!
//! It implements the [`harness_ports::EventLog`], [`harness_ports::BlobStore`],
//! [`harness_ports::SessionStore`] and [`harness_ports::SnapshotStore`] ports.
//! One writer thread commits in batches and answers only after the commit
//! (NFR-03); reads run on the blocking pool. [`SessionLock`] keeps a session to
//! one process at a time.
//!
//! # Example
//! ```
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! use harness_ports::{SessionRecord, SessionStatus, SessionStore};
//! use harness_store_sqlite::SqliteStore;
//! use harness_types::SessionId;
//!
//! let dir = tempfile::tempdir().unwrap();
//! let store = SqliteStore::open(dir.path()).unwrap();
//! let id = SessionId::new();
//! store.create_session(SessionRecord {
//!     id, created_at: 0, title: None, persona: "default".into(), cwd: dir.path().into(),
//!     status: SessionStatus::Open, parent: None, forked_at: None, current_segment: 0,
//! }).await.unwrap();
//! assert_eq!(store.sessions().await.unwrap().len(), 1);
//! assert!(dir.path().join(".harness/.gitignore").exists());
//! # });
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod blobs;
mod lock;
mod schema;
mod store;
mod text;
mod writer;

pub use lock::{LockError, SessionLock};
pub use schema::SCHEMA_VERSION;
pub use store::{HARNESS_GITIGNORE, STATE_DB, SqliteStore};

use harness_ports::StoreError;

/// Maps a SQLite error to the port's backend error.
pub(crate) fn backend(e: rusqlite::Error) -> StoreError {
    StoreError::Backend(e.to_string())
}
