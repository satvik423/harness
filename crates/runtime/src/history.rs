//! [`SessionHistory`]: the runtime's [`History`] for `recall` (FR-CTX-04).
//!
//! Events are read from the log and rendered like a compaction renders them,
//! except that tool results show their **full** output: `recall` is how the
//! model gets back what the capper trimmed (guardrail 03).

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use harness_context::compaction::render;
use harness_ports::{BlobStore, EventLog, History, HistoryError, RecallSelector, Recollection};
use harness_protocol::{BlobOrText, Event};
use harness_types::{ContentBlock, Seq, SessionId, TaskId};

use crate::journal::Journal;
use crate::memory::SessionMemory;

/// Renders what `selector` names from a stored session, for a front-end that
/// shows or attaches it itself (`/recall a2` in the TUI). Anchors come from the
/// session's `Compacted` events; nothing is recorded.
///
/// # Errors
/// As [`History::recall`].
pub async fn recall(
    log: Arc<dyn EventLog>,
    blobs: Arc<dyn BlobStore>,
    session: SessionId,
    selector: &RecallSelector,
) -> Result<Recollection, HistoryError> {
    let events = log
        .range(session, Seq::FIRST, Seq(u64::MAX))
        .await
        .map_err(|e| HistoryError::Store(e.to_string()))?;
    let mut memory = SessionMemory::default();
    let mut segment = 0;
    for logged in &events {
        segment = segment.max(logged.segment);
        if let Event::Compacted { record, .. } = &logged.envelope.event {
            memory.apply(record);
        }
    }
    SessionHistory {
        session,
        task: TaskId::new(),
        log,
        blobs,
        memory: Arc::new(Mutex::new(memory)),
        journal: None,
        segment,
    }
    .recall(selector)
    .await
}

/// Most events one range recall reads (paging happens in the tool).
const MAX_RANGE: u64 = 5_000;

/// Reads one session's history for one task.
pub(crate) struct SessionHistory {
    pub(crate) session: SessionId,
    pub(crate) task: TaskId,
    pub(crate) log: Arc<dyn EventLog>,
    pub(crate) blobs: Arc<dyn BlobStore>,
    pub(crate) memory: Arc<Mutex<SessionMemory>>,
    /// Where `Recalled` is recorded; `None` for a front-end's own recall.
    pub(crate) journal: Option<Arc<Journal>>,
    /// The current segment: searches cover the segments before it.
    pub(crate) segment: u32,
}

impl SessionHistory {
    async fn events(&self, from: Seq, to: Seq) -> Result<Vec<(Seq, Event)>, HistoryError> {
        let logged = self
            .log
            .range(self.session, from, to)
            .await
            .map_err(|e| HistoryError::Store(e.to_string()))?;
        let mut out = Vec::with_capacity(logged.len());
        for l in logged {
            let mut event = l.envelope.event;
            // Show the full output, not the capped form the model saw.
            if let Event::ToolFinished { full, sent, .. } = &mut event {
                let text = match full {
                    BlobOrText::Text { text } => Some(text.clone()),
                    BlobOrText::Blob { blob } => self
                        .blobs
                        .get(blob)
                        .await
                        .ok()
                        .map(|b| String::from_utf8_lossy(&b).into_owned()),
                };
                if let Some(text) = text {
                    *sent = vec![ContentBlock::text(text)];
                }
            }
            out.push((l.envelope.seq, event));
        }
        Ok(out)
    }

    fn lines(events: &[(Seq, Event)]) -> Vec<String> {
        render(events)
            .events
            .into_values()
            .flat_map(|e| e.lines)
            .collect()
    }
}

#[async_trait]
impl History for SessionHistory {
    async fn recall(&self, selector: &RecallSelector) -> Result<Recollection, HistoryError> {
        match selector {
            RecallSelector::Anchor(id) => {
                let anchor = {
                    let memory = self.memory.lock().unwrap_or_else(PoisonError::into_inner);
                    memory.anchors.iter().find(|a| &a.id == id).cloned()
                };
                let Some(anchor) = anchor else {
                    let known = {
                        let memory = self.memory.lock().unwrap_or_else(PoisonError::into_inner);
                        memory
                            .anchors
                            .iter()
                            .map(|a| a.id.clone())
                            .collect::<Vec<_>>()
                    };
                    return Err(HistoryError::NotFound(if known.is_empty() {
                        format!("no anchor {id}: this session has not been compacted yet")
                    } else {
                        format!("no anchor {id}; the anchors are {}", known.join(", "))
                    }));
                };
                let lines = Self::lines(&self.events(anchor.from, anchor.to).await?);
                Ok(Recollection {
                    label: format!("{} · events {}-{}", anchor.id, anchor.from.0, anchor.to.0),
                    anchor: Some(anchor.id),
                    lines,
                })
            }
            RecallSelector::Range { from, to } => {
                let to = Seq(to.0.min(from.0.saturating_add(MAX_RANGE)));
                let lines = Self::lines(&self.events(*from, to).await?);
                if lines.is_empty() {
                    return Err(HistoryError::NotFound(format!(
                        "events #{}-#{} hold no messages or tool results",
                        from.0, to.0
                    )));
                }
                Ok(Recollection {
                    label: format!("events {}-{}", from.0, to.0),
                    anchor: None,
                    lines,
                })
            }
            RecallSelector::Query { query, limit } => {
                if self.segment == 0 {
                    return Err(HistoryError::NotFound(
                        "there are no earlier segments to search; recall by event range instead"
                            .into(),
                    ));
                }
                let hits = self
                    .log
                    .search(self.session, query, limit.saturating_mul(4))
                    .await
                    .map_err(|e| HistoryError::Store(e.to_string()))?;
                let mut lines = Vec::new();
                let mut found = 0;
                for hit in hits
                    .into_iter()
                    .filter(|h| h.segment < self.segment)
                    .take(*limit)
                {
                    found += 1;
                    lines.extend(Self::lines(&self.events(hit.seq, hit.seq).await?));
                }
                if found == 0 {
                    return Err(HistoryError::NotFound(format!(
                        "no matches for {query:?} in earlier segments"
                    )));
                }
                Ok(Recollection {
                    label: format!("search {query:?} · {found} matches"),
                    anchor: None,
                    lines,
                })
            }
        }
    }

    async fn recalled(&self, anchor: Option<String>, tokens: u64) {
        let Some(journal) = &self.journal else {
            return;
        };
        let _ = journal
            .record(
                Some(self.task),
                Event::Recalled {
                    anchor,
                    tokens,
                    auto: false,
                },
            )
            .await;
    }
}
