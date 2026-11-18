//! Maps a provider-neutral [`ModelRequest`] to the Responses API request body.

use harness_ports::{Effort, ModelRequest, ProviderError, ToolSpec};
use harness_types::{ContentBlock, Message, Role};
use serde_json::{Map, Value, json};

use crate::OpenAiSettings;

/// Provider id written into opaque blocks and used to pick which ones to replay.
pub const PROVIDER_ID: &str = "openai";

/// Builds the streaming `POST /v1/responses` body.
///
/// The harness owns the conversation: `store` follows the settings (default
/// `false`) and encrypted reasoning is requested so it can be replayed.
///
/// An `output_schema` becomes a strict JSON-schema `text.format`; the reply's
/// text is then JSON matching it (ADR-044).
///
/// # Errors
/// [`ProviderError::Unsupported`] for content this phase cannot send yet
/// (images, documents).
pub fn build_responses_body(
    request: &ModelRequest,
    settings: &OpenAiSettings,
) -> Result<Value, ProviderError> {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(true));
    body.insert("store".into(), json!(settings.store));
    body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    body.insert("max_output_tokens".into(), json!(request.max_output));
    if !request.system.is_empty() {
        let instructions: Vec<&str> = request.system.iter().map(|p| p.text.as_str()).collect();
        body.insert("instructions".into(), json!(instructions.join("\n\n")));
    }
    let mut input = Vec::new();
    for message in &request.messages {
        push_items(message, &mut input)?;
    }
    body.insert("input".into(), Value::Array(input));
    if !request.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(request.tools.iter().map(tool).collect()),
        );
    }
    let mut reasoning = Map::new();
    if let Some(effort) = request.effort {
        reasoning.insert("effort".into(), json!(effort_name(effort)));
    }
    if request.show_thinking {
        reasoning.insert("summary".into(), json!("auto"));
    }
    if let Some(schema) = &request.output_schema {
        body.insert(
            "text".into(),
            json!({"format": {"type": "json_schema", "name": "harness_output", "schema": schema, "strict": true}}),
        );
    }
    if !reasoning.is_empty() {
        body.insert("reasoning".into(), Value::Object(reasoning));
    }
    Ok(Value::Object(body))
}

/// Appends the input items for one message, keeping block order.
fn push_items(message: &Message, items: &mut Vec<Value>) -> Result<(), ProviderError> {
    match message.role {
        Role::System => {
            items.push(json!({"type": "message", "role": "developer", "content": [{"type": "input_text", "text": message.text()}]}));
        }
        Role::User => {
            let mut texts = Vec::new();
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        texts.push(json!({"type": "input_text", "text": text}))
                    }
                    ContentBlock::ToolResult {
                        call_id,
                        content,
                        is_error,
                    } => {
                        let text = text_of(content)?;
                        let output = if *is_error {
                            format!("Error: {text}")
                        } else {
                            text
                        };
                        items.push(json!({"type": "function_call_output", "call_id": call_id, "output": output}));
                    }
                    other => unsupported(other)?,
                }
            }
            if !texts.is_empty() {
                items.push(json!({"type": "message", "role": "user", "content": texts}));
            }
        }
        Role::Assistant => {
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        items.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}));
                    }
                    ContentBlock::ToolCall { id, name, input } => {
                        items.push(json!({"type": "function_call", "call_id": id, "name": name, "arguments": input.to_string()}));
                    }
                    // Reasoning items must be replayed exactly; other providers' blocks are skipped.
                    ContentBlock::Opaque { provider, raw, .. } if provider == PROVIDER_ID => {
                        items.push(raw.clone())
                    }
                    ContentBlock::Opaque { .. } => {}
                    other => unsupported(other)?,
                }
            }
        }
    }
    Ok(())
}

/// Text of tool-result content; non-text content is not supported yet.
fn text_of(content: &[ContentBlock]) -> Result<String, ProviderError> {
    let mut parts = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text { text } => parts.push(text.as_str()),
            other => unsupported(other)?,
        }
    }
    Ok(parts.join("\n"))
}

fn unsupported(block: &ContentBlock) -> Result<(), ProviderError> {
    Err(ProviderError::Unsupported(match block {
        ContentBlock::Image { .. } | ContentBlock::Document { .. } => {
            "image and document input arrives in Phase 11".into()
        }
        _ => "this content block cannot be sent here".into(),
    }))
}

fn tool(spec: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": spec.name.as_str(),
        "description": spec.description,
        "parameters": spec.input_schema,
        "strict": spec.strict && strict_compatible(&spec.input_schema),
    })
}

/// `true` when OpenAI's strict mode can be used: every property is required
/// and no extra properties are allowed.
fn strict_compatible(schema: &Value) -> bool {
    let required: Vec<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let all_required = schema["properties"]
        .as_object()
        .is_none_or(|props| props.keys().all(|k| required.contains(&k.as_str())));
    all_required && schema["additionalProperties"] == json!(false)
}

/// OpenAI accepts low, medium and high; higher levels map to high.
fn effort_name(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High | Effort::Xhigh | Effort::Max => "high",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_ports::{CachePlan, SystemPart, SystemSlot, ToolName};
    use harness_types::BlobRef;

    fn tool(schema: Value) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("shell").unwrap(),
            description: "Run a command.".into(),
            input_schema: schema,
            strict: true,
            eager_input_streaming: false,
        }
    }

    fn strict_schema() -> Value {
        json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"], "additionalProperties": false})
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: "gpt-test".into(),
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
            tools: vec![tool(strict_schema())],
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
                            provider: "openai".into(),
                            model: "gpt-test".into(),
                            raw: json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "enc"}),
                        },
                        ContentBlock::Opaque {
                            provider: "anthropic".into(),
                            model: "claude".into(),
                            raw: json!({"type": "thinking"}),
                        },
                        ContentBlock::text("Running it."),
                        ContentBlock::ToolCall {
                            id: "call_1".into(),
                            name: "shell".into(),
                            input: json!({"command": "ls"}),
                        },
                    ],
                ),
                Message::new(
                    Role::User,
                    vec![ContentBlock::ToolResult {
                        call_id: "call_1".into(),
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
        build_responses_body(&request(), &OpenAiSettings::default()).unwrap()
    }

    #[test]
    fn body_matches_snapshot() {
        insta::assert_json_snapshot!("responses_body", body());
    }

    #[test]
    fn stateless_streaming_with_encrypted_reasoning() {
        let b = body();
        assert_eq!(b["stream"], true);
        assert_eq!(b["store"], false);
        assert_eq!(b["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(b["max_output_tokens"], 64_000);
        assert_eq!(b["reasoning"], json!({"effort": "high"}));
        assert_eq!(b["instructions"], "base\n\nsoul");
    }

    #[test]
    fn transcript_maps_to_input_items_in_order() {
        let input = body()["input"].clone();
        assert_eq!(
            input[0],
            json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment/>"}, {"type": "input_text", "text": "run ls"}]})
        );
        assert_eq!(
            input[1],
            json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "enc"})
        );
        assert_eq!(
            input[2],
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Running it."}]})
        );
        assert_eq!(
            input[3],
            json!({"type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{\"command\":\"ls\"}"})
        );
        assert_eq!(
            input[4],
            json!({"type": "function_call_output", "call_id": "call_1", "output": "a.txt"})
        );
        assert_eq!(
            input.as_array().unwrap().len(),
            5,
            "the anthropic block is skipped"
        );
    }

    #[test]
    fn failed_tool_results_are_marked_in_the_output() {
        let mut r = request();
        r.messages[2] = Message::new(
            Role::User,
            vec![ContentBlock::ToolResult {
                call_id: "call_1".into(),
                content: vec![ContentBlock::text("boom")],
                is_error: true,
            }],
        );
        let b = build_responses_body(&r, &OpenAiSettings::default()).unwrap();
        assert_eq!(b["input"][4]["output"], "Error: boom");
    }

    #[test]
    fn system_notes_become_developer_messages() {
        let mut r = request();
        r.messages
            .push(Message::new(Role::System, vec![ContentBlock::text("note")]));
        let b = build_responses_body(&r, &OpenAiSettings::default()).unwrap();
        assert_eq!(
            b["input"][5],
            json!({"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "note"}]})
        );
    }

    #[test]
    fn tools_are_functions_and_strict_only_when_compatible() {
        let b = body();
        assert_eq!(
            b["tools"][0],
            json!({"type": "function", "name": "shell", "description": "Run a command.", "parameters": strict_schema(), "strict": true})
        );
        let mut r = request();
        r.tools = vec![tool(
            json!({"type": "object", "properties": {"a": {}, "b": {}}, "required": ["a"], "additionalProperties": false}),
        )];
        assert_eq!(
            build_responses_body(&r, &OpenAiSettings::default()).unwrap()["tools"][0]["strict"],
            false
        );
    }

    #[test]
    fn show_thinking_asks_for_summaries_and_efforts_map_safely() {
        let mut r = request();
        r.show_thinking = true;
        r.effort = Some(Effort::Max);
        let b = build_responses_body(&r, &OpenAiSettings::default()).unwrap();
        assert_eq!(b["reasoning"], json!({"effort": "high", "summary": "auto"}));
    }

    #[test]
    fn sampling_parameters_are_never_sent() {
        let text = body().to_string();
        for key in [
            "temperature",
            "top_p",
            "tool_choice",
            "previous_response_id",
        ] {
            assert!(!text.contains(key), "{key}");
        }
    }

    #[test]
    fn images_are_unsupported_for_now() {
        let mut r = request();
        r.messages[0].content.push(ContentBlock::Image {
            blob: BlobRef::from_bytes([0; 32]),
            media_type: "image/png".into(),
        });
        assert!(matches!(
            build_responses_body(&r, &OpenAiSettings::default()),
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[test]
    fn an_output_schema_becomes_a_strict_json_schema_text_format() {
        let schema = json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false});
        let mut r = request();
        r.output_schema = Some(schema.clone());
        let b = build_responses_body(&r, &OpenAiSettings::default()).unwrap();
        assert_eq!(
            b["text"],
            json!({"format": {"type": "json_schema", "name": "harness_output", "schema": schema, "strict": true}})
        );
        assert!(
            build_responses_body(&request(), &OpenAiSettings::default())
                .unwrap()
                .get("text")
                .is_none()
        );
    }
}
