//! [`BroadcastSink`]: wraps events in envelopes and broadcasts them.

use std::sync::{Arc, Mutex, PoisonError};

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
    // Held while a number is assigned and the envelope sent, so subscribers
    // see events in sequence order even when tools emit from several tasks.
    next: Mutex<u64>,
    clock: Arc<dyn Clock>,
    tx: broadcast::Sender<Envelope>,
}

impl BroadcastSink {
    /// Creates a sink for `session` that sends on `tx`.
    #[must_use]
    pub fn new(session: SessionId, clock: Arc<dyn Clock>, tx: broadcast::Sender<Envelope>) -> Self {
        Self {
            session,
            next: Mutex::new(Seq::FIRST.0),
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
        let mut event = Some(event);
        self.emit_with(&mut |_| {
            event
                .take()
                .unwrap_or_else(|| unreachable!("emit_with calls the builder exactly once"))
        });
    }

    fn emit_with(&self, build: &mut dyn FnMut(Seq) -> Event) -> Seq {
        let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = Seq(*next);
        *next += 1;
        let envelope = Envelope {
            seq,
            ts: self.clock.now_ms(),
            session: self.session,
            event: build(seq),
        };
        // No subscribers is fine: nobody is watching this session right now.
        let _ = self.tx.send(envelope);
        seq
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
    fn emit_with_builds_the_event_from_its_own_sequence_number() {
        let (tx, mut rx) = broadcast::channel(8);
        let sink = BroadcastSink::new(SessionId::new(), Arc::new(FixedClock::new(0)), tx);
        sink.emit(Event::ToolStarted {
            call_id: "a".into(),
        });
        let seq = sink.emit_with(&mut |seq| Event::ToolStarted {
            call_id: format!("built-at-{}", seq.0),
        });
        let _ = rx.try_recv().unwrap();
        let built = rx.try_recv().unwrap();
        assert_eq!(seq, Seq(2));
        assert_eq!(built.seq, Seq(2));
        assert_eq!(
            built.event,
            Event::ToolStarted {
                call_id: "built-at-2".into()
            }
        );
    }

    #[test]
    fn concurrent_emits_arrive_in_sequence_order() {
        let (tx, mut rx) = broadcast::channel(4096);
        let sink = Arc::new(BroadcastSink::new(
            SessionId::new(),
            Arc::new(FixedClock::new(0)),
            tx,
        ));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let sink = Arc::clone(&sink);
                std::thread::spawn(move || {
                    for _ in 0..500 {
                        sink.emit(Event::ToolStarted {
                            call_id: "x".into(),
                        });
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let seqs: Vec<u64> = std::iter::from_fn(|| rx.try_recv().ok().map(|e| e.seq.0)).collect();
        assert_eq!(seqs, (1..=2000).collect::<Vec<_>>());
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
