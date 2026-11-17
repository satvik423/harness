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
//! Feed them the requests recorded by [`crate::FakeProvider::requests`]; an
//! empty result means the harness behaved.

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
