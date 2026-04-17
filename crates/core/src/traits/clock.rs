use crate::types::UnixNanos;
use chrono::{DateTime, Utc};

/// Trait for time abstraction. All code must use this instead of direct system time calls.
///
/// Implementations:
/// - `SystemClock`: real wall-clock time for production
/// - Test doubles can provide deterministic time for replay/testing
pub trait Clock: Send + Sync {
    fn now(&self) -> UnixNanos;
    fn wall_clock(&self) -> DateTime<Utc>;
}

/// Production clock using system time.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixNanos {
        let duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before Unix epoch");
        UnixNanos::new(duration.as_nanos() as u64)
    }

    fn wall_clock(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_returns_reasonable_timestamp() {
        let clock = SystemClock;
        let now = clock.now();
        // Should be after 2020-01-01 in nanos
        let jan_2020_nanos: u64 = 1_577_836_800_000_000_000;
        assert!(now.as_nanos() > jan_2020_nanos);
    }

    #[test]
    fn system_clock_wall_clock_is_current() {
        let clock = SystemClock;
        let dt = clock.wall_clock();
        assert!(dt.timestamp() > 1_577_836_800); // after 2020
    }

    fn _assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn clock_is_send_sync() {
        _assert_send_sync::<SystemClock>();
    }
}
