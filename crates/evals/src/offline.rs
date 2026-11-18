//! [`OfflineModel`]: a deterministic stand-in for a model, so the evals run in
//! the test suite without keys or network.
//!
//! It is honest about what it was shown: it answers a question only from the
//! text of the request it receives, and when asked to compact it pins the
//! facts it finds in the segment (exact quotes, as a careful model would). An
//! offline score therefore measures the harness: whether pinned facts survive
//! compactions and reach the next request. It says nothing about a real
//! model's judgement; the live runs measure that.

use async_trait::async_trait;
use futures::stream;
use harness_ports::{
    CacheStyle, Capabilities, ModelRequest, Provider, ProviderError, ProviderEvent, ProviderStream,
};
use harness_types::{ContentBlock, StopReason, Usage};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// A deterministic model for offline evals.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OfflineModel;

fn request_text(request: &ModelRequest) -> String {
    let mut text: Vec<String> = request.system.iter().map(|p| p.text.clone()).collect();
    for message in &request.messages {
        for block in &message.content {
            match block {
                ContentBlock::Text { text: t } => text.push(t.clone()),
                ContentBlock::ToolResult { content, .. } => {
                    for c in content {
                        if let ContentBlock::Text { text: t } = c {
                            text.push(t.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    text.join("\n")
}

fn tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// The facts in a line: the text from a marker to the next full stop.
fn facts_in(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in ["the deploy code for service ", "the "] {
        let mut rest = line;
        while let Some(at) = rest.find(marker) {
            let tail = &rest[at..];
            // A fact ends at a full stop followed by a space (or the line's end),
            // so "TLS 1.3" stays whole.
            let end = tail
                .find(". ")
                .unwrap_or_else(|| tail.trim_end().trim_end_matches('.').len());
            let fact = &tail[..end];
            let keep = if marker == "the " {
                fact.contains(" must use ")
            } else {
                true
            };
            if keep && !out.iter().any(|f: &String| f == fact) {
                out.push(fact.to_owned());
            }
            rest = &tail[end.max(1)..];
        }
    }
    out.retain(|f| f.starts_with("the deploy code") || f.contains(" must use "));
    out
}

/// The compaction reply: pins every fact line of the segment with its event.
fn compaction(request: &ModelRequest) -> String {
    let asked = request
        .messages
        .first()
        .map(|m| m.text())
        .unwrap_or_default();
    let mut pinned = Vec::new();
    let mut in_segment = false;
    for line in asked.lines() {
        if line == "<segment>" {
            in_segment = true;
            continue;
        }
        if !in_segment || !line.starts_with('#') {
            continue;
        }
        let Some((number, rest)) = line[1..].split_once(' ') else {
            continue;
        };
        let Ok(seq) = number.parse::<u64>() else {
            continue;
        };
        // Facts in the opener of a later segment were pinned before.
        if rest.contains("[p") {
            continue;
        }
        for fact in facts_in(rest) {
            pinned.push(json!({"quote": fact, "seq": seq, "kind": "id"}));
        }
    }
    json!({
        "goal": "Keep track of the facts the user gives and answer questions about them.",
        "state_of_work": "Facts were noted and texts summarised.",
        "decisions": [],
        "pinned": pinned,
        "superseded_pins": [],
        "files_touched": [],
        "open_questions": [],
        "next_steps": ["answer the user's next question"],
        "anchors": []
    })
    .to_string()
}

/// The answer to the last user message, from the request text only.
fn answer(request: &ModelRequest) -> String {
    let last = request
        .messages
        .last()
        .map(|m| m.text())
        .unwrap_or_default();
    let Some(service) = last
        .split("What is the deploy code for service ")
        .nth(1)
        .and_then(|rest| rest.split('?').next())
    else {
        return if last.starts_with("Summarise") {
            "A team discussed an engineering trade-off.".into()
        } else {
            "OK.".into()
        };
    };
    let everything = request_text(request);
    let marker = format!("deploy code for service {service} is ");
    everything
        .match_indices(&marker)
        .filter_map(|(at, _)| {
            everything[at + marker.len()..]
                .split(|c: char| c.is_whitespace() || c == '.')
                .next()
                .map(str::to_owned)
        })
        .find(|code| !code.is_empty())
        .unwrap_or_else(|| "I do not know.".into())
}

#[async_trait]
impl Provider for OfflineModel {
    fn id(&self) -> &str {
        "offline"
    }

    async fn capabilities(&self, _: &str) -> Result<Capabilities, ProviderError> {
        Ok(Capabilities {
            context_window: 200_000,
            max_output: 16_000,
            vision: false,
            pdf: false,
            parallel_tools: true,
            mid_conversation_system: false,
            preserved_reasoning: false,
            cache: CacheStyle::Explicit { max_breakpoints: 4 },
            structured_output: true,
            native_compaction: false,
            native_context_editing: false,
        })
    }

    async fn stream(
        &self,
        request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        Ok(reply(&request))
    }

    async fn count_tokens(&self, request: &ModelRequest) -> Result<u64, ProviderError> {
        Ok(tokens(&request_text(request)))
    }
}

/// The whole reply as one stream: text, usage (the request's real size, so
/// budgets trigger as they would live), end of turn.
fn reply(request: &ModelRequest) -> ProviderStream {
    let text = if request.output_schema.is_some() {
        compaction(request)
    } else {
        answer(request)
    };
    let usage = Usage {
        input: tokens(&request_text(request)),
        output: tokens(&text),
        ..Usage::default()
    };
    Box::pin(stream::iter(vec![
        Ok(ProviderEvent::BlockComplete {
            block: ContentBlock::text(text),
        }),
        Ok(ProviderEvent::Usage { usage }),
        Ok(ProviderEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_ports::CachePlan;
    use harness_types::{Message, Role};

    fn request(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "offline".into(),
            system: vec![],
            tools: vec![],
            messages,
            max_output: 100,
            effort: None,
            cache_plan: CachePlan::default(),
            output_schema: None,
            show_thinking: false,
        }
    }

    #[test]
    fn answers_only_from_what_it_was_shown() {
        let shown = request(vec![
            Message::user_text("the deploy code for service Falcon is 1234-AB. Reply with OK."),
            Message::new(Role::Assistant, vec![ContentBlock::text("OK.")]),
            Message::user_text(
                "What is the deploy code for service Falcon? Reply with the code only.",
            ),
        ]);
        assert_eq!(answer(&shown), "1234-AB");
        let forgotten = request(vec![Message::user_text(
            "What is the deploy code for service Falcon? Reply with the code only.",
        )]);
        assert_eq!(answer(&forgotten), "I do not know.");
    }

    #[test]
    fn compaction_pins_fact_lines_with_their_events() {
        let mut r = request(vec![Message::user_text(
            "Compact events #1 to #9.\n\n<segment>\n#4 user: Note: the deploy code for service Harbor is 0042-ZZ. Reply with OK.\n#6 | Requirement 2: the audit log #2 must use TLS 1.3. Keep this in mind.\n</segment>",
        )]);
        r.output_schema = Some(json!({}));
        let out: serde_json::Value = serde_json::from_str(&compaction(&r)).unwrap();
        let pins = out["pinned"].as_array().unwrap();
        assert_eq!(pins.len(), 2, "{pins:?}");
        assert_eq!(
            pins[0]["quote"],
            "the deploy code for service Harbor is 0042-ZZ"
        );
        assert_eq!(pins[0]["seq"], 4);
        assert_eq!(pins[1]["quote"], "the audit log #2 must use TLS 1.3");
        assert_eq!(pins[1]["seq"], 6);
    }
}
