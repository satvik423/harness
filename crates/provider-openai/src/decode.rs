//! [`ResponsesDecoder`]: turns Responses API server-sent events into [`ProviderEvent`]s.

use std::collections::HashMap;

use harness_ports::{ProviderError, ProviderEvent};
use harness_types::{ContentBlock, StopReason, Usage};
use serde_json::Value;

use crate::http::error_from_code;
use crate::request::PROVIDER_ID;

/// Stateful decoder for one streamed response.
///
/// Emits deltas for live display and one [`ProviderEvent::BlockComplete`] per
/// finished output item: assistant text, function calls (with parsed input) and
/// reasoning items (kept verbatim as [`ContentBlock::Opaque`], including their
/// `encrypted_content`, so they can be replayed).
///
/// # Examples
/// ```
/// use harness_provider_openai::ResponsesDecoder;
/// let mut d = ResponsesDecoder::new();
/// assert!(d.feed("response.in_progress", r#"{"type":"response.in_progress","response":{"model":"m"}}"#).unwrap().is_empty());
/// ```
#[derive(Debug, Default)]
pub struct ResponsesDecoder {
    model: String,
    /// Function calls by output index: (call id, name).
    calls: HashMap<u64, (String, String)>,
    saw_call: bool,
    refused: bool,
    done: bool,
}

impl ResponsesDecoder {
    /// A decoder for a new response.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` once the response has completed (or ended incomplete).
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Processes one server-sent event.
    ///
    /// # Errors
    /// [`ProviderError::Decode`] for malformed data or invalid function-call JSON,
    /// or the mapped error for `error` and `response.failed` events.
    pub fn feed(&mut self, event: &str, data: &str) -> Result<Vec<ProviderEvent>, ProviderError> {
        let data: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::Decode(format!("bad {event} event: {e}")))?;
        let kind = data["type"].as_str().unwrap_or(event).to_owned();
        let text = |field: &str| data[field].as_str().unwrap_or_default().to_owned();
        let index = data["output_index"].as_u64().unwrap_or_default();
        let mut out = Vec::new();
        match kind.as_str() {
            "response.created" | "response.in_progress" => {
                if let Some(model) = data["response"]["model"].as_str() {
                    self.model = model.to_owned();
                }
            }
            "response.output_item.added" => {
                let item = &data["item"];
                if item["type"] == "function_call" {
                    let id = item["call_id"].as_str().unwrap_or_default().to_owned();
                    let name = item["name"].as_str().unwrap_or_default().to_owned();
                    out.push(ProviderEvent::ToolCallStart {
                        id: id.clone(),
                        name: name.clone(),
                    });
                    self.calls.insert(index, (id, name));
                }
            }
            "response.output_text.delta" => out.push(ProviderEvent::TextDelta {
                text: text("delta"),
            }),
            "response.function_call_arguments.delta" => {
                if let Some((id, _)) = self.calls.get(&index) {
                    out.push(ProviderEvent::ToolCallInputDelta {
                        id: id.clone(),
                        json: text("delta"),
                    });
                }
            }
            "response.reasoning_summary_text.delta" => out.push(ProviderEvent::ThinkingDelta {
                text: text("delta"),
            }),
            "response.output_item.done" => self.complete(&data["item"], &mut out)?,
            "response.completed" => {
                self.done = true;
                out.push(ProviderEvent::Usage {
                    usage: usage(&data["response"]["usage"]),
                });
                let reason = if self.refused {
                    StopReason::Refusal { category: None }
                } else if self.saw_call {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                };
                out.push(ProviderEvent::Stop { reason });
            }
            "response.incomplete" => {
                self.done = true;
                let response = &data["response"];
                out.push(ProviderEvent::Usage {
                    usage: usage(&response["usage"]),
                });
                let reason = match response["incomplete_details"]["reason"]
                    .as_str()
                    .unwrap_or_default()
                {
                    "max_output_tokens" => StopReason::MaxTokens,
                    "content_filter" => StopReason::Refusal {
                        category: Some("content_filter".into()),
                    },
                    other => StopReason::Other {
                        raw: other.to_owned(),
                    },
                };
                out.push(ProviderEvent::Stop { reason });
            }
            "response.failed" => {
                let error = &data["response"]["error"];
                return Err(error_from_code(
                    error["code"].as_str().unwrap_or_default(),
                    error["message"].as_str().unwrap_or("the response failed"),
                ));
            }
            "error" => {
                return Err(error_from_code(
                    data["code"].as_str().unwrap_or_default(),
                    data["message"].as_str().unwrap_or("unknown error"),
                ));
            }
            _ => {}
        }
        Ok(out)
    }

    /// Emits the completed block(s) for one finished output item.
    fn complete(
        &mut self,
        item: &Value,
        out: &mut Vec<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        match item["type"].as_str().unwrap_or_default() {
            "message" => {
                for part in item["content"].as_array().into_iter().flatten() {
                    let text = match part["type"].as_str().unwrap_or_default() {
                        "output_text" => part["text"].as_str().unwrap_or_default(),
                        "refusal" => {
                            self.refused = true;
                            part["refusal"].as_str().unwrap_or_default()
                        }
                        _ => continue,
                    };
                    if !text.is_empty() {
                        out.push(ProviderEvent::BlockComplete {
                            block: ContentBlock::text(text),
                        });
                    }
                }
            }
            "function_call" => {
                let name = item["name"].as_str().unwrap_or_default().to_owned();
                let arguments = item["arguments"].as_str().unwrap_or_default();
                let input = if arguments.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(arguments).map_err(|e| {
                        ProviderError::Decode(format!(
                            "function call {name} has invalid JSON arguments: {e}"
                        ))
                    })?
                };
                self.saw_call = true;
                let id = item["call_id"].as_str().unwrap_or_default().to_owned();
                out.push(ProviderEvent::BlockComplete {
                    block: ContentBlock::ToolCall { id, name, input },
                });
            }
            // Reasoning (with encrypted content) and any other item types are replayed verbatim.
            _ => out.push(ProviderEvent::BlockComplete {
                block: ContentBlock::Opaque {
                    provider: PROVIDER_ID.into(),
                    model: self.model.clone(),
                    raw: item.clone(),
                },
            }),
        }
        Ok(())
    }
}

/// Converts Responses API usage. `input_tokens` includes cache reads and cache
/// writes, so both are subtracted to get the uncached input.
fn usage(u: &Value) -> Usage {
    let details = &u["input_tokens_details"];
    let input = u["input_tokens"].as_u64().unwrap_or_default();
    let cached = details["cached_tokens"].as_u64().unwrap_or_default();
    let written = details["cache_write_tokens"].as_u64().unwrap_or_default();
    Usage {
        input: input.saturating_sub(cached).saturating_sub(written),
        output: u["output_tokens"].as_u64().unwrap_or_default(),
        cache_read: cached,
        cache_write: written,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn feed_all(events: &[Value]) -> Result<Vec<ProviderEvent>, ProviderError> {
        let mut d = ResponsesDecoder::new();
        let mut out = Vec::new();
        for e in events {
            out.extend(d.feed(e["type"].as_str().unwrap(), &e.to_string())?);
        }
        Ok(out)
    }

    fn created() -> Value {
        json!({"type": "response.created", "response": {"id": "resp_1", "model": "gpt-test", "status": "in_progress"}})
    }

    fn completed() -> Value {
        json!({"type": "response.completed", "response": {"status": "completed", "usage": {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 60}, "output_tokens": 20}}})
    }

    fn blocks(events: &[ProviderEvent]) -> Vec<ContentBlock> {
        events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::BlockComplete { block } => Some(block.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn text_response_streams_and_completes() {
        let out = feed_all(&[
            created(),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "id": "msg_1", "role": "assistant", "content": []}}),
            json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "Hel"}),
            json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "lo"}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "message", "id": "msg_1", "role": "assistant", "content": [{"type": "output_text", "text": "Hello", "annotations": []}]}}),
            completed(),
        ])
        .unwrap();
        assert_eq!(out[0], ProviderEvent::TextDelta { text: "Hel".into() });
        assert_eq!(blocks(&out), vec![ContentBlock::text("Hello")]);
        assert_eq!(
            out[out.len() - 2],
            ProviderEvent::Usage {
                usage: Usage {
                    input: 40,
                    output: 20,
                    cache_read: 60,
                    cache_write: 0
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
    fn cache_writes_are_counted_separately_from_plain_input() {
        let done = json!({"type": "response.completed", "response": {"usage": {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 30, "cache_write_tokens": 50}, "output_tokens": 5}}});
        let out = feed_all(&[done]).unwrap();
        assert_eq!(
            out[0],
            ProviderEvent::Usage {
                usage: Usage {
                    input: 20,
                    output: 5,
                    cache_read: 30,
                    cache_write: 50
                }
            }
        );
    }

    #[test]
    fn function_calls_stream_arguments_and_end_with_tool_use() {
        let out = feed_all(&[
            created(),
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "shell", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 1, "item_id": "fc_1", "delta": "{\"command\":"}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 1, "item_id": "fc_1", "delta": "\"ls\"}"}),
            json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "shell", "arguments": "{\"command\":\"ls\"}"}}),
            completed(),
        ])
        .unwrap();
        assert_eq!(
            out[0],
            ProviderEvent::ToolCallStart {
                id: "call_1".into(),
                name: "shell".into()
            }
        );
        assert_eq!(
            out[1],
            ProviderEvent::ToolCallInputDelta {
                id: "call_1".into(),
                json: "{\"command\":".into()
            }
        );
        assert_eq!(
            blocks(&out),
            vec![ContentBlock::ToolCall {
                id: "call_1".into(),
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
    fn reasoning_items_are_kept_verbatim_and_summaries_stream() {
        let item = json!({"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "Checked."}], "encrypted_content": "gAAAA"});
        let out = feed_all(&[
            created(),
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "Checked."}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": item}),
        ])
        .unwrap();
        assert_eq!(
            out[0],
            ProviderEvent::ThinkingDelta {
                text: "Checked.".into()
            }
        );
        assert_eq!(
            blocks(&out),
            vec![ContentBlock::Opaque {
                provider: "openai".into(),
                model: "gpt-test".into(),
                raw: item
            }]
        );
    }

    #[test]
    fn empty_arguments_mean_an_empty_object_and_bad_json_is_an_error() {
        let done = |args: &str| json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "function_call", "call_id": "c", "name": "x", "arguments": args}});
        assert_eq!(
            blocks(&feed_all(&[done("")]).unwrap()),
            vec![ContentBlock::ToolCall {
                id: "c".into(),
                name: "x".into(),
                input: json!({})
            }]
        );
        assert!(matches!(
            feed_all(&[done("{\"cut")]),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn refusals_are_reported_as_refusal_stops() {
        let out = feed_all(&[
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "message", "role": "assistant", "content": [{"type": "refusal", "refusal": "I can't help with that."}]}}),
            completed(),
        ])
        .unwrap();
        assert_eq!(
            blocks(&out),
            vec![ContentBlock::text("I can't help with that.")]
        );
        assert_eq!(
            out.last(),
            Some(&ProviderEvent::Stop {
                reason: StopReason::Refusal { category: None }
            })
        );
    }

    #[test]
    fn incomplete_responses_map_their_reason() {
        let incomplete = |reason: &str| json!({"type": "response.incomplete", "response": {"status": "incomplete", "incomplete_details": {"reason": reason}, "usage": {"input_tokens": 1, "output_tokens": 1}}});
        assert_eq!(
            feed_all(&[incomplete("max_output_tokens")]).unwrap().last(),
            Some(&ProviderEvent::Stop {
                reason: StopReason::MaxTokens
            })
        );
        assert_eq!(
            feed_all(&[incomplete("content_filter")]).unwrap().last(),
            Some(&ProviderEvent::Stop {
                reason: StopReason::Refusal {
                    category: Some("content_filter".into())
                }
            })
        );
    }

    #[test]
    fn failures_and_error_events_map_to_provider_errors() {
        let failed = json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "server_error", "message": "oops"}}});
        assert_eq!(
            feed_all(&[failed]),
            Err(ProviderError::Server {
                status: 500,
                message: "oops".into()
            })
        );
        let error = json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow down"});
        assert_eq!(
            feed_all(&[error]),
            Err(ProviderError::RateLimited {
                retry_after_ms: None
            })
        );
    }

    #[test]
    fn done_only_after_completion() {
        let mut d = ResponsesDecoder::new();
        assert!(!d.is_done());
        d.feed("response.completed", &completed().to_string())
            .unwrap();
        assert!(d.is_done());
        assert!(matches!(
            d.feed("x", "not json"),
            Err(ProviderError::Decode(_))
        ));
    }
}
