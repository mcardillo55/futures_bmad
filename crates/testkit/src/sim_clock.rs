use chrono::{DateTime, TimeZone, Utc};
use futures_bmad_core::{Clock, UnixNanos};
use std::sync::atomic::{AtomicU64, Ordering};

/// Deterministic clock for testing. Thread-safe via AtomicU64.
pub struct SimClock {
    current: AtomicU64,
}

impl SimClock {
    pub fn new(start_nanos: u64) -> Self {
        Self {
            current: AtomicU64::new(start_nanos),
        }
    }

    pub fn advance_by(&self, nanos: u64) {
        self.current.fetch_add(nanos, Ordering::SeqCst);
    }

    pub fn set_time(&self, time: UnixNanos) {
        self.current.store(time.as_nanos(), Ordering::SeqCst);
    }
}

impl Clock for SimClock {
    fn now(&self) -> UnixNanos {
        UnixNanos::new(self.current.load(Ordering::SeqCst))
    }

    fn wall_clock(&self) -> DateTime<Utc> {
        let nanos = self.current.load(Ordering::SeqCst);
        let secs = (nanos / 1_000_000_000) as i64;
        let nsecs = (nanos % 1_000_000_000) as u32;
        Utc.timestamp_opt(secs, nsecs).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_advance() {
        let clock = SimClock::new(1000);
        assert_eq!(clock.now().as_nanos(), 1000);
        clock.advance_by(500);
        assert_eq!(clock.now().as_nanos(), 1500);
        clock.advance_by(500);
        assert_eq!(clock.now().as_nanos(), 2000);
    }

    #[test]
    fn set_time_overrides() {
        let clock = SimClock::new(1000);
        clock.set_time(UnixNanos::new(9999));
        assert_eq!(clock.now().as_nanos(), 9999);
    }

    #[test]
    fn wall_clock_converts() {
        use chrono::Datelike;
        // 2025-01-15T00:00:00Z in nanos
        let nanos = 1_736_899_200_000_000_000u64;
        let clock = SimClock::new(nanos);
        let dt = clock.wall_clock();
        assert_eq!(dt.year(), 2025);
    }

    fn _assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn sim_clock_is_send_sync() {
        _assert_send_sync::<SimClock>();
    }
}
