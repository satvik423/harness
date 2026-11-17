//! Built-in tools for the harness (Tech spec 3.5) and in-memory support stores.
//!
//! Phase 1 tools:
//!
//! - [`ReadFile`] (`read_file`): numbered lines, paged, recorded in the read ledger
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
//! assert_eq!(names, ["read_file", "shell"]);
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod read_file;
mod shell;
mod support;

use std::sync::Arc;

use harness_ports::{DuplicateTool, ToolRegistry};

pub use read_file::{MAX_LINE_CHARS, MAX_LINES, ReadFile};
pub use shell::{DEFAULT_TIMEOUT_S, MAX_OUTPUT_CHARS, MAX_TIMEOUT_S, Shell};
pub use support::{MemoryBlobStore, MemoryReadLedger, Unsandboxed, stamp_of};

/// A registry holding every built-in tool.
///
/// # Errors
/// [`DuplicateTool`] only if two built-ins share a name (a bug).
pub fn builtin_registry() -> Result<ToolRegistry, DuplicateTool> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFile))?;
    registry.register(Arc::new(Shell))?;
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
        };
        (ctx, sink, ledger)
    }
}
