//! Order manager — engine-side tracking of submitted orders and fill processing.
//!
//! Story 4.2 introduces the fill consumer half: a non-blocking poll of the
//! `FillQueueConsumer` (the engine end of the broker -> engine SPSC queue) on each
//! event-loop tick. Each [`FillEvent`] is matched against a tracked
//! [`TrackedOrder`], the order state is transitioned (Confirmed -> PartialFill ->
//! Filled, or terminal Rejected), and the fill is forwarded to the SQLite journal
//! as [`EngineEvent::OrderStateChange`] with the originating `decision_id`
//! preserved for causality tracing (NFR17).
//!
//! The full order state machine — including the WAL-write-before-submit invariant
//! — lands in story 4.4. This module ships the minimum surface needed to satisfy
//! story 4.2's ACs:
//!   * track outstanding orders + their `decision_id`
//!   * apply fills / rejections, validating state transitions
//!   * forward each transition to the journal

use std::collections::HashMap;

use futures_bmad_broker::FillQueueConsumer;
use futures_bmad_core::{FillEvent, FillType, OrderEvent, OrderState, RejectReason};
use tracing::{debug, info, warn};

use crate::persistence::journal::{
    EngineEvent as JournalEvent, JournalSender, OrderStateChangeRecord,
};

/// Lightweight in-memory record of a submitted order.
///
/// Ownership of the full WAL-backed state machine lives in story 4.4; this struct
/// carries only the fields the fill consumer needs to validate transitions and
/// keep the cumulative fill arithmetic correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackedOrder {
    pub order_id: u64,
    pub decision_id: u64,
    pub symbol_id: u32,
    pub original_quantity: u32,
    pub remaining_quantity: u32,
    pub state: OrderState,
}

impl TrackedOrder {
    /// Construct a tracked order from a freshly-submitted `OrderEvent`. The order
    /// starts in `Submitted` (the routing layer has accepted it but the exchange
    /// has not yet ack'd).
    pub fn from_submission(event: &OrderEvent) -> Self {
        Self {
            order_id: event.order_id,
            decision_id: event.decision_id,
            symbol_id: event.symbol_id,
            original_quantity: event.quantity,
            remaining_quantity: event.quantity,
            state: OrderState::Submitted,
        }
    }

    /// Transition into `Confirmed` once the exchange ack'd the new order. (Hooked
    /// in by the routing layer in a later story; the fill consumer here defends
    /// against unconfirmed orders by upgrading on the first fill if needed.)
    pub fn mark_confirmed(&mut self) -> bool {
        if matches!(self.state, OrderState::Submitted) {
            self.state = OrderState::Confirmed;
            true
        } else {
            false
        }
    }
}

/// Outcome of applying a single [`FillEvent`] — surfaced for testability so the
/// caller can assert on the resulting state transition without rummaging in logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillOutcome {
    /// Order moved to a non-terminal partial-fill state.
    Partial {
        order_id: u64,
        decision_id: u64,
        remaining: u32,
    },
    /// Order fully filled (terminal).
    Filled { order_id: u64, decision_id: u64 },
    /// Order rejected (terminal).
    Rejected {
        order_id: u64,
        decision_id: u64,
        reason: RejectReason,
    },
    /// Fill received for an order id we never submitted (or already terminal). The
    /// engine logs and skips the journal write rather than asserting — broker
    /// reconciliation in story 4.5 owns the resolution path.
    Orphan { order_id: u64 },
    /// Fill rejected because the requested transition is illegal under the order
    /// state machine (e.g., terminal -> non-terminal). Logged as warn; no journal
    /// write so the original terminal state remains authoritative.
    InvalidTransition {
        order_id: u64,
        from: OrderState,
        to: OrderState,
    },
}

/// Engine-side order tracker + fill consumer.
///
/// Owned by the hot-path event loop. `process_pending_fills()` is invoked once per
/// tick; it drains the `FillQueueConsumer` (non-blocking) and posts each
/// transition to the journal via `JournalSender::send` (which itself is
/// non-blocking — see story 4.1).
pub struct OrderManager {
    orders: HashMap<u64, TrackedOrder>,
    journal: JournalSender,
}

impl OrderManager {
    pub fn new(journal: JournalSender) -> Self {
        Self {
            orders: HashMap::new(),
            journal,
        }
    }

    /// Register a submitted order so subsequent fills can be matched.
    pub fn track(&mut self, order: TrackedOrder) {
        self.orders.insert(order.order_id, order);
    }

    /// Look up a tracked order (read-only).
    pub fn get(&self, order_id: u64) -> Option<&TrackedOrder> {
        self.orders.get(&order_id)
    }

    /// Number of currently tracked (non-terminal) orders.
    pub fn tracked_count(&self) -> usize {
        self.orders.len()
    }

    /// Drain the FillQueue and apply each fill. Returns the number of fills
    /// processed in this pass (regardless of outcome) so the event loop can
    /// observe activity.
    pub fn process_pending_fills(&mut self, consumer: &mut FillQueueConsumer) -> usize {
        let mut count = 0usize;
        while let Some(fill) = consumer.try_pop() {
            count += 1;
            let _ = self.apply_fill(&fill);
        }
        count
    }

    /// Apply a single fill against the tracked order set. Public so unit tests
    /// can drive the state machine without going through the SPSC queue.
    pub fn apply_fill(&mut self, fill: &FillEvent) -> FillOutcome {
        // Look up + remove eagerly so we can mutate without overlapping
        // borrows — re-insert below if the transition keeps the order live.
        let mut tracked = match self.orders.remove(&fill.order_id) {
            Some(t) => t,
            None => {
                warn!(
                    target: "order_manager",
                    order_id = fill.order_id,
                    decision_id = fill.decision_id,
                    "fill received for unknown order — broker reconciliation will resolve"
                );
                return FillOutcome::Orphan {
                    order_id: fill.order_id,
                };
            }
        };

        // Auto-upgrade Submitted -> Confirmed on first non-rejection fill: a
        // partial/full fill implies the exchange ack'd the order even if the
        // explicit OrderConfirm message was lost or hasn't arrived yet.
        // Rejections from `Submitted` are themselves valid (Submitted -> Rejected
        // is a direct transition), so we only auto-upgrade when the incoming
        // fill is a fill — not a reject.
        if matches!(tracked.state, OrderState::Submitted)
            && !matches!(fill.fill_type, FillType::Rejected { .. })
        {
            tracked.state = OrderState::Confirmed;
        }

        let from_state = tracked.state;
        let (to_state, outcome) = match fill.fill_type {
            FillType::Full => (
                OrderState::Filled,
                FillOutcome::Filled {
                    order_id: tracked.order_id,
                    decision_id: tracked.decision_id,
                },
            ),
            FillType::Partial { remaining } => (
                OrderState::PartialFill,
                FillOutcome::Partial {
                    order_id: tracked.order_id,
                    decision_id: tracked.decision_id,
                    remaining,
                },
            ),
            FillType::Rejected { reason } => {
                warn!(
                    target: "order_manager",
                    order_id = fill.order_id,
                    decision_id = fill.decision_id,
                    ?reason,
                    "order rejected"
                );
                (
                    OrderState::Rejected,
                    FillOutcome::Rejected {
                        order_id: tracked.order_id,
                        decision_id: tracked.decision_id,
                        reason,
                    },
                )
            }
        };

        // Validate the transition. If the FillEvent would force an illegal
        // transition (e.g., we already saw a Filled and now get another Full),
        // log + skip without journal-writing — leave the original tracked state
        // untouched so the audit trail's terminal record is the authoritative
        // one.
        if !from_state.can_transition_to(to_state) {
            warn!(
                target: "order_manager",
                order_id = fill.order_id,
                decision_id = fill.decision_id,
                ?from_state,
                ?to_state,
                "invalid fill state transition — fill ignored"
            );
            // Re-insert the original tracked state so subsequent fills (if any)
            // can still be evaluated.
            self.orders.insert(tracked.order_id, tracked);
            return FillOutcome::InvalidTransition {
                order_id: fill.order_id,
                from: from_state,
                to: to_state,
            };
        }

        // Apply state + cumulative-fill bookkeeping.
        tracked.state = to_state;
        match fill.fill_type {
            FillType::Partial { remaining } => {
                tracked.remaining_quantity = remaining;
            }
            FillType::Full => {
                tracked.remaining_quantity = 0;
            }
            FillType::Rejected { .. } => { /* qty unchanged */ }
        }

        // Journal the transition — best-effort; the journal channel is bounded
        // and may drop under extreme backpressure (logged inside JournalSender).
        let record = OrderStateChangeRecord {
            timestamp: fill.timestamp,
            order_id: fill.order_id,
            decision_id: Some(fill.decision_id),
            from_state: format!("{from_state:?}"),
            to_state: format!("{to_state:?}"),
        };
        let sent = self.journal.send(JournalEvent::OrderStateChange(record));
        if !sent {
            debug!(
                target: "order_manager",
                order_id = fill.order_id,
                "journal send dropped state change (backpressure)"
            );
        }

        // Terminal? Drop the tracking entry; non-terminal partials stay live.
        if !tracked.state.is_terminal() {
            self.orders.insert(tracked.order_id, tracked);
        } else {
            info!(
                target: "order_manager",
                order_id = fill.order_id,
                decision_id = fill.decision_id,
                final_state = ?to_state,
                "order reached terminal state"
            );
        }

        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::journal::EventJournal;
    use futures_bmad_broker::create_order_fill_queues;
    use futures_bmad_core::{FixedPrice, OrderType, Side, UnixNanos};

    fn make_tracked(order_id: u64, decision_id: u64, qty: u32) -> TrackedOrder {
        let event = OrderEvent {
            order_id,
            symbol_id: 1,
            side: Side::Buy,
            quantity: qty,
            order_type: OrderType::Market,
            decision_id,
            timestamp: UnixNanos::new(1),
        };
        let mut t = TrackedOrder::from_submission(&event);
        t.state = OrderState::Confirmed; // simulate exchange-ack already received
        t
    }

    fn make_fill(order_id: u64, decision_id: u64, fill_type: FillType, size: u32) -> FillEvent {
        FillEvent {
            order_id,
            fill_price: FixedPrice::new(17_929),
            fill_size: size,
            timestamp: UnixNanos::new(2),
            side: Side::Buy,
            decision_id,
            fill_type,
        }
    }

    fn journal() -> JournalSender {
        let (tx, _rx) = EventJournal::channel();
        tx
    }

    /// Task 6.4 — FillType::Full transitions state to Filled.
    #[test]
    fn full_fill_transitions_to_filled() {
        let mut mgr = OrderManager::new(journal());
        mgr.track(make_tracked(1, 11, 3));
        let outcome = mgr.apply_fill(&make_fill(1, 11, FillType::Full, 3));
        assert!(matches!(
            outcome,
            FillOutcome::Filled {
                order_id: 1,
                decision_id: 11
            }
        ));
        // Terminal -> tracking entry dropped.
        assert!(mgr.get(1).is_none());
    }

    /// Task 6.5 — FillType::Partial transitions to PartialFill with correct remaining.
    #[test]
    fn partial_fill_records_remaining() {
        let mut mgr = OrderManager::new(journal());
        mgr.track(make_tracked(2, 22, 5));
        let outcome = mgr.apply_fill(&make_fill(2, 22, FillType::Partial { remaining: 3 }, 2));
        assert!(matches!(
            outcome,
            FillOutcome::Partial {
                order_id: 2,
                decision_id: 22,
                remaining: 3
            }
        ));
        let tracked = mgr.get(2).unwrap();
        assert_eq!(tracked.state, OrderState::PartialFill);
        assert_eq!(tracked.remaining_quantity, 3);

        // Subsequent partial then full to verify the sequence works through to terminal.
        mgr.apply_fill(&make_fill(2, 22, FillType::Partial { remaining: 1 }, 2));
        let tracked = mgr.get(2).unwrap();
        assert_eq!(tracked.state, OrderState::PartialFill);
        assert_eq!(tracked.remaining_quantity, 1);

        let final_outcome = mgr.apply_fill(&make_fill(2, 22, FillType::Full, 1));
        assert!(matches!(final_outcome, FillOutcome::Filled { .. }));
        assert!(mgr.get(2).is_none());
    }

    /// Task 6.6 — rejection processing transitions to Rejected, reason preserved.
    #[test]
    fn rejection_preserves_reason() {
        let mut mgr = OrderManager::new(journal());
        let mut tracked = make_tracked(3, 33, 1);
        // From Submitted -> Rejected is a valid transition; force the lifecycle
        // start state to exercise that arc explicitly.
        tracked.state = OrderState::Submitted;
        mgr.track(tracked);

        let outcome = mgr.apply_fill(&make_fill(
            3,
            33,
            FillType::Rejected {
                reason: RejectReason::InsufficientMargin,
            },
            0,
        ));
        assert!(matches!(
            outcome,
            FillOutcome::Rejected {
                order_id: 3,
                decision_id: 33,
                reason: RejectReason::InsufficientMargin
            }
        ));
        assert!(mgr.get(3).is_none());
    }

    /// Task 6.7 — decision_id propagates from OrderEvent through tracking, fill,
    /// and the journal record.
    #[test]
    fn decision_id_propagates_to_journal_record() {
        // Use a real journal channel + receiver so we can observe the dispatched event.
        let (sender, receiver) = EventJournal::channel();
        let mut mgr = OrderManager::new(sender);

        let event = OrderEvent {
            order_id: 7,
            symbol_id: 1,
            side: Side::Buy,
            quantity: 1,
            order_type: OrderType::Market,
            decision_id: 4242,
            timestamp: UnixNanos::new(100),
        };
        let mut tracked = TrackedOrder::from_submission(&event);
        tracked.state = OrderState::Confirmed;
        mgr.track(tracked);

        mgr.apply_fill(&make_fill(7, 4242, FillType::Full, 1));

        // Drain the channel and assert on the OrderStateChange's decision_id.
        let raw = receiver_drain(&receiver);
        assert_eq!(raw.len(), 1);
        match &raw[0] {
            JournalEvent::OrderStateChange(rec) => {
                assert_eq!(rec.order_id, 7);
                assert_eq!(rec.decision_id, Some(4242));
                assert_eq!(rec.from_state, "Confirmed");
                assert_eq!(rec.to_state, "Filled");
            }
            other => panic!("expected OrderStateChange, got {other:?}"),
        }
    }

    /// Fill for an unknown order is reported as Orphan and does not panic.
    #[test]
    fn orphan_fill_is_logged_not_panicked() {
        let mut mgr = OrderManager::new(journal());
        let outcome = mgr.apply_fill(&make_fill(999, 0, FillType::Full, 1));
        assert!(matches!(outcome, FillOutcome::Orphan { order_id: 999 }));
    }

    /// End-to-end: process_pending_fills drains the SPSC FillQueue and applies
    /// each event in order.
    #[test]
    fn process_pending_fills_drains_queue() {
        let (_op, _oc, mut fp, mut fc) = create_order_fill_queues();
        let mut mgr = OrderManager::new(journal());
        mgr.track(make_tracked(10, 100, 2));

        // Push partial then full.
        assert!(fp.try_push(make_fill(10, 100, FillType::Partial { remaining: 1 }, 1,)));
        assert!(fp.try_push(make_fill(10, 100, FillType::Full, 1)));

        let processed = mgr.process_pending_fills(&mut fc);
        assert_eq!(processed, 2);
        assert!(mgr.get(10).is_none(), "terminal state should drop tracking");
    }

    /// Helper — drain a receiver into a vec without blocking.
    fn receiver_drain(rx: &crate::persistence::journal::JournalReceiver) -> Vec<JournalEvent> {
        // JournalReceiver doesn't expose try_recv publicly, so we expose this only
        // inside the engine crate via a quick helper. Cycle is bounded by the
        // events the test pushed.
        let mut out = Vec::new();
        // Spin briefly — channel sends are synchronous, so the events are already
        // enqueued by the time apply_fill returns.
        for _ in 0..10 {
            match rx.try_recv_for_test() {
                Some(evt) => out.push(evt),
                None => break,
            }
        }
        out
    }
}
