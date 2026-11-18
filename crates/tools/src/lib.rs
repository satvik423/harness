//! Built-in tools for the harness (Tech spec 3.5) and in-memory support stores.
//!
//! Tools:
//!
//! - [`ReadFile`] (`read_file`): numbered lines, paged, recorded in the read ledger
//! - [`WriteFile`] (`write_file`), [`EditFile`] (`edit_file`): change files that
//!   were read in this session and are unchanged since; atomic writes
//! - [`ApplyPatch`] (`apply_patch`): multi-file Codex-format patches, all-or-nothing
//!   (parser and planner in [`patch`])
//! - [`Glob`] (`glob`), [`Grep`] (`grep`): gitignore-aware search on the ripgrep libraries
//! - [`Todo`] (`todo`): the model's checklist; [`AskUser`] (`ask_user`): ask the user
//! - [`PtyStart`], [`PtyWrite`], [`PtyRead`], [`PtyKill`]: interactive programs on
//!   pseudo-terminals, owned by a [`PtyManager`]
//!
//! File contents are returned inside an `<untrusted>` wrapper
//! ([`harness_ports::wrap_untrusted`]): data, never instructions.
//! - [`Shell`] (`shell`): `/bin/sh -c` in its own process group, live output, timeout
//!
//! Support implementations of the ports: [`MemoryReadLedger`],
//! [`MemoryBlobStore`] and [`Unsandboxed`].
//!
//! # Example
//! ```
//! use harness_ports::ToolRegistry;
//! let registry = harness_tools::builtin_registry().unwrap();
//! let names: Vec<String> = registry.specs().into_iter().map(|s| s.name.to_string()).collect();
//! assert_eq!(
//!     names,
//!     [
//!         "apply_patch", "ask_user", "edit_file", "glob", "grep", "pty_kill", "pty_read",
//!         "pty_start", "pty_write", "read_file", "shell", "todo", "write_file"
//!     ]
//! );
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod ask_user;
mod edit_file;
mod fs_write;
mod glob;
mod grep;
pub mod patch;
mod pty;
mod read_file;
mod shell;
mod support;
mod todo;
mod write_file;

use std::sync::Arc;

use harness_ports::{DuplicateTool, ToolRegistry};

pub use ask_user::{AskUser, NO_USER_ANSWER};
pub use edit_file::EditFile;
pub use glob::{Glob, MAX_GLOB_RESULTS};
pub use grep::{Grep, MAX_GREP_LINES};
pub use patch::ApplyPatch;
pub use pty::{
    MAX_PTY_BUFFER, MAX_PTY_SESSIONS, MAX_PTY_WAIT_MS, PtyKill, PtyManager, PtyRead, PtyStart,
    PtyWrite, pty_tools,
};
pub use read_file::{MAX_LINE_CHARS, MAX_LINES, ReadFile};
pub use shell::{DEFAULT_TIMEOUT_S, MAX_KEPT_BYTES, MAX_TIMEOUT_S, Shell};
pub use support::{MemoryBlobStore, MemoryReadLedger, Unsandboxed, stamp_of};
pub use todo::Todo;
pub use write_file::WriteFile;

/// A registry holding every built-in tool.
///
/// # Errors
/// [`DuplicateTool`] only if two built-ins share a name (a bug).
pub fn builtin_registry() -> Result<ToolRegistry, DuplicateTool> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFile))?;
    registry.register(Arc::new(ApplyPatch))?;
    registry.register(Arc::new(WriteFile))?;
    registry.register(Arc::new(EditFile))?;
    registry.register(Arc::new(Glob))?;
    registry.register(Arc::new(Grep))?;
    registry.register(Arc::new(Shell))?;
    registry.register(Arc::new(Todo))?;
    registry.register(Arc::new(AskUser))?;
    for tool in pty_tools(&Arc::new(PtyManager::default())) {
        registry.register(tool)?;
    }
    Ok(registry)
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Builds a [`ToolCtx`](harness_ports::ToolCtx) rooted at a directory for tool tests.
    use std::path::Path;
    use std::sync::Arc;

    use harness_ports::ToolCtx;
    use harness_testkit::RecordingSink;
    use harness_types::{SessionId, TaskId};
    use tokio_util::sync::CancellationToken;

    use crate::{MemoryBlobStore, MemoryReadLedger, Unsandboxed};

    pub(crate) fn ctx(dir: &Path) -> (ToolCtx, Arc<RecordingSink>, Arc<MemoryReadLedger>) {
        let sink = Arc::new(RecordingSink::default());
        let ledger = Arc::new(MemoryReadLedger::default());
        let ctx = ToolCtx {
            cwd: dir.to_path_buf(),
            session: SessionId::new(),
            task: TaskId::new(),
            call_id: "call-1".into(),
            cancel: CancellationToken::new(),
            sandbox: Arc::new(Unsandboxed),
            blobs: Arc::new(MemoryBlobStore::default()),
            ledger: ledger.clone(),
            events: sink.clone(),
            writable_roots: vec![dir.to_path_buf()],
            user: Arc::new(harness_ports::NoUser),
            snapshots: Arc::new(harness_ports::NoSnapshots),
        };
        (ctx, sink, ledger)
    }

    /// Records every `before_write` call; fails them all when `fail` is set.
    #[derive(Default)]
    pub(crate) struct RecordedSnapshots {
        pub(crate) seen: std::sync::Mutex<Vec<(std::path::PathBuf, Option<Vec<u8>>)>>,
        pub(crate) fail: bool,
    }

    #[async_trait::async_trait]
    impl harness_ports::FileSnapshots for RecordedSnapshots {
        async fn before_write(
            &self,
            path: &Path,
            current: Option<&[u8]>,
        ) -> Result<(), harness_ports::StoreError> {
            if self.fail {
                return Err(harness_ports::StoreError::Backend("disk full".into()));
            }
            self.seen
                .lock()
                .unwrap()
                .push((path.to_path_buf(), current.map(<[u8]>::to_vec)));
            Ok(())
        }
    }
}
