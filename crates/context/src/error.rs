//! [`ContextError`]: rejected transcript operations and instruction-loading failures.

use std::path::PathBuf;

use thiserror::Error;

/// Errors from the context engine. Transcript errors indicate a runtime bug:
/// they are raised instead of producing a request that breaks an invariant.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextError {
    /// A message was appended in an order the transcript does not allow.
    #[error("cannot append here: {0}")]
    OutOfOrder(String),
    /// A message with no content blocks.
    #[error("messages must have at least one content block")]
    Empty,
    /// Tool results did not answer exactly the pending tool calls (I2).
    #[error("tool results {got:?} do not match the pending tool calls {expected:?}")]
    ToolResultsMismatch {
        /// Ids of the pending calls.
        expected: Vec<String>,
        /// Ids the results answered.
        got: Vec<String>,
    },
    /// A project instruction file could not be read.
    #[error("cannot read {path}: {message}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The OS error.
        message: String,
    },
}
