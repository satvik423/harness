//! [`SessionLock`]: one process runs a session at a time.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use harness_types::SessionId;
use nix::fcntl::{Flock, FlockArg};
use thiserror::Error;

/// Why a session lock was not taken.
#[derive(Debug, Error)]
pub enum LockError {
    /// Another process runs the session.
    #[error("session {0} is open in another harness process")]
    Held(SessionId),
    /// The lock file could not be created.
    #[error("cannot create the lock file {path}: {message}")]
    Io {
        /// The lock file.
        path: PathBuf,
        /// The I/O error.
        message: String,
    },
}

/// An exclusive advisory lock on `.harness/locks/<session>.lock`, released when
/// dropped (or when the process dies, so a crash never leaves a stale lock).
#[derive(Debug)]
pub struct SessionLock {
    _lock: Flock<File>,
    path: PathBuf,
}

impl SessionLock {
    /// Takes the lock without waiting.
    ///
    /// # Errors
    /// [`LockError::Held`] if another process holds it, [`LockError::Io`] if
    /// the lock file cannot be opened.
    pub fn acquire(locks_dir: &Path, session: SessionId) -> Result<Self, LockError> {
        let path = locks_dir.join(format!("{session}.lock"));
        let io = |e: std::io::Error| LockError::Io {
            path: path.clone(),
            message: e.to_string(),
        };
        std::fs::create_dir_all(locks_dir).map_err(io)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io)?;
        let mut lock = Flock::lock(file, FlockArg::LockExclusiveNonblock)
            .map_err(|_| LockError::Held(session))?;
        // For people inspecting the file; the lock itself is what counts.
        let _ = lock.set_len(0);
        let _ = writeln!(lock, "{}", std::process::id());
        Ok(Self { _lock: lock, path })
    }

    /// The lock file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_cannot_be_taken_again_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let id = SessionId::new();
        let first = SessionLock::acquire(dir.path(), id).unwrap();
        assert!(matches!(
            SessionLock::acquire(dir.path(), id),
            Err(LockError::Held(_))
        ));
        assert!(SessionLock::acquire(dir.path(), SessionId::new()).is_ok());
        drop(first);
        assert!(SessionLock::acquire(dir.path(), id).is_ok());
    }
}
