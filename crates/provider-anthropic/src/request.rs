//! Maps a provider-neutral [`ModelRequest`] to the Messages API request body.

use harness_ports::{Effort, ModelRequest, ProviderError, ToolSpec};
use harness_types::{ContentBlock, Message, Role};
use serde_json::{Map, Value, json};

use crate::AnthropicSettings;

/// Provider id written into opaque blocks and used to pick which ones to replay.
pub const PROVIDER_ID: &str = "anthropic";

/// Builds the streaming `POST /v1/messages` body.
///
/// Never sends `temperature`, `top_p` or `top_k` (rejected by API version
/// `2026-10-21`), assistant prefill, or a forced `tool_choice`.
///
/// An `output_schema` becomes `output_config.format` (JSON schema); the reply's
/// text is then JSON matching it (ADR-044).
///
/// # Errors
/// [`ProviderError::Unsupported`] for content this phase cannot send yet
/// (images, documents).
pub fn build_messages_body(
    request: &ModelRequest,
    settings: &AnthropicSettings,
) -> Result<Value, ProviderError> {
    let ephemeral = json!({"type": "ephemeral"});
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("max_tokens".into(), json!(request.max_output));
    body.insert("stream".into(), json!(true));

    if !request.system.is_empty() {
        let mut system: Vec<Value> = request
            .system
            .iter()
            .map(|p| json!({"type": "text", "text": p.text}))
            .collect();
        if request.cache_plan.system_breakpoint
            && let Some(last) = system.last_mut()
        {
            last["cache_control"] = ephemeral.clone();
        }
        body.insert("system".into(), Value::Array(system));
    }

    let mut messages = request
        .messages
        .iter()
        .map(message)
        .collect::<Result<Vec<_>, _>>()?;
    // Opener breakpoint: the first block of the first user message is the segment opener.
    if request.cache_plan.opener_breakpoint
        && let Some(first) = messages.first_mut().filter(|m| m["role"] == "user")
        && let Some(block) = first["content"].get_mut(0).filter(|b| b["type"] == "text")
    {
        block["cache_control"] = ephemeral.clone();
    }
    body.insert("messages".into(), Value::Array(messages));

    if !request.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(request.tools.iter().map(tool).collect()),
        );
    }

    let mut thinking = json!({"type": "adaptive"});
    if request.show_thinking {
        thinking["display"] = json!("summarized");
    }
    thinking["block_binding"] =
        json!({"prefix_mismatch_behavior": settings.thinking_mismatch.as_str()});
    body.insert("thinking".into(), thinking);

    let mut output_config = Map::new();
    if let Some(effort) = request.effort {
        output_config.insert("effort".into(), json!(effort_name(effort)));
    }
    if let Some(schema) = &request.output_schema {
        output_config.insert(
            "format".into(),
            json!({"type": "json_schema", "schema": schema}),
        );
    }
    if !output_config.is_empty() {
        body.insert("output_config".into(), Value::Object(output_config));
    }
    if request.cache_plan.auto_tail {
        body.insert("cache_control".into(), ephemeral);
    }
    if settings.server_fallbacks {
        body.insert("fallbacks".into(), json!("default"));
    }
    Ok(Value::Object(body))
}

/// Maps one message. Opaque blocks from other providers are dropped; the API
/// could not verify them anyway.
fn message(message: &Message) -> Result<Value, ProviderError> {
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => {
            // Mid-conversation system notes are plain text.
            let text = message.text();
            return Ok(json!({"role": "system", "content": text}));
        }
    };
    let mut content = Vec::with_capacity(message.content.len());
    for block in &message.content {
        if let Some(value) = content_block(block)? {
            content.push(value);
        }
    }
    Ok(json!({"role": role, "content": content}))
}

fn content_block(block: &ContentBlock) -> Result<Option<Value>, ProviderError> {
    Ok(match block {
        ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
        ContentBlock::ToolCall { id, name, input } => {
            Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
        }
        ContentBlock::ToolResult {
            call_id,
            content,
            is_error,
        } => {
            let inner = content
                .iter()
                .map(content_block)
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            Some(
                json!({"type": "tool_result", "tool_use_id": call_id, "content": inner, "is_error": is_error}),
            )
        }
        ContentBlock::Opaque { provider, raw, .. } => {
            (provider == PROVIDER_ID).then(|| raw.clone())
        }
        ContentBlock::Image { .. } | ContentBlock::Document { .. } => {
            return Err(ProviderError::Unsupported(
                "image and document input arrives in Phase 11".into(),
            ));
        }
    })
}

fn tool(spec: &ToolSpec) -> Value {
    let mut t = json!({"name": spec.name.as_str(), "description": spec.description, "input_schema": spec.input_schema});
    if spec.strict {
        t["strict"] = json!(true);
    }
    if spec.eager_input_streaming {
        t["eager_input_streaming"] = json!(true);
    }
    t
}

/// Builds the `POST /v1/messages/count_tokens` body: the messages body without
/// the generation-only fields.
///
/// # Errors
/// As [`build_messages_body`].
pub fn build_count_body(
    request: &ModelRequest,
    settings: &AnthropicSettings,
) -> Result<Value, ProviderError> {
    let mut body = build_messages_body(request, settings)?;
    if let Some(obj) = body.as_object_mut() {
        for key in ["stream", "max_tokens", "cache_control", "fallbacks"] {
            obj.remove(key);
        }
    }
    Ok(body)
}

fn effort_name(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh => "xhigh",
        Effort::Max => "max",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_ports::{CachePlan, SystemPart, SystemSlot, ToolName};
    use harness_types::BlobRef;

    fn tool() -> ToolSpec {
        ToolSpec {
            name: ToolName::new("shell").unwrap(),
            description: "Run a command.".into(),
            input_schema: json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"], "additionalProperties": false}),
            strict: true,
            eager_input_streaming: false,
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: "claude-opus-5-5".into(),
            system: vec![
                SystemPart {
                    slot: SystemSlot::Base,
                    text: "base".into(),
                },
                SystemPart {
                    slot: SystemSlot::Soul,
                    text: "soul".into(),
                },
            ],
            tools: vec![tool()],
            messages: vec![
                Message::new(
                    Role::User,
                    vec![
                        ContentBlock::text("<environment/>"),
                        ContentBlock::text("run ls"),
                    ],
                ),
                Message::new(
                    Role::Assistant,
                    vec![
                        ContentBlock::Opaque {
                            provider: "anthropic".into(),
                            model: "claude-opus-5-5".into(),
                            raw: json!({"type": "thinking", "thinking": "", "signature": "sig"}),
                        },
                        ContentBlock::Opaque {
                            provider: "openai".into(),
                            model: "gpt".into(),
                            raw: json!({"type": "reasoning"}),
                        },
                        ContentBlock::text("Running it."),
                        ContentBlock::ToolCall {
                            id: "toolu_1".into(),
                            name: "shell".into(),
                            input: json!({"command": "ls"}),
                        },
                    ],
                ),
                Message::new(
                    Role::User,
                    vec![ContentBlock::ToolResult {
                        call_id: "toolu_1".into(),
                        content: vec![ContentBlock::text("a.txt")],
                        is_error: false,
                    }],
                ),
            ],
            max_output: 64_000,
            effort: Some(Effort::High),
            cache_plan: CachePlan::default(),
            output_schema: None,
            show_thinking: false,
        }
    }

    fn body() -> Value {
        build_messages_body(&request(), &AnthropicSettings::default()).unwrap()
    }

    #[test]
    fn body_matches_snapshot() {
        insta::assert_json_snapshot!("messages_body", body());
    }

    #[test]
    fn sampling_parameters_and_tool_choice_are_never_sent() {
        let text = body().to_string();
        for key in ["temperature", "top_p", "top_k", "tool_choice"] {
            assert!(!text.contains(key), "{key} must not be sent");
        }
    }

    #[test]
    fn cache_breakpoints_are_on_system_opener_and_tail() {
        let b = body();
        assert!(b["system"][0].get("cache_control").is_none());
        assert_eq!(
            b["system"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(
            b["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert!(
            b["messages"][0]["content"][1]
                .get("cache_control")
                .is_none()
        );
        assert_eq!(b["cache_control"], json!({"type": "ephemeral"}));
    }

    #[test]
    fn thinking_effort_fallbacks_and_binding_are_set() {
        let b = body();
        assert_eq!(
            b["thinking"],
            json!({"type": "adaptive", "block_binding": {"prefix_mismatch_behavior": "drop_block"}})
        );
        assert_eq!(b["output_config"], json!({"effort": "high"}));
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["stream"], true);
        assert_eq!(b["max_tokens"], 64_000);
    }

    #[test]
    fn show_thinking_requests_summaries() {
        let mut r = request();
        r.show_thinking = true;
        let b = build_messages_body(&r, &AnthropicSettings::default()).unwrap();
        assert_eq!(b["thinking"]["display"], "summarized");
    }

    #[test]
    fn only_anthropic_opaque_blocks_are_replayed_verbatim() {
        let content = body()["messages"][1]["content"].clone();
        assert_eq!(
            content[0],
            json!({"type": "thinking", "thinking": "", "signature": "sig"})
        );
        assert_eq!(
            content.as_array().unwrap().len(),
            3,
            "the openai block is skipped: {content}"
        );
        assert_eq!(
            content[2],
            json!({"type": "tool_use", "id": "toolu_1", "name": "shell", "input": {"command": "ls"}})
        );
    }

    #[test]
    fn tool_results_map_to_tool_result_blocks() {
        let result = body()["messages"][2]["content"][0].clone();
        assert_eq!(
            result,
            json!({"type": "tool_result", "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "a.txt"}], "is_error": false})
        );
    }

    #[test]
    fn tools_carry_strict_and_eager_flags_only_when_set() {
        let b = body();
        assert_eq!(b["tools"][0]["strict"], true);
        assert!(b["tools"][0].get("eager_input_streaming").is_none());
    }

    #[test]
    fn system_role_messages_are_sent_as_system_messages() {
        let mut r = request();
        r.messages
            .push(Message::new(Role::System, vec![ContentBlock::text("note")]));
        let b = build_messages_body(&r, &AnthropicSettings::default()).unwrap();
        assert_eq!(
            b["messages"][3],
            json!({"role": "system", "content": "note"})
        );
    }

    #[test]
    fn images_are_unsupported_for_now() {
        let mut r = request();
        r.messages[0].content.push(ContentBlock::Image {
            blob: BlobRef::from_bytes([0; 32]),
            media_type: "image/png".into(),
        });
        assert!(matches!(
            build_messages_body(&r, &AnthropicSettings::default()),
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[test]
    fn an_output_schema_becomes_output_config_format_next_to_effort() {
        let schema = json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false});
        let mut r = request();
        r.output_schema = Some(schema.clone());
        let b = build_messages_body(&r, &AnthropicSettings::default()).unwrap();
        assert_eq!(
            b["output_config"],
            json!({"effort": "high", "format": {"type": "json_schema", "schema": schema}})
        );
        r.effort = None;
        let b = build_messages_body(&r, &AnthropicSettings::default()).unwrap();
        assert_eq!(
            b["output_config"],
            json!({"format": {"type": "json_schema", "schema": schema}})
        );
    }

    #[test]
    fn count_body_drops_generation_fields() {
        let b = build_count_body(&request(), &AnthropicSettings::default()).unwrap();
        for key in ["stream", "max_tokens", "cache_control", "fallbacks"] {
            assert!(b.get(key).is_none(), "{key}");
        }
        assert_eq!(b["model"], "claude-opus-5-5");
    }
}
