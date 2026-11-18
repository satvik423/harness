//! The [`History`] port: what the `recall` tool reads (FR-CTX-04, Phase 5).
//!
//! The runtime implements it over the event log and the session's anchors and
//! returns events already rendered as numbered lines, with tool outputs in
//! full (the capper's footer points here for trimmed outputs). The tool only
//! pages and formats.

use async_trait::async_trait;
use harness_types::Seq;
use thiserror::Error;

/// What to recall: exactly one selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecallSelector {
    /// An anchor's whole range (`a2`).
    Anchor(String),
    /// Exact events, both ends included.
    Range {
        /// First event.
        from: Seq,
        /// Last event.
        to: Seq,
    },
    /// A full-text search over earlier segments, best matches first.
    Query {
        /// The search words.
        query: String,
        /// At most this many matching events.
        limit: usize,
    },
}

/// Recalled events, rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recollection {
    /// What was recalled, for the header (`a2 · events 211-560`).
    pub label: String,
    /// The anchor, when recalled by anchor.
    pub anchor: Option<String>,
    /// Rendered lines, each starting with `#seq`.
    pub lines: Vec<String>,
}

/// Why nothing was recalled.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HistoryError {
    /// The selector names nothing (unknown anchor, empty range, no matches).
    #[error("{0}")]
    NotFound(String),
    /// No history here (a tool run outside a session).
    #[error("there is no session history here")]
    Unavailable,
    /// The log could not be read.
    #[error("cannot read the session history: {0}")]
    Store(String),
}

/// Reads the session's history for `recall`.
#[async_trait]
pub trait History: Send + Sync {
    /// Renders the events `selector` names.
    ///
    /// # Errors
    /// [`HistoryError::NotFound`] when it names nothing,
    /// [`HistoryError::Unavailable`] outside a session, [`HistoryError::Store`]
    /// when the log cannot be read.
    async fn recall(&self, selector: &RecallSelector) -> Result<Recollection, HistoryError>;

    /// Records that `tokens` of history were recalled into the transcript
    /// (`Event::Recalled`).
    async fn recalled(&self, anchor: Option<String>, tokens: u64);
}

/// A [`History`] with nothing in it (tests, tools run outside a session).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHistory;

#[async_trait]
impl History for NoHistory {
    async fn recall(&self, _: &RecallSelector) -> Result<Recollection, HistoryError> {
        Err(HistoryError::Unavailable)
    }

    async fn recalled(&self, _: Option<String>, _: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_history_has_nothing() {
        assert_eq!(
            NoHistory.recall(&RecallSelector::Anchor("a1".into())).await,
            Err(HistoryError::Unavailable)
        );
    }
}
