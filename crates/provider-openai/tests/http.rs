//! The OpenAI provider's HTTP behaviour against a mock server (no network, no API key).

// Test-only file: helpers may unwrap (clippy's test exemption covers only #[test] fns).
#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use futures::StreamExt;
use harness_ports::{CachePlan, ModelRequest, Provider, ProviderError, ProviderEvent};
use harness_provider_openai::{ApiKey, OpenAiProvider, OpenAiSettings};
use harness_types::{ContentBlock, Message, StopReason};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TEXT_SSE: &str = "event: response.created\n\
data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-test\",\"status\":\"in_progress\"}}\n\n\
event: response.output_text.delta\n\
data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"Hi\"}\n\n\
event: response.output_item.done\n\
data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hi\"}]}}\n\n\
event: response.completed\n\
data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2}}}\n\n";

fn request() -> ModelRequest {
    ModelRequest {
        model: "gpt-test".into(),
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

fn provider(server: &MockServer) -> OpenAiProvider {
    OpenAiProvider::new(
        OpenAiSettings {
            base_url: server.uri(),
            ..OpenAiSettings::default()
        },
        ApiKey::new("test-key"),
    )
    .unwrap()
}

fn sse(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
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

async fn collect(p: &OpenAiProvider) -> Vec<Result<ProviderEvent, ProviderError>> {
    p.stream(request(), CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await
}

#[tokio::test]
async fn streams_a_response_with_bearer_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer test-key"))
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
    let body: serde_json::Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(
        (body["stream"].clone(), body["store"].clone()),
        (serde_json::json!(true), serde_json::json!(false))
    );
}

#[tokio::test]
async fn transient_errors_are_retried() {
    let server = MockServer::start().await;
    let busy = ResponseTemplate::new(503)
        .insert_header("retry-after-ms", "10")
        .set_body_string("{}");
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(Sequence(std::sync::Mutex::new(vec![busy, sse(TEXT_SSE)])))
        .expect(2)
        .mount(&server)
        .await;
    assert!(collect(&provider(&server)).await.iter().all(Result::is_ok));
}

#[tokio::test]
async fn exhausted_quota_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(429).set_body_string(
            r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let err = provider(&server)
        .stream(request(), CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("quota"), "{err}");
}

#[tokio::test]
async fn a_stream_cut_short_is_an_error() {
    let server = MockServer::start().await;
    let cut = &TEXT_SSE[..TEXT_SSE.find("event: response.completed").unwrap()];
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(sse(cut))
        .mount(&server)
        .await;
    assert!(matches!(
        collect(&provider(&server)).await.last(),
        Some(Err(ProviderError::Network(_)))
    ));
}

#[tokio::test]
async fn cancellation_ends_the_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
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
    assert!(matches!(
        provider(&server).stream(request(), cancel).await,
        Err(ProviderError::Cancelled)
    ));
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn capabilities_check_that_the_model_exists() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/gpt-test"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"id":"gpt-test","object":"model","owned_by":"openai"}"#),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models/nope"))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            r#"{"error":{"message":"The model `nope` does not exist","code":"model_not_found"}}"#,
        ))
        .mount(&server)
        .await;
    let p = provider(&server);
    assert!(
        p.capabilities("gpt-test")
            .await
            .unwrap()
            .preserved_reasoning
    );
    assert!(matches!(
        p.capabilities("nope").await,
        Err(ProviderError::InvalidRequest { status: 404, .. })
    ));
}

#[tokio::test]
async fn count_tokens_is_a_local_estimate() {
    let server = MockServer::start().await;
    let p = provider(&server);
    assert_eq!(
        p.count_tokens(&request()).await.unwrap(),
        2,
        "\"hello\" is 5 chars, so 2 tokens"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn provider_id_is_openai() {
    let p = OpenAiProvider::new(OpenAiSettings::default(), ApiKey::new("k")).unwrap();
    assert_eq!(p.id(), "openai");
}
