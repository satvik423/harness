//! [`FakeProvider`]: a [`Provider`] that replays scripted turns and records requests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures::StreamExt;
use harness_ports::{
    CacheStyle, Capabilities, ModelRequest, Provider, ProviderError, ProviderEvent, ProviderStream,
};
use harness_types::{ContentBlock, StopReason, Usage};
use tokio_util::sync::CancellationToken;

type Turn = Vec<Result<ProviderEvent, ProviderError>>;

/// A deterministic provider for tests.
///
/// Each call to [`Provider::stream`] pops the next scripted turn and records
/// the request, so tests can assert on exactly what the harness sent (for
/// example the append-only invariant, Tech spec 3.13 I1). When the script runs
/// out, `stream` returns [`ProviderError::Unsupported`].
///
/// # Examples
/// ```
/// use harness_testkit::FakeProvider;
/// let fake = FakeProvider::new().with_text_turn("Hello!");
/// assert_eq!(fake.remaining_turns(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct FakeProvider {
    script: Arc<Mutex<VecDeque<Turn>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    capabilities: Capabilities,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeProvider {
    /// Creates a fake with an empty script and generous default capabilities.
    #[must_use]
    pub fn new() -> Self {
        Self {
            script: Arc::default(),
            requests: Arc::default(),
            capabilities: Capabilities {
                context_window: 200_000,
                max_output: 32_000,
                vision: true,
                pdf: true,
                parallel_tools: true,
                mid_conversation_system: true,
                preserved_reasoning: true,
                cache: CacheStyle::Explicit { max_breakpoints: 4 },
                structured_output: true,
                native_compaction: false,
                native_context_editing: false,
            },
        }
    }

    /// Replaces the reported capabilities.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Appends a scripted turn made of arbitrary events and errors.
    #[must_use]
    pub fn with_turn(self, events: Vec<Result<ProviderEvent, ProviderError>>) -> Self {
        self.lock_script().push_back(events);
        self
    }

    /// Appends a turn that streams `text`, reports small usage, and ends the turn.
    #[must_use]
    pub fn with_text_turn(self, text: &str) -> Self {
        let usage = Usage {
            input: 10,
            output: estimate_tokens(text.chars().count()),
            ..Usage::default()
        };
        self.with_turn(vec![
            Ok(ProviderEvent::TextDelta {
                text: text.to_owned(),
            }),
            Ok(ProviderEvent::BlockComplete {
                block: ContentBlock::text(text),
            }),
            Ok(ProviderEvent::Usage { usage }),
            Ok(ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            }),
        ])
    }

    /// Appends a turn that requests one tool call with the given JSON input.
    #[must_use]
    pub fn with_tool_call_turn(self, id: &str, name: &str, input: &serde_json::Value) -> Self {
        self.with_tool_calls_turn(&[(id, name, input.clone())])
    }

    /// Appends a turn that requests several tool calls in one reply.
    #[must_use]
    pub fn with_tool_calls_turn(self, calls: &[(&str, &str, serde_json::Value)]) -> Self {
        let mut events: Turn = calls
            .iter()
            .flat_map(|(id, name, input)| tool_call_events(id, name, input))
            .collect();
        events.push(Ok(ProviderEvent::Usage {
            usage: Usage {
                input: 10,
                output: 5,
                ..Usage::default()
            },
        }));
        events.push(Ok(ProviderEvent::Stop {
            reason: StopReason::ToolUse,
        }));
        self.with_turn(events)
    }

    /// Number of scripted turns not yet consumed.
    #[must_use]
    pub fn remaining_turns(&self) -> usize {
        self.lock_script().len()
    }

    /// Drops the next scripted turn without serving it (for continuing a
    /// script in a second process after some turns were used).
    pub fn skip_turn(&mut self) {
        self.lock_script().pop_front();
    }

    /// Every request received so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn lock_script(&self) -> std::sync::MutexGuard<'_, VecDeque<Turn>> {
        // A panicking test may poison the lock; the data is still usable.
        self.script.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    async fn capabilities(&self, _model: &str) -> Result<Capabilities, ProviderError> {
        Ok(self.capabilities.clone())
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        let turn = self
            .lock_script()
            .pop_front()
            .ok_or_else(|| ProviderError::Unsupported("FakeProvider script exhausted".into()))?;
        // Check cancellation before every item, like a real network stream would.
        let stream = futures::stream::unfold(
            (turn.into_iter(), cancel, false),
            |(mut items, cancel, done)| async move {
                if done {
                    return None;
                }
                if cancel.is_cancelled() {
                    return Some((Err(ProviderError::Cancelled), (items, cancel, true)));
                }
                items.next().map(|item| (item, (items, cancel, false)))
            },
        );
        Ok(stream.boxed())
    }

    /// Deterministic estimate: one token per four characters of message text.
    async fn count_tokens(&self, request: &ModelRequest) -> Result<u64, ProviderError> {
        let chars: usize = request
            .messages
            .iter()
            .map(|m| m.text().chars().count())
            .sum();
        Ok(estimate_tokens(chars))
    }
}

/// Start, two input fragments, and the completed block for one tool call.
///
/// The JSON is split in two so consumers must accumulate input deltas, as with
/// real providers.
fn tool_call_events(id: &str, name: &str, input: &serde_json::Value) -> Turn {
    let json = input.to_string();
    let mid = json
        .char_indices()
        .nth(json.chars().count() / 2)
        .map_or(json.len(), |(i, _)| i);
    let (first, second) = json.split_at(mid);
    vec![
        Ok(ProviderEvent::ToolCallStart {
            id: id.to_owned(),
            name: name.to_owned(),
        }),
        Ok(ProviderEvent::ToolCallInputDelta {
            id: id.to_owned(),
            json: first.to_owned(),
        }),
        Ok(ProviderEvent::ToolCallInputDelta {
            id: id.to_owned(),
            json: second.to_owned(),
        }),
        Ok(ProviderEvent::BlockComplete {
            block: ContentBlock::ToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                input: input.clone(),
            },
        }),
    ]
}

/// One token per four characters, rounded up.
fn estimate_tokens(chars: usize) -> u64 {
    u64::try_from(chars).unwrap_or(u64::MAX).div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use harness_types::Message;

    fn request(text: &str) -> ModelRequest {
        ModelRequest {
            model: "fake-1".into(),
            system: vec![],
            tools: vec![],
            messages: vec![Message::user_text(text)],
            max_output: 1000,
            effort: None,
            cache_plan: harness_ports::CachePlan::default(),
            output_schema: None,
            show_thinking: false,
        }
    }

    async fn collect(stream: ProviderStream) -> Vec<Result<ProviderEvent, ProviderError>> {
        stream.collect().await
    }

    #[tokio::test]
    async fn text_turn_streams_text_usage_and_end_turn() {
        let fake = FakeProvider::new().with_text_turn("Hi there");
        let events = collect(
            fake.stream(request("hello"), CancellationToken::new())
                .await
                .unwrap(),
        )
        .await;
        let events: Vec<ProviderEvent> = events.into_iter().map(Result::unwrap).collect();
        assert_eq!(
            events.first(),
            Some(&ProviderEvent::TextDelta {
                text: "Hi there".into()
            })
        );
        assert!(events.contains(&ProviderEvent::BlockComplete {
            block: ContentBlock::text("Hi there")
        }));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ProviderEvent::Usage { .. }))
        );
        assert_eq!(
            events.last(),
            Some(&ProviderEvent::Stop {
                reason: StopReason::EndTurn
            })
        );
    }

    #[tokio::test]
    async fn tool_call_turn_ends_with_tool_use() {
        let input = serde_json::json!({"command": "ls"});
        let fake = FakeProvider::new().with_tool_call_turn("c1", "shell", &input);
        let events: Vec<ProviderEvent> = collect(
            fake.stream(request("x"), CancellationToken::new())
                .await
                .unwrap(),
        )
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
        assert_eq!(
            events[0],
            ProviderEvent::ToolCallStart {
                id: "c1".into(),
                name: "shell".into()
            }
        );
        let json: String = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::ToolCallInputDelta { json, .. } => Some(json.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            input
        );
        assert!(events.contains(&ProviderEvent::BlockComplete {
            block: ContentBlock::ToolCall {
                id: "c1".into(),
                name: "shell".into(),
                input: input.clone()
            }
        }));
        assert_eq!(
            events.last(),
            Some(&ProviderEvent::Stop {
                reason: StopReason::ToolUse
            })
        );
    }

    #[tokio::test]
    async fn multi_call_turn_completes_each_call_in_order() {
        let fake = FakeProvider::new().with_tool_calls_turn(&[
            ("a", "read_file", serde_json::json!({"path": "x"})),
            ("b", "shell", serde_json::json!({"command": "ls"})),
        ]);
        let ids: Vec<String> = collect(
            fake.stream(request("x"), CancellationToken::new())
                .await
                .unwrap(),
        )
        .await
        .into_iter()
        .filter_map(|e| match e.unwrap() {
            ProviderEvent::BlockComplete {
                block: ContentBlock::ToolCall { id, .. },
            } => Some(id),
            _ => None,
        })
        .collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[tokio::test]
    async fn requests_are_recorded_in_order() {
        let fake = FakeProvider::new().with_text_turn("a").with_text_turn("b");
        let _ = fake
            .stream(request("first"), CancellationToken::new())
            .await
            .unwrap();
        let _ = fake
            .stream(request("second"), CancellationToken::new())
            .await
            .unwrap();
        let texts: Vec<String> = fake
            .requests()
            .iter()
            .map(|r| r.messages[0].text())
            .collect();
        assert_eq!(texts, ["first", "second"]);
        assert_eq!(fake.remaining_turns(), 0);
    }

    #[tokio::test]
    async fn exhausted_script_is_an_error() {
        let fake = FakeProvider::new();
        assert!(matches!(
            fake.stream(request("x"), CancellationToken::new()).await,
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn cancelled_stream_yields_cancelled_and_stops() {
        let fake = FakeProvider::new().with_text_turn("never seen");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let events = collect(fake.stream(request("x"), cancel).await.unwrap()).await;
        assert_eq!(events, vec![Err(ProviderError::Cancelled)]);
    }

    #[tokio::test]
    async fn scripted_errors_are_passed_through() {
        let fake = FakeProvider::new().with_turn(vec![Err(ProviderError::Timeout)]);
        let events = collect(
            fake.stream(request("x"), CancellationToken::new())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(events, vec![Err(ProviderError::Timeout)]);
    }

    #[tokio::test]
    async fn count_tokens_is_chars_over_four_rounded_up() {
        let fake = FakeProvider::new();
        assert_eq!(fake.count_tokens(&request("12345678")).await.unwrap(), 2);
        assert_eq!(fake.count_tokens(&request("123456789")).await.unwrap(), 3);
    }
}
