//! Checkers for the transcript invariants of Tech spec 3.13 (P1-T9).
//!
//! - **I1, append-only:** within a segment, request *n + 1* has the same model,
//!   system prompt and tools as request *n*, and its messages start with all of
//!   request *n*'s messages, unchanged.
//! - **I2, tool pairing:** every assistant tool call is answered by exactly one
//!   tool result, all in the user message that immediately follows it.
//! - **No prefill:** a request never ends with an assistant message (current
//!   Claude models reject assistant prefill).
//!
//! - **I4:** compaction never starts while a tool call waits for its result.
//! - **I5:** every pinned fact is an exact substring of its source event.
//! - **I6:** a compaction's anchors cover all of its segment's events, in
//!   order, with no overlap.
//!
//! Feed the request checks the requests recorded by
//! [`crate::FakeProvider::requests`] ([`check_segments`] splits a session that
//! compacted into its segments), and [`check_events`] a session's logged
//! events; an empty result means the harness behaved.

use std::collections::BTreeSet;
use std::fmt;

use harness_ports::ModelRequest;
use harness_types::Role;

/// One broken invariant, with the index of the offending request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// I1: the model changed between requests of one segment.
    ModelChanged {
        /// Index of the later request.
        request: usize,
    },
    /// I1: the system prompt changed.
    SystemChanged {
        /// Index of the later request.
        request: usize,
    },
    /// I1: the tool list changed.
    ToolsChanged {
        /// Index of the later request.
        request: usize,
    },
    /// I1: the later request has fewer messages than the earlier one.
    MessagesShrunk {
        /// Index of the later request.
        request: usize,
    },
    /// I1: a message already sent was changed.
    MessageRewritten {
        /// Index of the later request.
        request: usize,
        /// Index of the changed message.
        message: usize,
    },
    /// I2: a tool call has no result in the following user message.
    UnansweredToolCall {
        /// Request index.
        request: usize,
        /// Index of the assistant message with the call.
        message: usize,
        /// The call id.
        call_id: String,
    },
    /// I2: a tool result does not answer a call in the preceding assistant message.
    ResultWithoutCall {
        /// Request index.
        request: usize,
        /// Index of the user message with the result.
        message: usize,
        /// The call id the result claims to answer.
        call_id: String,
    },
    /// The request ends with an assistant message (prefill).
    EndsWithAssistant {
        /// Request index.
        request: usize,
    },
    /// I4: compaction started while a tool call had no result.
    CompactedMidCall {
        /// Sequence number of the `CompactionStarted` event.
        seq: u64,
        /// The unanswered call.
        call_id: String,
    },
    /// I5: a pinned quote is not in its source event.
    PinNotVerbatim {
        /// The pin's id.
        pin: u32,
        /// Its claimed source event.
        seq: u64,
    },
    /// I6: the anchors leave a gap, overlap, or leave the segment.
    AnchorsBroken {
        /// The compacted segment.
        segment: u32,
    },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Checks I1 across consecutive requests of one segment.
#[must_use]
pub fn check_append_only(requests: &[ModelRequest]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (request, pair) in requests.windows(2).enumerate().map(|(i, w)| (i + 1, w)) {
        let (prev, cur) = (&pair[0], &pair[1]);
        if prev.model != cur.model {
            violations.push(Violation::ModelChanged { request });
        }
        if prev.system != cur.system {
            violations.push(Violation::SystemChanged { request });
        }
        if prev.tools != cur.tools {
            violations.push(Violation::ToolsChanged { request });
        }
        if cur.messages.len() < prev.messages.len() {
            violations.push(Violation::MessagesShrunk { request });
            continue;
        }
        for (message, (a, b)) in prev.messages.iter().zip(&cur.messages).enumerate() {
            if a != b {
                violations.push(Violation::MessageRewritten { request, message });
            }
        }
    }
    violations
}

/// Checks I2 and the no-prefill rule within one request.
#[must_use]
pub fn check_tool_pairing(request_index: usize, request: &ModelRequest) -> Vec<Violation> {
    let messages = &request.messages;
    let mut violations = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        let call_ids = m.tool_call_ids();
        if m.role == Role::Assistant && !call_ids.is_empty() {
            let answered: BTreeSet<&str> = messages
                .get(i + 1)
                .filter(|next| next.role == Role::User)
                .map(|next| next.tool_result_ids().into_iter().collect())
                .unwrap_or_default();
            for id in call_ids.into_iter().filter(|id| !answered.contains(id)) {
                violations.push(Violation::UnansweredToolCall {
                    request: request_index,
                    message: i,
                    call_id: id.to_owned(),
                });
            }
        }
        let result_ids = m.tool_result_ids();
        if !result_ids.is_empty() {
            let expected: BTreeSet<&str> = i
                .checked_sub(1)
                .and_then(|p| messages.get(p))
                .filter(|prev| prev.role == Role::Assistant)
                .map(|prev| prev.tool_call_ids().into_iter().collect())
                .unwrap_or_default();
            for id in result_ids.into_iter().filter(|id| !expected.contains(id)) {
                violations.push(Violation::ResultWithoutCall {
                    request: request_index,
                    message: i,
                    call_id: id.to_owned(),
                });
            }
        }
    }
    if messages.last().is_some_and(|m| m.role == Role::Assistant) {
        violations.push(Violation::EndsWithAssistant {
            request: request_index,
        });
    }
    violations
}

/// Runs every check over a sequence of requests from one segment.
#[must_use]
pub fn check_all(requests: &[ModelRequest]) -> Vec<Violation> {
    let mut v = check_append_only(requests);
    for (i, r) in requests.iter().enumerate() {
        v.extend(check_tool_pairing(i, r));
    }
    v
}

/// Runs [`check_all`] on each segment of a session that may have compacted.
///
/// Compaction requests (those with an output schema) are checked on their
/// own; agent requests are grouped into segments by their first message (the
/// segment opener). Request indexes in violations are global.
#[must_use]
pub fn check_segments(requests: &[ModelRequest]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut group: Vec<(usize, &ModelRequest)> = Vec::new();
    let flush = |group: &mut Vec<(usize, &ModelRequest)>, out: &mut Vec<Violation>| {
        let owned: Vec<ModelRequest> = group.iter().map(|(_, r)| (*r).clone()).collect();
        let offset = group.first().map_or(0, |(i, _)| *i);
        out.extend(check_all(&owned).into_iter().map(|v| shift(v, offset)));
        group.clear();
    };
    for (i, request) in requests.iter().enumerate() {
        if request.output_schema.is_some() {
            violations.extend(check_tool_pairing(i, request));
            continue;
        }
        let new_segment = group
            .last()
            .is_some_and(|(_, prev)| prev.messages.first() != request.messages.first());
        if new_segment {
            flush(&mut group, &mut violations);
        }
        group.push((i, request));
    }
    flush(&mut group, &mut violations);
    violations
}

fn shift(v: Violation, by: usize) -> Violation {
    match v {
        Violation::ModelChanged { request } => Violation::ModelChanged {
            request: request + by,
        },
        Violation::SystemChanged { request } => Violation::SystemChanged {
            request: request + by,
        },
        Violation::ToolsChanged { request } => Violation::ToolsChanged {
            request: request + by,
        },
        Violation::MessagesShrunk { request } => Violation::MessagesShrunk {
            request: request + by,
        },
        Violation::MessageRewritten { request, message } => Violation::MessageRewritten {
            request: request + by,
            message,
        },
        Violation::UnansweredToolCall {
            request,
            message,
            call_id,
        } => Violation::UnansweredToolCall {
            request: request + by,
            message,
            call_id,
        },
        Violation::ResultWithoutCall {
            request,
            message,
            call_id,
        } => Violation::ResultWithoutCall {
            request: request + by,
            message,
            call_id,
        },
        Violation::EndsWithAssistant { request } => Violation::EndsWithAssistant {
            request: request + by,
        },
        other => other,
    }
}

/// The text a pinned quote may be copied from, per event (an independent
/// reading of the log, not the runtime's renderer).
fn event_text(event: &harness_protocol::Event) -> Option<String> {
    use harness_protocol::Event;
    use harness_types::ContentBlock;
    let texts = |blocks: &[ContentBlock]| {
        blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    match event {
        Event::UserInput { message, .. } | Event::AssistantMessage { message, .. } => {
            Some(texts(&message.content))
        }
        Event::ToolFinished { sent, .. } => Some(texts(sent)),
        Event::SystemNote { text, .. } => Some(text.clone()),
        Event::ModeChanged { note, .. } => Some(note.clone()),
        _ => None,
    }
}

/// Checks I4, I5 and I6 over a session's logged events, in order.
#[must_use]
pub fn check_events(events: &[(harness_types::Seq, harness_protocol::Event)]) -> Vec<Violation> {
    use harness_protocol::Event;
    use harness_types::ContentBlock;
    let mut violations = Vec::new();
    let mut pending: BTreeSet<String> = BTreeSet::new();
    let texts: std::collections::BTreeMap<u64, String> = events
        .iter()
        .filter_map(|(seq, e)| event_text(e).map(|t| (seq.0, t)))
        .collect();
    for (seq, event) in events {
        match event {
            Event::AssistantMessage { message, .. } => {
                for block in &message.content {
                    if let ContentBlock::ToolCall { id, .. } = block {
                        pending.insert(id.clone());
                    }
                }
            }
            Event::ToolFinished { call_id, .. } => {
                pending.remove(call_id);
            }
            Event::CompactionStarted { .. } => {
                if let Some(call_id) = pending.iter().next() {
                    violations.push(Violation::CompactedMidCall {
                        seq: seq.0,
                        call_id: call_id.clone(),
                    });
                }
            }
            Event::Compacted { record, .. } => {
                for pin in record.pins.iter().filter(|p| p.superseded_by.is_none()) {
                    if !texts
                        .get(&pin.seq.0)
                        .is_some_and(|t| t.contains(&pin.quote))
                    {
                        violations.push(Violation::PinNotVerbatim {
                            pin: pin.id,
                            seq: pin.seq.0,
                        });
                    }
                }
                let mut next = record.from.0;
                let mut ok = true;
                for anchor in &record.anchors {
                    ok &= anchor.from.0 == next && anchor.to.0 >= anchor.from.0;
                    next = anchor.to.0 + 1;
                }
                if !ok || next != record.to.0 + 1 {
                    violations.push(Violation::AnchorsBroken {
                        segment: record.segment,
                    });
                }
            }
            _ => {}
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_ports::{CachePlan, SystemPart, SystemSlot};
    use harness_types::{ContentBlock, Message};
    use serde_json::json;

    fn req(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "m".into(),
            system: vec![SystemPart {
                slot: SystemSlot::Base,
                text: "base".into(),
            }],
            tools: vec![],
            messages,
            max_output: 100,
            effort: None,
            cache_plan: CachePlan::default(),
            output_schema: None,
            show_thinking: false,
        }
    }

    fn user(t: &str) -> Message {
        Message::user_text(t)
    }
    fn assistant(t: &str) -> Message {
        Message::new(Role::Assistant, vec![ContentBlock::text(t)])
    }
    fn calls(ids: &[&str]) -> Message {
        Message::new(
            Role::Assistant,
            ids.iter()
                .map(|id| ContentBlock::ToolCall {
                    id: (*id).into(),
                    name: "shell".into(),
                    input: json!({}),
                })
                .collect(),
        )
    }
    fn results(ids: &[&str]) -> Message {
        Message::new(
            Role::User,
            ids.iter()
                .map(|id| ContentBlock::ToolResult {
                    call_id: (*id).into(),
                    content: vec![],
                    is_error: false,
                })
                .collect(),
        )
    }

    #[test]
    fn growing_transcript_is_append_only() {
        let a = req(vec![user("hi")]);
        let b = req(vec![user("hi"), calls(&["c1"]), results(&["c1"])]);
        assert!(check_all(&[a, b]).is_empty());
    }

    #[test]
    fn identical_resend_is_allowed() {
        let a = req(vec![user("hi")]);
        assert!(check_append_only(&[a.clone(), a]).is_empty());
    }

    #[test]
    fn rewritten_message_is_detected() {
        let a = req(vec![user("hi"), assistant("one"), user("next")]);
        let b = req(vec![
            user("hi"),
            assistant("ONE"),
            user("next"),
            assistant("x"),
            user("y"),
        ]);
        assert_eq!(
            check_append_only(&[a, b]),
            vec![Violation::MessageRewritten {
                request: 1,
                message: 1
            }]
        );
    }

    #[test]
    fn shrunk_transcript_is_detected() {
        let a = req(vec![user("a"), assistant("b"), user("c")]);
        let b = req(vec![user("a")]);
        assert_eq!(
            check_append_only(&[a, b]),
            vec![Violation::MessagesShrunk { request: 1 }]
        );
    }

    #[test]
    fn system_tools_and_model_changes_are_detected() {
        let a = req(vec![user("a")]);
        let mut b = a.clone();
        b.system[0].text = "changed".into();
        b.model = "other".into();
        b.tools.push(harness_ports::ToolSpec {
            name: harness_ports::ToolName::new("t").unwrap(),
            description: String::new(),
            input_schema: json!({}),
            strict: false,
            eager_input_streaming: false,
        });
        let v = check_append_only(&[a, b]);
        assert!(v.contains(&Violation::SystemChanged { request: 1 }));
        assert!(v.contains(&Violation::ToolsChanged { request: 1 }));
        assert!(v.contains(&Violation::ModelChanged { request: 1 }));
    }

    #[test]
    fn unanswered_call_is_detected() {
        let r = req(vec![user("a"), calls(&["c1", "c2"]), results(&["c1"])]);
        assert_eq!(
            check_tool_pairing(0, &r),
            vec![Violation::UnansweredToolCall {
                request: 0,
                message: 1,
                call_id: "c2".into()
            }]
        );
    }

    #[test]
    fn results_split_across_messages_are_detected() {
        let r = req(vec![
            user("a"),
            calls(&["c1", "c2"]),
            results(&["c1"]),
            results(&["c2"]),
        ]);
        let v = check_tool_pairing(0, &r);
        assert!(v.contains(&Violation::UnansweredToolCall {
            request: 0,
            message: 1,
            call_id: "c2".into()
        }));
        assert!(v.contains(&Violation::ResultWithoutCall {
            request: 0,
            message: 3,
            call_id: "c2".into()
        }));
    }

    #[test]
    fn orphan_result_is_detected() {
        let r = req(vec![results(&["ghost"])]);
        assert_eq!(
            check_tool_pairing(0, &r),
            vec![Violation::ResultWithoutCall {
                request: 0,
                message: 0,
                call_id: "ghost".into()
            }]
        );
    }

    #[test]
    fn trailing_assistant_message_is_prefill() {
        let r = req(vec![user("a"), assistant("I think")]);
        assert_eq!(
            check_tool_pairing(0, &r),
            vec![Violation::EndsWithAssistant { request: 0 }]
        );
    }
}
