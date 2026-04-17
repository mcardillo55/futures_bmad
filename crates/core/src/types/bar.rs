use super::{FixedPrice, UnixNanos};

/// OHLCV bar with fixed-point prices and nanosecond timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub open: FixedPrice,
    pub high: FixedPrice,
    pub low: FixedPrice,
    pub close: FixedPrice,
    pub volume: u64,
    pub timestamp: UnixNanos,
}
