//! [`FixedClock`]: a [`Clock`] tests can set and advance.

use std::sync::atomic::{AtomicI64, Ordering};

use harness_ports::Clock;

/// A manually controlled clock. Starts at the given Unix time in milliseconds.
///
/// # Examples
/// ```
/// use harness_ports::Clock;
/// use harness_testkit::FixedClock;
/// let clock = FixedClock::new(1_000);
/// clock.advance_ms(500);
/// assert_eq!(clock.now_ms(), 1_500);
/// ```
#[derive(Debug, Default)]
pub struct FixedClock(AtomicI64);

impl FixedClock {
    /// Creates a clock reading `now_ms`.
    #[must_use]
    pub fn new(now_ms: i64) -> Self {
        Self(AtomicI64::new(now_ms))
    }

    /// Sets the time.
    pub fn set_ms(&self, now_ms: i64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }

    /// Moves the time forward (or back, with a negative delta).
    pub fn advance_ms(&self, delta_ms: i64) {
        self.0.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_advance_change_the_reading() {
        let c = FixedClock::new(10);
        c.advance_ms(5);
        assert_eq!(c.now_ms(), 15);
        c.set_ms(100);
        assert_eq!(c.now_ms(), 100);
    }
}
