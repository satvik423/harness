//! The opener of a segment after a compaction (P5-T5): environment, the
//! summary from the record by a fixed template, pinned facts verbatim and the
//! anchor index (Tech spec 3.13, SDD 2.6).

use std::fmt::Write as _;

use super::validate::{Anchor, CompactionRecord};

/// What a compaction keeps. `SummaryOnly` is the needle eval's baseline: the
/// same summary without pins, anchors or recall hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompactionStyle {
    /// Summary, pinned facts and anchors (the default).
    #[default]
    Anchored,
    /// The summary alone (baseline for evals).
    SummaryOnly,
}

/// `18.4k` style token counts.
fn tokens(n: u64) -> String {
    if n >= 1_000 {
        #[allow(clippy::cast_precision_loss)]
        let k = n as f64 / 1_000.0;
        format!("{k:.1}k")
    } else {
        n.to_string()
    }
}

/// One anchor as the index shows it, for example
/// `[[a2 | ev 211-560 | 18.4k tok | Migrating users; has: exact psql error]]`.
#[must_use]
pub fn anchor_line(anchor: &Anchor) -> String {
    let has = if anchor.has.is_empty() {
        String::new()
    } else {
        format!("; has: {}", anchor.has.join("; "))
    };
    format!(
        "[[{} | ev {}-{} | {} tok | {}{has}]]",
        anchor.id,
        anchor.from.0,
        anchor.to.0,
        tokens(anchor.tokens),
        anchor.topic
    )
}

fn list(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n## {title}");
    for item in items {
        let _ = writeln!(out, "- {item}");
    }
}

/// The opener: `environment` (a rendered `<environment>` block), then the
/// compacted context. `anchors` are every anchor of the session so far, the
/// record's own included.
#[must_use]
pub fn render_opener(
    environment: &str,
    record: &CompactionRecord,
    anchors: &[Anchor],
    style: CompactionStyle,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{environment}");
    let _ = writeln!(
        out,
        "<compacted-context segment=\"{}\" events=\"{}-{}\">",
        record.segment + 1,
        record.from.0,
        record.to.0
    );
    match style {
        CompactionStyle::Anchored => out.push_str(
            "The conversation so far was compacted into this summary. Nothing was deleted: \
             the pinned facts below are exact quotes, and recall fetches the exact events \
             behind any anchor, event range or search query. Use recall instead of guessing \
             when you need a detail the summary does not give.\n",
        ),
        CompactionStyle::SummaryOnly => {
            out.push_str("The conversation so far was compacted into this summary.\n");
        }
    }
    let _ = writeln!(out, "\n## Goal\n{}", record.goal.trim());
    let _ = writeln!(out, "\n## State of work\n{}", record.state_of_work.trim());
    let decisions: Vec<String> = record
        .decisions
        .iter()
        .map(|d| match d.seq {
            Some(seq) => format!("{} (#{})", d.text, seq.0),
            None => d.text.clone(),
        })
        .collect();
    list(&mut out, "Decisions", &decisions);
    list(&mut out, "Files touched", &record.files_touched);
    list(&mut out, "Open questions", &record.open_questions);
    list(&mut out, "Next steps", &record.next_steps);
    if style == CompactionStyle::Anchored {
        let pins: Vec<String> = record
            .pins
            .iter()
            .filter(|p| p.active())
            .map(|p| format!("[p{} · {} · #{}] {}", p.id, p.kind.name(), p.seq.0, p.quote))
            .collect();
        list(&mut out, "Pinned facts (exact quotes)", &pins);
        let index: Vec<String> = anchors.iter().map(anchor_line).collect();
        list(&mut out, "Anchors (recall {\"anchor\": \"aN\"})", &index);
    }
    out.push_str("</compacted-context>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::schema::PinKind;
    use crate::compaction::validate::{KeyDecision, PinnedFact};
    use harness_types::Seq;

    fn record() -> CompactionRecord {
        CompactionRecord {
            segment: 0,
            from: Seq(1),
            to: Seq(560),
            goal: "Migrate the users table".into(),
            state_of_work: "Schema done, data copy failing".into(),
            decisions: vec![KeyDecision {
                text: "keep soft deletes".into(),
                seq: Some(Seq(300)),
            }],
            pins: vec![
                PinnedFact {
                    id: 1,
                    quote: "must support Postgres 14".into(),
                    seq: Seq(3),
                    kind: PinKind::Requirement,
                    superseded_by: None,
                },
                PinnedFact {
                    id: 2,
                    quote: "use MySQL".into(),
                    seq: Seq(2),
                    kind: PinKind::Decision,
                    superseded_by: Some(Seq(3)),
                },
            ],
            files_touched: vec!["users.sql".into()],
            open_questions: vec![],
            next_steps: vec!["fix tenant_id".into()],
            anchors: vec![Anchor {
                id: "a2".into(),
                segment: 0,
                from: Seq(211),
                to: Seq(560),
                topic: "Migrating users table".into(),
                has: vec!["exact psql error".into(), "user decision".into()],
                tokens: 18_400,
            }],
        }
    }

    #[test]
    fn the_opener_has_summary_active_pins_and_anchor_index() {
        let r = record();
        let o = render_opener(
            "<environment>e</environment>",
            &r,
            &r.anchors,
            CompactionStyle::Anchored,
        );
        assert!(
            o.starts_with(
                "<environment>e</environment>\n<compacted-context segment=\"1\" events=\"1-560\">"
            ),
            "{o}"
        );
        assert!(o.contains("## Goal\nMigrate the users table"));
        assert!(o.contains("- keep soft deletes (#300)"));
        assert!(o.contains("- [p1 · requirement · #3] must support Postgres 14"));
        assert!(!o.contains("use MySQL"), "superseded pins are left out");
        assert!(o.contains(
            "[[a2 | ev 211-560 | 18.4k tok | Migrating users table; has: exact psql error; user decision]]"
        ));
        assert!(o.ends_with("</compacted-context>"));
    }

    #[test]
    fn the_summary_only_baseline_has_no_pins_anchors_or_recall() {
        let r = record();
        let o = render_opener(
            "<environment>e</environment>",
            &r,
            &r.anchors,
            CompactionStyle::SummaryOnly,
        );
        assert!(o.contains("## Goal"));
        for absent in ["Pinned facts", "[[a2", "recall"] {
            assert!(!o.contains(absent), "{absent} in {o}");
        }
    }
}
