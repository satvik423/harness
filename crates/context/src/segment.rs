//! [`SegmentContext`]: the append-only transcript of one segment and the
//! request built from it (Tech spec 2.6 layout, invariants I1 and I2).

use std::collections::BTreeSet;

use harness_ports::{CachePlan, ModelRequest, SystemPart, SystemSlot, ToolSpec};
use harness_types::{ContentBlock, Message, Role};

use crate::prompt::BASE_PROMPT;
use crate::{ActivePersona, ContextError};

/// Everything needed to open a segment.
#[derive(Debug, Clone)]
pub struct SegmentSpec {
    /// The persona (soul, model, effort, output limit).
    pub persona: ActivePersona,
    /// Tool definitions; sorted by name when the segment opens.
    pub tools: Vec<ToolSpec>,
    /// Rendered project instructions (slot 5), if any.
    pub instructions: Option<String>,
    /// Rendered environment block for the opener.
    pub environment: String,
    /// Segment number (0 for a new session).
    pub segment: u32,
}

/// One segment's frozen system prompt and tools plus its append-only transcript.
///
/// The only ways to change the transcript are the three `push_*` methods, and
/// each refuses an append that would break I1 (append-only) or I2 (every tool
/// call answered, all results in the next user message). The segment opener
/// (environment block) is the first content block of the first user message,
/// so the opener cache breakpoint lands on it.
#[derive(Debug, Clone)]
pub struct SegmentContext {
    segment: u32,
    model: String,
    persona: ActivePersona,
    system: Vec<SystemPart>,
    tools: Vec<ToolSpec>,
    opener: String,
    messages: Vec<Message>,
}

impl SegmentContext {
    /// Freezes the system prompt and tools and starts an empty transcript.
    #[must_use]
    pub fn open(spec: SegmentSpec) -> Self {
        let mut tools = spec.tools;
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        let mut system = vec![
            SystemPart {
                slot: SystemSlot::Base,
                text: BASE_PROMPT.to_owned(),
            },
            SystemPart {
                slot: SystemSlot::Soul,
                text: spec.persona.soul.clone(),
            },
        ];
        if let Some(text) = spec.instructions {
            system.push(SystemPart {
                slot: SystemSlot::Instructions,
                text,
            });
        }
        system.sort_by_key(|part| part.slot);
        Self {
            segment: spec.segment,
            model: spec.persona.model.model().to_owned(),
            persona: spec.persona,
            system,
            tools,
            opener: spec.environment,
            messages: Vec::new(),
        }
    }

    /// The segment number.
    #[must_use]
    pub fn segment(&self) -> u32 {
        self.segment
    }

    /// The transcript so far.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Appends user input. Allowed at the start, after an assistant reply with
    /// no tool calls, or after a user message whose turn produced no reply. The
    /// first message is prefixed with the opener.
    ///
    /// # Errors
    /// [`ContextError::Empty`] or [`ContextError::OutOfOrder`].
    pub fn push_user(&mut self, content: Vec<ContentBlock>) -> Result<(), ContextError> {
        if content.is_empty() {
            return Err(ContextError::Empty);
        }
        if content.iter().any(|b| {
            matches!(
                b,
                ContentBlock::ToolResult { .. } | ContentBlock::ToolCall { .. }
            )
        }) {
            return Err(ContextError::OutOfOrder(
                "tool calls and results cannot be sent as user input".into(),
            ));
        }
        let content = match self.messages.last() {
            None => std::iter::once(ContentBlock::text(self.opener.clone()))
                .chain(content)
                .collect(),
            Some(m) if m.role == Role::Assistant && m.tool_call_ids().is_empty() => content,
            Some(m) if m.role == Role::Assistant => {
                return Err(ContextError::OutOfOrder(
                    "the last assistant message has unanswered tool calls; append tool results first".into(),
                ));
            }
            // The previous turn produced no assistant message (refusal or empty
            // reply). Providers merge consecutive user turns, so this is valid.
            Some(_) => content,
        };
        self.messages.push(Message::new(Role::User, content));
        Ok(())
    }

    /// Appends an assistant reply; the previous message must be from the user.
    ///
    /// # Errors
    /// [`ContextError::Empty`] or [`ContextError::OutOfOrder`].
    pub fn push_assistant(&mut self, message: Message) -> Result<(), ContextError> {
        if message.role != Role::Assistant {
            return Err(ContextError::OutOfOrder(
                "only assistant messages can be appended here".into(),
            ));
        }
        if message.content.is_empty() {
            return Err(ContextError::Empty);
        }
        if self.messages.last().is_none_or(|m| m.role != Role::User) {
            return Err(ContextError::OutOfOrder(
                "an assistant reply must follow a user message".into(),
            ));
        }
        self.messages.push(message);
        Ok(())
    }

    /// Appends the results of the last assistant message's tool calls, all in one user message.
    ///
    /// # Errors
    /// [`ContextError::OutOfOrder`] if no calls are pending, or
    /// [`ContextError::ToolResultsMismatch`] unless the results answer exactly the pending calls.
    pub fn push_tool_results(&mut self, results: Vec<ContentBlock>) -> Result<(), ContextError> {
        let expected: Vec<String> = self
            .pending_tool_calls()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        if expected.is_empty() {
            return Err(ContextError::OutOfOrder(
                "no tool calls are waiting for results".into(),
            ));
        }
        let got: Vec<String> = results
            .iter()
            .map(|b| match b {
                ContentBlock::ToolResult { call_id, .. } => call_id.clone(),
                _ => String::new(), // a non-result block can never match a call id
            })
            .collect();
        let got_set: BTreeSet<&String> = got.iter().collect();
        let expected_set: BTreeSet<&String> = expected.iter().collect();
        if got_set.len() != got.len() || got_set != expected_set || got.iter().any(String::is_empty)
        {
            return Err(ContextError::ToolResultsMismatch { expected, got });
        }
        self.messages.push(Message::new(Role::User, results));
        Ok(())
    }

    /// Tool calls of the last assistant message that still need results, in order.
    #[must_use]
    pub fn pending_tool_calls(&self) -> Vec<(String, String, serde_json::Value)> {
        match self.messages.last() {
            Some(m) if m.role == Role::Assistant => m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall { id, name, input } => {
                        Some((id.clone(), name.clone(), input.clone()))
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The model request for the current transcript.
    #[must_use]
    pub fn request(&self) -> ModelRequest {
        ModelRequest {
            model: self.model.clone(),
            system: self.system.clone(),
            tools: self.tools.clone(),
            messages: self.messages.clone(),
            max_output: self.persona.max_output,
            effort: self.persona.effort,
            cache_plan: CachePlan::default(),
            output_schema: None,
            show_thinking: self.persona.show_thinking,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_ports::ToolName;
    use harness_testkit::invariants::check_all;
    use serde_json::json;

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(name).unwrap(),
            description: name.into(),
            input_schema: json!({"type": "object"}),
            strict: true,
            eager_input_streaming: false,
        }
    }

    fn segment() -> SegmentContext {
        SegmentContext::open(SegmentSpec {
            persona: crate::builtin_default(),
            tools: vec![tool("shell"), tool("read_file")],
            instructions: Some("use pnpm".into()),
            environment: "<environment>env</environment>".into(),
            segment: 0,
        })
    }

    fn call(id: &str) -> ContentBlock {
        ContentBlock::ToolCall {
            id: id.into(),
            name: "shell".into(),
            input: json!({"command": "ls"}),
        }
    }

    fn result(id: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            call_id: id.into(),
            content: vec![ContentBlock::text("ok")],
            is_error: false,
        }
    }

    #[test]
    fn system_parts_are_in_slot_order_and_tools_sorted() {
        let r = segment().request();
        let slots: Vec<SystemSlot> = r.system.iter().map(|p| p.slot).collect();
        assert_eq!(
            slots,
            [SystemSlot::Base, SystemSlot::Soul, SystemSlot::Instructions]
        );
        assert!(
            r.system[0]
                .text
                .starts_with("You are an AI agent running inside harness")
        );
        assert_eq!(r.system[2].text, "use pnpm");
        let names: Vec<String> = r.tools.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, ["read_file", "shell"]);
    }

    #[test]
    fn request_uses_persona_model_settings() {
        let r = segment().request();
        assert_eq!(r.model, "claude-opus-5-5");
        assert_eq!(r.max_output, 64_000);
        assert_eq!(r.effort, Some(harness_ports::Effort::High));
        assert_eq!(r.cache_plan, CachePlan::default());
    }

    #[test]
    fn first_user_message_starts_with_the_opener() {
        let mut s = segment();
        s.push_user(vec![ContentBlock::text("hi")]).unwrap();
        assert_eq!(
            s.messages()[0].content,
            vec![
                ContentBlock::text("<environment>env</environment>"),
                ContentBlock::text("hi")
            ]
        );
        s.push_assistant(Message::new(
            Role::Assistant,
            vec![ContentBlock::text("hello")],
        ))
        .unwrap();
        s.push_user(vec![ContentBlock::text("again")]).unwrap();
        assert_eq!(s.messages()[2].content, vec![ContentBlock::text("again")]);
    }

    #[test]
    fn requests_stay_append_only_across_a_tool_round() {
        let mut s = segment();
        let mut requests = Vec::new();
        s.push_user(vec![ContentBlock::text("run ls")]).unwrap();
        requests.push(s.request());
        s.push_assistant(Message::new(
            Role::Assistant,
            vec![ContentBlock::text("running"), call("c1"), call("c2")],
        ))
        .unwrap();
        s.push_tool_results(vec![result("c2"), result("c1")])
            .unwrap();
        requests.push(s.request());
        s.push_assistant(Message::new(
            Role::Assistant,
            vec![ContentBlock::text("done")],
        ))
        .unwrap();
        s.push_user(vec![ContentBlock::text("thanks")]).unwrap();
        requests.push(s.request());
        assert!(
            check_all(&requests).is_empty(),
            "{:?}",
            check_all(&requests)
        );
    }

    #[test]
    fn user_input_cannot_follow_unanswered_tool_calls() {
        let mut s = segment();
        s.push_user(vec![ContentBlock::text("go")]).unwrap();
        s.push_assistant(Message::new(Role::Assistant, vec![call("c1")]))
            .unwrap();
        assert!(matches!(
            s.push_user(vec![ContentBlock::text("x")]),
            Err(ContextError::OutOfOrder(_))
        ));
        assert_eq!(s.pending_tool_calls().len(), 1);
    }

    #[test]
    fn consecutive_user_messages_are_allowed_after_an_empty_reply() {
        // A refusal or empty reply leaves the user's message last; the next input
        // must still be accepted (the provider merges consecutive user turns).
        let mut s = segment();
        s.push_user(vec![ContentBlock::text("a")]).unwrap();
        s.push_user(vec![ContentBlock::text("b")]).unwrap();
        assert_eq!(s.messages().len(), 2);
        assert_eq!(s.messages()[1].content, vec![ContentBlock::text("b")]);
    }

    #[test]
    fn assistant_must_follow_a_user_message_and_have_content() {
        let mut s = segment();
        assert!(matches!(
            s.push_assistant(Message::new(Role::Assistant, vec![ContentBlock::text("x")])),
            Err(ContextError::OutOfOrder(_))
        ));
        s.push_user(vec![ContentBlock::text("a")]).unwrap();
        assert_eq!(
            s.push_assistant(Message::new(Role::Assistant, vec![])),
            Err(ContextError::Empty)
        );
        assert!(matches!(
            s.push_assistant(Message::user_text("not assistant")),
            Err(ContextError::OutOfOrder(_))
        ));
    }

    #[test]
    fn tool_results_must_answer_exactly_the_pending_calls() {
        let mut s = segment();
        s.push_user(vec![ContentBlock::text("go")]).unwrap();
        s.push_assistant(Message::new(Role::Assistant, vec![call("c1"), call("c2")]))
            .unwrap();
        assert!(matches!(
            s.push_tool_results(vec![result("c1")]),
            Err(ContextError::ToolResultsMismatch { .. })
        ));
        assert!(matches!(
            s.push_tool_results(vec![result("c1"), result("c2"), result("c3")]),
            Err(ContextError::ToolResultsMismatch { .. })
        ));
        assert!(matches!(
            s.push_tool_results(vec![result("c1"), result("c1")]),
            Err(ContextError::ToolResultsMismatch { .. })
        ));
        assert!(matches!(
            s.push_tool_results(vec![result("c1"), ContentBlock::text("x")]),
            Err(ContextError::ToolResultsMismatch { .. })
        ));
        s.push_tool_results(vec![result("c1"), result("c2")])
            .unwrap();
        assert!(s.pending_tool_calls().is_empty());
    }

    #[test]
    fn tool_results_without_pending_calls_are_out_of_order() {
        let mut s = segment();
        s.push_user(vec![ContentBlock::text("go")]).unwrap();
        assert!(matches!(
            s.push_tool_results(vec![result("c1")]),
            Err(ContextError::OutOfOrder(_))
        ));
    }

    #[test]
    fn no_instructions_means_no_instructions_slot() {
        let s = SegmentContext::open(SegmentSpec {
            persona: crate::builtin_default(),
            tools: vec![],
            instructions: None,
            environment: "e".into(),
            segment: 3,
        });
        assert_eq!(s.request().system.len(), 2);
        assert_eq!(s.segment(), 3);
    }
}
