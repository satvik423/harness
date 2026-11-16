//! Small runtime ports: [`EventSink`] and [`Clock`].

use harness_protocol::Event;

/// Somewhere to send live events (bounded and non-blocking in real implementations).
pub trait EventSink: Send + Sync {
    /// Emits an event. Must not block; a slow subscriber may miss deltas.
    fn emit(&self, event: Event);
}

/// A source of wall-clock time, injectable so tests are deterministic.
pub trait Clock: Send + Sync {
    /// Current Unix time in milliseconds.
    fn now_ms(&self) -> i64;
}

/// The real system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        // A clock before 1970 is a broken system; report 0 rather than panic.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_reports_a_time_after_2025() {
        // 2025-01-01T00:00:00Z
        assert!(SystemClock.now_ms() > 1_735_689_600_000);
    }
}
