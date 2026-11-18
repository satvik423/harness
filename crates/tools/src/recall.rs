//! The `recall` tool (FR-CTX-04, Tech spec 3.13): fetches earlier events by
//! anchor, event range or search query, in token-bounded pages.

use async_trait::async_trait;
use harness_ports::{
    HistoryError, RecallSelector, RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput,
    ToolSpec, wrap_untrusted,
};
use harness_types::{ContentBlock, Risk, Seq};
use serde::Deserialize;
use serde_json::{Value, json};

/// Default page size in tokens.
pub const DEFAULT_RECALL_TOKENS: u64 = 2_000;
/// Largest page size in tokens.
pub const MAX_RECALL_TOKENS: u64 = 8_000;
/// Default number of search matches.
const DEFAULT_LIMIT: usize = 5;
/// Most search matches.
const MAX_LIMIT: usize = 20;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    anchor: Option<String>,
    seq_from: Option<u64>,
    seq_to: Option<u64>,
    query: Option<String>,
    limit: Option<usize>,
    max_tokens: Option<u64>,
    page: Option<usize>,
}

/// Brings back exact earlier events after a compaction, or the full output of
/// a trimmed tool result. Read-only.
#[derive(Debug, Clone, Copy, Default)]
pub struct Recall;

fn tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// Splits `lines` into pages of at most `max` estimated tokens, cutting a
/// line that is longer than a page.
fn pages(lines: &[String], max: u64) -> Vec<Vec<String>> {
    let max_chars = usize::try_from(max.saturating_mul(4))
        .unwrap_or(usize::MAX)
        .max(1);
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut size = 0;
    for line in lines {
        let chars: Vec<char> = line.chars().collect();
        for piece in chars.chunks(max_chars) {
            let piece: String = piece.iter().collect();
            let t = tokens(&piece) + 1;
            if size + t > max && !current.is_empty() {
                out.push(std::mem::take(&mut current));
                size = 0;
            }
            size += t;
            current.push(piece);
        }
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

impl Recall {
    fn selector(input: &Input) -> Result<(RecallSelector, Value), ToolError> {
        let given = usize::from(input.anchor.is_some())
            + usize::from(input.seq_from.is_some() || input.seq_to.is_some())
            + usize::from(input.query.is_some());
        if given != 1 {
            return Err(ToolError::InvalidInput(
                "give exactly one of: anchor, seq_from/seq_to, query".into(),
            ));
        }
        if let Some(anchor) = &input.anchor {
            return Ok((
                RecallSelector::Anchor(anchor.trim().to_owned()),
                json!({"anchor": anchor.trim()}),
            ));
        }
        if let Some(query) = &input.query {
            let limit = input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
            if query.trim().is_empty() {
                return Err(ToolError::InvalidInput("the query is empty".into()));
            }
            return Ok((
                RecallSelector::Query {
                    query: query.trim().to_owned(),
                    limit,
                },
                json!({"query": query.trim(), "limit": limit}),
            ));
        }
        let from = input.seq_from.or(input.seq_to).unwrap_or(0);
        let to = input.seq_to.unwrap_or(from);
        if from == 0 || to < from {
            return Err(ToolError::InvalidInput(
                "seq_from must be at least 1 and not after seq_to".into(),
            ));
        }
        Ok((
            RecallSelector::Range {
                from: Seq(from),
                to: Seq(to),
            },
            json!({"seq_from": from, "seq_to": to}),
        ))
    }
}

#[async_trait]
impl Tool for Recall {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("recall").unwrap_or_else(|_| unreachable!("valid literal")),
            description: format!(
                "Fetch exact earlier events of this session: by anchor (an [[aN ...]] entry in the \
                 compacted context), by event range (seq_from/seq_to, for example the full output of \
                 a trimmed tool result), or by search query over earlier segments. Give exactly one \
                 selector. Results come in pages of max_tokens (default {DEFAULT_RECALL_TOKENS}, at most \
                 {MAX_RECALL_TOKENS}); ask for the next page with page. Use it instead of guessing when \
                 you need a detail the summary does not give."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "anchor": {"type": "string", "description": "Anchor id, for example a2."},
                    "seq_from": {"type": "integer", "minimum": 1, "description": "First event number."},
                    "seq_to": {"type": "integer", "minimum": 1, "description": "Last event number (default: seq_from)."},
                    "query": {"type": "string", "description": "Words to search for in earlier segments."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": "Most search matches (default 5)."},
                    "max_tokens": {"type": "integer", "minimum": 100, "maximum": MAX_RECALL_TOKENS, "description": "Page size in tokens."},
                    "page": {"type": "integer", "minimum": 1, "description": "Page number (default 1)."}
                },
                "additionalProperties": false
            }),
            strict: false,
            eager_input_streaming: false,
        }
    }

    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        Risk::ReadOnly { paths: Vec::new() }
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    fn output_cap_tokens(&self) -> Option<u64> {
        // The page size is the cap; the header and footer need a little more.
        Some(MAX_RECALL_TOKENS + 200)
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let (selector, mut again) = Self::selector(&input)?;
        let max = input
            .max_tokens
            .unwrap_or(DEFAULT_RECALL_TOKENS)
            .clamp(100, MAX_RECALL_TOKENS);
        let recollection = ctx.history.recall(&selector).await.map_err(|e| match e {
            HistoryError::NotFound(m) => ToolError::Refused(m),
            other => ToolError::Failed(other.to_string()),
        })?;
        let pages = pages(&recollection.lines, max);
        let page = input.page.unwrap_or(1).max(1);
        let Some(lines) = pages.get(page - 1) else {
            return Err(ToolError::InvalidInput(format!(
                "page {page} does not exist; there are {} pages",
                pages.len()
            )));
        };
        let body = lines.join("\n");
        let used = tokens(&body);
        let mut text = format!(
            "[recall {} · page {page} of {} · {used} tokens]\n{body}",
            recollection.label,
            pages.len()
        );
        if page < pages.len() {
            if let Value::Object(map) = &mut again {
                map.insert("page".into(), json!(page + 1));
                if input.max_tokens.is_some() {
                    map.insert("max_tokens".into(), json!(max));
                }
            }
            text.push_str(&format!("\n[more: recall {again}]"));
        }
        ctx.history
            .recalled(recollection.anchor.clone(), used)
            .await;
        Ok(ToolOutput {
            content: vec![ContentBlock::text(wrap_untrusted("recall", &text))],
            summary: format!("{} · page {page} of {}", recollection.label, pages.len()),
            touched: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ctx;
    use harness_ports::{History, Recollection};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Scripted {
        asked: Mutex<Vec<RecallSelector>>,
        noted: Mutex<Vec<(Option<String>, u64)>>,
    }

    #[async_trait]
    impl History for Scripted {
        async fn recall(&self, s: &RecallSelector) -> Result<Recollection, HistoryError> {
            self.asked.lock().unwrap().push(s.clone());
            match s {
                RecallSelector::Anchor(a) if a == "a2" => Ok(Recollection {
                    label: "a2 · events 211-560".into(),
                    anchor: Some("a2".into()),
                    lines: (211..=260)
                        .map(|i| format!("#{i} user: {}", "w".repeat(150)))
                        .collect(),
                }),
                RecallSelector::Anchor(a) => Err(HistoryError::NotFound(format!("no anchor {a}"))),
                _ => Ok(Recollection {
                    label: "events 812-812".into(),
                    anchor: None,
                    lines: vec![
                        "#812 result shell: ERROR: column \"tenant_id\" does not exist".into(),
                    ],
                }),
            }
        }
        async fn recalled(&self, anchor: Option<String>, tokens: u64) {
            self.noted.lock().unwrap().push((anchor, tokens));
        }
    }

    async fn recall(input: Value) -> (Result<ToolOutput, ToolError>, Arc<Scripted>) {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, _) = ctx(dir.path());
        let history = Arc::new(Scripted::default());
        c.history = history.clone();
        (Recall.run(input, c).await, history)
    }

    fn text(out: &ToolOutput) -> String {
        match &out.content[0] {
            ContentBlock::Text { text } => text.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn an_anchor_comes_back_in_pages_with_a_next_page_hint() {
        let (out, history) = recall(json!({"anchor": "a2"})).await;
        let t = text(&out.unwrap());
        assert!(t.contains("[recall a2 · events 211-560 · page 1 of"), "{t}");
        assert!(t.contains("#211 user:"));
        assert!(
            t.contains(r#"[more: recall {"anchor":"a2","page":2}]"#),
            "{t}"
        );
        assert!(t.starts_with("<untrusted"), "{t}");
        let noted = history.noted.lock().unwrap().clone();
        assert_eq!(noted[0].0.as_deref(), Some("a2"));
        assert!(noted[0].1 <= DEFAULT_RECALL_TOKENS);

        let (out, _) = recall(json!({"anchor": "a2", "page": 2, "max_tokens": 8000})).await;
        assert!(
            out.is_err(),
            "one 8k page holds everything, so page 2 does not exist"
        );
    }

    #[tokio::test]
    async fn a_range_returns_the_exact_events() {
        let (out, history) = recall(json!({"seq_from": 812})).await;
        let t = text(&out.unwrap());
        assert!(t.contains("column \"tenant_id\" does not exist"));
        assert!(!t.contains("[more:"));
        assert_eq!(
            history.asked.lock().unwrap()[0],
            RecallSelector::Range {
                from: Seq(812),
                to: Seq(812)
            }
        );
    }

    #[tokio::test]
    async fn exactly_one_selector_is_required_and_unknown_anchors_are_refused() {
        for bad in [
            json!({}),
            json!({"anchor": "a1", "query": "x"}),
            json!({"seq_from": 9, "seq_to": 3}),
            json!({"query": "  "}),
        ] {
            let (out, _) = recall(bad.clone()).await;
            assert!(matches!(out, Err(ToolError::InvalidInput(_))), "{bad}");
        }
        let (out, _) = recall(json!({"anchor": "a9"})).await;
        assert_eq!(out, Err(ToolError::Refused("no anchor a9".into())));
    }

    #[test]
    fn long_lines_are_cut_across_pages() {
        let lines = vec!["x".repeat(1_000)];
        let p = pages(&lines, 100);
        assert!(p.len() >= 2);
        assert!(
            p.iter()
                .all(|page| page.iter().map(|l| tokens(l) + 1).sum::<u64>() <= 101)
        );
    }
}
