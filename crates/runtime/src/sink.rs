//! [`BroadcastSink`]: wraps events in envelopes and broadcasts them.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use harness_ports::{Clock, EventSink};
use harness_protocol::{Envelope, Event};
use harness_types::{Seq, SessionId};
use tokio::sync::broadcast;

/// Assigns sequence numbers and timestamps, then broadcasts to every subscriber.
///
/// In Phase 1 every event (including streaming deltas) gets a sequence number
/// from this counter. From Phase 3 the event log assigns numbers to persisted events.
pub struct BroadcastSink {
    session: SessionId,
    next: AtomicU64,
    clock: Arc<dyn Clock>,
    tx: broadcast::Sender<Envelope>,
}

impl BroadcastSink {
    /// Creates a sink for `session` that sends on `tx`.
    #[must_use]
    pub fn new(session: SessionId, clock: Arc<dyn Clock>, tx: broadcast::Sender<Envelope>) -> Self {
        Self {
            session,
            next: AtomicU64::new(Seq::FIRST.0),
            clock,
            tx,
        }
    }
}

impl std::fmt::Debug for BroadcastSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BroadcastSink")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl EventSink for BroadcastSink {
    fn emit(&self, event: Event) {
        let seq = Seq(self.next.fetch_add(1, Ordering::SeqCst));
        let envelope = Envelope {
            seq,
            ts: self.clock.now_ms(),
            session: self.session,
            event,
        };
        // No subscribers is fine: nobody is watching this session right now.
        let _ = self.tx.send(envelope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_testkit::FixedClock;

    #[test]
    fn envelopes_get_increasing_sequence_numbers_and_the_clock_time() {
        let (tx, mut rx) = broadcast::channel(8);
        let session = SessionId::new();
        let sink = BroadcastSink::new(session, Arc::new(FixedClock::new(42)), tx);
        sink.emit(Event::ToolStarted {
            call_id: "a".into(),
        });
        sink.emit(Event::ToolStarted {
            call_id: "b".into(),
        });
        let first = rx.try_recv().unwrap();
        let second = rx.try_recv().unwrap();
        assert_eq!((first.seq, second.seq), (Seq(1), Seq(2)));
        assert_eq!((first.ts, first.session), (42, session));
    }

    #[test]
    fn emitting_without_subscribers_does_not_panic() {
        let (tx, rx) = broadcast::channel(8);
        drop(rx);
        BroadcastSink::new(SessionId::new(), Arc::new(FixedClock::new(0)), tx).emit(
            Event::ToolStarted {
                call_id: "a".into(),
            },
        );
    }
}
