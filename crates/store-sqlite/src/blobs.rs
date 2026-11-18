//! Blob files: `.harness/blobs/ab/cdef…` (content-addressed by BLAKE3).

use std::path::{Path, PathBuf};

use harness_ports::StoreError;
use harness_types::BlobRef;

/// Where a blob's bytes live: the first two hex digits are the folder.
pub(crate) fn blob_path(dir: &Path, blob: &BlobRef) -> PathBuf {
    let hex = blob.to_hex();
    dir.join(&hex[..2]).join(&hex[2..])
}

/// The content address of `bytes`.
pub(crate) fn address(bytes: &[u8]) -> BlobRef {
    BlobRef::from_bytes(*blake3::hash(bytes).as_bytes())
}

/// Removes a blob file; `true` if it was there.
pub(crate) fn remove(dir: &Path, blob: &BlobRef) -> bool {
    std::fs::remove_file(blob_path(dir, blob)).is_ok()
}

fn io(e: std::io::Error) -> StoreError {
    StoreError::Backend(e.to_string())
}

/// Writes the blob file if it is not there yet (temp file + rename, so a crash
/// never leaves a partial blob under its final name).
pub(crate) fn write(dir: &Path, blob: &BlobRef, bytes: &[u8]) -> Result<(), StoreError> {
    let path = blob_path(dir, blob);
    if path.exists() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::Backend("bad blob path".into()))?;
    std::fs::create_dir_all(parent).map_err(io)?;
    let tmp = parent.join(format!(".{}.tmp-{}", blob.to_hex(), std::process::id()));
    std::fs::write(&tmp, bytes).map_err(io)?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(e)
    })
}

/// Reads a blob and checks that its bytes still match its address.
pub(crate) fn read(dir: &Path, blob: &BlobRef) -> Result<Vec<u8>, StoreError> {
    let bytes = match std::fs::read(blob_path(dir, blob)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StoreError::NotFound(format!("blob {blob}")));
        }
        Err(e) => return Err(io(e)),
    };
    if address(&bytes) != *blob {
        return Err(StoreError::Corrupt(format!(
            "blob {blob} does not match its contents"
        )));
    }
    Ok(bytes)
}
