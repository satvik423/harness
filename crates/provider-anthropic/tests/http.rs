//! The provider's HTTP behaviour against a mock server (no network, no API key).

// Test-only file: helpers may unwrap (clippy's test exemption covers only #[test] fns).
#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use futures::StreamExt;
use harness_ports::{CachePlan, ModelRequest, Provider, ProviderError, ProviderEvent};
use harness_provider_anthropic::{AnthropicProvider, AnthropicSettings, ApiKey};
use harness_types::{ContentBlock, Message, StopReason};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TEXT_SSE: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-opus-5-5\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

fn request() -> ModelRequest {
    ModelRequest {
        model: "claude-opus-5-5".into(),
        system: vec![],
        tools: vec![],
        messages: vec![Message::user_text("hello")],
        max_output: 1024,
        effort: None,
        cache_plan: CachePlan::default(),
        output_schema: None,
        show_thinking: false,
    }
}

fn provider(server: &MockServer) -> AnthropicProvider {
    let settings = AnthropicSettings {
        base_url: server.uri(),
        ..AnthropicSettings::default()
    };
    AnthropicProvider::new(settings, ApiKey::new("test-key")).unwrap()
}

fn sse(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

async fn collect(p: &AnthropicProvider) -> Vec<Result<ProviderEvent, ProviderError>> {
    p.stream(request(), CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await
}

/// Responds with each template in turn, then repeats the last.
struct Sequence(std::sync::Mutex<Vec<ResponseTemplate>>);

impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let mut list = self.0.lock().unwrap();
        if list.len() > 1 {
            list.remove(0)
        } else {
            list[0].clone()
        }
    }
}

#[tokio::test]
async fn streams_a_reply_with_the_required_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2026-10-21"))
        .respond_with(sse(TEXT_SSE))
        .expect(1)
        .mount(&server)
        .await;
    let events: Vec<ProviderEvent> = collect(&provider(&server))
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert!(events.contains(&ProviderEvent::BlockComplete {
        block: ContentBlock::text("Hi")
    }));
    assert_eq!(
        events.last(),
        Some(&ProviderEvent::Stop {
            reason: StopReason::EndTurn
        })
    );

    let received = &server.received_requests().await.unwrap()[0];
    let beta = received
        .headers
        .get("anthropic-beta")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        beta.contains("thinking-binding-controls-2026-08-01"),
        "{beta}"
    );
    let body: serde_json::Value = serde_json::from_slice(&received.body).unwrap();
    assert_eq!(body["stream"], true);
}

#[tokio::test]
async fn overloaded_then_ok_is_retried() {
    let server = MockServer::start().await;
    let overloaded = ResponseTemplate::new(529)
        .insert_header("retry-after-ms", "10")
        .set_body_string(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Sequence(std::sync::Mutex::new(vec![
            overloaded,
            sse(TEXT_SSE),
        ])))
        .expect(2)
        .mount(&server)
        .await;
    let events = collect(&provider(&server)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
}

#[tokio::test]
async fn retry_after_is_honoured() {
    let server = MockServer::start().await;
    let limited = ResponseTemplate::new(429)
        .insert_header("retry-after-ms", "300")
        .set_body_string("{}");
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Sequence(std::sync::Mutex::new(vec![
            limited,
            sse(TEXT_SSE),
        ])))
        .mount(&server)
        .await;
    let start = Instant::now();
    let _ = collect(&provider(&server)).await;
    assert!(start.elapsed() >= Duration::from_millis(300));
}

#[tokio::test]
async fn auth_errors_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_string(
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let err = provider(&server)
        .stream(request(), CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert_eq!(err, ProviderError::Auth("invalid x-api-key".into()));
}

#[tokio::test]
async fn persistent_server_errors_give_up_after_max_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(500)
                .insert_header("retry-after-ms", "1")
                .set_body_string("boom"),
        )
        .expect(3)
        .mount(&server)
        .await;
    let settings = AnthropicSettings {
        base_url: server.uri(),
        max_attempts: 3,
        ..AnthropicSettings::default()
    };
    let p = AnthropicProvider::new(settings, ApiKey::new("k")).unwrap();
    assert!(matches!(
        p.stream(request(), CancellationToken::new()).await,
        Err(ProviderError::Server { status: 500, .. })
    ));
}

#[tokio::test]
async fn a_stream_cut_short_is_an_error() {
    let server = MockServer::start().await;
    let cut = &TEXT_SSE[..TEXT_SSE.find("event: message_delta").unwrap()];
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse(cut))
        .mount(&server)
        .await;
    let events = collect(&provider(&server)).await;
    assert!(
        matches!(events.last(), Some(Err(ProviderError::Network(_)))),
        "{events:?}"
    );
}

#[tokio::test]
async fn cancellation_ends_the_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse(TEXT_SSE).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.cancel();
    });
    let start = Instant::now();
    let result = provider(&server).stream(request(), cancel).await;
    assert!(
        matches!(result, Err(ProviderError::Cancelled)),
        "{:?}",
        result.err()
    );
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn count_tokens_reads_input_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages/count_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"input_tokens": 1234}"#))
        .mount(&server)
        .await;
    assert_eq!(
        provider(&server).count_tokens(&request()).await.unwrap(),
        1234
    );
    let body: serde_json::Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert!(body.get("stream").is_none() && body.get("max_tokens").is_none());
}

#[tokio::test]
async fn capabilities_come_from_the_models_endpoint_and_are_cached() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/claude-opus-5-5"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"id":"claude-opus-5-5","type":"model","max_input_tokens":1000000,"max_tokens":128000}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let p = provider(&server);
    let caps = p.capabilities("claude-opus-5-5").await.unwrap();
    assert_eq!((caps.context_window, caps.max_output), (1_000_000, 128_000));
    let _ = p.capabilities("claude-opus-5-5").await.unwrap();
}

#[test]
fn provider_id_is_anthropic() {
    let p = AnthropicProvider::new(AnthropicSettings::default(), ApiKey::new("k")).unwrap();
    assert_eq!(p.id(), "anthropic");
}
