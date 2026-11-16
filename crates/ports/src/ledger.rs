//! The [`ReadLedger`] port: which files the model has read, for the
//! read-before-edit guard (FR-TOOL-03).

use std::path::Path;

use harness_types::BlobRef;
use serde::{Deserialize, Serialize};

/// A snapshot of a file's identity at the time it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileStamp {
    /// BLAKE3 hash of the content.
    pub hash: BlobRef,
    /// Modification time, Unix milliseconds.
    pub mtime_ms: i64,
    /// Size in bytes.
    pub len: u64,
}

/// Result of checking a file against the ledger before an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerCheck {
    /// Read in this session and unchanged since: the edit may proceed.
    Fresh,
    /// Never read in this session: refuse the edit.
    NeverRead,
    /// Changed on disk since it was read: refuse and ask the model to re-read.
    ChangedSinceRead,
}

impl LedgerCheck {
    /// Compares the stamp recorded at read time (if any) with the current one.
    ///
    /// The content hash decides; a changed mtime with identical content is still
    /// [`LedgerCheck::Fresh`], so `touch` or a formatter that rewrites the same
    /// bytes does not force a re-read.
    ///
    /// # Examples
    /// ```
    /// use harness_ports::{FileStamp, LedgerCheck};
    /// use harness_types::BlobRef;
    /// let s = FileStamp { hash: BlobRef::from_bytes([1; 32]), mtime_ms: 5, len: 10 };
    /// assert_eq!(LedgerCheck::compare(Some(&s), &s), LedgerCheck::Fresh);
    /// assert_eq!(LedgerCheck::compare(None, &s), LedgerCheck::NeverRead);
    /// ```
    #[must_use]
    pub fn compare(recorded: Option<&FileStamp>, current: &FileStamp) -> Self {
        match recorded {
            None => Self::NeverRead,
            Some(r) if r.hash == current.hash => Self::Fresh,
            Some(_) => Self::ChangedSinceRead,
        }
    }
}

/// Records file reads and checks edits against them.
pub trait ReadLedger: Send + Sync {
    /// Records that `path` was read with the given stamp (replacing any earlier record).
    fn record(&self, path: &Path, stamp: FileStamp);

    /// Checks `path`'s current stamp against what was recorded.
    fn check(&self, path: &Path, current: &FileStamp) -> LedgerCheck;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(hash: u8, mtime_ms: i64) -> FileStamp {
        FileStamp {
            hash: BlobRef::from_bytes([hash; 32]),
            mtime_ms,
            len: 3,
        }
    }

    #[test]
    fn unrecorded_file_was_never_read() {
        assert_eq!(
            LedgerCheck::compare(None, &stamp(1, 1)),
            LedgerCheck::NeverRead
        );
    }

    #[test]
    fn same_content_is_fresh_even_if_mtime_changed() {
        assert_eq!(
            LedgerCheck::compare(Some(&stamp(1, 1)), &stamp(1, 99)),
            LedgerCheck::Fresh
        );
    }

    #[test]
    fn different_content_changed_since_read() {
        assert_eq!(
            LedgerCheck::compare(Some(&stamp(1, 1)), &stamp(2, 1)),
            LedgerCheck::ChangedSinceRead
        );
    }
}
