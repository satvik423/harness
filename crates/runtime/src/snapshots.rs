//! [`TurnSnapshots`]: the [`FileSnapshots`] given to each tool call, bound to
//! the session and the assistant message whose calls are running.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harness_ports::{BlobStore, FileSnapshots, Snapshot, SnapshotStore, StoreError};
use harness_types::{Seq, SessionId};

/// Stores a file's previous contents as a blob and records the snapshot under
/// `turn` (the `AssistantMessage` sequence number), before the tool writes.
pub(crate) struct TurnSnapshots {
    pub(crate) session: SessionId,
    pub(crate) turn: Seq,
    pub(crate) blobs: Arc<dyn BlobStore>,
    pub(crate) store: Arc<dyn SnapshotStore>,
}

#[async_trait]
impl FileSnapshots for TurnSnapshots {
    async fn before_write(&self, path: &Path, current: Option<&[u8]>) -> Result<(), StoreError> {
        let before = match current {
            Some(bytes) => Some(
                self.blobs
                    .put(bytes.to_vec(), "application/octet-stream", None)
                    .await?,
            ),
            None => None,
        };
        self.store
            .record_snapshot(
                self.session,
                self.turn,
                Snapshot {
                    path: path.to_path_buf(),
                    before,
                },
            )
            .await
    }
}
