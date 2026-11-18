//! `harness debug last-request | diff-requests` (P4-T8, NFR-12): the exact
//! provider request bodies of a session, rebuilt from its event log, and the
//! append-only check made visible.

use std::fmt::Write as _;

use harness_config::Config;
use harness_ports::ModelRequest;
use harness_provider_anthropic::build_messages_body;
use harness_provider_openai::build_responses_body;
use similar::{ChangeTag, TextDiff};

use crate::run::{anthropic_settings, openai_settings};

/// The JSON body the provider received for `request`, pretty-printed and with
/// anything that looks like a key redacted.
///
/// # Errors
/// A message for an unknown provider or a body that cannot be built.
pub fn body(provider: &str, request: &ModelRequest, config: &Config) -> Result<String, String> {
    let value = match provider {
        "anthropic" => {
            build_messages_body(request, &anthropic_settings(&config.providers.anthropic))
        }
        "openai" => build_responses_body(request, &openai_settings(&config.providers.openai)),
        other => return Err(format!("unknown provider {other:?}")),
    }
    .map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    Ok(redact(&text))
}

/// Replaces anything shaped like an API key (`sk-…`, `sk-ant-…`, 20+ key
/// characters) with `sk-…[redacted]`. Bodies carry no credentials, but tool
/// output in a transcript might (guardrail 19).
///
/// # Examples
/// ```
/// let t = harness_cli::debug::redact("key sk-ant-api03-abcdefghijklmnopqrstuv here");
/// assert_eq!(t, "key sk-…[redacted] here");
/// ```
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("sk-") {
        let after = &rest[i + 3..];
        let len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
            .count();
        out.push_str(&rest[..i]);
        if len >= 20 {
            out.push_str("sk-…[redacted]");
        } else {
            out.push_str(&rest[i..i + 3 + len]);
        }
        rest = &after[len..];
    }
    out.push_str(rest);
    out
}

/// The two requests `diff-requests` compares: the last two of the newest
/// segment that has two. A compaction starts a new transcript, so requests of
/// different segments are never compared.
///
/// # Errors
/// A message explaining why there is nothing to compare.
#[allow(clippy::type_complexity)]
pub fn latest_pair(
    history: &[harness_runtime::RecordedRequest],
) -> Result<
    (
        &harness_runtime::RecordedRequest,
        &harness_runtime::RecordedRequest,
    ),
    String,
> {
    let Some(last) = history.last() else {
        return Err("this session has not sent a request yet".into());
    };
    let same: Vec<&harness_runtime::RecordedRequest> = history
        .iter()
        .filter(|r| r.segment == last.segment)
        .collect();
    match same.as_slice() {
        [.., older, newer] => Ok((older, newer)),
        _ if last.segment > 0 => Err(format!(
            "the last request (event #{}) is the first of segment {}: a compaction starts a new transcript, so there is no earlier request in this segment to compare it with",
            last.seq.0, last.segment
        )),
        _ => Err("this session has sent fewer than two requests".into()),
    }
}

/// A report comparing two consecutive requests: whether the newer one only
/// appends to the older one (I1), then a line diff of the bodies.
#[must_use]
pub fn diff_report(
    older: &ModelRequest,
    newer: &ModelRequest,
    older_body: &str,
    newer_body: &str,
) -> String {
    let mut out = String::new();
    let same_prefix =
        older.system == newer.system && older.tools == newer.tools && older.model == newer.model;
    let appended = newer.messages.len() >= older.messages.len()
        && newer.messages[..older.messages.len()] == older.messages[..];
    let verdict = if same_prefix && appended {
        format!(
            "append-only: yes — same system prompt, tools and model; the {} earlier messages are unchanged and {} were appended",
            older.messages.len(),
            newer.messages.len() - older.messages.len()
        )
    } else {
        let mut why = Vec::new();
        if older.system != newer.system {
            why.push("the system prompt changed");
        }
        if older.tools != newer.tools {
            why.push("the tools changed");
        }
        if older.model != newer.model {
            why.push("the model changed");
        }
        if !appended {
            why.push("earlier messages changed");
        }
        format!("append-only: NO — {}", why.join(", "))
    };
    let _ = writeln!(out, "{verdict}\n");
    let diff = TextDiff::from_lines(older_body, newer_body);
    for group in diff.grouped_ops(2) {
        for op in &group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    ChangeTag::Equal => ' ',
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                };
                let _ = write!(out, "{sign} {change}");
            }
        }
        out.push_str("  …\n");
    }
    out
}

#[cfg(test)]
mod tests {
    fn recorded(seq: u64, segment: u32) -> harness_runtime::RecordedRequest {
        harness_runtime::RecordedRequest {
            seq: harness_types::Seq(seq),
            segment,
            request: harness_ports::ModelRequest {
                model: "m".into(),
                system: vec![],
                tools: vec![],
                messages: vec![],
                max_output: 1,
                effort: None,
                cache_plan: harness_ports::CachePlan::default(),
                output_schema: None,
                show_thinking: false,
            },
        }
    }

    #[test]
    fn requests_of_different_segments_are_never_compared() {
        let history = vec![recorded(5, 0), recorded(9, 0), recorded(20, 1)];
        let message = latest_pair(&history).unwrap_err();
        assert!(message.contains("first of segment 1"), "{message}");
        let history = vec![
            recorded(5, 0),
            recorded(9, 0),
            recorded(20, 1),
            recorded(25, 1),
        ];
        let (a, b) = latest_pair(&history).unwrap();
        assert_eq!((a.seq.0, b.seq.0), (20, 25));
        assert!(
            latest_pair(&[recorded(5, 0)])
                .unwrap_err()
                .contains("fewer than two")
        );
        assert!(latest_pair(&[]).is_err());
    }

    use super::*;
    use harness_ports::{CachePlan, SystemPart, SystemSlot};
    use harness_types::{ContentBlock, Message, Role};

    fn request(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "gpt-6-luna".into(),
            system: vec![SystemPart {
                slot: SystemSlot::Base,
                text: "base".into(),
            }],
            tools: vec![],
            messages,
            max_output: 1_000,
            effort: None,
            cache_plan: CachePlan::default(),
            output_schema: None,
            show_thinking: false,
        }
    }

    #[test]
    fn keys_are_redacted_and_short_sk_words_kept() {
        assert_eq!(
            redact("sk-proj-ABCDEFGHIJKLMNOPQRSTUVWX1234"),
            "sk-…[redacted]"
        );
        assert_eq!(redact("task-sk-1 and sk-short"), "task-sk-1 and sk-short");
        assert_eq!(redact("no keys"), "no keys");
    }

    #[test]
    fn bodies_are_built_by_the_provider_adapter() {
        let config = Config::default();
        let req = request(vec![Message::user_text("hi")]);
        let openai = body("openai", &req, &config).unwrap();
        assert!(
            openai.contains("\"model\": \"gpt-6-luna\"") && openai.contains("\"store\": false"),
            "{openai}"
        );
        let anthropic = body("anthropic", &req, &config).unwrap();
        assert!(anthropic.contains("\"max_tokens\""), "{anthropic}");
        assert!(body("nope", &req, &config).is_err());
    }

    #[test]
    fn appended_requests_pass_and_edits_are_named() {
        let a = request(vec![Message::user_text("hi")]);
        let mut b = a.clone();
        b.messages.push(Message::new(
            Role::Assistant,
            vec![ContentBlock::text("hello")],
        ));
        let config = Config::default();
        let (ba, bb) = (
            body("openai", &a, &config).unwrap(),
            body("openai", &b, &config).unwrap(),
        );
        let report = diff_report(&a, &b, &ba, &bb);
        assert!(report.starts_with("append-only: yes"), "{report}");
        assert!(report.contains("+ "), "{report}");
        let mut c = b.clone();
        c.messages[0] = Message::user_text("edited");
        c.system[0].text = "changed".into();
        let report = diff_report(&b, &c, &bb, &body("openai", &c, &config).unwrap());
        assert!(
            report.starts_with("append-only: NO")
                && report.contains("system prompt")
                && report.contains("earlier messages"),
            "{report}"
        );
    }
}
