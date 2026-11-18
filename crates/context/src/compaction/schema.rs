//! What the compaction model returns: [`CompactionOutput`] and its JSON schema
//! (Tech spec 3.13).

pub use harness_protocol::PinKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A decision and the event it was made in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionNote {
    /// The decision, in the model's words.
    pub text: String,
    /// The event it was made in.
    pub seq: u64,
}

/// A fact the model proposes to keep verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinProposal {
    /// Exact text copied from the event (checked: I5).
    pub quote: String,
    /// The event it was copied from.
    pub seq: u64,
    /// What kind of fact it is.
    pub kind: PinKind,
}

/// An earlier pin that a later event replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supersession {
    /// The earlier pin's id.
    pub pin_id: u32,
    /// The event that replaced it.
    pub by_seq: u64,
}

/// An event range the model proposes as an anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorProposal {
    /// First event of the range.
    pub from_seq: u64,
    /// Last event of the range.
    pub to_seq: u64,
    /// What happened in the range.
    pub topic: String,
    /// The kinds of exact detail the range holds ("exact error text", …).
    pub has: Vec<String>,
}

/// The structured summary of one segment, exactly as the compaction model
/// returned it (before validation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionOutput {
    /// What the user is trying to achieve.
    pub goal: String,
    /// Where the work stands.
    pub state_of_work: String,
    /// Decisions made, with their events.
    pub decisions: Vec<DecisionNote>,
    /// Facts to keep verbatim.
    pub pinned: Vec<PinProposal>,
    /// Earlier pins that no longer hold.
    pub superseded_pins: Vec<Supersession>,
    /// Files created or changed.
    pub files_touched: Vec<String>,
    /// Questions still open.
    pub open_questions: Vec<String>,
    /// What to do next.
    pub next_steps: Vec<String>,
    /// Event ranges with what they hold.
    pub anchors: Vec<AnchorProposal>,
}

/// The JSON schema for [`CompactionOutput`], in the subset both providers'
/// strict structured output accept: every property required, no extra
/// properties, no numeric or length limits.
///
/// # Examples
/// ```
/// let schema = harness_context::compaction::output_schema();
/// assert_eq!(schema["additionalProperties"], false);
/// ```
#[must_use]
pub fn output_schema() -> Value {
    fn object(properties: Value) -> Value {
        let required: Vec<String> = properties
            .as_object()
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default();
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        })
    }
    let strings = json!({"type": "array", "items": {"type": "string"}});
    object(json!({
        "goal": {"type": "string"},
        "state_of_work": {"type": "string"},
        "decisions": {"type": "array", "items": object(json!({
            "text": {"type": "string"},
            "seq": {"type": "integer"}
        }))},
        "pinned": {"type": "array", "items": object(json!({
            "quote": {"type": "string"},
            "seq": {"type": "integer"},
            "kind": {"type": "string", "enum": ["requirement", "decision", "constraint", "id"]}
        }))},
        "superseded_pins": {"type": "array", "items": object(json!({
            "pin_id": {"type": "integer"},
            "by_seq": {"type": "integer"}
        }))},
        "files_touched": strings,
        "open_questions": strings,
        "next_steps": strings,
        "anchors": {"type": "array", "items": object(json!({
            "from_seq": {"type": "integer"},
            "to_seq": {"type": "integer"},
            "topic": {"type": "string"},
            "has": strings
        }))}
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(v: &Value, check: &mut dyn FnMut(&Value)) {
        check(v);
        match v {
            Value::Object(m) => m.values().for_each(|x| walk(x, check)),
            Value::Array(a) => a.iter().for_each(|x| walk(x, check)),
            _ => {}
        }
    }

    #[test]
    fn the_schema_fits_strict_structured_output() {
        let schema = output_schema();
        walk(&schema, &mut |v| {
            if v.get("type") == Some(&json!("object")) {
                assert_eq!(v["additionalProperties"], false, "{v}");
                let props: Vec<&String> = v["properties"].as_object().unwrap().keys().collect();
                let required: Vec<&str> = v["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r.as_str().unwrap())
                    .collect();
                assert_eq!(props.len(), required.len(), "{v}");
            }
            for banned in ["minimum", "maximum", "minLength", "maxLength", "multipleOf"] {
                assert!(v.get(banned).is_none(), "{banned} in {v}");
            }
        });
    }

    #[test]
    fn a_reply_matching_the_schema_parses() {
        let reply = json!({
            "goal": "migrate users", "state_of_work": "half done",
            "decisions": [{"text": "keep soft deletes", "seq": 12}],
            "pinned": [{"quote": "must support Postgres 14", "seq": 3, "kind": "requirement"}],
            "superseded_pins": [], "files_touched": ["users.sql"],
            "open_questions": [], "next_steps": ["run tests"],
            "anchors": [{"from_seq": 1, "to_seq": 20, "topic": "setup", "has": ["exact psql error"]}]
        });
        let out: CompactionOutput = serde_json::from_value(reply).unwrap();
        assert_eq!(out.pinned[0].kind, PinKind::Requirement);
    }
}
