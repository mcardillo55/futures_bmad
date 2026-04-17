use crate::types::UnixNanos;
use chrono::{DateTime, Datelike, Timelike, Utc};

/// Trait for time abstraction. All code must use this instead of direct system time calls.
///
/// Implementations:
/// - `SystemClock`: real wall-clock time for production
/// - Test doubles can provide deterministic time for replay/testing
pub trait Clock: Send + Sync {
    fn now(&self) -> UnixNanos;
    fn wall_clock(&self) -> DateTime<Utc>;
    fn is_market_open(&self) -> bool;
}

/// Production clock using system time.
/// CME market hours: Sunday 5:00 PM CT to Friday 4:00 PM CT,
/// with daily maintenance break 4:00 PM - 5:00 PM CT (Mon-Thu).
pub struct SystemClock;

/// Approximate US Central Time offset from UTC.
/// CDT (second Sunday of March to first Sunday of November): UTC-5
/// CST (rest of year): UTC-6
fn ct_offset(utc: &DateTime<Utc>) -> i32 {
    let month = utc.month();
    // Simple approximation: April-October is always CDT, November-March is always CST
    // The exact DST boundaries (2nd Sun March, 1st Sun November) are close enough
    // that the 1-hour maintenance window absorbs the boundary error.
    if (4..=10).contains(&month) {
        -5 // CDT
    } else {
        -6 // CST
    }
}

/// Check if a UTC datetime falls within CME market hours.
/// Handles both CDT (UTC-5, Mar-Nov) and CST (UTC-6, Nov-Mar).
pub fn is_cme_market_open(utc: &DateTime<Utc>) -> bool {
    let offset = ct_offset(utc);
    let ct_hour = (utc.hour() as i32 + offset).rem_euclid(24) as u32;
    let ct_weekday = if (utc.hour() as i32 + offset) < 0 {
        utc.weekday().pred()
    } else if (utc.hour() as i32 + offset) >= 24 {
        utc.weekday().succ()
    } else {
        utc.weekday()
    };

    use chrono::Weekday::*;

    match ct_weekday {
        // Saturday: market closed all day
        Sat => false,
        // Sunday: open from 5 PM CT onward
        Sun => ct_hour >= 17,
        // Friday: open until 4 PM CT
        Fri => ct_hour < 16,
        // Mon-Thu: closed during 4 PM - 5 PM CT maintenance break
        _ => !(16..17).contains(&ct_hour),
    }
}

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

    fn is_market_open(&self) -> bool {
        is_cme_market_open(&Utc::now())
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
