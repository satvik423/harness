//! What a compaction produced (Phase 5, ADR-045): the validated record carried
//! by [`crate::Event::Compacted`]. `harness_context::compaction` builds it.

use harness_types::Seq;
use serde::{Deserialize, Serialize};

/// What kind of fact a pin is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinKind {
    /// Something the result must do or have.
    Requirement,
    /// A choice that was made.
    Decision,
    /// A limit the work must respect.
    Constraint,
    /// An identifier, name, number or path that must be exact.
    Id,
}

impl PinKind {
    /// The kind's name as written in openers.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Requirement => "requirement",
            Self::Decision => "decision",
            Self::Constraint => "constraint",
            Self::Id => "id",
        }
    }
}

/// A fact kept verbatim in every later opener.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedFact {
    /// Session-wide pin number (`p1`, `p2`, …).
    pub id: u32,
    /// The exact text, copied from `seq`.
    pub quote: String,
    /// The event the quote comes from.
    pub seq: Seq,
    /// What kind of fact it is.
    pub kind: PinKind,
    /// The event that replaced it, if it no longer holds.
    pub superseded_by: Option<Seq>,
}

impl PinnedFact {
    /// Whether the fact still holds.
    #[must_use]
    pub fn active(&self) -> bool {
        self.superseded_by.is_none()
    }
}

/// A described event range that `recall` can fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    /// Session-wide anchor id (`a1`, `a2`, …).
    pub id: String,
    /// The segment the range belongs to.
    pub segment: u32,
    /// First event.
    pub from: Seq,
    /// Last event.
    pub to: Seq,
    /// What happened in the range.
    pub topic: String,
    /// The kinds of exact detail it holds.
    pub has: Vec<String>,
    /// Estimated tokens of the rendered range.
    pub tokens: u64,
}

/// A decision and the event it was made in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyDecision {
    /// The decision.
    pub text: String,
    /// Where it was made, if the event is in the segment.
    pub seq: Option<Seq>,
}

/// A validated compaction of one segment, carried by `Event::Compacted`
/// (ADR-045): the source of the next segment's opener and of `recall` anchors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionRecord {
    /// The segment that was compacted.
    pub segment: u32,
    /// First and last events of that segment.
    pub from: Seq,
    /// Last event of that segment.
    pub to: Seq,
    /// What the user is trying to achieve.
    pub goal: String,
    /// Where the work stands.
    pub state_of_work: String,
    /// Decisions made.
    pub decisions: Vec<KeyDecision>,
    /// Every pin of the session so far, active or superseded.
    pub pins: Vec<PinnedFact>,
    /// Files created or changed.
    pub files_touched: Vec<String>,
    /// Questions still open.
    pub open_questions: Vec<String>,
    /// What to do next.
    pub next_steps: Vec<String>,
    /// This segment's anchors, in order, covering `from..=to`.
    pub anchors: Vec<Anchor>,
}
