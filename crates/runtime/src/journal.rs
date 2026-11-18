//! [`Journal`]: the session's single writer. Records events in the event log,
//! then broadcasts them.
//!
//! Every persisted event is committed before it is broadcast and before the
//! runtime does anything that depends on it (NFR-03). The journal assigns
//! sequence numbers itself (it is the only writer for its session), so an
//! event can be built from its own number, such as the recall footer of a
//! capped tool result (ADR-030, ADR-031). Live deltas (text, thinking, tool
//! output) are broadcast only; they carry the number of the latest committed event.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use harness_ports::{Clock, EventLog, EventSink, LoggedEvent, StoreError};
use harness_protocol::{Envelope, Event};
use harness_types::{Seq, SessionId, TaskId};
use tokio::sync::{Mutex, broadcast};

/// Records and broadcasts one session's events.
pub(crate) struct Journal {
    session: SessionId,
    log: Arc<dyn EventLog>,
    clock: Arc<dyn Clock>,
    tx: broadcast::Sender<Envelope>,
    /// The next sequence number; held while an event is appended and sent, so
    /// numbers stay gapless and subscribers see events in order.
    next: Mutex<u64>,
    /// The latest committed sequence number (0 before the first event).
    last: AtomicU64,
    segment: AtomicU32,
}

impl Journal {
    /// A journal continuing after `last` (or starting a new session).
    pub(crate) fn new(
        session: SessionId,
        log: Arc<dyn EventLog>,
        clock: Arc<dyn Clock>,
        tx: broadcast::Sender<Envelope>,
        last: Option<Seq>,
        segment: u32,
    ) -> Self {
        let last = last.map_or(0, |s| s.0);
        Self {
            session,
            log,
            clock,
            tx,
            next: Mutex::new(last + 1),
            last: AtomicU64::new(last),
            segment: AtomicU32::new(segment),
        }
    }

    /// The segment new events belong to.
    pub(crate) fn set_segment(&self, segment: u32) {
        self.segment.store(segment, Ordering::SeqCst);
    }

    /// Commits `event`, then broadcasts it. Returns its sequence number.
    ///
    /// # Errors
    /// The log's error; the event was not stored or broadcast, and a live
    /// `event_log` error tells subscribers.
    pub(crate) async fn record(
        &self,
        task: Option<TaskId>,
        event: Event,
    ) -> Result<Seq, StoreError> {
        self.record_with(task, move |_| event).await
    }

    /// Like [`Journal::record`], building the event from its sequence number.
    ///
    /// # Errors
    /// As [`Journal::record`].
    pub(crate) async fn record_with<F>(
        &self,
        task: Option<TaskId>,
        build: F,
    ) -> Result<Seq, StoreError>
    where
        F: FnOnce(Seq) -> Event + Send,
    {
        let mut next = self.next.lock().await;
        let seq = Seq(*next);
        let envelope = Envelope {
            seq,
            ts: self.clock.now_ms(),
            session: self.session,
            event: build(seq),
        };
        let logged = LoggedEvent {
            envelope: envelope.clone(),
            task,
            segment: self.segment.load(Ordering::SeqCst),
        };
        if let Err(e) = self.log.append(logged).await {
            tracing::error!(error = %e, kind = envelope.event.kind(), "event log append failed");
            self.live(Event::Error {
                kind: "event_log".into(),
                message: format!("could not save an event: {e}"),
                hint: Some("Check free disk space and the permissions of .harness/.".into()),
            });
            return Err(e);
        }
        *next += 1;
        self.last.store(seq.0, Ordering::SeqCst);
        // No subscribers is fine: nobody is watching this session right now.
        let _ = self.tx.send(envelope);
        Ok(seq)
    }

    /// Broadcasts an event without storing it (deltas, and errors about the log itself).
    pub(crate) fn live(&self, event: Event) {
        let _ = self.tx.send(Envelope {
            seq: Seq(self.last.load(Ordering::SeqCst)),
            ts: self.clock.now_ms(),
            session: self.session,
            event,
        });
    }
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

/// Tools emit live output through this; persisted events go through `record`.
impl EventSink for Journal {
    fn emit(&self, event: Event) {
        if event.is_persisted() {
            // A tool must not create history on its own; show it, but say so.
            tracing::warn!(
                kind = event.kind(),
                "a persisted event was emitted live and not stored"
            );
        }
        self.live(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_ports::{SessionRecord, SessionStatus, SessionStore};
    use harness_testkit::{FixedClock, MemoryStore};

    async fn journal() -> (
        Journal,
        Arc<MemoryStore>,
        broadcast::Receiver<Envelope>,
        SessionId,
    ) {
        let store = Arc::new(MemoryStore::default());
        let id = SessionId::new();
        store
            .create_session(SessionRecord {
                id,
                created_at: 0,
                title: None,
                persona: "p".into(),
                cwd: "/".into(),
                status: SessionStatus::Open,
                parent: None,
                forked_at: None,
                current_segment: 0,
            })
            .await
            .unwrap();
        let (tx, rx) = broadcast::channel(64);
        let j = Journal::new(id, store.clone(), Arc::new(FixedClock::new(7)), tx, None, 0);
        (j, store, rx, id)
    }

    fn started(id: &str) -> Event {
        Event::ToolStarted { call_id: id.into() }
    }

    #[tokio::test]
    async fn events_are_stored_before_they_are_broadcast_with_gapless_numbers() {
        let (j, store, mut rx, id) = journal().await;
        assert_eq!(j.record(None, started("a")).await.unwrap(), Seq(1));
        let built = j
            .record_with(None, |seq| started(&format!("at-{}", seq.0)))
            .await
            .unwrap();
        assert_eq!(built, Seq(2));
        let stored = store.events(id);
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[1].envelope.event, started("at-2"));
        let first = rx.try_recv().unwrap();
        assert_eq!((first.seq, first.ts), (Seq(1), 7));
        assert_eq!(rx.try_recv().unwrap().event, started("at-2"));
    }

    #[tokio::test]
    async fn live_events_are_not_stored_and_carry_the_last_number() {
        let (j, store, mut rx, id) = journal().await;
        j.record(None, started("a")).await.unwrap();
        let _ = rx.try_recv();
        j.emit(Event::ToolOutput {
            call_id: "a".into(),
            chunk: "x".into(),
        });
        assert_eq!(store.events(id).len(), 1);
        assert_eq!(rx.try_recv().unwrap().seq, Seq(1));
    }

    #[tokio::test]
    async fn a_failed_append_broadcasts_an_error_and_keeps_the_number() {
        let (j, _store, mut rx, _) = journal().await;
        // A delta cannot be stored: the append fails.
        let err = j
            .record(
                None,
                Event::TextDelta {
                    task: TaskId::new(),
                    text: "x".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::NotPersistable(_)));
        assert!(
            matches!(rx.try_recv().unwrap().event, Event::Error { kind, .. } if kind == "event_log")
        );
        assert_eq!(j.record(None, started("a")).await.unwrap(), Seq(1));
    }

    #[tokio::test]
    async fn a_resumed_journal_continues_the_numbering() {
        let (j, store, _rx, id) = journal().await;
        j.record(None, started("a")).await.unwrap();
        let (tx, _rx2) = broadcast::channel(8);
        let resumed = Journal::new(
            id,
            store.clone(),
            Arc::new(FixedClock::new(0)),
            tx,
            Some(Seq(1)),
            0,
        );
        assert_eq!(resumed.record(None, started("b")).await.unwrap(), Seq(2));
    }
}
