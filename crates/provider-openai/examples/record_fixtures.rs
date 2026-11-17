//! Records real Responses API responses for the offline replay tests (P1-T4).
//!
//! ```sh
//! OPENAI_API_KEY="$(secret-tool lookup service harness username openai)" OPENAI_MODEL=<model-id> \
//!   cargo run -p harness-provider-openai --example record_fixtures
//! ```
//!
//! Writes response bodies only (never request headers) to
//! `fixtures/streams/openai/`. The follow-up request replays the recorded tool
//! turn, including its encrypted reasoning item, to prove reasoning round-trips.
#![allow(clippy::print_stdout, clippy::print_stderr, missing_docs)]

use std::error::Error;
use std::path::{Path, PathBuf};

use harness_ports::{
    CachePlan, Effort, ModelRequest, ProviderEvent, SystemPart, SystemSlot, ToolSpec,
};
use harness_provider_openai::{OpenAiSettings, ResponsesDecoder, build_responses_body};
use harness_types::{ContentBlock, Message, Role};
use serde_json::Value;

fn request(
    model: &str,
    messages: Vec<Message>,
    tools: Vec<ToolSpec>,
    effort: Effort,
    show_thinking: bool,
) -> ModelRequest {
    ModelRequest {
        model: model.into(),
        system: vec![SystemPart { slot: SystemSlot::Base, text: "You are a test assistant for the harness fixture recorder. Follow instructions exactly.".into() }],
        tools,
        messages,
        max_output: 4_096,
        effort: Some(effort),
        cache_plan: CachePlan::default(),
        output_schema: None,
        show_thinking,
    }
}

struct Recorder {
    client: reqwest::Client,
    key: String,
    settings: OpenAiSettings,
    dir: PathBuf,
}

impl Recorder {
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(u16, String), Box<dyn Error>> {
        let response = request.bearer_auth(&self.key).send().await?;
        let status = response.status().as_u16();
        Ok((status, response.text().await?))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<(u16, String), Box<dyn Error>> {
        self.send(
            self.client
                .post(format!("{}{path}", self.settings.base_url))
                .json(body),
        )
        .await
    }

    async fn get(&self, path: &str) -> Result<(u16, String), Box<dyn Error>> {
        self.send(self.client.get(format!("{}{path}", self.settings.base_url)))
            .await
    }

    fn save(
        &self,
        name: &str,
        status: u16,
        text: &str,
        expect_ok: bool,
    ) -> Result<(), Box<dyn Error>> {
        if expect_ok && status != 200 {
            return Err(format!("{name}: HTTP {status}: {text}").into());
        }
        std::fs::write(self.dir.join(name), text)?;
        println!("recorded {name:<36} HTTP {status}  {} bytes", text.len());
        Ok(())
    }
}

/// Parses an SSE body into (event, data) pairs.
fn sse_events(body: &str) -> Vec<(String, String)> {
    body.split("\n\n")
        .filter_map(|frame| {
            let mut event = String::new();
            let mut data = String::new();
            for line in frame.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = v.trim().to_owned();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push_str(v.trim_start());
                }
            }
            (!data.is_empty()).then_some((event, data))
        })
        .collect()
}

/// Decodes a recorded stream into the completed content blocks.
fn blocks(body: &str) -> Result<Vec<ContentBlock>, Box<dyn Error>> {
    let mut decoder = ResponsesDecoder::new();
    let mut out = Vec::new();
    for (event, data) in sse_events(body) {
        for e in decoder.feed(&event, &data)? {
            if let ProviderEvent::BlockComplete { block } = e {
                out.push(block);
            }
        }
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let key =
        std::env::var("OPENAI_API_KEY").map_err(|_| "set OPENAI_API_KEY (see the file header)")?;
    let model = std::env::var("OPENAI_MODEL")
        .map_err(|_| "set OPENAI_MODEL to the model id to record with")?;
    if key.trim().is_empty() {
        return Err("OPENAI_API_KEY is empty".into());
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/streams/openai");
    std::fs::create_dir_all(&dir)?;
    let r = Recorder {
        client: reqwest::Client::new(),
        key: key.trim().to_owned(),
        settings: OpenAiSettings::default(),
        dir,
    };
    let tools = harness_tools::builtin_registry()?.specs();

    // 1. Plain text reply.
    let text = request(
        &model,
        vec![Message::user_text(
            "Reply with exactly: Hello from harness.",
        )],
        vec![],
        Effort::Low,
        false,
    );
    let (status, body) = r
        .post("/v1/responses", &build_responses_body(&text, &r.settings)?)
        .await?;
    r.save("text_reply.sse", status, &body, true)?;

    // 2. A function call.
    let prompt = Message::user_text("Use the shell tool to run exactly this command: echo fixture");
    let tool = request(
        &model,
        vec![prompt.clone()],
        tools.clone(),
        Effort::Low,
        false,
    );
    let (status, tool_body) = r
        .post("/v1/responses", &build_responses_body(&tool, &r.settings)?)
        .await?;
    r.save("tool_use.sse", status, &tool_body, true)?;

    // 3. Follow-up with the function output, replaying turn 2 (reasoning included) verbatim.
    let assistant = blocks(&tool_body)?;
    let call_id = assistant
        .iter()
        .find_map(|b| match b {
            ContentBlock::ToolCall { id, .. } => Some(id.clone()),
            _ => None,
        })
        .ok_or("the tool_use fixture contains no function call")?;
    let result = ContentBlock::ToolResult {
        call_id,
        content: vec![ContentBlock::text("fixture\n[exit code 0]")],
        is_error: false,
    };
    let followup = request(
        &model,
        vec![
            prompt,
            Message::new(Role::Assistant, assistant),
            Message::new(Role::User, vec![result]),
        ],
        tools,
        Effort::Low,
        false,
    );
    let (status, body) = r
        .post(
            "/v1/responses",
            &build_responses_body(&followup, &r.settings)?,
        )
        .await?;
    r.save("tool_followup.sse", status, &body, true)?;

    // 4. Reasoning summary.
    let reasoning = request(
        &model,
        vec![Message::user_text(
            "Is 1001 a prime number? Think it through, then answer in one sentence.",
        )],
        vec![],
        Effort::High,
        true,
    );
    let (status, body) = r
        .post(
            "/v1/responses",
            &build_responses_body(&reasoning, &r.settings)?,
        )
        .await?;
    r.save("reasoning_summary.sse", status, &body, true)?;

    // 5. Model lookup.
    let (status, body) = r.get(&format!("/v1/models/{model}")).await?;
    r.save("model.json", status, &body, true)?;

    // 6. An error response (unknown model; costs nothing).
    let mut bad = text.clone();
    bad.model = "model-that-does-not-exist".into();
    let (status, body) = r
        .post("/v1/responses", &build_responses_body(&bad, &r.settings)?)
        .await?;
    r.save(&format!("error_{status}.json"), status, &body, false)?;
    Ok(())
}
