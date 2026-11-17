//! [`StreamDecoder`]: turns Messages API server-sent events into [`ProviderEvent`]s.

use std::collections::HashMap;

use harness_ports::{ProviderError, ProviderEvent};
use harness_types::{ContentBlock, StopReason, Usage};
use serde_json::{Value, json};

use crate::http::error_from_sse;
use crate::request::PROVIDER_ID;

/// A content block being assembled from deltas.
#[derive(Debug)]
enum Partial {
    Text(String),
    Tool {
        id: String,
        name: String,
        json: String,
        initial: Value,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    /// Any other block type, kept exactly as received at `content_block_start`.
    Raw(Value),
}

/// Stateful decoder for one streamed reply.
///
/// Emits deltas for live display and one [`ProviderEvent::BlockComplete`] per
/// finished content block, in order. Thinking blocks (and unknown block types)
/// become [`ContentBlock::Opaque`] so they can be replayed verbatim.
///
/// # Examples
/// ```
/// use harness_provider_anthropic::StreamDecoder;
/// let mut d = StreamDecoder::new();
/// let events = d.feed("ping", r#"{"type":"ping"}"#).unwrap();
/// assert!(events.is_empty());
/// ```
#[derive(Debug, Default)]
pub struct StreamDecoder {
    model: String,
    blocks: HashMap<u64, Partial>,
    usage: Usage,
    stop: Option<StopReason>,
    done: bool,
}

impl StreamDecoder {
    /// A decoder for a new reply.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` once `message_stop` has been seen.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Processes one server-sent event.
    ///
    /// # Errors
    /// [`ProviderError::Decode`] for malformed data or invalid tool-call JSON,
    /// or the mapped error for an `error` event.
    pub fn feed(&mut self, event: &str, data: &str) -> Result<Vec<ProviderEvent>, ProviderError> {
        let data: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::Decode(format!("bad {event} event: {e}")))?;
        let kind = if event.is_empty() {
            data["type"].as_str().unwrap_or_default()
        } else {
            event
        };
        let mut out = Vec::new();
        match kind {
            "message_start" => self.message_start(&data, &mut out),
            "content_block_start" => self.block_start(&data, &mut out),
            "content_block_delta" => self.block_delta(&data, &mut out),
            "content_block_stop" => self.block_stop(&data, &mut out)?,
            "message_delta" => self.message_delta(&data),
            "message_stop" => {
                self.done = true;
                out.push(ProviderEvent::Usage { usage: self.usage });
                let reason = self.stop.clone().unwrap_or_else(|| StopReason::Other {
                    raw: "missing stop_reason".into(),
                });
                out.push(ProviderEvent::Stop { reason });
            }
            "error" => {
                let error = &data["error"];
                return Err(error_from_sse(
                    error["type"].as_str().unwrap_or_default(),
                    error["message"].as_str().unwrap_or("unknown error"),
                ));
            }
            // `ping` and event types added in future API versions.
            _ => {}
        }
        Ok(out)
    }

    fn message_start(&mut self, data: &Value, out: &mut Vec<ProviderEvent>) {
        let message = &data["message"];
        self.model = message["model"].as_str().unwrap_or_default().to_owned();
        self.read_usage(&message["usage"]);
        // Each entry means the API changed our input (for example dropped a
        // thinking block). With an append-only transcript that signals a bug.
        for t in message["input_transformations"]
            .as_array()
            .into_iter()
            .flatten()
        {
            out.push(ProviderEvent::Notice {
                message: format!(
                    "provider transformed the input: {} ({}) at {}",
                    t["type"].as_str().unwrap_or("?"),
                    t["reason"].as_str().unwrap_or("?"),
                    t["path"].as_str().unwrap_or("?"),
                ),
            });
        }
    }

    fn read_usage(&mut self, usage: &Value) {
        let field = |name: &str| usage[name].as_u64();
        if let Some(v) = field("input_tokens") {
            self.usage.input = v;
        }
        if let Some(v) = field("output_tokens") {
            self.usage.output = v;
        }
        if let Some(v) = field("cache_read_input_tokens") {
            self.usage.cache_read = v;
        }
        if let Some(v) = field("cache_creation_input_tokens") {
            self.usage.cache_write = v;
        }
    }

    fn block_start(&mut self, data: &Value, out: &mut Vec<ProviderEvent>) {
        let index = data["index"].as_u64().unwrap_or_default();
        let block = &data["content_block"];
        let text_of = |field: &str| block[field].as_str().unwrap_or_default().to_owned();
        let partial = match block["type"].as_str().unwrap_or_default() {
            "text" => Partial::Text(text_of("text")),
            "tool_use" => {
                let (id, name) = (text_of("id"), text_of("name"));
                out.push(ProviderEvent::ToolCallStart {
                    id: id.clone(),
                    name: name.clone(),
                });
                let initial = block.get("input").cloned().unwrap_or_else(|| json!({}));
                Partial::Tool {
                    id,
                    name,
                    json: String::new(),
                    initial,
                }
            }
            "thinking" => Partial::Thinking {
                thinking: text_of("thinking"),
                signature: text_of("signature"),
            },
            _ => Partial::Raw(block.clone()),
        };
        self.blocks.insert(index, partial);
    }

    fn block_delta(&mut self, data: &Value, out: &mut Vec<ProviderEvent>) {
        let index = data["index"].as_u64().unwrap_or_default();
        let delta = &data["delta"];
        let Some(partial) = self.blocks.get_mut(&index) else {
            return;
        };
        let piece = |field: &str| delta[field].as_str().unwrap_or_default().to_owned();
        match (delta["type"].as_str().unwrap_or_default(), partial) {
            ("text_delta", Partial::Text(text)) => {
                let t = piece("text");
                text.push_str(&t);
                out.push(ProviderEvent::TextDelta { text: t });
            }
            ("input_json_delta", Partial::Tool { id, json, .. }) => {
                let fragment = piece("partial_json");
                json.push_str(&fragment);
                out.push(ProviderEvent::ToolCallInputDelta {
                    id: id.clone(),
                    json: fragment,
                });
            }
            ("thinking_delta", Partial::Thinking { thinking, .. }) => {
                let t = piece("thinking");
                thinking.push_str(&t);
                if !t.is_empty() {
                    out.push(ProviderEvent::ThinkingDelta { text: t });
                }
            }
            ("signature_delta", Partial::Thinking { signature, .. }) => {
                signature.push_str(&piece("signature"))
            }
            _ => {}
        }
    }

    fn block_stop(
        &mut self,
        data: &Value,
        out: &mut Vec<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        let index = data["index"].as_u64().unwrap_or_default();
        let Some(partial) = self.blocks.remove(&index) else {
            return Ok(());
        };
        let opaque = |raw: Value| ContentBlock::Opaque {
            provider: PROVIDER_ID.into(),
            model: self.model.clone(),
            raw,
        };
        let block = match partial {
            // The API rejects empty text blocks in input, so they can never be replayed.
            Partial::Text(text) if text.is_empty() => return Ok(()),
            Partial::Text(text) => ContentBlock::Text { text },
            Partial::Tool {
                id,
                name,
                json,
                initial,
            } => {
                let input = if json.trim().is_empty() {
                    initial
                } else {
                    serde_json::from_str(&json).map_err(|e| {
                        ProviderError::Decode(format!(
                            "tool call {name} has invalid JSON input (the reply may have been cut off): {e}"
                        ))
                    })?
                };
                ContentBlock::ToolCall { id, name, input }
            }
            Partial::Thinking {
                thinking,
                signature,
            } => opaque(json!({"type": "thinking", "thinking": thinking, "signature": signature})),
            Partial::Raw(raw) => opaque(raw),
        };
        out.push(ProviderEvent::BlockComplete { block });
        Ok(())
    }

    fn message_delta(&mut self, data: &Value) {
        self.read_usage(&data["usage"]);
        if let Some(reason) = data["delta"]["stop_reason"].as_str() {
            self.stop = Some(match reason {
                "end_turn" => StopReason::EndTurn,
                "tool_use" => StopReason::ToolUse,
                "max_tokens" => StopReason::MaxTokens,
                "pause_turn" => StopReason::PauseTurn,
                "refusal" => StopReason::Refusal {
                    category: data["delta"]["stop_details"]["category"]
                        .as_str()
                        .map(str::to_owned),
                },
                other => StopReason::Other {
                    raw: other.to_owned(),
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(events: &[(&str, Value)]) -> Result<Vec<ProviderEvent>, ProviderError> {
        let mut d = StreamDecoder::new();
        let mut out = Vec::new();
        for (name, data) in events {
            out.extend(d.feed(name, &data.to_string())?);
        }
        Ok(out)
    }

    fn start() -> (&'static str, Value) {
        (
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_1", "model": "claude-opus-5-5", "role": "assistant", "content": [], "usage": {"input_tokens": 100, "cache_read_input_tokens": 70, "cache_creation_input_tokens": 5, "output_tokens": 1}}}),
        )
    }

    fn stop(reason: &str) -> Vec<(&'static str, Value)> {
        vec![
            (
                "message_delta",
                json!({"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {"output_tokens": 42}}),
            ),
            ("message_stop", json!({"type": "message_stop"})),
        ]
    }

    fn completed(events: &[ProviderEvent]) -> Vec<ContentBlock> {
        events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::BlockComplete { block } => Some(block.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn text_reply_streams_deltas_then_completes() {
        let mut events = vec![
            start(),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}}),
            ),
            ("ping", json!({"type": "ping"})),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "lo"}}),
            ),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
        ];
        events.extend(stop("end_turn"));
        let out = feed_all(&events).unwrap();
        assert_eq!(out[0], ProviderEvent::TextDelta { text: "Hel".into() });
        assert_eq!(completed(&out), vec![ContentBlock::text("Hello")]);
        assert_eq!(
            out[out.len() - 2],
            ProviderEvent::Usage {
                usage: Usage {
                    input: 100,
                    output: 42,
                    cache_read: 70,
                    cache_write: 5
                }
            }
        );
        assert_eq!(
            out.last(),
            Some(&ProviderEvent::Stop {
                reason: StopReason::EndTurn
            })
        );
    }

    #[test]
    fn tool_use_input_is_assembled_from_fragments() {
        let mut events = vec![
            start(),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "shell", "input": {}}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"comm"}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "and\": \"ls\"}"}}),
            ),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
        ];
        events.extend(stop("tool_use"));
        let out = feed_all(&events).unwrap();
        assert_eq!(
            out[0],
            ProviderEvent::ToolCallStart {
                id: "toolu_1".into(),
                name: "shell".into()
            }
        );
        assert_eq!(
            completed(&out),
            vec![ContentBlock::ToolCall {
                id: "toolu_1".into(),
                name: "shell".into(),
                input: json!({"command": "ls"})
            }]
        );
        assert_eq!(
            out.last(),
            Some(&ProviderEvent::Stop {
                reason: StopReason::ToolUse
            })
        );
    }

    #[test]
    fn tool_use_without_input_deltas_uses_the_initial_input() {
        let events = [
            start(),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t", "name": "x", "input": {}}}),
            ),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
        ];
        let out = feed_all(&events).unwrap();
        assert_eq!(
            completed(&out),
            vec![ContentBlock::ToolCall {
                id: "t".into(),
                name: "x".into(),
                input: json!({})
            }]
        );
    }

    #[test]
    fn invalid_tool_json_is_a_decode_error() {
        let events = [
            start(),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t", "name": "x", "input": {}}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"cut"}}),
            ),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
        ];
        assert!(matches!(feed_all(&events), Err(ProviderError::Decode(_))));
    }

    #[test]
    fn thinking_becomes_an_opaque_block_with_its_signature() {
        let events = [
            start(),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Let me "}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "check."}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "EqQB"}}),
            ),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
        ];
        let out = feed_all(&events).unwrap();
        assert!(out.contains(&ProviderEvent::ThinkingDelta {
            text: "Let me ".into()
        }));
        assert_eq!(
            completed(&out),
            vec![ContentBlock::Opaque {
                provider: "anthropic".into(),
                model: "claude-opus-5-5".into(),
                raw: json!({"type": "thinking", "thinking": "Let me check.", "signature": "EqQB"}),
            }]
        );
    }

    #[test]
    fn unknown_and_redacted_blocks_are_kept_raw() {
        let raw = json!({"type": "redacted_thinking", "data": "abc"});
        let events = [
            start(),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": raw}),
            ),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
        ];
        let out = feed_all(&events).unwrap();
        assert!(matches!(&completed(&out)[0], ContentBlock::Opaque { raw: r, .. } if *r == raw));
    }

    #[test]
    fn empty_text_blocks_are_dropped() {
        let events = [
            start(),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
        ];
        assert!(completed(&feed_all(&events).unwrap()).is_empty());
    }

    #[test]
    fn refusal_carries_its_category() {
        let events = [
            start(),
            (
                "message_delta",
                json!({"type": "message_delta", "delta": {"stop_reason": "refusal", "stop_details": {"type": "refusal", "category": "cyber"}}, "usage": {"output_tokens": 0}}),
            ),
            ("message_stop", json!({"type": "message_stop"})),
        ];
        let out = feed_all(&events).unwrap();
        assert_eq!(
            out.last(),
            Some(&ProviderEvent::Stop {
                reason: StopReason::Refusal {
                    category: Some("cyber".into())
                }
            })
        );
    }

    #[test]
    fn input_transformations_become_notices() {
        let mut s = start();
        s.1["message"]["input_transformations"] = json!([{"type": "thinking_dropped", "path": "messages.1.content.0", "reason": "prefix_binding_mismatch"}]);
        let out = feed_all(&[s]).unwrap();
        assert!(
            matches!(&out[0], ProviderEvent::Notice { message } if message.contains("prefix_binding_mismatch") && message.contains("messages.1.content.0"))
        );
    }

    #[test]
    fn error_events_map_to_provider_errors() {
        let out = feed_all(&[
            start(),
            (
                "error",
                json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
            ),
        ]);
        assert_eq!(
            out,
            Err(ProviderError::Overloaded {
                retry_after_ms: None
            })
        );
    }

    #[test]
    fn malformed_data_is_a_decode_error() {
        let mut d = StreamDecoder::new();
        assert!(matches!(
            d.feed("message_start", "not json"),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn done_after_message_stop() {
        let mut d = StreamDecoder::new();
        assert!(!d.is_done());
        d.feed("message_stop", r#"{"type":"message_stop"}"#)
            .unwrap();
        assert!(d.is_done());
    }
}
