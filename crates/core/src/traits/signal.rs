use crate::events::MarketEvent;
use crate::order_book::OrderBook;
use crate::traits::clock::Clock;
use crate::types::UnixNanos;

/// Snapshot of a signal's state for deterministic replay and audit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalSnapshot {
    pub name: &'static str,
    pub value: Option<f64>,
    pub valid: bool,
    pub timestamp: UnixNanos,
}

/// Trait for incremental market microstructure signals.
///
/// Each `update()` call is O(1) — signals maintain rolling state and update
/// incrementally on each market event. `reset()` clears all internal state
/// for replay. `snapshot()` captures current state for deterministic audit.
pub trait Signal: Send {
    /// Incrementally update signal with new market data. Returns signal value if valid.
    fn update(
        &mut self,
        book: &OrderBook,
        trade: Option<&MarketEvent>,
        clock: &dyn Clock,
    ) -> Option<f64>;

    /// Signal identifier.
    fn name(&self) -> &'static str;

    /// Whether the signal has sufficient data to produce valid output.
    fn is_valid(&self) -> bool;

    /// Clear all internal state for replay.
    fn reset(&mut self);

    /// Capture current state for deterministic replay and audit.
    fn snapshot(&self) -> SignalSnapshot;
}
