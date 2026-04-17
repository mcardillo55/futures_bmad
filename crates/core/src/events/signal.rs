use crate::types::UnixNanos;

#[derive(Debug, Clone)]
pub struct SignalEvent {
    pub signal_name: &'static str,
    pub value: f64,
    pub timestamp: UnixNanos,
    pub book_snapshot_id: Option<u64>,
}
