//! What compactions left behind for the rest of the session (Phase 5): the
//! pinned facts and every anchor so far. Rebuilt from `Compacted` events on
//! resume; read by later compactions and by `recall`.

use harness_context::compaction::{Anchor, CompactionRecord, PinnedFact};

/// Context-engine settings of a session (`[context]` in the config).
#[derive(Debug, Clone, PartialEq)]
pub struct ContextSettings {
    /// Compact when input reaches this share of the model's window.
    pub compact_at_ratio: f64,
    /// Compact at this many input tokens at the latest.
    pub compact_at_tokens: u64,
    /// Model id (on the session's provider) for compaction; `None` uses the
    /// segment's model.
    pub compaction_model: Option<String>,
    /// What a compaction keeps (`SummaryOnly` is the eval baseline).
    pub style: harness_context::compaction::CompactionStyle,
}

impl Default for ContextSettings {
    fn default() -> Self {
        Self {
            compact_at_ratio: 0.7,
            compact_at_tokens: 250_000,
            compaction_model: None,
            style: harness_context::compaction::CompactionStyle::Anchored,
        }
    }
}

/// The session's pins and anchors.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SessionMemory {
    /// Every pin so far, active or superseded (the latest record's list).
    pub(crate) pins: Vec<PinnedFact>,
    /// Every anchor so far, oldest first.
    pub(crate) anchors: Vec<Anchor>,
}

impl SessionMemory {
    /// Takes in a compaction's record.
    pub(crate) fn apply(&mut self, record: &CompactionRecord) {
        self.pins.clone_from(&record.pins);
        self.anchors.extend(record.anchors.iter().cloned());
    }
}
