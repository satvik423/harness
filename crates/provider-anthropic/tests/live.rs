//! Opt-in live smoke test (P5-T11): replays a conversation with its thinking
//! blocks exactly, with `prefix_mismatch_behavior: "error"`, so any edit to the
//! history makes the API fail the request instead of dropping blocks.
//!
//! Needs `ANTHROPIC_API_KEY` and costs a few cents:
//!
//! ```sh
//! ANTHROPIC_API_KEY=… cargo test -p harness-provider-anthropic --test live -- --ignored
//! ```

use futures::StreamExt as _;
use harness_ports::{ApiKey, CachePlan, Effort, ModelRequest, Provider, ProviderEvent};
use harness_provider_anthropic::{AnthropicProvider, AnthropicSettings, ThinkingMismatch};
use harness_types::{ContentBlock, Message, Role, StopReason};
use tokio_util::sync::CancellationToken;

async fn reply(
    provider: &AnthropicProvider,
    messages: Vec<Message>,
) -> (Vec<ContentBlock>, StopReason) {
    let request = ModelRequest {
        model: "claude-opus-5-5".into(),
        system: vec![],
        tools: vec![],
        messages,
        max_output: 2_000,
        effort: Some(Effort::Low),
        cache_plan: CachePlan::default(),
        output_schema: None,
        show_thinking: true,
    };
    let mut stream = provider
        .stream(request, CancellationToken::new())
        .await
        .expect("the request starts");
    let mut blocks = Vec::new();
    let mut stop = StopReason::Other { raw: "none".into() };
    while let Some(event) = stream.next().await {
        match event.expect("no stream error: a history edit would fail here") {
            ProviderEvent::BlockComplete { block } => blocks.push(block),
            ProviderEvent::Stop { reason } => stop = reason,
            _ => {}
        }
    }
    (blocks, stop)
}

#[tokio::test]
#[ignore = "live: needs ANTHROPIC_API_KEY and costs a few cents"]
async fn thinking_blocks_replay_exactly_with_mismatches_as_errors() {
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY is set");
    let settings = AnthropicSettings {
        thinking_mismatch: ThinkingMismatch::Error,
        ..AnthropicSettings::default()
    };
    let provider = AnthropicProvider::new(settings, ApiKey::new(key)).unwrap();
    let first = Message::user_text("Think it through, then answer with one number: 17 × 23?");
    let (blocks, stop) = reply(&provider, vec![first.clone()]).await;
    assert_eq!(stop, StopReason::EndTurn);
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Opaque { .. })),
        "the reply carries a thinking block to replay"
    );
    let (again, stop) = reply(
        &provider,
        vec![
            first,
            Message::new(Role::Assistant, blocks),
            Message::user_text("Now double it. One number."),
        ],
    )
    .await;
    assert_eq!(stop, StopReason::EndTurn);
    assert!(
        again
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("782")))
    );
}
