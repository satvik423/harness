//! Checks a [`CompactionOutput`] against the segment it summarises and turns
//! it into a [`CompactionRecord`] (Tech spec 3.13, P5-T4).
//!
//! - **I5:** a pin is kept only if its quote is an exact substring of the
//!   event it names, and that event is in the segment.
//! - **I6:** anchors are clipped to the segment, overlaps are cut, and gaps
//!   are filled with automatic `unlabelled` anchors cut at step boundaries,
//!   so the anchors cover every event of the segment exactly once.
//! - Earlier active pins are kept; ones the model marks superseded are
//!   switched off, with the event that replaced them.

pub use harness_protocol::{Anchor, CompactionRecord, KeyDecision, PinnedFact};
use harness_types::Seq;

use super::render::Rendered;
use super::schema::CompactionOutput;

/// Largest automatic anchor before it is cut at a step boundary.
pub const AUTO_ANCHOR_TOKENS: u64 = 8_000;

/// Topic of anchors the harness made to fill gaps.
pub const UNLABELLED: &str = "unlabelled";

/// Validates `output` against `rendered` (the events of `segment`).
///
/// `prior_pins` are the session's pins before this compaction; `prior_anchors`
/// is how many anchors earlier segments made (ids continue after them).
/// Returns the record and a message for each thing dropped or repaired.
#[must_use]
pub fn validate(
    output: CompactionOutput,
    rendered: &Rendered,
    segment: u32,
    prior_pins: &[PinnedFact],
    prior_anchors: usize,
) -> (CompactionRecord, Vec<String>) {
    let mut notes = Vec::new();
    let (first, last) = rendered.span.unwrap_or((Seq(0), Seq(0)));
    let inside = |seq: u64| seq >= first.0 && seq <= last.0;

    // Pins: earlier ones first, then superseded ones switched off, then new ones.
    let mut pins: Vec<PinnedFact> = prior_pins.to_vec();
    for s in &output.superseded_pins {
        match pins.iter_mut().find(|p| p.id == s.pin_id && p.active()) {
            Some(pin) if inside(s.by_seq) => pin.superseded_by = Some(Seq(s.by_seq)),
            Some(_) => notes.push(format!(
                "pin p{} was marked superseded by event #{}, which is not in this segment; kept",
                s.pin_id, s.by_seq
            )),
            None => notes.push(format!(
                "pin p{} cannot be superseded: no such active pin",
                s.pin_id
            )),
        }
    }
    let mut next_id = pins.iter().map(|p| p.id).max().unwrap_or(0) + 1;
    for proposal in output.pinned {
        let quote = proposal.quote;
        let verified = !quote.trim().is_empty()
            && inside(proposal.seq)
            && rendered
                .events
                .get(&Seq(proposal.seq))
                .is_some_and(|e| e.raw.contains(&quote));
        if !verified {
            notes.push(format!(
                "dropped pin {quote:?}: not an exact quote of event #{}",
                proposal.seq
            ));
            continue;
        }
        if pins
            .iter()
            .any(|p| p.active() && p.quote == quote && p.seq.0 == proposal.seq)
        {
            continue;
        }
        pins.push(PinnedFact {
            id: next_id,
            quote,
            seq: Seq(proposal.seq),
            kind: proposal.kind,
            superseded_by: None,
        });
        next_id += 1;
    }

    // Anchors: clip, sort, cut overlaps, fill gaps.
    let mut ranges: Vec<(u64, u64, String, Vec<String>)> = Vec::new();
    for a in output.anchors {
        let from = a.from_seq.max(first.0);
        let to = a.to_seq.min(last.0);
        if from > to || rendered.span.is_none() {
            notes.push(format!(
                "dropped anchor {:?} ({}-{}): outside the segment",
                a.topic, a.from_seq, a.to_seq
            ));
            continue;
        }
        let topic = if a.topic.trim().is_empty() {
            UNLABELLED.to_owned()
        } else {
            a.topic
        };
        ranges.push((from, to, topic, a.has));
    }
    ranges.sort_by_key(|r| (r.0, r.1));
    let mut covered: Vec<(u64, u64, String, Vec<String>)> = Vec::new();
    for (from, to, topic, has) in ranges {
        let from = covered.last().map_or(from, |p| from.max(p.1 + 1));
        if from > to {
            notes.push(format!(
                "dropped anchor {topic:?}: it overlapped earlier ones"
            ));
            continue;
        }
        covered.push((from, to, topic, has));
    }
    let mut filled = Vec::new();
    if rendered.span.is_some() {
        let mut cursor = first.0;
        for range in covered {
            if range.0 > cursor {
                filled.extend(auto_anchors(rendered, cursor, range.0 - 1));
            }
            cursor = range.1 + 1;
            filled.push(range);
        }
        if cursor <= last.0 {
            filled.extend(auto_anchors(rendered, cursor, last.0));
        }
    }
    let anchors = filled
        .into_iter()
        .enumerate()
        .map(|(i, (from, to, topic, has))| Anchor {
            id: format!("a{}", prior_anchors + i + 1),
            segment,
            from: Seq(from),
            to: Seq(to),
            topic,
            has,
            tokens: rendered.tokens_between(Seq(from), Seq(to)),
        })
        .collect();

    let decisions = output
        .decisions
        .into_iter()
        .map(|d| KeyDecision {
            seq: inside(d.seq).then_some(Seq(d.seq)),
            text: d.text,
        })
        .collect();
    let record = CompactionRecord {
        segment,
        from: first,
        to: last,
        goal: output.goal,
        state_of_work: output.state_of_work,
        decisions,
        pins,
        files_touched: output.files_touched,
        open_questions: output.open_questions,
        next_steps: output.next_steps,
        anchors,
    };
    (record, notes)
}

/// Unlabelled anchors for `from..=to`, cut at step boundaries so none is much
/// larger than [`AUTO_ANCHOR_TOKENS`].
fn auto_anchors(rendered: &Rendered, from: u64, to: u64) -> Vec<(u64, u64, String, Vec<String>)> {
    let mut out = Vec::new();
    let mut start = from;
    let mut size = 0;
    for (seq, event) in rendered.events.range(Seq(from)..=Seq(to)) {
        if event.step_start && seq.0 > start && size >= AUTO_ANCHOR_TOKENS {
            out.push((start, seq.0 - 1, UNLABELLED.to_owned(), Vec::new()));
            start = seq.0;
            size = 0;
        }
        size += event.tokens;
    }
    out.push((start, to, UNLABELLED.to_owned(), Vec::new()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::render::render;
    use crate::compaction::schema::{AnchorProposal, PinKind, PinProposal, Supersession};
    use harness_protocol::Event;
    use harness_types::{Message, TaskId};
    use proptest::prelude::*;

    fn segment(n: u64, from: u64) -> Rendered {
        let task = TaskId::new();
        let events: Vec<(Seq, Event)> = (from..from + n)
            .map(|i| {
                (
                    Seq(i),
                    Event::UserInput {
                        task,
                        message: Message::user_text(format!(
                            "message {i}: must support Postgres {i}"
                        )),
                    },
                )
            })
            .collect();
        render(&events)
    }

    fn output() -> CompactionOutput {
        CompactionOutput {
            goal: "g".into(),
            state_of_work: "s".into(),
            decisions: vec![],
            pinned: vec![],
            superseded_pins: vec![],
            files_touched: vec![],
            open_questions: vec![],
            next_steps: vec![],
            anchors: vec![],
        }
    }

    fn pin(quote: &str, seq: u64) -> PinProposal {
        PinProposal {
            quote: quote.into(),
            seq,
            kind: PinKind::Requirement,
        }
    }

    fn anchor(from: u64, to: u64) -> AnchorProposal {
        AnchorProposal {
            from_seq: from,
            to_seq: to,
            topic: format!("t{from}"),
            has: vec!["x".into()],
        }
    }

    #[test]
    fn only_exact_quotes_of_events_in_the_segment_are_pinned() {
        let r = segment(5, 10);
        let mut out = output();
        out.pinned = vec![
            pin("must support Postgres 12", 12),
            pin("must support Postgres 99", 12),
            pin("must support Postgres 3", 3),
            pin("   ", 11),
        ];
        let (record, notes) = validate(out, &r, 0, &[], 0);
        assert_eq!(record.pins.len(), 1);
        assert_eq!(record.pins[0].quote, "must support Postgres 12");
        assert_eq!((record.pins[0].id, record.pins[0].seq), (1, Seq(12)));
        assert_eq!(notes.len(), 3, "{notes:?}");
    }

    #[test]
    fn earlier_pins_stay_unless_superseded_and_ids_continue() {
        let r = segment(5, 10);
        let prior = vec![
            PinnedFact {
                id: 1,
                quote: "old".into(),
                seq: Seq(2),
                kind: PinKind::Decision,
                superseded_by: None,
            },
            PinnedFact {
                id: 2,
                quote: "kept".into(),
                seq: Seq(3),
                kind: PinKind::Id,
                superseded_by: None,
            },
        ];
        let mut out = output();
        out.superseded_pins = vec![Supersession {
            pin_id: 1,
            by_seq: 11,
        }];
        out.pinned = vec![pin("message 11", 11)];
        let (record, _) = validate(out, &r, 1, &prior, 0);
        assert_eq!(record.pins.len(), 3);
        assert_eq!(record.pins[0].superseded_by, Some(Seq(11)));
        assert!(record.pins[1].active());
        assert_eq!(record.pins[2].id, 3);
        // Earlier pins are copied byte for byte (M2).
        assert_eq!(record.pins[1], prior[1]);
    }

    #[test]
    fn anchors_are_clipped_cut_and_gaps_filled() {
        let r = segment(10, 1);
        let mut out = output();
        out.anchors = vec![anchor(3, 5), anchor(4, 7), anchor(0, 1), anchor(20, 30)];
        let (record, notes) = validate(out, &r, 0, &[], 2);
        let ranges: Vec<(u64, u64, &str)> = record
            .anchors
            .iter()
            .map(|a| (a.from.0, a.to.0, a.topic.as_str()))
            .collect();
        assert_eq!(
            ranges,
            [
                (1, 1, "t0"),
                (2, 2, UNLABELLED),
                (3, 5, "t3"),
                (6, 7, "t4"),
                (8, 10, UNLABELLED)
            ]
        );
        assert_eq!(
            record.anchors[0].id, "a3",
            "ids continue after earlier segments"
        );
        assert!(notes.iter().any(|n| n.contains("outside")), "{notes:?}");
    }

    proptest! {
        /// I6: whatever the model proposes, the anchors cover every event of
        /// the segment exactly once, in order.
        #[test]
        fn anchors_always_cover_the_segment_without_overlap(
            n in 1u64..40,
            proposals in proptest::collection::vec((0u64..60, 0u64..60), 0..12)
        ) {
            let r = segment(n, 5);
            let mut out = output();
            out.anchors = proposals.into_iter().map(|(a, b)| anchor(a, b)).collect();
            let (record, _) = validate(out, &r, 0, &[], 0);
            let mut expect = 5;
            for a in &record.anchors {
                prop_assert_eq!(a.from.0, expect);
                prop_assert!(a.to.0 >= a.from.0);
                expect = a.to.0 + 1;
            }
            prop_assert_eq!(expect, 5 + n);
        }
    }
}
