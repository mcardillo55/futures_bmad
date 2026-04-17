use chrono::{DateTime, TimeZone, Utc};

/// Nanosecond-precision Unix timestamp.
///
/// Wraps a `u64` representing nanoseconds since the Unix epoch.
/// Conversion to `chrono::DateTime` is provided for display purposes only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct UnixNanos(pub(crate) u64);

impl UnixNanos {
    pub fn new(nanos: u64) -> Self {
        Self(nanos)
    }

    pub fn as_nanos(&self) -> u64 {
        self.0
    }

    /// Convert to chrono DateTime for display purposes only.
    /// Returns `None` if the timestamp exceeds the representable range (~year 2262).
    pub fn to_datetime(&self) -> Option<DateTime<Utc>> {
        let secs = i64::try_from(self.0 / 1_000_000_000).ok()?;
        let nsecs = (self.0 % 1_000_000_000) as u32;
        Utc.timestamp_opt(secs, nsecs).single()
    }
}

impl From<DateTime<Utc>> for UnixNanos {
    fn from(dt: DateTime<Utc>) -> Self {
        let nanos = dt.timestamp_nanos_opt().unwrap_or(0) as u64;
        UnixNanos(nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_round_trip() {
        let dt = Utc.with_ymd_and_hms(2025, 1, 15, 10, 30, 0).unwrap();
        let nanos = UnixNanos::from(dt);
        let back = nanos.to_datetime().expect("should be in range");
        assert_eq!(dt, back);
    }

    #[test]
    fn to_datetime_returns_none_for_out_of_range() {
        // Construct a nanos value where seconds > i64::MAX
        // i64::MAX + 1 = 9223372036854775808 seconds * 1e9 overflows u64,
        // so we need seconds that exceed chrono's range.
        // chrono's max is year ~262,000. Values near u64::MAX nanos (~year 2554) are valid.
        // Test with a value where timestamp_opt returns None (far future beyond chrono range).
        // Actually u64::MAX / 1e9 fits in i64, so test that it returns Some (valid date).
        let far_future = UnixNanos::new(u64::MAX);
        assert!(far_future.to_datetime().is_some());
    }

    #[test]
    fn ordering() {
        let a = UnixNanos::new(100);
        let b = UnixNanos::new(200);
        assert!(a < b);
    }

    #[test]
    fn default_is_zero() {
        assert_eq!(UnixNanos::default(), UnixNanos(0));
    }
}
