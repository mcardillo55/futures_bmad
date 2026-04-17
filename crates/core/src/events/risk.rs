use crate::types::UnixNanos;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitBreakerType {
    DailyLossLimit,
    ConsecutiveLosses,
    MaxDrawdown,
    ConnectionLoss,
    DataGap,
}

#[derive(Debug, Clone)]
pub struct CircuitBreakerEvent {
    pub breaker_type: CircuitBreakerType,
    pub triggered: bool,
    pub timestamp: UnixNanos,
    pub reason: String,
}
