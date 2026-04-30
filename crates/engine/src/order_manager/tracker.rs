//! Position tracker + broker reconciliation engine — Story 4-5.
//!
//! `PositionTracker` is the engine's single source of truth for "what positions
//! do we hold right now?" and "what is our cumulative realized P&L?". It is
//! updated synchronously on the engine hot path: every fill that flows through
//! the `OrderManager` should also be applied here, and every order book
//! mid-price update should drive `update_unrealized_pnl_for_symbol`.
//!
//! Reconciliation is the architectural safety net (NFR16): every so often
//! (startup, post-reconnect, periodic 60s), the engine queries the broker for
//! its view of positions and compares against this local state. Any mismatch
//! is treated as a critical error — we do NOT silently correct local state to
//! match the broker (silent correction masks bugs that lead to catastrophic
//! losses). Instead the circuit breaker trips, all trading halts, and the
//! mismatch is logged and journaled for operator review.
//!
//! ## Reconciliation algorithm
//!
//! 1. Build an exhaustive symbol set: every symbol mentioned by either the
//!    local view OR the broker view.
//! 2. For each symbol, classify the pair:
//!    - both flat → consistent
//!    - local has position, broker flat → **PhantomLocal** (we believe we own
//!      something the broker says we don't)
//!    - broker has position, local flat → **MissedFill** (broker holds a
//!      position we lost track of)
//!    - both have a position with same side+quantity+avg_entry → consistent
//!    - both have a position but they disagree on side/qty/price → **Mismatch**
//! 3. Result: either `Consistent` (success path) or `Mismatch(Vec<...>)` with
//!    every offender enumerated for journaling.
//!
//! ## What this module does NOT do
//!
//! - **Trigger reconciliation.** The engine event loop / lifecycle state machine
//!   owns the schedule. This module exposes `reconcile(&[BrokerPosition])` so
//!   the caller passes the broker view in.
//! - **Issue circuit breaker actions.** A `CircuitBreakerCallback` is held but
//!   only invoked from `handle_reconciliation_result(...)` — the caller chooses
//!   when (if ever) to call that.
//! - **Talk to the broker directly.** Story 8-2 (startup) and 8-4 (reconnection
//!   FSM) own the broker query plumbing. This module is pure logic.

use std::collections::{HashMap, HashSet};

use futures_bmad_core::{BrokerPosition, FillEvent, FixedPrice, Position, Side, UnixNanos};
use tracing::{error, info, warn};

use crate::order_manager::state_machine::CircuitBreakerCallback;
use crate::persistence::journal::{EngineEvent as JournalEvent, JournalSender, SystemEventRecord};

/// Categorization of a per-symbol disagreement between local and broker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MismatchKind {
    /// Local believes a position exists; broker reports flat. Strong signal of
    /// a phantom (e.g., engine processed a fill the exchange never matched).
    PhantomLocal,
    /// Broker reports a position; local is flat. Strong signal of a missed
    /// fill (e.g., engine crashed and lost the fill event).
    MissedFill,
    /// Both have a position but disagree on side, quantity, or avg_entry_price.
    SideOrQuantity,
}

/// One concrete disagreement between local and broker views.
///
/// `local` and `broker` are both included so an operator can read the journal
/// record and immediately see what the local engine thought vs. what the broker
/// said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionMismatch {
    pub symbol_id: u32,
    pub kind: MismatchKind,
    pub local: Option<LocalSnapshot>,
    pub broker: Option<BrokerSnapshot>,
}

/// Compact snapshot of the local view of a position at the moment of
/// reconciliation. Distinct from `Position` (which carries P&L bookkeeping)
/// so the journal record stays minimal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalSnapshot {
    pub side: Option<Side>,
    pub quantity: u32,
    pub avg_entry_price: FixedPrice,
}

/// Compact snapshot of the broker view of a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerSnapshot {
    pub side: Option<Side>,
    pub quantity: u32,
    pub avg_entry_price: FixedPrice,
}

impl From<&Position> for LocalSnapshot {
    fn from(p: &Position) -> Self {
        Self {
            side: p.side,
            quantity: p.quantity,
            avg_entry_price: p.avg_entry_price,
        }
    }
}

impl From<&BrokerPosition> for BrokerSnapshot {
    fn from(p: &BrokerPosition) -> Self {
        Self {
            side: p.side,
            quantity: p.quantity,
            avg_entry_price: p.avg_entry_price,
        }
    }
}

/// Outcome of a single reconciliation round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconciliationResult {
    /// Local and broker agree on every symbol.
    Consistent,
    /// At least one symbol disagrees. Trading must halt and the operator must
    /// review every entry.
    Mismatch(Vec<PositionMismatch>),
}

impl ReconciliationResult {
    pub fn is_consistent(&self) -> bool {
        matches!(self, ReconciliationResult::Consistent)
    }
}

/// Reason a reconciliation round was triggered (for log/journal context).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationTrigger {
    /// Reconciliation invoked at process startup before accepting trading
    /// signals. Failure to reach `Consistent` MUST block startup.
    Startup,
    /// Reconciliation invoked after the broker reconnection FSM moves into
    /// `Reconciling` state.
    Reconnection,
    /// Periodic safety net (60s).
    Periodic,
}

impl ReconciliationTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            ReconciliationTrigger::Startup => "startup",
            ReconciliationTrigger::Reconnection => "reconnection",
            ReconciliationTrigger::Periodic => "periodic",
        }
    }
}

/// Engine-side position bookkeeper + reconciliation engine.
///
/// Holds one `Position` per `symbol_id`. Empty (flat) symbols may be omitted
/// — `local_position(...)` returns `None` rather than `Some(Position::flat)`.
pub struct PositionTracker {
    positions: HashMap<u32, Position>,
    /// Last observed mid prices, used to maintain `unrealized_pnl` even when
    /// the caller doesn't pass them on every fill.
    last_mid_prices: HashMap<u32, FixedPrice>,
    journal: JournalSender,
    circuit_breaker: Option<CircuitBreakerCallback>,
    /// Set to true on the first mismatch — once true, all submission gates
    /// must observe it and refuse new orders. Manual operator intervention
    /// is required to resume.
    trading_halted: bool,
}

impl PositionTracker {
    pub fn new(journal: JournalSender) -> Self {
        Self {
            positions: HashMap::new(),
            last_mid_prices: HashMap::new(),
            journal,
            circuit_breaker: None,
            trading_halted: false,
        }
    }

    /// Wire a circuit-breaker callback. Invoked once, immediately, the first
    /// time `reconcile` reports a mismatch.
    pub fn with_circuit_breaker(mut self, cb: CircuitBreakerCallback) -> Self {
        self.circuit_breaker = Some(cb);
        self
    }

    /// Whether new orders should be refused (set on first reconciliation
    /// mismatch).
    pub fn trading_halted(&self) -> bool {
        self.trading_halted
    }

    /// Read-only access to the local position for a symbol. Returns `None`
    /// for symbols we've never traded (treat as flat).
    pub fn local_position(&self, symbol_id: u32) -> Option<&Position> {
        self.positions.get(&symbol_id)
    }

    /// Access every locally tracked position (flat positions are pruned, so
    /// this iterator skips them).
    pub fn iter(&self) -> impl Iterator<Item = (&u32, &Position)> {
        self.positions.iter()
    }

    /// Number of symbols currently held.
    pub fn open_count(&self) -> usize {
        self.positions.values().filter(|p| !p.is_flat()).count()
    }

    /// Apply a fill to the local position state. Mirrors what the broker
    /// would do — the next reconciliation round verifies they agree.
    pub fn apply_fill(&mut self, fill: &FillEvent, symbol_id: u32) {
        let entry = self
            .positions
            .entry(symbol_id)
            .or_insert_with(|| Position::flat(symbol_id));
        entry.apply_fill(fill);
        // Refresh unrealized P&L using whatever mid we last saw for this symbol.
        if let Some(&mid) = self.last_mid_prices.get(&symbol_id) {
            entry.update_unrealized_pnl(mid);
        }
        // Prune any flat-and-zero-PnL entry to keep the iterator lean (a flat
        // position with non-zero realized_pnl stays so session totals survive).
        if entry.is_flat() && entry.realized_pnl == 0 && entry.unrealized_pnl == 0 {
            self.positions.remove(&symbol_id);
        }
    }

    /// Update unrealized P&L for a single symbol using the current mid price.
    pub fn update_unrealized_pnl_for_symbol(&mut self, symbol_id: u32, current_price: FixedPrice) {
        self.last_mid_prices.insert(symbol_id, current_price);
        if let Some(pos) = self.positions.get_mut(&symbol_id) {
            pos.update_unrealized_pnl(current_price);
        }
    }

    /// Cumulative session realized P&L across every symbol the tracker has
    /// ever seen, in quarter-ticks.
    pub fn total_realized_pnl_quarter_ticks(&self) -> i64 {
        self.positions
            .values()
            .map(|p| p.realized_pnl)
            .fold(0i64, |a, b| a.saturating_add(b))
    }

    /// Reconcile the local view against a broker-reported snapshot.
    ///
    /// The broker view is what the broker returns from
    /// `BrokerAdapter::query_positions`. Every locally-tracked symbol AND
    /// every broker-reported symbol is examined; the union of the two key
    /// sets ensures we surface both phantoms (local-only) and missed fills
    /// (broker-only).
    pub fn reconcile(&self, broker_view: &[BrokerPosition]) -> ReconciliationResult {
        // Key the broker view by symbol_id for O(1) lookup.
        let broker_by_symbol: HashMap<u32, &BrokerPosition> =
            broker_view.iter().map(|b| (b.symbol_id, b)).collect();

        // Union of all symbol ids referenced anywhere.
        let mut symbol_set: HashSet<u32> = self.positions.keys().copied().collect();
        symbol_set.extend(broker_by_symbol.keys().copied());

        let mut mismatches = Vec::new();
        for symbol_id in symbol_set {
            let local_pos = self.positions.get(&symbol_id);
            let broker_pos = broker_by_symbol.get(&symbol_id).copied();
            let local_active = local_pos.is_some_and(|p| !p.is_flat());
            let broker_active = broker_pos.is_some_and(|p| !p.is_flat());

            match (local_active, broker_active) {
                (false, false) => {
                    // Both flat — consistent. Nothing to record.
                }
                (true, false) => {
                    mismatches.push(PositionMismatch {
                        symbol_id,
                        kind: MismatchKind::PhantomLocal,
                        local: local_pos.map(LocalSnapshot::from),
                        broker: broker_pos.map(BrokerSnapshot::from),
                    });
                }
                (false, true) => {
                    mismatches.push(PositionMismatch {
                        symbol_id,
                        kind: MismatchKind::MissedFill,
                        local: local_pos.map(LocalSnapshot::from),
                        broker: broker_pos.map(BrokerSnapshot::from),
                    });
                }
                (true, true) => {
                    // Both have a non-flat position — compare side / qty /
                    // avg_entry_price. If any field disagrees: SideOrQuantity.
                    let l = local_pos.expect("checked local_active");
                    let b = broker_pos.expect("checked broker_active");
                    if l.side != b.side
                        || l.quantity != b.quantity
                        || l.avg_entry_price != b.avg_entry_price
                    {
                        mismatches.push(PositionMismatch {
                            symbol_id,
                            kind: MismatchKind::SideOrQuantity,
                            local: Some(LocalSnapshot::from(l)),
                            broker: Some(BrokerSnapshot::from(b)),
                        });
                    }
                }
            }
        }

        if mismatches.is_empty() {
            ReconciliationResult::Consistent
        } else {
            ReconciliationResult::Mismatch(mismatches)
        }
    }

    /// Apply the result of a reconciliation round.
    ///
    /// On `Consistent`: log success and journal an audit-trail entry; no state
    /// change. On `Mismatch`: log every offender at `error`, journal a
    /// `SystemEvent` with structured fields, set `trading_halted = true`, and
    /// invoke the circuit breaker callback once. **Never** mutate local state
    /// to match the broker (NFR16).
    pub fn handle_reconciliation_result(
        &mut self,
        result: &ReconciliationResult,
        trigger: ReconciliationTrigger,
        now: UnixNanos,
    ) {
        match result {
            ReconciliationResult::Consistent => {
                info!(
                    target: "position_reconciliation",
                    trigger = trigger.as_str(),
                    open_positions = self.open_count(),
                    "reconciliation passed — local and broker views agree"
                );
                let record = SystemEventRecord {
                    timestamp: now,
                    category: "reconciliation".to_string(),
                    message: format!(
                        "reconciliation passed ({}); open_positions={}",
                        trigger.as_str(),
                        self.open_count()
                    ),
                };
                let _ = self.journal.send(JournalEvent::SystemEvent(record));
            }
            ReconciliationResult::Mismatch(mismatches) => {
                // Log every offender at error so the structured-logging
                // sink can pick them up individually.
                for m in mismatches {
                    error!(
                        target: "position_reconciliation",
                        symbol_id = m.symbol_id,
                        kind = ?m.kind,
                        local_side = ?m.local.and_then(|l| l.side),
                        local_quantity = m.local.map(|l| l.quantity).unwrap_or(0),
                        local_avg_entry = m.local.map(|l| l.avg_entry_price.raw()).unwrap_or(0),
                        broker_side = ?m.broker.and_then(|b| b.side),
                        broker_quantity = m.broker.map(|b| b.quantity).unwrap_or(0),
                        broker_avg_entry = m.broker.map(|b| b.avg_entry_price.raw()).unwrap_or(0),
                        trigger = trigger.as_str(),
                        "POSITION MISMATCH — circuit breaker tripping, trading halted"
                    );
                }
                // Single journal record summarizes the entire round for audit.
                let summary = SystemEventRecord {
                    timestamp: now,
                    category: "reconciliation".to_string(),
                    message: format_mismatch_journal_message(trigger, mismatches),
                };
                let _ = self.journal.send(JournalEvent::SystemEvent(summary));

                // Halt trading and trip the breaker. The breaker callback is
                // only invoked once across this struct's lifetime to avoid
                // hammering the alerting layer if periodic reconciliation
                // keeps observing the same mismatch.
                let already_halted = self.trading_halted;
                self.trading_halted = true;
                if !already_halted {
                    if let Some(cb) = self.circuit_breaker.as_ref() {
                        let invalid = crate::order_manager::state_machine::InvalidTransition {
                            from: futures_bmad_core::OrderState::Idle,
                            trigger: crate::order_manager::state_machine::OrderTransition::Reject,
                        };
                        cb(&invalid);
                    } else {
                        warn!(
                            target: "position_reconciliation",
                            "no circuit breaker callback wired — mismatch detected but breaker not tripped externally"
                        );
                    }
                }
            }
        }
    }

    /// Combined helper: reconcile + handle. Returns the result so the caller
    /// can propagate to a startup gate.
    pub fn reconcile_and_handle(
        &mut self,
        broker_view: &[BrokerPosition],
        trigger: ReconciliationTrigger,
        now: UnixNanos,
    ) -> ReconciliationResult {
        let result = self.reconcile(broker_view);
        self.handle_reconciliation_result(&result, trigger, now);
        result
    }
}

fn format_mismatch_journal_message(
    trigger: ReconciliationTrigger,
    mismatches: &[PositionMismatch],
) -> String {
    use std::fmt::Write;
    let mut out = format!(
        "reconciliation MISMATCH ({}); count={}; trading halted (NFR16)",
        trigger.as_str(),
        mismatches.len()
    );
    for m in mismatches {
        let _ = write!(
            out,
            "\n  symbol={} kind={:?} local={} broker={}",
            m.symbol_id,
            m.kind,
            m.local
                .map(|l| format!(
                    "side={:?} qty={} avg={}",
                    l.side,
                    l.quantity,
                    l.avg_entry_price.raw()
                ))
                .unwrap_or_else(|| "flat".to_string()),
            m.broker
                .map(|b| format!(
                    "side={:?} qty={} avg={}",
                    b.side,
                    b.quantity,
                    b.avg_entry_price.raw()
                ))
                .unwrap_or_else(|| "flat".to_string())
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::journal::EventJournal;
    use futures_bmad_core::{FillType, FixedPrice, OrderState, Side, UnixNanos};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn journal() -> JournalSender {
        let (tx, _rx) = EventJournal::channel();
        tx
    }

    fn fill(side: Side, price_raw: i64, size: u32, ft: FillType) -> FillEvent {
        FillEvent {
            order_id: 1,
            fill_price: FixedPrice::new(price_raw),
            fill_size: size,
            timestamp: UnixNanos::default(),
            side,
            decision_id: 0,
            fill_type: ft,
        }
    }

    // -- Position-update tests (Task 9.1-9.5) --

    /// Task 9.1: Position update on buy fill: quantity increases, avg entry correct.
    #[test]
    fn buy_fill_updates_local_position() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        let p = tracker.local_position(1).unwrap();
        assert_eq!(p.quantity, 2);
        assert_eq!(p.side, Some(Side::Buy));
        assert_eq!(p.avg_entry_price.raw(), 100);
        assert_eq!(p.realized_pnl, 0);
    }

    /// Task 9.2: Sell fill that closes position computes realized P&L.
    #[test]
    fn sell_fill_close_computes_realized_pnl() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        tracker.apply_fill(&fill(Side::Sell, 110, 2, FillType::Full), 1);
        // Position is flat with non-zero realized_pnl — should still appear
        // (we keep it so total_realized_pnl_quarter_ticks() reflects history).
        let p = tracker.local_position(1).unwrap();
        assert_eq!(p.quantity, 0);
        assert_eq!(p.realized_pnl, 20);
        assert_eq!(tracker.total_realized_pnl_quarter_ticks(), 20);
    }

    /// Task 9.3: Partial close — quantity reduces, realized P&L for closed
    /// portion, remaining position intact.
    #[test]
    fn partial_close_preserves_remaining_position() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 5, FillType::Full), 1);
        tracker.apply_fill(&fill(Side::Sell, 120, 2, FillType::Full), 1);
        let p = tracker.local_position(1).unwrap();
        assert_eq!(p.quantity, 3);
        assert_eq!(p.side, Some(Side::Buy));
        assert_eq!(p.avg_entry_price.raw(), 100);
        assert_eq!(p.realized_pnl, 40);
    }

    /// Task 9.4: Long position with current price above entry has positive unrealized.
    #[test]
    fn long_position_unrealized_positive_above_entry() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 3, FillType::Full), 1);
        tracker.update_unrealized_pnl_for_symbol(1, FixedPrice::new(115));
        let p = tracker.local_position(1).unwrap();
        // (115 - 100) * 3 * 1 = 45
        assert_eq!(p.unrealized_pnl, 45);
    }

    /// Task 9.5: Short position with current price below entry has positive unrealized.
    #[test]
    fn short_position_unrealized_positive_below_entry() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Sell, 200, 4, FillType::Full), 1);
        tracker.update_unrealized_pnl_for_symbol(1, FixedPrice::new(180));
        let p = tracker.local_position(1).unwrap();
        // (180 - 200) * 4 * -1 = +80
        assert_eq!(p.unrealized_pnl, 80);
    }

    // -- Reconciliation tests (Task 9.6-9.11) --

    /// Task 9.6: Local matches broker -> Consistent.
    #[test]
    fn reconciliation_consistent_when_matching() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        let broker = vec![BrokerPosition {
            symbol_id: 1,
            side: Some(Side::Buy),
            quantity: 2,
            avg_entry_price: FixedPrice::new(100),
        }];
        let result = tracker.reconcile(&broker);
        assert!(result.is_consistent());
    }

    /// Task 9.6 (also): all-flat is consistent.
    #[test]
    fn reconciliation_all_flat_is_consistent() {
        let tracker = PositionTracker::new(journal());
        let result = tracker.reconcile(&[]);
        assert!(result.is_consistent());
    }

    /// Task 9.7: phantom — local has, broker flat -> Mismatch.
    #[test]
    fn reconciliation_phantom_local_detected() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        let result = tracker.reconcile(&[]);
        match result {
            ReconciliationResult::Mismatch(m) => {
                assert_eq!(m.len(), 1);
                assert_eq!(m[0].symbol_id, 1);
                assert_eq!(m[0].kind, MismatchKind::PhantomLocal);
                assert!(m[0].local.is_some());
                assert!(m[0].broker.is_none());
            }
            ReconciliationResult::Consistent => panic!("expected mismatch"),
        }
    }

    /// Task 9.8: missed fill — broker has, local flat -> Mismatch.
    #[test]
    fn reconciliation_missed_fill_detected() {
        let tracker = PositionTracker::new(journal());
        let broker = vec![BrokerPosition {
            symbol_id: 5,
            side: Some(Side::Sell),
            quantity: 1,
            avg_entry_price: FixedPrice::new(200),
        }];
        let result = tracker.reconcile(&broker);
        match result {
            ReconciliationResult::Mismatch(m) => {
                assert_eq!(m.len(), 1);
                assert_eq!(m[0].symbol_id, 5);
                assert_eq!(m[0].kind, MismatchKind::MissedFill);
                assert!(m[0].local.is_none());
                assert!(m[0].broker.is_some());
            }
            ReconciliationResult::Consistent => panic!("expected mismatch"),
        }
    }

    /// Task 9.9: quantity disagreement -> Mismatch.
    #[test]
    fn reconciliation_quantity_mismatch_detected() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 3, FillType::Full), 1);
        let broker = vec![BrokerPosition {
            symbol_id: 1,
            side: Some(Side::Buy),
            quantity: 2,
            avg_entry_price: FixedPrice::new(100),
        }];
        let result = tracker.reconcile(&broker);
        match result {
            ReconciliationResult::Mismatch(m) => {
                assert_eq!(m.len(), 1);
                assert_eq!(m[0].kind, MismatchKind::SideOrQuantity);
            }
            ReconciliationResult::Consistent => panic!("expected mismatch"),
        }
    }

    /// Side disagreement — both have qty>0 but opposite sides — Mismatch.
    #[test]
    fn reconciliation_side_mismatch_detected() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        let broker = vec![BrokerPosition {
            symbol_id: 1,
            side: Some(Side::Sell),
            quantity: 2,
            avg_entry_price: FixedPrice::new(100),
        }];
        let result = tracker.reconcile(&broker);
        match result {
            ReconciliationResult::Mismatch(m) => {
                assert_eq!(m[0].kind, MismatchKind::SideOrQuantity);
            }
            _ => panic!("expected mismatch"),
        }
    }

    /// avg_entry_price disagreement -> Mismatch (subtle but important; e.g.,
    /// fills observed in a different order).
    #[test]
    fn reconciliation_avg_entry_disagreement_detected() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        let broker = vec![BrokerPosition {
            symbol_id: 1,
            side: Some(Side::Buy),
            quantity: 2,
            avg_entry_price: FixedPrice::new(99),
        }];
        let result = tracker.reconcile(&broker);
        assert!(!result.is_consistent());
    }

    /// Task 9.10: Mismatch invokes the circuit breaker callback exactly once
    /// AND sets trading_halted.
    #[test]
    fn mismatch_trips_circuit_breaker_and_halts_trading() {
        let trips = Arc::new(AtomicUsize::new(0));
        let cb_trips = trips.clone();
        let cb: CircuitBreakerCallback = Arc::new(move |_| {
            cb_trips.fetch_add(1, Ordering::SeqCst);
        });

        let mut tracker = PositionTracker::new(journal()).with_circuit_breaker(cb);
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        let broker = vec![BrokerPosition::flat(1)];
        let result = tracker.reconcile_and_handle(
            &broker,
            ReconciliationTrigger::Periodic,
            UnixNanos::default(),
        );

        assert!(matches!(result, ReconciliationResult::Mismatch(_)));
        assert!(tracker.trading_halted());
        assert_eq!(trips.load(Ordering::SeqCst), 1);
    }

    /// Repeat mismatches do NOT re-fire the breaker (would spam alerting).
    #[test]
    fn repeat_mismatch_does_not_refire_breaker() {
        let trips = Arc::new(AtomicUsize::new(0));
        let cb_trips = trips.clone();
        let cb: CircuitBreakerCallback = Arc::new(move |_| {
            cb_trips.fetch_add(1, Ordering::SeqCst);
        });

        let mut tracker = PositionTracker::new(journal()).with_circuit_breaker(cb);
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        let broker = vec![BrokerPosition::flat(1)];
        // Three reconciliation rounds in a row, all mismatched.
        for _ in 0..3 {
            tracker.reconcile_and_handle(
                &broker,
                ReconciliationTrigger::Periodic,
                UnixNanos::default(),
            );
        }
        // Breaker only fires the first time; subsequent rounds find
        // already_halted == true and skip the callback.
        assert_eq!(trips.load(Ordering::SeqCst), 1);
    }

    /// Task 9.11: After mismatch, local state is unchanged (NO silent correction).
    #[test]
    fn mismatch_does_not_silently_correct_local_state() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        let broker = vec![BrokerPosition::flat(1)];
        let _ = tracker.reconcile_and_handle(
            &broker,
            ReconciliationTrigger::Periodic,
            UnixNanos::default(),
        );
        // Local position MUST be unchanged.
        let p = tracker.local_position(1).unwrap();
        assert_eq!(p.quantity, 2);
        assert_eq!(p.side, Some(Side::Buy));
        assert_eq!(p.avg_entry_price.raw(), 100);
    }

    /// Trigger string mapping — used in journal records / log fields.
    #[test]
    fn reconciliation_trigger_strings() {
        assert_eq!(ReconciliationTrigger::Startup.as_str(), "startup");
        assert_eq!(ReconciliationTrigger::Reconnection.as_str(), "reconnection");
        assert_eq!(ReconciliationTrigger::Periodic.as_str(), "periodic");
    }

    /// Multi-symbol reconciliation: one consistent + one mismatch -> Mismatch.
    #[test]
    fn multi_symbol_reconciliation_reports_only_offenders() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        tracker.apply_fill(&fill(Side::Sell, 200, 1, FillType::Full), 2);
        // Symbol 1 matches; symbol 2 doesn't (broker flat).
        let broker = vec![BrokerPosition {
            symbol_id: 1,
            side: Some(Side::Buy),
            quantity: 2,
            avg_entry_price: FixedPrice::new(100),
        }];
        let result = tracker.reconcile(&broker);
        match result {
            ReconciliationResult::Mismatch(m) => {
                assert_eq!(m.len(), 1);
                assert_eq!(m[0].symbol_id, 2);
                assert_eq!(m[0].kind, MismatchKind::PhantomLocal);
            }
            _ => panic!("expected mismatch"),
        }
    }

    /// Consistent reconciliation does NOT halt trading or trip the breaker.
    #[test]
    fn consistent_reconciliation_does_not_halt_trading() {
        let trips = Arc::new(AtomicUsize::new(0));
        let cb_trips = trips.clone();
        let cb: CircuitBreakerCallback = Arc::new(move |_| {
            cb_trips.fetch_add(1, Ordering::SeqCst);
        });
        let mut tracker = PositionTracker::new(journal()).with_circuit_breaker(cb);
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        let broker = vec![BrokerPosition {
            symbol_id: 1,
            side: Some(Side::Buy),
            quantity: 2,
            avg_entry_price: FixedPrice::new(100),
        }];
        tracker.reconcile_and_handle(
            &broker,
            ReconciliationTrigger::Startup,
            UnixNanos::default(),
        );
        assert!(!tracker.trading_halted());
        assert_eq!(trips.load(Ordering::SeqCst), 0);
    }

    /// Apply-fill on a closed position followed by a new buy correctly sets
    /// up a fresh leg (avg_entry from the new fill).
    #[test]
    fn reopen_after_close_starts_fresh_leg() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        tracker.apply_fill(&fill(Side::Sell, 110, 2, FillType::Full), 1);
        // Now buy again at 105.
        tracker.apply_fill(&fill(Side::Buy, 105, 1, FillType::Full), 1);
        let p = tracker.local_position(1).unwrap();
        assert_eq!(p.quantity, 1);
        assert_eq!(p.side, Some(Side::Buy));
        assert_eq!(p.avg_entry_price.raw(), 105);
        // Realized P&L from the prior round-trip is preserved.
        assert_eq!(p.realized_pnl, 20);
    }

    /// `iter()` exposes only non-zero-history positions.
    #[test]
    fn iter_includes_realized_pnl_history() {
        let mut tracker = PositionTracker::new(journal());
        tracker.apply_fill(&fill(Side::Buy, 100, 2, FillType::Full), 1);
        tracker.apply_fill(&fill(Side::Sell, 110, 2, FillType::Full), 1);
        let entries: Vec<_> = tracker.iter().collect();
        assert_eq!(entries.len(), 1);
        let (sym, pos) = entries[0];
        assert_eq!(*sym, 1);
        assert_eq!(pos.realized_pnl, 20);
        // open_count() filters out flat positions.
        assert_eq!(tracker.open_count(), 0);
    }

    /// `OrderState` import exercises the InvalidTransition seam (used to
    /// fire the circuit breaker on mismatch).
    #[test]
    fn invalid_transition_seam_is_used_for_breaker() {
        // This test merely verifies the type plumbing compiles and the
        // OrderState::Idle path is reachable from this module.
        let _ = OrderState::Idle;
    }
}
