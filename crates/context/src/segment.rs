//! [`SegmentContext`]: the append-only transcript of one segment and the
//! request built from it (Tech spec 2.6 layout, invariants I1 and I2).

use std::collections::BTreeSet;

use harness_ports::{CachePlan, Effort, ModelRequest, SystemPart, SystemSlot, ToolSpec};
use harness_types::{ContentBlock, Message, Role};
use serde::{Deserialize, Serialize};

use crate::prompt::BASE_PROMPT;
use crate::{ActivePersona, ContextError, estimate_tokens};

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

/// Everything frozen when a segment opens: the request prefix and the opener.
///
/// Stored as a blob referenced by `SegmentOpened` (ADR-032), so a resumed
/// segment sends exactly the same prefix even if `AGENTS.md`, the tools or the
/// config changed since. Changing any of it mid-segment would break the prompt
/// cache and invalidate earlier thinking blocks (guardrails 02, 05, 06).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentFrame {
    /// Segment number.
    pub segment: u32,
    /// Persona name, for display.
    pub persona: String,
    /// Provider id (`anthropic`, `openai`): a resumed segment must use the same one.
    pub provider: String,
    /// Provider model id (no `provider/` prefix).
    pub model: String,
    /// System prompt parts in slot order.
    pub system: Vec<SystemPart>,
    /// Tool definitions sorted by name.
    pub tools: Vec<ToolSpec>,
    /// Maximum output tokens.
    pub max_output: u64,
    /// Reasoning effort.
    pub effort: Option<Effort>,
    /// Whether thinking summaries are streamed.
    pub show_thinking: bool,
    /// The opener text placed before the first user input.
    pub opener: String,
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
    frame: SegmentFrame,
    messages: Vec<Message>,
    /// System notes waiting to be sent with the next user message.
    notes: Vec<String>,
    /// The input tokens the provider reported for the last request, and how
    /// many messages that request held (P5-T1).
    measured: Option<(u64, usize)>,
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
        Self::from_frame(frame_of(
            spec.persona,
            system,
            tools,
            spec.environment,
            spec.segment,
        ))
    }

    /// Continues a segment from its stored frame, with an empty transcript;
    /// replay the stored messages with [`SegmentContext::replay_user`],
    /// [`SegmentContext::push_assistant`] and [`SegmentContext::push_tool_results`].
    #[must_use]
    pub fn from_frame(frame: SegmentFrame) -> Self {
        Self {
            frame,
            messages: Vec::new(),
            notes: Vec::new(),
            measured: None,
        }
    }

    /// Records the input tokens the provider reported for the request built
    /// from the current transcript (call it before appending the reply).
    pub fn record_input(&mut self, total_input: u64) {
        self.measured = Some((total_input, self.messages.len()));
    }

    /// Estimated input tokens of the next request if `extra` (new user input)
    /// were appended: the last measured input plus an estimate of everything
    /// appended since, or an estimate of the whole request when nothing was
    /// measured yet in this segment.
    #[must_use]
    pub fn estimated_input(&self, extra: &[ContentBlock]) -> u64 {
        let notes: u64 = self.notes.iter().map(|n| estimate_tokens(n)).sum();
        let extra: u64 = extra.iter().map(block_tokens).sum();
        let base = match self.measured {
            Some((total, mark)) => {
                total
                    + self
                        .messages
                        .get(mark..)
                        .unwrap_or_default()
                        .iter()
                        .map(message_tokens)
                        .sum::<u64>()
            }
            None => {
                let system: u64 = self
                    .frame
                    .system
                    .iter()
                    .map(|p| estimate_tokens(&p.text))
                    .sum();
                let tools: u64 = self
                    .frame
                    .tools
                    .iter()
                    .map(|t| {
                        estimate_tokens(&t.description)
                            + estimate_tokens(&t.input_schema.to_string())
                    })
                    .sum();
                let opener = if self.messages.is_empty() {
                    estimate_tokens(&self.frame.opener)
                } else {
                    0
                };
                system + tools + opener + self.messages.iter().map(message_tokens).sum::<u64>()
            }
        };
        base + notes + extra
    }

    /// The frozen part of the segment.
    #[must_use]
    pub fn frame(&self) -> &SegmentFrame {
        &self.frame
    }

    /// The segment number.
    #[must_use]
    pub fn segment(&self) -> u32 {
        self.frame.segment
    }

    /// Queues a system note (for example "these files were reverted"). It is
    /// sent at the start of the next user message, so nothing already sent changes.
    pub fn push_note(&mut self, text: impl Into<String>) {
        self.notes.push(text.into());
    }

    /// Notes queued for the next user message.
    #[must_use]
    pub fn pending_notes(&self) -> &[String] {
        &self.notes
    }

    /// Appends a stored user message exactly as it was sent (opener and notes
    /// included), when rebuilding a segment from the event log. Clears queued notes.
    ///
    /// # Errors
    /// The same ordering errors as [`SegmentContext::push_user`].
    pub fn replay_user(&mut self, message: Message) -> Result<(), ContextError> {
        if message.role != Role::User {
            return Err(ContextError::OutOfOrder(
                "only user messages can be replayed here".into(),
            ));
        }
        if message.content.is_empty() {
            return Err(ContextError::Empty);
        }
        self.check_user_turn()?;
        self.messages.push(message);
        self.notes.clear();
        Ok(())
    }

    fn check_user_turn(&self) -> Result<(), ContextError> {
        match self.messages.last() {
            Some(m) if m.role == Role::Assistant && !m.tool_call_ids().is_empty() => {
                Err(ContextError::OutOfOrder(
                    "the last assistant message has unanswered tool calls; append tool results first".into(),
                ))
            }
            _ => Ok(()),
        }
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
        // After a user message whose turn produced no reply (refusal, empty
        // reply), another user message is valid: providers merge them (ADR-024).
        self.check_user_turn()?;
        let opener = self
            .messages
            .is_empty()
            .then(|| ContentBlock::text(self.frame.opener.clone()));
        let notes = self
            .notes
            .drain(..)
            .map(|n| ContentBlock::text(format!("<system-note>\n{n}\n</system-note>")));
        let content = opener.into_iter().chain(notes).chain(content).collect();
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

    /// `true` if the transcript ends with an assistant reply that calls no
    /// tools: the turn is over and only new user input can follow.
    #[must_use]
    pub fn ends_with_reply(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|m| m.role == Role::Assistant && m.tool_call_ids().is_empty())
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
            model: self.frame.model.clone(),
            system: self.frame.system.clone(),
            tools: self.frame.tools.clone(),
            messages: self.messages.clone(),
            max_output: self.frame.max_output,
            effort: self.frame.effort,
            cache_plan: CachePlan::default(),
            output_schema: None,
            show_thinking: self.frame.show_thinking,
        }
    }
}

/// Estimated tokens of one message.
fn message_tokens(message: &Message) -> u64 {
    message.content.iter().map(block_tokens).sum()
}

/// Rough size of an image or document before it is ingested (Phase 11).
const ATTACHMENT_TOKENS: u64 = 1_500;

/// Estimated tokens of one content block.
fn block_tokens(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Text { text } => estimate_tokens(text),
        ContentBlock::Image { .. } | ContentBlock::Document { .. } => ATTACHMENT_TOKENS,
        ContentBlock::ToolCall { name, input, .. } => {
            estimate_tokens(name) + estimate_tokens(&input.to_string())
        }
        ContentBlock::ToolResult { content, .. } => content.iter().map(block_tokens).sum(),
        ContentBlock::Opaque { raw, .. } => estimate_tokens(&raw.to_string()),
    }
}

impl SegmentFrame {
    /// The frame of the segment that follows this one after a compaction: the
    /// same system prompt, tools and model settings, a new number and opener.
    #[must_use]
    pub fn successor(&self, segment: u32, opener: String) -> Self {
        Self {
            segment,
            opener,
            ..self.clone()
        }
    }
}

fn frame_of(
    persona: ActivePersona,
    system: Vec<SystemPart>,
    tools: Vec<ToolSpec>,
    opener: String,
    segment: u32,
) -> SegmentFrame {
    SegmentFrame {
        segment,
        provider: persona.model.provider().to_owned(),
        model: persona.model.model().to_owned(),
        persona: persona.name,
        system,
        tools,
        max_output: persona.max_output,
        effort: persona.effort,
        show_thinking: persona.show_thinking,
        opener,
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

    #[test]
    fn the_estimate_starts_from_the_last_measured_input_and_adds_what_came_after() {
        let mut s = segment();
        let fresh = s.estimated_input(&[ContentBlock::text("x".repeat(400))]);
        assert!(fresh > 100, "the whole request is estimated: {fresh}");
        s.push_user(vec![ContentBlock::text("hi")]).unwrap();
        s.record_input(5_000);
        assert_eq!(s.estimated_input(&[]), 5_000);
        s.push_assistant(Message::new(
            Role::Assistant,
            vec![ContentBlock::text("y".repeat(40))],
        ))
        .unwrap();
        s.push_note("n".repeat(8));
        assert_eq!(
            s.estimated_input(&[ContentBlock::text("z".repeat(4))]),
            5_000 + 10 + 2 + 1
        );
    }

    #[test]
    fn a_successor_frame_keeps_the_prefix_and_changes_number_and_opener() {
        let s = segment();
        let next = s
            .frame()
            .successor(1, "<environment>new</environment>".into());
        assert_eq!(next.segment, 1);
        assert_eq!(next.opener, "<environment>new</environment>");
        assert_eq!(next.system, s.frame().system);
        assert_eq!(next.tools, s.frame().tools);
        assert_eq!(next.model, s.frame().model);
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

    #[test]
    fn a_segment_rebuilt_from_its_frame_and_messages_gives_the_same_request() {
        let mut original = segment();
        original.push_user(vec![ContentBlock::text("hi")]).unwrap();
        original
            .push_assistant(Message::new(Role::Assistant, vec![call("c1")]))
            .unwrap();
        original.push_tool_results(vec![result("c1")]).unwrap();

        let json = serde_json::to_string(original.frame()).unwrap();
        let frame: SegmentFrame = serde_json::from_str(&json).unwrap();
        let mut rebuilt = SegmentContext::from_frame(frame);
        let messages = original.messages().to_vec();
        rebuilt.replay_user(messages[0].clone()).unwrap();
        rebuilt.push_assistant(messages[1].clone()).unwrap();
        rebuilt
            .push_tool_results(messages[2].content.clone())
            .unwrap();
        assert_eq!(rebuilt.request(), original.request());
    }

    #[test]
    fn replayed_user_messages_are_kept_verbatim_and_ordered() {
        let mut s = segment();
        let stored = Message::new(
            Role::User,
            vec![
                ContentBlock::text("<environment>env</environment>"),
                ContentBlock::text("x"),
            ],
        );
        s.replay_user(stored.clone()).unwrap();
        assert_eq!(s.messages()[0], stored, "no second opener is added");
        s.push_assistant(Message::new(Role::Assistant, vec![call("c1")]))
            .unwrap();
        assert!(s.replay_user(Message::user_text("too early")).is_err());
        assert!(
            s.replay_user(Message::new(Role::Assistant, vec![ContentBlock::text("a")]))
                .is_err()
        );
    }

    #[test]
    fn notes_go_into_the_next_user_message_after_the_opener() {
        let mut s = segment();
        s.push_note("Reverted a.txt");
        assert_eq!(s.pending_notes(), ["Reverted a.txt"]);
        s.push_user(vec![ContentBlock::text("go on")]).unwrap();
        let texts: Vec<String> = s.messages()[0]
            .content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(
            texts,
            [
                "<environment>env</environment>",
                "<system-note>\nReverted a.txt\n</system-note>",
                "go on"
            ]
        );
        assert!(s.pending_notes().is_empty());
        assert!(check_all(&[s.request()]).is_empty());
    }

    #[test]
    fn ends_with_reply_only_after_a_final_answer() {
        let mut s = segment();
        assert!(!s.ends_with_reply());
        s.push_user(vec![ContentBlock::text("hi")]).unwrap();
        assert!(!s.ends_with_reply());
        s.push_assistant(Message::new(Role::Assistant, vec![call("c1")]))
            .unwrap();
        assert!(!s.ends_with_reply());
        s.push_tool_results(vec![result("c1")]).unwrap();
        s.push_assistant(Message::new(
            Role::Assistant,
            vec![ContentBlock::text("done")],
        ))
        .unwrap();
        assert!(s.ends_with_reply());
    }
}
