//! Replays real Responses API recordings (`fixtures/streams/openai/`, recorded
//! with gpt-6-luna by `examples/record_openai_fixtures.rs`) through the provider.

// Test-only file: helpers may unwrap (clippy's test exemption covers only #[test] fns).
#![allow(clippy::unwrap_used)]

use futures::StreamExt;
use harness_ports::{CachePlan, Effort, ModelRequest, Provider, ProviderError, ProviderEvent};
use harness_provider_openai::{ApiKey, OpenAiProvider, OpenAiSettings};
use harness_types::{ContentBlock, Message, Role, StopReason};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/../../fixtures/streams/openai/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn request(messages: Vec<Message>) -> ModelRequest {
    ModelRequest {
        model: "gpt-6-luna".into(),
        system: vec![],
        tools: harness_tools::builtin_registry().unwrap().specs(),
        messages,
        max_output: 4_096,
        effort: Some(Effort::Low),
        cache_plan: CachePlan::default(),
        output_schema: None,
        show_thinking: false,
    }
}

async fn serve(name: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(fixture(name)),
        )
        .mount(&server)
        .await;
    server
}

fn provider(server: &MockServer) -> OpenAiProvider {
    OpenAiProvider::new(
        OpenAiSettings {
            base_url: server.uri(),
            ..OpenAiSettings::default()
        },
        ApiKey::new("test"),
    )
    .unwrap()
}

async fn replay(name: &str, messages: Vec<Message>) -> Vec<ProviderEvent> {
    let server = serve(name).await;
    provider(&server)
        .stream(request(messages), CancellationToken::new())
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await
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

fn stop(events: &[ProviderEvent]) -> StopReason {
    match events.last() {
        Some(ProviderEvent::Stop { reason }) => reason.clone(),
        other => panic!("stream did not end with Stop: {other:?}"),
    }
}

#[tokio::test]
async fn text_reply_decodes_to_one_text_block() {
    let events = replay(
        "text_reply.sse",
        vec![Message::user_text(
            "Reply with exactly: Hello from harness.",
        )],
    )
    .await;
    assert_eq!(stop(&events), StopReason::EndTurn);
    match blocks(&events).as_slice() {
        [ContentBlock::Text { text }] => assert!(text.contains("Hello from harness"), "{text}"),
        other => panic!("{other:?}"),
    }
    let streamed: String = events
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(streamed.contains("Hello from harness"));
    assert!(events.iter().any(|e| matches!(e, ProviderEvent::Usage { usage } if usage.total_input() > 0 && usage.output > 0)));
}

#[tokio::test]
async fn tool_use_decodes_the_shell_call() {
    let events = replay(
        "tool_use.sse",
        vec![Message::user_text(
            "Use the shell tool to run exactly this command: echo fixture",
        )],
    )
    .await;
    assert_eq!(stop(&events), StopReason::ToolUse);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProviderEvent::ToolCallStart { name, .. } if name == "shell"))
    );
    let calls: Vec<(String, Value)> = blocks(&events)
        .into_iter()
        .filter_map(|b| match b {
            ContentBlock::ToolCall { name, input, .. } => Some((name, input)),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        vec![("shell".to_owned(), json!({"command": "echo fixture"}))]
    );
}

#[tokio::test]
async fn followup_after_a_tool_result_ends_the_turn() {
    let events = replay("tool_followup.sse", vec![Message::user_text("x")]).await;
    assert_eq!(stop(&events), StopReason::EndTurn);
    assert!(
        matches!(blocks(&events).as_slice(), [ContentBlock::Text { text }] if !text.is_empty())
    );
}

#[tokio::test]
async fn reasoning_summaries_stream_and_reasoning_items_are_kept_verbatim() {
    let events = replay(
        "reasoning_summary.sse",
        vec![Message::user_text("Is 1001 prime?")],
    )
    .await;
    assert_eq!(stop(&events), StopReason::EndTurn);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProviderEvent::ThinkingDelta { text } if !text.is_empty()))
    );
    let b = blocks(&events);
    let reasoning = b.iter().find_map(|block| match block {
        ContentBlock::Opaque { provider, raw, .. }
            if provider == "openai" && raw["type"] == "reasoning" =>
        {
            Some(raw.clone())
        }
        _ => None,
    });
    let reasoning = reasoning.expect("a reasoning item");
    assert!(
        reasoning["encrypted_content"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(
        b.iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if !text.is_empty()))
    );
}

#[tokio::test]
async fn replaying_a_recorded_turn_sends_call_and_output_items() {
    // Decode the recorded tool turn, then send it back with a result, as the runtime does.
    let prompt = Message::user_text("Use the shell tool to run exactly this command: echo fixture");
    let assistant = blocks(&replay("tool_use.sse", vec![prompt.clone()]).await);
    let call_id = assistant
        .iter()
        .find_map(|b| match b {
            ContentBlock::ToolCall { id, .. } => Some(id.clone()),
            _ => None,
        })
        .unwrap();
    let result = ContentBlock::ToolResult {
        call_id: call_id.clone(),
        content: vec![ContentBlock::text("fixture\n[exit code 0]")],
        is_error: false,
    };

    let server = serve("tool_followup.sse").await;
    let messages = vec![
        prompt,
        Message::new(Role::Assistant, assistant),
        Message::new(Role::User, vec![result]),
    ];
    let _: Vec<_> = provider(&server)
        .stream(request(messages), CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await;

    let body: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    let input = body["input"].as_array().unwrap();
    assert!(
        input
            .iter()
            .any(|i| i["type"] == "function_call" && i["call_id"] == call_id.as_str())
    );
    assert!(input.iter().any(|i| i["type"] == "function_call_output"
        && i["call_id"] == call_id.as_str()
        && i["output"] == "fixture\n[exit code 0]"));
}

#[tokio::test]
async fn recorded_model_lookup_and_error_are_understood() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/gpt-6-luna"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("model.json")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(404).set_body_string(fixture("error_404.json")))
        .mount(&server)
        .await;
    let p = provider(&server);
    assert!(p.capabilities("gpt-6-luna").await.is_ok());
    let err = p
        .stream(
            request(vec![Message::user_text("x")]),
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&err, ProviderError::InvalidRequest { status: 404, message } if message.contains("does not exist")),
        "{err:?}"
    );
}
