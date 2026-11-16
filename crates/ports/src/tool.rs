//! The [`Tool`] port: something the model can call.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use harness_types::{ContentBlock, Risk, SessionId, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{BlobStore, EventSink, ReadLedger, Sandbox};

/// Error returned when a [`ToolName`] is invalid.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid tool name {0:?}: use 1-64 characters from A-Z, a-z, 0-9, '_' and '-'")]
pub struct InvalidToolName(pub String);

/// A validated tool name: 1–64 characters from `[A-Za-z0-9_-]`.
///
/// Both major providers accept this alphabet; MCP tools are namespaced as
/// `mcp__server__tool` within it.
///
/// # Examples
/// ```
/// use harness_ports::ToolName;
/// assert!(ToolName::new("mcp__github__get_issue").is_ok());
/// assert!(ToolName::new("bad name").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ToolName(String);

impl ToolName {
    /// Validates and wraps a tool name.
    ///
    /// # Errors
    /// Returns [`InvalidToolName`] if the name is empty, longer than 64
    /// characters, or contains a character outside `[A-Za-z0-9_-]`.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidToolName> {
        let name = name.into();
        let valid = (1..=64).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if valid {
            Ok(Self(name))
        } else {
            Err(InvalidToolName(name))
        }
    }

    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ToolName {
    type Error = InvalidToolName;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ToolName> for String {
    fn from(value: ToolName) -> Self {
        value.0
    }
}

/// A tool definition as sent to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name.
    pub name: ToolName,
    /// What the tool does and when to use it (shown to the model).
    pub description: String,
    /// JSON Schema of the tool input.
    pub input_schema: Value,
    /// Ask the provider to guarantee schema-valid input (small-input tools).
    pub strict: bool,
    /// Stream large inputs as they are generated; the harness validates them
    /// itself before running (`write_file`, `apply_patch`).
    pub eager_input_streaming: bool,
}

/// Context for computing a tool call's [`Risk`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskCtx {
    /// Working directory of the task.
    pub cwd: PathBuf,
    /// Roots the task may write to.
    pub writable_roots: Vec<PathBuf>,
}

/// Everything a running tool may use. Built by the runtime for each call.
#[derive(Clone)]
pub struct ToolCtx {
    /// Working directory of the task.
    pub cwd: PathBuf,
    /// The session.
    pub session: SessionId,
    /// The task.
    pub task: TaskId,
    /// Cancelled when the call is interrupted; tools must stop promptly.
    pub cancel: CancellationToken,
    /// Wraps process execution (Phase 13; a pass-through until then).
    pub sandbox: Arc<dyn Sandbox>,
    /// Content-addressed storage for large outputs and attachments.
    pub blobs: Arc<dyn BlobStore>,
    /// Files read in this session, for the read-before-edit guard.
    pub ledger: Arc<dyn ReadLedger>,
    /// Emits live output events (for example [`harness_protocol::Event::ToolOutput`]).
    pub events: Arc<dyn EventSink>,
    /// Roots the task may write to.
    pub writable_roots: Vec<PathBuf>,
}

impl fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolCtx")
            .field("cwd", &self.cwd)
            .field("session", &self.session)
            .field("task", &self.task)
            .field("writable_roots", &self.writable_roots)
            .finish_non_exhaustive()
    }
}

/// What a tool returns on success.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolOutput {
    /// Result content for the model (capped by the runtime when recorded).
    pub content: Vec<ContentBlock>,
    /// One-line summary for the UI.
    pub summary: String,
    /// Files the tool changed (for snapshots and `/undo`).
    pub touched: Vec<PathBuf>,
}

/// Why a tool call failed. The model sees the message as an error result.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolError {
    /// The input did not match the schema or made no sense.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The tool refused (for example the read-before-edit guard).
    #[error("refused: {0}")]
    Refused(String),
    /// The tool ran and failed.
    #[error("failed: {0}")]
    Failed(String),
    /// The tool exceeded its time limit.
    #[error("timed out after {0} s")]
    Timeout(u64),
    /// The call was interrupted.
    #[error("interrupted by user")]
    Cancelled,
}

/// Something the model can call: built-in, MCP-backed or skill-provided.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The definition sent to the model.
    fn spec(&self) -> ToolSpec;

    /// What this call would touch. Policy decides allow, ask or deny from it.
    fn risk(&self, input: &Value, ctx: &RiskCtx) -> Risk;

    /// `true` only for pure reads that may run in parallel with other calls.
    fn parallel_safe(&self) -> bool {
        false
    }

    /// Per-tool output cap in tokens, overriding the global cap.
    fn output_cap_tokens(&self) -> Option<u64> {
        None
    }

    /// Runs the call.
    ///
    /// # Errors
    /// Returns a [`ToolError`]; the runtime turns it into an error tool result.
    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_and_namespaced_names() {
        for n in ["read_file", "apply-patch", "mcp__github__get_issue", "A1"] {
            assert_eq!(ToolName::new(n).unwrap().as_str(), n);
        }
    }

    #[test]
    fn accepts_exactly_64_characters() {
        assert!(ToolName::new("a".repeat(64)).is_ok());
    }

    #[test]
    fn rejects_empty_too_long_and_bad_characters() {
        for n in [
            String::new(),
            "a".repeat(65),
            "has space".into(),
            "dot.name".into(),
            "ünïcode".into(),
        ] {
            assert_eq!(ToolName::new(n.clone()), Err(InvalidToolName(n)));
        }
    }

    #[test]
    fn deserialisation_validates_the_name() {
        assert!(serde_json::from_str::<ToolName>("\"ok_name\"").is_ok());
        assert!(serde_json::from_str::<ToolName>("\"bad name\"").is_err());
    }

    #[test]
    fn tool_name_displays_and_converts_to_string() {
        let n = ToolName::new("grep").unwrap();
        assert_eq!(n.to_string(), "grep");
        assert_eq!(String::from(n), "grep");
        assert!(ToolName::try_from("x y".to_owned()).is_err());
    }

    #[test]
    fn tool_errors_render_readable_messages() {
        assert_eq!(ToolError::Timeout(120).to_string(), "timed out after 120 s");
        assert_eq!(ToolError::Cancelled.to_string(), "interrupted by user");
    }
}
