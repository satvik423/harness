//! Compaction in the runtime (P5-T2/T3, Tech spec 3.13): closes the current
//! segment with a validated summary and opens the next one.
//!
//! The flow: `CompactionStarted` → render the segment's events → ask the
//! compaction model (structured output) → validate (I5, I6) → `Compacted` with
//! the record → a new frame and `SegmentOpened`. Nothing already sent is
//! changed; the next request starts at the new opener (guardrails 02 and 04).
//! On any failure the current segment stays as it was.

use futures::StreamExt as _;
use harness_context::compaction::{
    compaction_request, parse_output, render, render_opener, validate,
};
use harness_context::{Environment, SegmentContext, SegmentFrame};
use harness_ports::{ModeControl as _, ProviderEvent};
use harness_protocol::Event;
use harness_types::{ContentBlock, Message, Seq, StopReason};
use std::sync::PoisonError;

use crate::mode::environment_note;
use crate::turn::TaskCtx;

/// What a compaction may change besides the opener (an in-session `/model`
/// or `/effort` switch, ADR-046).
#[derive(Debug, Clone, Default)]
pub(crate) struct FrameChange {
    /// New provider id and model id.
    pub(crate) model: Option<(String, String)>,
    /// New effort (`Some(None)` clears it).
    pub(crate) effort: Option<Option<harness_ports::Effort>>,
}

/// Why compaction did not happen.
#[derive(Debug)]
pub(crate) enum CompactError {
    /// The task was interrupted or cancelled meanwhile.
    Stopped,
    /// The event log refused a write.
    LogFailed,
    /// Anything else, with a message for the user.
    Failed(String),
}

/// Compacts `segment` in place: on success it is the next segment, with the
/// old segment's unsent notes carried over.
pub(crate) async fn compact(
    ctx: &TaskCtx,
    segment: &mut SegmentContext,
    focus: Option<&str>,
    change: FrameChange,
) -> Result<(), CompactError> {
    let from = segment.segment();
    ctx.record(Event::CompactionStarted { segment: from })
        .await
        .map_err(|_| CompactError::LogFailed)?;
    let before = segment.estimated_input(&[]);
    let events: Vec<(Seq, Event)> = ctx
        .deps
        .log
        .range(ctx.session, Seq::FIRST, Seq(u64::MAX))
        .await
        .map_err(|e| CompactError::Failed(format!("cannot read the segment: {e}")))?
        .into_iter()
        .filter(|l| l.segment == from)
        .map(|l| (l.envelope.seq, l.envelope.event))
        .collect();
    let rendered = render(&events);
    let (prior_pins, prior_anchors) = {
        let memory = ctx.memory.lock().unwrap_or_else(PoisonError::into_inner);
        (memory.pins.clone(), memory.anchors.clone())
    };
    let model = ctx
        .context
        .compaction_model
        .clone()
        .unwrap_or_else(|| segment.frame().model.clone());
    let request = compaction_request(&model, &rendered, &prior_pins, focus);
    let text = ask(ctx, request).await?;
    let output = parse_output(&text).map_err(CompactError::Failed)?;
    let (record, dropped) = validate(output, &rendered, from, &prior_pins, prior_anchors.len());
    if !dropped.is_empty() {
        ctx.note(Event::Warning {
            kind: "compaction".into(),
            message: format!("compaction repaired its summary: {}", dropped.join("; ")),
        })
        .await;
    }

    // The opener: a fresh environment (with the mode), the summary, pins, anchors.
    let cwd = ctx.cwd.clone();
    let now = ctx.deps.clock.now_ms();
    let mut environment = tokio::task::spawn_blocking(move || Environment::gather(&cwd, now))
        .await
        .map_err(|e| CompactError::Failed(format!("gathering the environment failed: {e}")))?;
    environment
        .notes
        .push(environment_note(ctx.mode.current(), ctx.approval));
    let mut anchors = prior_anchors;
    anchors.extend(record.anchors.iter().cloned());
    let opener = render_opener(&environment.render(), &record, &anchors, ctx.context.style);
    let frame = changed(segment.frame().successor(from + 1, opener), change);
    let bytes = serde_json::to_vec(&frame)
        .map_err(|e| CompactError::Failed(format!("cannot encode the segment frame: {e}")))?;
    let blob = ctx
        .deps
        .blobs
        .put(bytes, "application/json", Some("segment-frame.json"))
        .await
        .map_err(|e| CompactError::Failed(format!("cannot store the segment frame: {e}")))?;

    // Notes not yet sent move to the new segment; replay carries them the same
    // way, so mode notes are queued on the old segment first.
    for note in ctx.mode.take_notes() {
        segment.push_note(note);
    }
    let mut next = SegmentContext::from_frame(frame.clone());
    for note in segment.pending_notes() {
        next.push_note(note.clone());
    }
    let after = next.estimated_input(&[]);
    ctx.record(Event::Compacted {
        from_segment: from,
        to_segment: from + 1,
        anchors: u32::try_from(record.anchors.len()).unwrap_or(u32::MAX),
        before,
        after,
        record: Box::new(record.clone()),
    })
    .await
    .map_err(|_| CompactError::LogFailed)?;
    ctx.journal.set_segment(from + 1);
    ctx.record(Event::SegmentOpened {
        segment: from + 1,
        opener: Message::user_text(frame.opener.clone()),
        frame: blob,
    })
    .await
    .map_err(|_| CompactError::LogFailed)?;
    ctx.memory
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .apply(&record);
    *segment = next;
    Ok(())
}

fn changed(mut frame: SegmentFrame, change: FrameChange) -> SegmentFrame {
    if let Some((provider, model)) = change.model {
        frame.provider = provider;
        frame.model = model;
    }
    if let Some(effort) = change.effort {
        frame.effort = effort;
    }
    frame
}

/// Sends the compaction request and returns the reply's text. Usage is
/// recorded (and priced) like any model call.
async fn ask(ctx: &TaskCtx, request: harness_ports::ModelRequest) -> Result<String, CompactError> {
    let mut stream = match ctx
        .deps
        .provider
        .stream(request, ctx.cancel.child_token())
        .await
    {
        Ok(stream) => stream,
        Err(_) if ctx.cancel.is_cancelled() => return Err(CompactError::Stopped),
        Err(e) => {
            return Err(CompactError::Failed(format!(
                "the compaction call failed: {e}"
            )));
        }
    };
    let mut text = String::new();
    let mut stop = None;
    loop {
        let item = tokio::select! {
            item = stream.next() => item,
            () = ctx.cancel.cancelled() => return Err(CompactError::Stopped),
        };
        match item {
            None => break,
            Some(Ok(ProviderEvent::BlockComplete {
                block: ContentBlock::Text { text: t },
            })) => text.push_str(&t),
            Some(Ok(ProviderEvent::Usage { usage })) => {
                ctx.note(Event::Usage {
                    task: ctx.task,
                    usage,
                    cost_micros: ctx.price.map_or(0, |p| p.cost_micros(&usage)),
                })
                .await;
            }
            Some(Ok(ProviderEvent::Stop { reason })) => stop = Some(reason),
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                return Err(CompactError::Failed(format!(
                    "the compaction call failed: {e}"
                )));
            }
        }
    }
    match stop {
        Some(StopReason::MaxTokens) => Err(CompactError::Failed(
            "the summary hit the output limit".into(),
        )),
        Some(StopReason::Refusal { .. }) => Err(CompactError::Failed(
            "the model declined to summarise".into(),
        )),
        _ => Ok(text),
    }
}
