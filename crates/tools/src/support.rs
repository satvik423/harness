//! In-memory support implementations used until the real stores exist:
//! [`MemoryReadLedger`] (kept), [`MemoryBlobStore`] (replaced by SQLite in
//! Phase 3) and [`Unsandboxed`] (replaced by bubblewrap in Phase 13).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use harness_ports::{
    BlobMeta, BlobStore, CommandSpec, FileStamp, LedgerCheck, ReadLedger, Sandbox, SandboxError,
    SandboxMode, StoreError,
};
use harness_types::BlobRef;

/// Computes the [`FileStamp`] of file contents read from disk.
#[must_use]
pub fn stamp_of(bytes: &[u8], metadata: &std::fs::Metadata) -> FileStamp {
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    FileStamp {
        hash: BlobRef::from_bytes(*blake3::hash(bytes).as_bytes()),
        mtime_ms,
        len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    }
}

/// The ledger key for `path`: canonical when the file exists, so `src/a.rs`
/// and `/abs/proj/src/a.rs` refer to the same entry.
fn ledger_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Session-scoped record of files read, for the read-before-edit guard.
#[derive(Debug, Default)]
pub struct MemoryReadLedger {
    entries: Mutex<HashMap<PathBuf, FileStamp>>,
}

impl ReadLedger for MemoryReadLedger {
    fn record(&self, path: &Path, stamp: FileStamp) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(ledger_key(path), stamp);
    }

    fn check(&self, path: &Path, current: &FileStamp) -> LedgerCheck {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        LedgerCheck::compare(entries.get(&ledger_key(path)), current)
    }
}

/// Content-addressed bytes held in memory (lost when the process exits).
#[derive(Debug, Default)]
pub struct MemoryBlobStore {
    blobs: Mutex<HashMap<BlobRef, (Vec<u8>, BlobMeta)>>,
}

#[async_trait]
impl BlobStore for MemoryBlobStore {
    async fn put(
        &self,
        bytes: Vec<u8>,
        media_type: &str,
        original_name: Option<&str>,
    ) -> Result<BlobRef, StoreError> {
        let blob = BlobRef::from_bytes(*blake3::hash(&bytes).as_bytes());
        let meta = BlobMeta {
            blob,
            media_type: media_type.to_owned(),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            original_name: original_name.map(str::to_owned),
        };
        // Identical bytes are stored once; the first name seen is kept.
        self.blobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(blob)
            .or_insert((bytes, meta));
        Ok(blob)
    }

    async fn get(&self, blob: &BlobRef) -> Result<Vec<u8>, StoreError> {
        self.blobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(blob)
            .map(|(bytes, _)| bytes.clone())
            .ok_or_else(|| StoreError::NotFound(format!("blob {blob}")))
    }

    async fn meta(&self, blob: &BlobRef) -> Result<Option<BlobMeta>, StoreError> {
        Ok(self
            .blobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(blob)
            .map(|(_, m)| m.clone()))
    }
}

/// A [`Sandbox`] that runs commands unchanged. Used until Phase 13; every
/// command still needs user approval in Phase 1.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unsandboxed;

impl Sandbox for Unsandboxed {
    fn mode(&self) -> SandboxMode {
        SandboxMode::FullAccess
    }

    fn prepare(&self, command: CommandSpec) -> Result<CommandSpec, SandboxError> {
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_hashes_content_with_blake3() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        let stamp = stamp_of(b"abc", &std::fs::metadata(&path).unwrap());
        assert_eq!(stamp.hash.as_bytes(), blake3::hash(b"abc").as_bytes());
        assert_eq!(stamp.len, 3);
        assert!(stamp.mtime_ms > 0);
    }

    #[test]
    fn ledger_reports_never_read_fresh_and_changed() {
        let ledger = MemoryReadLedger::default();
        let s1 = FileStamp {
            hash: BlobRef::from_bytes([1; 32]),
            mtime_ms: 1,
            len: 1,
        };
        let s2 = FileStamp {
            hash: BlobRef::from_bytes([2; 32]),
            mtime_ms: 2,
            len: 1,
        };
        assert_eq!(ledger.check(Path::new("/a"), &s1), LedgerCheck::NeverRead);
        ledger.record(Path::new("/a"), s1);
        assert_eq!(ledger.check(Path::new("/a"), &s1), LedgerCheck::Fresh);
        assert_eq!(
            ledger.check(Path::new("/a"), &s2),
            LedgerCheck::ChangedSinceRead
        );
    }

    #[tokio::test]
    async fn blob_store_deduplicates_and_returns_bytes() {
        let store = MemoryBlobStore::default();
        let a = store
            .put(b"hello".to_vec(), "text/plain", Some("a.txt"))
            .await
            .unwrap();
        let b = store
            .put(b"hello".to_vec(), "text/plain", None)
            .await
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.as_bytes(), blake3::hash(b"hello").as_bytes());
        assert_eq!(store.get(&a).await.unwrap(), b"hello");
        let meta = store.meta(&a).await.unwrap().unwrap();
        assert_eq!(
            (
                meta.bytes,
                meta.media_type.as_str(),
                meta.original_name.as_deref()
            ),
            (5, "text/plain", Some("a.txt"))
        );
    }

    #[tokio::test]
    async fn missing_blob_is_not_found() {
        let store = MemoryBlobStore::default();
        let r = BlobRef::from_bytes([9; 32]);
        assert!(matches!(store.get(&r).await, Err(StoreError::NotFound(_))));
        assert_eq!(store.meta(&r).await.unwrap(), None);
    }

    #[test]
    fn unsandboxed_passes_commands_through() {
        let cmd = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![],
            cwd: "/".into(),
            env: vec![],
            network: true,
        };
        assert_eq!(Unsandboxed.prepare(cmd.clone()).unwrap(), cmd);
        assert_eq!(Unsandboxed.mode(), SandboxMode::FullAccess);
    }
}
