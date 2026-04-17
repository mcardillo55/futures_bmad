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
    pub fn to_datetime(&self) -> DateTime<Utc> {
        let secs = (self.0 / 1_000_000_000) as i64;
        let nsecs = (self.0 % 1_000_000_000) as u32;
        Utc.timestamp_opt(secs, nsecs).unwrap()
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
        let back = nanos.to_datetime();
        assert_eq!(dt, back);
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
