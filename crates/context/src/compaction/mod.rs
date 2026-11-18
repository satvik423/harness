//! Anchored compaction (Tech spec 3.13, ADR-004, Phase 5): turning a full
//! segment into a summary, verbatim pinned facts and descriptive anchors that
//! open the next segment.
//!
//! The pieces, in the order the runtime uses them:
//!
//! 1. [`render`] the segment's events as numbered text;
//! 2. [`compaction_request`] asks the model for a [`CompactionOutput`]
//!    (structured output, [`output_schema`]); [`parse_output`] reads the reply;
//! 3. [`validate`] checks quotes (I5) and anchor coverage (I6) and merges pins
//!    into a [`CompactionRecord`];
//! 4. [`render_opener`] writes the next segment's opener from the record.
//!
//! Everything here is pure; the runtime does the I/O.

mod opener;
mod render;
mod request;
mod schema;
mod validate;

pub use opener::{CompactionStyle, anchor_line, render_opener};
pub use render::{EventText, Rendered, render};
pub use request::{COMPACTION_MAX_OUTPUT, COMPACTION_PROMPT, compaction_request};
pub use schema::{
    AnchorProposal, CompactionOutput, DecisionNote, PinKind, PinProposal, Supersession,
    output_schema,
};
pub use validate::{
    AUTO_ANCHOR_TOKENS, Anchor, CompactionRecord, KeyDecision, PinnedFact, UNLABELLED, validate,
};

/// Parses the compaction model's reply text. A Markdown code fence around the
/// JSON is tolerated.
///
/// # Errors
/// A message naming what is wrong when the text is not a valid
/// [`CompactionOutput`].
///
/// # Examples
/// ```
/// use harness_context::compaction::parse_output;
/// assert!(parse_output("not json").is_err());
/// ```
pub fn parse_output(text: &str) -> Result<CompactionOutput, String> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|t| t.strip_suffix("```"))
        .unwrap_or(trimmed);
    serde_json::from_str(body.trim())
        .map_err(|e| format!("the compaction reply is not valid JSON for the schema: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_and_plain_replies_parse() {
        let json = r#"{"goal":"g","state_of_work":"s","decisions":[],"pinned":[],"superseded_pins":[],"files_touched":[],"open_questions":[],"next_steps":[],"anchors":[]}"#;
        assert!(parse_output(json).is_ok());
        assert!(parse_output(&format!("```json\n{json}\n```")).is_ok());
        assert!(
            parse_output(r#"{"goal": 1}"#)
                .unwrap_err()
                .contains("not valid JSON")
        );
    }
}
