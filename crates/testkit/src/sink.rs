//! [`RecordingSink`]: an [`EventSink`] that keeps every event for assertions.

use std::sync::{Mutex, PoisonError};

use harness_ports::EventSink;
use harness_protocol::Event;

/// Collects emitted events in order.
///
/// # Examples
/// ```
/// use harness_ports::EventSink;
/// use harness_protocol::Event;
/// use harness_testkit::RecordingSink;
/// let sink = RecordingSink::default();
/// sink.emit(Event::ToolStarted { call_id: "c1".into() });
/// assert_eq!(sink.events().len(), 1);
/// ```
#[derive(Debug, Default)]
pub struct RecordingSink {
    events: Mutex<Vec<Event>>,
}

impl RecordingSink {
    /// Every event emitted so far, in order.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }
}
