use crate::traits::RegimeState;
use crate::types::UnixNanos;

#[derive(Debug, Clone, Copy)]
pub struct RegimeTransition {
    pub from: RegimeState,
    pub to: RegimeState,
    pub timestamp: UnixNanos,
}

#[derive(Debug, Clone)]
pub struct ConnectionStateChange {
    pub connected: bool,
    pub endpoint: String,
    pub timestamp: UnixNanos,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct HeartbeatEvent {
    pub timestamp: UnixNanos,
    pub sequence: u64,
}
