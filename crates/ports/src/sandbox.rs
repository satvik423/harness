//! The [`Sandbox`] port: wraps process execution (implemented in Phase 13).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How much a sandboxed process may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// Read anything, write nothing.
    ReadOnly,
    /// Write inside the workspace (and temp); network off unless approved.
    WorkspaceWrite,
    /// No restrictions.
    FullAccess,
}

/// A process to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Program to execute.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
    /// Whether network access is allowed for this command.
    pub network: bool,
}

/// Errors from a sandbox backend.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SandboxError {
    /// The backend (for example `bwrap`) is not available on this machine.
    #[error("sandbox backend unavailable: {0}")]
    Unavailable(String),
    /// The command cannot run under the current mode.
    #[error("command not permitted in this sandbox: {0}")]
    NotPermitted(String),
}

/// Rewrites a command so it runs inside the sandbox.
pub trait Sandbox: Send + Sync {
    /// The active mode.
    fn mode(&self) -> SandboxMode;

    /// Returns the command to actually execute (for example wrapped in `bwrap`).
    ///
    /// # Errors
    /// Returns a [`SandboxError`] if the command cannot be sandboxed.
    fn prepare(&self, command: CommandSpec) -> Result<CommandSpec, SandboxError>;
}
