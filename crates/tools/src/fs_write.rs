//! Shared guard and writer for the file-changing tools (`write_file`,
//! `edit_file`, `apply_patch`).
//!
//! - [`check_fresh`]: the read-before-edit guard (FR-TOOL-03). An existing file
//!   may only be changed if it was read in this session and is unchanged since.
//! - [`write_atomic`]: writes through a temporary file in the same directory and
//!   renames it into place, so a crash never leaves a half-written file.
//! - [`record_written`]: updates the ledger after a write, so the model can keep
//!   editing a file it just wrote without reading it again.

use std::path::Path;

use harness_ports::{LedgerCheck, ReadLedger, ToolCtx, ToolError};
use harness_types::TaskId;

use crate::support::stamp_of;

/// What is on disk at a path before a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Existing {
    /// Nothing is there.
    Missing,
    /// A file with these contents.
    File(Vec<u8>),
}

/// Reads `path` and checks it against the ledger.
///
/// A missing file passes (creating files needs no read). An existing file must
/// be [`LedgerCheck::Fresh`].
///
/// # Errors
/// [`ToolError::Refused`] if the file was never read or changed since it was
/// read; [`ToolError::InvalidInput`] if `path` is a directory;
/// [`ToolError::Failed`] if it cannot be read.
pub(crate) async fn check_fresh(
    ledger: &dyn ReadLedger,
    path: &Path,
    shown: &str,
) -> Result<Existing, ToolError> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Existing::Missing),
        Err(e) => return Err(ToolError::Failed(format!("cannot inspect {shown}: {e}"))),
    };
    if metadata.is_dir() {
        return Err(ToolError::InvalidInput(format!("{shown} is a directory")));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| ToolError::Failed(format!("cannot read {shown}: {e}")))?;
    match ledger.check(path, &stamp_of(&bytes, &metadata)) {
        LedgerCheck::Fresh => Ok(Existing::File(bytes)),
        LedgerCheck::NeverRead => Err(ToolError::Refused(format!(
            "{shown} has not been read in this session. Read it with read_file first, then make the change."
        ))),
        LedgerCheck::ChangedSinceRead => Err(ToolError::Refused(format!(
            "{shown} changed on disk since you read it. Read it again with read_file, then make the change against the current contents."
        ))),
    }
}

/// Writes `bytes` to `path` atomically, creating parent directories and keeping
/// the permissions of a file being replaced.
///
/// # Errors
/// [`ToolError::Failed`] with the I/O error if any step fails; the original file
/// is left untouched in that case.
pub(crate) async fn write_atomic(
    path: &Path,
    bytes: &[u8],
    shown: &str,
    task: TaskId,
) -> Result<(), ToolError> {
    let fail = |what: &str, e: std::io::Error| ToolError::Failed(format!("{what} {shown}: {e}"));
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| ToolError::InvalidInput(format!("{shown} has no parent directory")))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| fail("cannot create the directory for", e))?;
    let name = path
        .file_name()
        .ok_or_else(|| ToolError::InvalidInput(format!("{shown} has no file name")))?;
    let tmp = parent.join(tmp_name(name, task));
    let permissions = tokio::fs::metadata(path)
        .await
        .ok()
        .map(|m| m.permissions());
    let result = async {
        tokio::fs::write(&tmp, bytes)
            .await
            .map_err(|e| fail("cannot write", e))?;
        if let Some(permissions) = permissions {
            tokio::fs::set_permissions(&tmp, permissions)
                .await
                .map_err(|e| fail("cannot set permissions on", e))?;
        }
        tokio::fs::rename(&tmp, path)
            .await
            .map_err(|e| fail("cannot replace", e))
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result
}

/// Records `path`'s current contents for undo, before a write (FR-SES-05).
///
/// # Errors
/// [`ToolError::Failed`] if the snapshot cannot be stored; the caller must not
/// write then.
pub(crate) async fn snapshot(
    ctx: &ToolCtx,
    path: &Path,
    current: Option<&[u8]>,
    shown: &str,
) -> Result<(), ToolError> {
    ctx.snapshots
        .before_write(path, current)
        .await
        .map_err(|e| {
            ToolError::Failed(format!(
                "cannot save {shown} for undo ({e}); nothing was changed"
            ))
        })
}

/// Name of the temporary file used to replace `name` atomically.
///
/// Unique per task and file, hidden, and placed in the same directory as the
/// target so the final rename stays on one filesystem (rename is only atomic there).
pub(crate) fn tmp_name(name: &std::ffi::OsStr, task: TaskId) -> String {
    format!(".{}.harness-{task}.tmp", name.to_string_lossy())
}

/// Records the new contents of a written file in the ledger.
pub(crate) async fn record_written(ledger: &dyn ReadLedger, path: &Path) {
    if let Ok(metadata) = tokio::fs::metadata(path).await
        && let Ok(bytes) = tokio::fs::read(path).await
    {
        ledger.record(path, stamp_of(&bytes, &metadata));
    }
}

/// Number of lines in `text`, counting a final line without a newline.
pub(crate) fn line_count(text: &str) -> usize {
    text.lines().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryReadLedger;

    fn record(ledger: &MemoryReadLedger, path: &Path) {
        let bytes = std::fs::read(path).unwrap();
        ledger.record(path, stamp_of(&bytes, &std::fs::metadata(path).unwrap()));
    }

    #[tokio::test]
    async fn missing_file_passes_the_guard() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = MemoryReadLedger::default();
        let got = check_fresh(&ledger, &dir.path().join("new.txt"), "new.txt").await;
        assert_eq!(got, Ok(Existing::Missing));
    }

    #[tokio::test]
    async fn unread_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "x").unwrap();
        let err = check_fresh(&MemoryReadLedger::default(), &path, "a.txt")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Refused(m) if m.contains("has not been read") && m.contains("read_file")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn file_changed_since_read_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one").unwrap();
        let ledger = MemoryReadLedger::default();
        record(&ledger, &path);
        std::fs::write(&path, "two").unwrap();
        let err = check_fresh(&ledger, &path, "a.txt").await.unwrap_err();
        assert!(
            matches!(&err, ToolError::Refused(m) if m.contains("changed on disk")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn fresh_file_returns_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one").unwrap();
        let ledger = MemoryReadLedger::default();
        record(&ledger, &path);
        let got = check_fresh(&ledger, &path, "a.txt").await;
        assert_eq!(got, Ok(Existing::File(b"one".to_vec())));
    }

    #[tokio::test]
    async fn directories_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = check_fresh(&MemoryReadLedger::default(), dir.path(), "dir")
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{err}");
    }

    #[tokio::test]
    async fn atomic_write_creates_parents_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c.txt");
        write_atomic(&path, b"hi", "a/b/c.txt", TaskId::new())
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi");
        let names: Vec<_> = std::fs::read_dir(dir.path().join("a/b"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["c.txt"]);
    }

    #[tokio::test]
    async fn atomic_write_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.sh");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        write_atomic(&path, b"new", "run.sh", TaskId::new())
            .await
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[tokio::test]
    async fn record_written_makes_the_file_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "x").unwrap();
        let ledger = MemoryReadLedger::default();
        record_written(&ledger, &path).await;
        assert!(matches!(
            check_fresh(&ledger, &path, "a.txt").await,
            Ok(Existing::File(_))
        ));
    }

    #[test]
    fn line_count_counts_a_final_unterminated_line() {
        assert_eq!(line_count(""), 0);
        assert_eq!(line_count("a"), 1);
        assert_eq!(line_count("a\nb\n"), 2);
    }
}

#[cfg(test)]
mod snapshot_tests {
    use std::sync::Arc;

    use serde_json::json;

    use crate::test_support::{RecordedSnapshots, ctx};
    use crate::{ApplyPatch, EditFile, ReadFile, WriteFile};
    use harness_ports::{Tool, ToolError};

    fn setup(
        fail: bool,
    ) -> (
        tempfile::TempDir,
        harness_ports::ToolCtx,
        Arc<RecordedSnapshots>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, _) = ctx(dir.path());
        let snaps = Arc::new(RecordedSnapshots {
            fail,
            ..RecordedSnapshots::default()
        });
        c.snapshots = snaps.clone();
        (dir, c, snaps)
    }

    #[tokio::test]
    async fn write_and_edit_snapshot_the_previous_contents_first() {
        let (dir, c, snaps) = setup(false);
        std::fs::write(dir.path().join("a.txt"), "one").unwrap();
        WriteFile
            .run(json!({"path": "new.txt", "content": "x"}), c.clone())
            .await
            .unwrap();
        ReadFile
            .run(json!({"path": "a.txt"}), c.clone())
            .await
            .unwrap();
        EditFile
            .run(
                json!({"path": "a.txt", "old": "one", "new": "two"}),
                c.clone(),
            )
            .await
            .unwrap();
        let seen = snaps.seen.lock().unwrap().clone();
        assert_eq!(seen[0], (dir.path().join("new.txt"), None));
        assert_eq!(seen[1], (dir.path().join("a.txt"), Some(b"one".to_vec())));
    }

    #[tokio::test]
    async fn a_patch_snapshots_every_file_it_changes() {
        let (dir, c, snaps) = setup(false);
        std::fs::write(dir.path().join("old.txt"), "bye\n").unwrap();
        ApplyPatch
            .run(
                json!({"patch": "*** Begin Patch\n*** Add File: n.txt\n+1\n*** Delete File: old.txt\n*** End Patch"}),
                c,
            )
            .await
            .unwrap();
        let seen = snaps.seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            [
                (dir.path().join("n.txt"), None),
                (dir.path().join("old.txt"), Some(b"bye\n".to_vec()))
            ]
        );
    }

    #[tokio::test]
    async fn no_file_is_written_when_its_snapshot_fails() {
        let (dir, c, _) = setup(true);
        let err = WriteFile
            .run(json!({"path": "a.txt", "content": "x"}), c.clone())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Failed(m) if m.contains("for undo")),
            "{err}"
        );
        assert!(!dir.path().join("a.txt").exists());
        let err = ApplyPatch
            .run(
                json!({"patch": "*** Begin Patch\n*** Add File: b.txt\n+1\n*** End Patch"}),
                c,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Failed(_)));
        assert!(!dir.path().join("b.txt").exists());
    }
}
