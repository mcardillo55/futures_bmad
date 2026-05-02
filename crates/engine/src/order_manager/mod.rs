//! Order manager — engine-side coordinator for order submission, fills, and
//! the per-order state machine + WAL persistence.
//!
//! Story 4.2 introduced the fill consumer (drain `FillQueueConsumer`, journal
//! each transition). Story 4.3 layered the bracket-order submodule. Story 4.4
//! ties the per-order [`OrderStateMachine`] (state_machine.rs) and the
//! write-ahead log (wal.rs) into one coordinator.
//!
//! The submission ordering invariant from the architecture spec lands here:
//!
//! ```text
//!   1. WAL.write_before_submit()     ── INSERT INTO pending_orders, fsync
//!   2. state_machine.transition(Submit) — Idle -> Submitted, captures submitted_at
//!   3. order_queue.try_push()         ── push OrderEvent onto SPSC to broker
//! ```
//!
//! If the engine crashes between (1) and (2), startup recovery finds the row
//! in `pending_orders`, queries the broker for actual status, and reconciles.
//! If the broker queue is full at (3), we mark the WAL row as `Rejected` (the
//! order never reached the exchange) and surface the failure to the caller.
//!
//! Timeout watchdog (Task 4): every event-loop tick, [`OrderManager::tick`]
//! scans active state machines whose state is `Submitted` and whose
//! `submitted_at` is older than 5 seconds. Each is transitioned to `Uncertain`,
//! global submissions are paused, and the broker is queried for status. The
//! reconciliation answer arrives via [`OrderManager::resolve_uncertain`] which
//! transitions through `PendingRecon -> Resolved` and re-arms submissions when
//! no Uncertain orders remain.

pub mod bracket;
pub mod state_machine;
pub mod tracker;
pub mod wal;

pub use bracket::{
    BracketManager, BracketSubmissionError, FlattenContext, FlattenOutcome as BracketFlattenOutcome,
};
pub use state_machine::{
    CircuitBreakerCallback, InvalidTransition, OrderStateMachine, OrderTransition,
};
pub use tracker::{
    BrokerSnapshot, LocalSnapshot, MismatchKind, PositionMismatch, PositionTracker,
    ReconciliationResult, ReconciliationTrigger,
};
pub use wal::{OrderWal, PendingOrder, WalError};

use std::collections::HashMap;

use futures_bmad_broker::{FillQueueConsumer, OrderQueueProducer};
use futures_bmad_core::{
    FillEvent, FillType, OrderEvent, OrderState, RejectReason, TradeSource, UnixNanos,
};
use tracing::{debug, error, info, warn};

use crate::persistence::journal::{
    EngineEvent as JournalEvent, JournalSender, OrderStateChangeRecord, SystemEventRecord,
};

/// Default Submitted -> Uncertain timeout (architecture spec NFR / Order State
/// Machine: 5 seconds).
pub const DEFAULT_UNCERTAIN_TIMEOUT_NANOS: u64 = 5_000_000_000;

/// Maximum total time PendingRecon can stay un-resolved before tripping the
/// circuit breaker (architecture spec: "30s total before escalation"; Task 5.5).
pub const PENDING_RECON_ESCALATION_NANOS: u64 = 30_000_000_000;

/// Lightweight in-memory record of a submitted order.
///
/// The "lightweight" half of the per-order state — full state-machine
/// transition logic lives in [`OrderStateMachine`]. This struct retains only
/// the per-order routing context the fill consumer needs (cumulative fills,
/// symbol_id, original quantity).
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
    /// Construct a tracked order from a freshly-submitted `OrderEvent`. The
    /// state starts in `Submitted` (the WAL has been written and the routing
    /// layer has accepted it but the exchange has not yet ack'd).
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

    /// Transition into `Confirmed` once the exchange ack'd the new order.
    pub fn mark_confirmed(&mut self) -> bool {
        if matches!(self.state, OrderState::Submitted) {
            self.state = OrderState::Confirmed;
            true
        } else {
            false
        }
    }
}

/// Outcome of applying a single [`FillEvent`].
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
    /// Fill received for an order id we never submitted (or already terminal).
    Orphan { order_id: u64 },
    /// Fill rejected because the requested transition is illegal under the
    /// state machine (e.g., terminal -> non-terminal). Logged as warn; no
    /// journal write so the original terminal state remains authoritative.
    InvalidTransition {
        order_id: u64,
        from: OrderState,
        to: OrderState,
    },
    /// Fill rejected because `fill_size` is inconsistent with the tracked
    /// remaining quantity (over-fill or zero-size non-rejected fill).
    /// Carryover (4-2 S-1, S-2): malformed or replayed exchange messages
    /// must not corrupt position arithmetic downstream.
    InvalidFillSize {
        order_id: u64,
        fill_size: u32,
        remaining_quantity: u32,
    },
}

/// Outcome of a submission attempt — surfaced for testability.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    /// Submissions are paused because at least one order is in Uncertain /
    /// PendingRecon.
    #[error("submissions paused: order(s) in uncertain state")]
    SubmissionsPaused,
    /// WAL write failed before the order could be enqueued. The order is NOT
    /// on the queue, the WAL has no record (or has been rolled back). Caller
    /// must treat the order as never-submitted.
    #[error("wal write failed: {0}")]
    WalWriteFailed(String),
    /// State-machine transition refused (e.g., resubmitting the same order_id).
    #[error("state machine refused submit: {0}")]
    StateMachine(InvalidTransition),
    /// SPSC queue rejected the push (broker not draining). The WAL row is
    /// flipped to `Rejected` so it does not appear in `recover_pending`.
    #[error("order queue full — submission failed")]
    QueueFull,
    /// `submit_order` was called with an order_id we already track.
    #[error("duplicate order_id: {0}")]
    DuplicateOrderId(u64),
}

/// Broker-side answer to a `query_order_status` issued during reconciliation.
///
/// The broker layer (Story 4.5 will wire the live Rithmic query) returns one
/// of these for each Uncertain order. The order manager uses the answer to
/// decide whether to flatten (open position, no bracket), keep waiting
/// (bracket protects), or simply mark the order Rejected/Resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerOrderStatus {
    /// Broker reports the order filled at the exchange.
    Filled,
    /// Broker reports the order rejected (never reached an exchange match).
    Rejected,
    /// Broker still has no terminal answer — keep polling.
    StillPending,
    /// Broker has no record of this order_id — it never reached the exchange.
    Unknown,
}

/// Resolution outcome after `resolve_uncertain` has applied a broker answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// Broker reports filled, bracket protects the position — no further
    /// action needed beyond marking Resolved.
    FilledBracketProtects,
    /// Broker reports filled but no bracket exists — caller MUST engage
    /// FlattenRetry on the supplied (symbol_id, side, quantity).
    FilledFlattenRequired {
        symbol_id: u32,
        side: futures_bmad_core::Side,
        quantity: u32,
    },
    /// Broker reports rejected — the order never created a position. Marked
    /// Resolved (effectively Rejected); submissions resume if no other
    /// Uncertain orders remain.
    RejectedNoPosition,
    /// Broker still pending or unknown — caller should retry the query.
    KeepPolling,
    /// Order id not tracked or not in PendingRecon — caller logged and skipped.
    NotApplicable,
    /// 30s escalation reached — circuit breaker has been tripped via the
    /// per-order callback. Submissions remain paused until manual intervention.
    EscalatedToCircuitBreaker,
    /// Broker reports `Filled` and no bracket protects the position, but the
    /// order's original side cannot be determined (no WAL or row missing) so we
    /// cannot safely synthesize a flatten direction. Caller MUST escalate to
    /// the circuit breaker — silently dropping a flatten on a confirmed fill is
    /// a position-safety violation (NFR16).
    /// Carryover (4-4 S-4).
    FilledFlattenSideUnknown { order_id: u64, symbol_id: u32 },
}

/// Engine-side order tracker + fill consumer + state-machine + WAL coordinator.
pub struct OrderManager {
    orders: HashMap<u64, TrackedOrder>,
    state_machines: HashMap<u64, OrderStateMachine>,
    /// Per-order recon-start timestamp — used to escalate to circuit breaker
    /// after [`PENDING_RECON_ESCALATION_NANOS`] without a terminal answer.
    recon_started_at: HashMap<u64, UnixNanos>,
    /// Per-order bracket id (for the FilledBracketProtects vs FilledFlattenRequired
    /// branch in `resolve_uncertain`).
    bracket_ids: HashMap<u64, u64>,
    journal: JournalSender,
    wal: Option<OrderWal>,
    submissions_paused: bool,
    uncertain_timeout_nanos: u64,
    circuit_breaker: Option<CircuitBreakerCallback>,
}

impl OrderManager {
    /// Construct a journal-only manager (no WAL, no state machines beyond
    /// fill processing). This is the shape used by story 4.2's fill-consumer
    /// tests; full WAL-backed flow uses [`OrderManager::with_wal`].
    pub fn new(journal: JournalSender) -> Self {
        Self {
            orders: HashMap::new(),
            state_machines: HashMap::new(),
            recon_started_at: HashMap::new(),
            bracket_ids: HashMap::new(),
            journal,
            wal: None,
            submissions_paused: false,
            uncertain_timeout_nanos: DEFAULT_UNCERTAIN_TIMEOUT_NANOS,
            circuit_breaker: None,
        }
    }

    /// Construct a fully-wired manager with WAL persistence and a circuit
    /// breaker callback.
    pub fn with_wal(journal: JournalSender, wal: OrderWal) -> Self {
        Self {
            wal: Some(wal),
            ..Self::new(journal)
        }
    }

    /// Override the Submitted -> Uncertain timeout (test hook).
    pub fn with_uncertain_timeout(mut self, nanos: u64) -> Self {
        self.uncertain_timeout_nanos = nanos.max(1);
        self
    }

    /// Register the circuit-breaker callback. Wired into every per-order
    /// state machine constructed via `submit_order`. The callback fires on
    /// invalid transitions and on PendingRecon escalation.
    pub fn with_circuit_breaker(mut self, cb: CircuitBreakerCallback) -> Self {
        self.circuit_breaker = Some(cb);
        self
    }

    /// Register a submitted order so subsequent fills can be matched. Used
    /// by tests and by callers that bypass `submit_order` (e.g., bracket
    /// manager wires its own submissions through the queue).
    pub fn track(&mut self, order: TrackedOrder) {
        self.orders.insert(order.order_id, order);
    }

    pub fn get(&self, order_id: u64) -> Option<&TrackedOrder> {
        self.orders.get(&order_id)
    }

    pub fn state_machine(&self, order_id: u64) -> Option<&OrderStateMachine> {
        self.state_machines.get(&order_id)
    }

    pub fn tracked_count(&self) -> usize {
        self.orders.len()
    }

    pub fn submissions_paused(&self) -> bool {
        self.submissions_paused
    }

    /// Submit a new order: WAL-write then push to the SPSC order queue.
    ///
    /// On success the per-order `OrderStateMachine` is in `Submitted` and the
    /// `TrackedOrder` mirrors the same state. The caller is expected to have
    /// allocated `event.order_id` via the engine's monotonic counter.
    pub fn submit_order(
        &mut self,
        event: OrderEvent,
        producer: &mut OrderQueueProducer,
        bracket_id: Option<u64>,
    ) -> Result<(), SubmitError> {
        if self.submissions_paused {
            warn!(
                target: "order_manager",
                order_id = event.order_id,
                decision_id = event.decision_id,
                "submission refused — paused (uncertain order in flight)"
            );
            return Err(SubmitError::SubmissionsPaused);
        }
        if self.orders.contains_key(&event.order_id)
            || self.state_machines.contains_key(&event.order_id)
        {
            return Err(SubmitError::DuplicateOrderId(event.order_id));
        }

        // Step 1: WAL write before any externally-visible side effect.
        if let Some(wal) = self.wal.as_ref()
            && let Err(err) = wal.write_before_submit(&event, bracket_id)
        {
            error!(
                target: "order_manager",
                order_id = event.order_id,
                error = %err,
                "wal write_before_submit failed — refusing submission"
            );
            return Err(SubmitError::WalWriteFailed(err.to_string()));
        }

        // Step 2: state-machine transition Idle -> Submitted (records submitted_at).
        let mut sm = OrderStateMachine::new(event.order_id, event.decision_id);
        if let Some(cb) = self.circuit_breaker.as_ref() {
            sm = sm.with_circuit_breaker(cb.clone());
        }
        if let Err(err) = sm.transition(OrderTransition::Submit, event.timestamp) {
            // Roll back the WAL row so it doesn't appear in recover_pending —
            // this is a defensive path; the only way Submit can fail is if
            // the SM was constructed in a non-Idle state, which never happens
            // through this public API.
            if let Some(wal) = self.wal.as_ref() {
                let _ = wal.mark_resolved(event.order_id, OrderState::Rejected);
            }
            return Err(SubmitError::StateMachine(err));
        }

        // Step 3: push onto the broker queue. If the queue refuses, mark the
        // WAL row as Rejected (the order never reached the exchange).
        if !producer.try_push(event) {
            if let Some(wal) = self.wal.as_ref() {
                let _ = wal.mark_resolved(event.order_id, OrderState::Rejected);
            }
            error!(
                target: "order_manager",
                order_id = event.order_id,
                decision_id = event.decision_id,
                "order queue full — rolled back WAL row to Rejected"
            );
            return Err(SubmitError::QueueFull);
        }

        // All three steps succeeded — wire up the in-memory tracker.
        self.state_machines.insert(event.order_id, sm);
        self.orders
            .insert(event.order_id, TrackedOrder::from_submission(&event));
        if let Some(bid) = bracket_id {
            self.bracket_ids.insert(event.order_id, bid);
        }
        info!(
            target: "order_manager",
            order_id = event.order_id,
            decision_id = event.decision_id,
            symbol_id = event.symbol_id,
            quantity = event.quantity,
            ?bracket_id,
            "order submitted (WAL + state machine + queue)"
        );

        // Journal the Idle -> Submitted transition so the audit trail is
        // complete from order birth. `source` is overridden by the
        // JournalSender when paper / replay orchestrators have stamped one;
        // otherwise it stays at the default `Live` value.
        let record = OrderStateChangeRecord {
            timestamp: event.timestamp,
            order_id: event.order_id,
            decision_id: Some(event.decision_id),
            from_state: format!("{:?}", OrderState::Idle),
            to_state: format!("{:?}", OrderState::Submitted),
            source: TradeSource::default(),
        };
        let _ = self.journal.send(JournalEvent::OrderStateChange(record));
        Ok(())
    }

    /// Drain the FillQueue and apply each fill (story 4.2 path, preserved).
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
    ///
    /// This path predates the per-order state machine and now bridges into
    /// it: when a fill arrives for an order we have a state machine for, the
    /// SM is updated alongside the legacy `TrackedOrder.state` mirror. Both
    /// are kept in sync — the SM is the authority for transition validation,
    /// the `TrackedOrder` carries the cumulative-fill arithmetic.
    pub fn apply_fill(&mut self, fill: &FillEvent) -> FillOutcome {
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

        // Carryover (4-2 S-1, S-2): validate fill_size before any state mutation.
        match fill.fill_type {
            FillType::Full => {
                if fill.fill_size == 0 {
                    warn!(
                        target: "order_manager",
                        order_id = fill.order_id,
                        decision_id = fill.decision_id,
                        remaining = tracked.remaining_quantity,
                        "FillType::Full with fill_size == 0 — rejecting as protocol bug"
                    );
                    self.orders.insert(tracked.order_id, tracked);
                    return FillOutcome::InvalidFillSize {
                        order_id: fill.order_id,
                        fill_size: fill.fill_size,
                        remaining_quantity: tracked.remaining_quantity,
                    };
                }
                if fill.fill_size > tracked.remaining_quantity {
                    warn!(
                        target: "order_manager",
                        order_id = fill.order_id,
                        decision_id = fill.decision_id,
                        fill_size = fill.fill_size,
                        remaining = tracked.remaining_quantity,
                        "over-fill: fill_size > remaining_quantity"
                    );
                    self.orders.insert(tracked.order_id, tracked);
                    return FillOutcome::InvalidFillSize {
                        order_id: fill.order_id,
                        fill_size: fill.fill_size,
                        remaining_quantity: tracked.remaining_quantity,
                    };
                }
            }
            FillType::Partial { remaining } => {
                if fill.fill_size == 0 {
                    warn!(
                        target: "order_manager",
                        order_id = fill.order_id,
                        decision_id = fill.decision_id,
                        "FillType::Partial with fill_size == 0 — rejecting"
                    );
                    self.orders.insert(tracked.order_id, tracked);
                    return FillOutcome::InvalidFillSize {
                        order_id: fill.order_id,
                        fill_size: fill.fill_size,
                        remaining_quantity: tracked.remaining_quantity,
                    };
                }
                if fill.fill_size > tracked.remaining_quantity
                    || remaining >= tracked.remaining_quantity
                {
                    warn!(
                        target: "order_manager",
                        order_id = fill.order_id,
                        decision_id = fill.decision_id,
                        fill_size = fill.fill_size,
                        remaining_in_event = remaining,
                        tracked_remaining = tracked.remaining_quantity,
                        "partial fill arithmetic inconsistent — rejecting"
                    );
                    self.orders.insert(tracked.order_id, tracked);
                    return FillOutcome::InvalidFillSize {
                        order_id: fill.order_id,
                        fill_size: fill.fill_size,
                        remaining_quantity: tracked.remaining_quantity,
                    };
                }
            }
            FillType::Rejected { .. } => { /* size invariants don't apply */ }
        }

        // Auto-upgrade Submitted -> Confirmed on first non-rejection fill.
        // Carryover (4-4 S-3 / 4-2 N-4): journal the implicit transition so the
        // audit trail does not have Confirmed appearing without a predecessor.
        if matches!(tracked.state, OrderState::Submitted)
            && !matches!(fill.fill_type, FillType::Rejected { .. })
        {
            tracked.state = OrderState::Confirmed;
            // Mirror onto the state machine if we have one.
            if let Some(sm) = self.state_machines.get_mut(&fill.order_id) {
                let _ = sm.transition(OrderTransition::Confirm, fill.timestamp);
            }
            let upgrade_record = OrderStateChangeRecord {
                timestamp: fill.timestamp,
                order_id: fill.order_id,
                decision_id: Some(fill.decision_id),
                from_state: format!("{:?}", OrderState::Submitted),
                to_state: format!("{:?}", OrderState::Confirmed),
                source: TradeSource::default(),
            };
            let _ = self
                .journal
                .send(JournalEvent::OrderStateChange(upgrade_record));
        }

        let from_state = tracked.state;
        let (trigger, to_state, outcome) = match fill.fill_type {
            FillType::Full => (
                OrderTransition::Fill,
                OrderState::Filled,
                FillOutcome::Filled {
                    order_id: tracked.order_id,
                    decision_id: tracked.decision_id,
                },
            ),
            FillType::Partial { remaining: _ } => (
                OrderTransition::PartiallyFill,
                OrderState::PartialFill,
                FillOutcome::Partial {
                    order_id: tracked.order_id,
                    decision_id: tracked.decision_id,
                    remaining: match fill.fill_type {
                        FillType::Partial { remaining } => remaining,
                        _ => 0,
                    },
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
                    OrderTransition::Reject,
                    OrderState::Rejected,
                    FillOutcome::Rejected {
                        order_id: tracked.order_id,
                        decision_id: tracked.decision_id,
                        reason,
                    },
                )
            }
        };

        // State machine is the authority on validity. If we have one, use it.
        if let Some(sm) = self.state_machines.get_mut(&fill.order_id) {
            if let Err(err) = sm.transition(trigger, fill.timestamp) {
                warn!(
                    target: "order_manager",
                    order_id = fill.order_id,
                    decision_id = fill.decision_id,
                    from = ?err.from,
                    ?trigger,
                    "invalid fill state transition — fill ignored"
                );
                self.orders.insert(tracked.order_id, tracked);
                return FillOutcome::InvalidTransition {
                    order_id: fill.order_id,
                    from: from_state,
                    to: to_state,
                };
            }
        } else {
            // No state machine (legacy/test path) — fall back to the canonical
            // OrderState arc check.
            if !from_state.can_transition_to(to_state) {
                warn!(
                    target: "order_manager",
                    order_id = fill.order_id,
                    decision_id = fill.decision_id,
                    ?from_state,
                    ?to_state,
                    "invalid fill state transition (no SM) — fill ignored"
                );
                self.orders.insert(tracked.order_id, tracked);
                return FillOutcome::InvalidTransition {
                    order_id: fill.order_id,
                    from: from_state,
                    to: to_state,
                };
            }
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

        // Journal the transition.
        let record = OrderStateChangeRecord {
            timestamp: fill.timestamp,
            order_id: fill.order_id,
            decision_id: Some(fill.decision_id),
            from_state: format!("{from_state:?}"),
            to_state: format!("{to_state:?}"),
            source: TradeSource::default(),
        };
        let sent = self.journal.send(JournalEvent::OrderStateChange(record));
        if !sent {
            debug!(
                target: "order_manager",
                order_id = fill.order_id,
                "journal send dropped state change (backpressure)"
            );
        }

        // WAL bookkeeping for terminal outcomes.
        // Carryover (4-4 S-1): if mark_resolved fails the in-memory state still
        // advances but the WAL row stays at the previous non-terminal state.
        // Promote the failure from warn-only to a journaled SystemEvent so a
        // forensic operator can see the durability lapse without grepping logs.
        // (Recovery still works — `recover_pending` will re-find the row and
        // the broker query reconciles — but the gap is now explicit.)
        if to_state.is_terminal()
            && let Some(wal) = self.wal.as_ref()
            && let Err(err) = wal.mark_resolved(fill.order_id, to_state)
        {
            warn!(
                target: "order_manager",
                order_id = fill.order_id,
                error = %err,
                "wal mark_resolved failed for terminal fill"
            );
            let durability_record = SystemEventRecord {
                timestamp: fill.timestamp,
                category: "order_wal".to_string(),
                message: format!(
                    "wal mark_resolved failed for order {} (final={:?}): {} — recovery will reconcile via broker query",
                    fill.order_id, to_state, err
                ),
            };
            let _ = self
                .journal
                .send(JournalEvent::SystemEvent(durability_record));
        }

        // Terminal? Drop tracking; non-terminal partials stay live.
        if !tracked.state.is_terminal() {
            self.orders.insert(tracked.order_id, tracked);
        } else {
            self.state_machines.remove(&fill.order_id);
            self.bracket_ids.remove(&fill.order_id);
            self.recon_started_at.remove(&fill.order_id);
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

    /// Per-tick housekeeping: scan Submitted orders, transition to Uncertain
    /// on timeout, escalate stuck PendingRecon orders.
    pub fn tick(&mut self, now: UnixNanos) {
        self.check_timeouts(now);
        self.check_pending_recon_escalation(now);
    }

    /// Find every `Submitted` order whose `submitted_at` is older than the
    /// configured timeout and transition each to `Uncertain`. Pauses
    /// submissions for the duration that any Uncertain order remains unresolved.
    pub fn check_timeouts(&mut self, now: UnixNanos) {
        // Collect order_ids that need transition — we cannot mutate the map
        // while iterating.
        let mut to_uncertain: Vec<u64> = Vec::new();
        for (oid, sm) in self.state_machines.iter() {
            if matches!(sm.state(), OrderState::Submitted)
                && let Some(submitted_at) = sm.submitted_at()
            {
                let elapsed = now.as_nanos().saturating_sub(submitted_at.as_nanos());
                if elapsed > self.uncertain_timeout_nanos {
                    to_uncertain.push(*oid);
                }
            }
        }

        for oid in to_uncertain {
            if let Some(sm) = self.state_machines.get_mut(&oid) {
                let elapsed = sm
                    .submitted_at()
                    .map(|t| now.as_nanos().saturating_sub(t.as_nanos()))
                    .unwrap_or(0);
                warn!(
                    target: "order_manager",
                    order_id = oid,
                    decision_id = sm.decision_id(),
                    elapsed_nanos = elapsed,
                    "order in Submitted past timeout — transitioning to Uncertain"
                );
                if sm.transition(OrderTransition::Timeout, now).is_ok() {
                    self.submissions_paused = true;
                    if let Some(tracked) = self.orders.get_mut(&oid) {
                        tracked.state = OrderState::Uncertain;
                    }
                    if let Some(wal) = self.wal.as_ref() {
                        let _ = wal.update_state(oid, OrderState::Uncertain);
                    }
                    let record = OrderStateChangeRecord {
                        timestamp: now,
                        order_id: oid,
                        decision_id: Some(sm.decision_id()),
                        from_state: format!("{:?}", OrderState::Submitted),
                        to_state: format!("{:?}", OrderState::Uncertain),
                        source: TradeSource::default(),
                    };
                    let _ = self.journal.send(JournalEvent::OrderStateChange(record));
                }
            }
        }
    }

    /// Mark an Uncertain order as having dispatched the broker query —
    /// transitions Uncertain -> PendingRecon and starts the escalation timer.
    /// Caller (event loop) is responsible for actually issuing the query.
    pub fn begin_recon(&mut self, order_id: u64, now: UnixNanos) -> Result<(), InvalidTransition> {
        let sm = self
            .state_machines
            .get_mut(&order_id)
            .ok_or(InvalidTransition {
                from: OrderState::Idle,
                trigger: OrderTransition::BeginRecon,
            })?;
        sm.transition(OrderTransition::BeginRecon, now)?;
        self.recon_started_at.insert(order_id, now);
        if let Some(tracked) = self.orders.get_mut(&order_id) {
            tracked.state = OrderState::PendingRecon;
        }
        if let Some(wal) = self.wal.as_ref() {
            let _ = wal.update_state(order_id, OrderState::PendingRecon);
        }
        let record = OrderStateChangeRecord {
            timestamp: now,
            order_id,
            decision_id: Some(sm.decision_id()),
            from_state: format!("{:?}", OrderState::Uncertain),
            to_state: format!("{:?}", OrderState::PendingRecon),
            source: TradeSource::default(),
        };
        let _ = self.journal.send(JournalEvent::OrderStateChange(record));
        Ok(())
    }

    /// Apply the broker's answer to an Uncertain reconciliation query.
    pub fn resolve_uncertain(
        &mut self,
        order_id: u64,
        broker_status: BrokerOrderStatus,
        now: UnixNanos,
    ) -> ResolveOutcome {
        let Some(sm) = self.state_machines.get(&order_id) else {
            warn!(
                target: "order_manager",
                order_id,
                "resolve_uncertain called for unknown order"
            );
            return ResolveOutcome::NotApplicable;
        };
        if !matches!(sm.state(), OrderState::PendingRecon) {
            warn!(
                target: "order_manager",
                order_id,
                state = ?sm.state(),
                "resolve_uncertain called on non-PendingRecon order"
            );
            return ResolveOutcome::NotApplicable;
        }

        match broker_status {
            BrokerOrderStatus::Filled => {
                let bracket_id = self.bracket_ids.get(&order_id).copied();
                let tracked = self.orders.get(&order_id).copied();
                self.transition_to_resolved(order_id, now);
                if bracket_id.is_some() {
                    info!(
                        target: "order_manager",
                        order_id,
                        ?bracket_id,
                        "uncertain order filled with bracket — resolved, bracket protects position"
                    );
                    ResolveOutcome::FilledBracketProtects
                } else if let Some(t) = tracked {
                    error!(
                        target: "order_manager",
                        order_id,
                        decision_id = t.decision_id,
                        "uncertain order filled WITHOUT bracket — flatten required"
                    );
                    // Flatten the position side opposite to the original
                    // order's side. The TrackedOrder doesn't carry side
                    // directly; the WAL row does.
                    // Carryover (4-4 S-4): if we cannot determine the side
                    // (no WAL or row missing), we MUST NOT silently drop the
                    // flatten — escalate to the circuit breaker. Silently
                    // collapsing into NotApplicable was a position-safety
                    // violation per NFR16.
                    if let Some(flatten_side) = self.flatten_side_for(order_id) {
                        ResolveOutcome::FilledFlattenRequired {
                            symbol_id: t.symbol_id,
                            side: flatten_side,
                            quantity: t.original_quantity,
                        }
                    } else {
                        error!(
                            target: "order_manager",
                            order_id,
                            decision_id = t.decision_id,
                            symbol_id = t.symbol_id,
                            "flatten side cannot be determined (no WAL or row missing) — escalating to circuit breaker"
                        );
                        if let Some(cb) = self.circuit_breaker.as_ref() {
                            let invalid = InvalidTransition {
                                from: OrderState::PendingRecon,
                                trigger: OrderTransition::Resolve,
                            };
                            cb(&invalid);
                        }
                        let escalation = SystemEventRecord {
                            timestamp: now,
                            category: "order_manager".to_string(),
                            message: format!(
                                "flatten side unknown for order {} (symbol={}) on resolved Filled — escalated to circuit breaker",
                                order_id, t.symbol_id
                            ),
                        };
                        let _ = self.journal.send(JournalEvent::SystemEvent(escalation));
                        ResolveOutcome::FilledFlattenSideUnknown {
                            order_id,
                            symbol_id: t.symbol_id,
                        }
                    }
                } else {
                    ResolveOutcome::NotApplicable
                }
            }
            BrokerOrderStatus::Rejected | BrokerOrderStatus::Unknown => {
                self.transition_to_resolved(order_id, now);
                info!(
                    target: "order_manager",
                    order_id,
                    "uncertain order rejected or unknown to broker — resolved"
                );
                ResolveOutcome::RejectedNoPosition
            }
            BrokerOrderStatus::StillPending => {
                debug!(
                    target: "order_manager",
                    order_id,
                    "broker still has no terminal answer — keep polling"
                );
                ResolveOutcome::KeepPolling
            }
        }
    }

    /// Compute the side that would flatten the position created by `order_id`.
    /// Returns None if we never tracked the order. Filling a Buy creates a
    /// long, so the flatten side is Sell (and vice versa).
    fn flatten_side_for(&self, order_id: u64) -> Option<futures_bmad_core::Side> {
        // We need the original submission side. The TrackedOrder doesn't carry
        // it explicitly, but the WAL row does. Defer to the WAL.
        if let Some(wal) = self.wal.as_ref()
            && let Ok(Some(p)) = wal.get(order_id)
        {
            return Some(match p.side {
                futures_bmad_core::Side::Buy => futures_bmad_core::Side::Sell,
                futures_bmad_core::Side::Sell => futures_bmad_core::Side::Buy,
            });
        }
        None
    }

    fn transition_to_resolved(&mut self, order_id: u64, now: UnixNanos) {
        if let Some(sm) = self.state_machines.get_mut(&order_id)
            && sm.transition(OrderTransition::Resolve, now).is_ok()
        {
            if let Some(tracked) = self.orders.get_mut(&order_id) {
                tracked.state = OrderState::Resolved;
            }
            if let Some(wal) = self.wal.as_ref() {
                let _ = wal.mark_resolved(order_id, OrderState::Resolved);
            }
            let record = OrderStateChangeRecord {
                timestamp: now,
                order_id,
                decision_id: Some(sm.decision_id()),
                from_state: format!("{:?}", OrderState::PendingRecon),
                to_state: format!("{:?}", OrderState::Resolved),
                source: TradeSource::default(),
            };
            let _ = self.journal.send(JournalEvent::OrderStateChange(record));
        }
        // Drop tracking and re-arm submissions if no other Uncertain/PendingRecon
        // orders remain.
        self.recon_started_at.remove(&order_id);
        self.bracket_ids.remove(&order_id);
        self.state_machines.remove(&order_id);
        self.orders.remove(&order_id);
        self.maybe_resume_submissions();
    }

    fn maybe_resume_submissions(&mut self) {
        let any_uncertain = self
            .state_machines
            .values()
            .any(|sm| matches!(sm.state(), OrderState::Uncertain | OrderState::PendingRecon));
        if !any_uncertain && self.submissions_paused {
            self.submissions_paused = false;
            info!(
                target: "order_manager",
                "all uncertain orders resolved — submissions resumed"
            );
        }
    }

    fn check_pending_recon_escalation(&mut self, now: UnixNanos) {
        let mut to_escalate: Vec<u64> = Vec::new();
        for (oid, started) in self.recon_started_at.iter() {
            let elapsed = now.as_nanos().saturating_sub(started.as_nanos());
            if elapsed >= PENDING_RECON_ESCALATION_NANOS {
                to_escalate.push(*oid);
            }
        }
        for oid in to_escalate {
            error!(
                target: "order_manager",
                order_id = oid,
                "PendingRecon escalation: order has been in recon for >=30s — tripping circuit breaker"
            );
            // Surface to the breaker callback if registered.
            if let Some(cb) = self.circuit_breaker.as_ref() {
                let invalid = InvalidTransition {
                    from: OrderState::PendingRecon,
                    trigger: OrderTransition::Resolve,
                };
                cb(&invalid);
            }
            // Clear the timer so we don't re-escalate every tick. Submissions
            // remain paused — only manual recovery clears panic.
            self.recon_started_at.remove(&oid);
        }
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
        t.state = OrderState::Confirmed;
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

    fn make_event(order_id: u64, decision_id: u64, qty: u32) -> OrderEvent {
        OrderEvent {
            order_id,
            symbol_id: 1,
            side: Side::Buy,
            quantity: qty,
            order_type: OrderType::Market,
            decision_id,
            timestamp: UnixNanos::new(1_700_000_000_000_000_000),
        }
    }

    // -------- Story 4.2 carryover tests preserved --------

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
        assert!(mgr.get(1).is_none());
    }

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

        mgr.apply_fill(&make_fill(2, 22, FillType::Partial { remaining: 1 }, 2));
        let tracked = mgr.get(2).unwrap();
        assert_eq!(tracked.remaining_quantity, 1);

        let final_outcome = mgr.apply_fill(&make_fill(2, 22, FillType::Full, 1));
        assert!(matches!(final_outcome, FillOutcome::Filled { .. }));
        assert!(mgr.get(2).is_none());
    }

    #[test]
    fn rejection_preserves_reason() {
        let mut mgr = OrderManager::new(journal());
        let mut tracked = make_tracked(3, 33, 1);
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

    #[test]
    fn decision_id_propagates_to_journal_record() {
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

    #[test]
    fn orphan_fill_is_logged_not_panicked() {
        let mut mgr = OrderManager::new(journal());
        let outcome = mgr.apply_fill(&make_fill(999, 0, FillType::Full, 1));
        assert!(matches!(outcome, FillOutcome::Orphan { order_id: 999 }));
    }

    #[test]
    fn process_pending_fills_drains_queue() {
        let (_op, _oc, mut fp, mut fc) = create_order_fill_queues();
        let mut mgr = OrderManager::new(journal());
        mgr.track(make_tracked(10, 100, 2));

        assert!(fp.try_push(make_fill(10, 100, FillType::Partial { remaining: 1 }, 1,)));
        assert!(fp.try_push(make_fill(10, 100, FillType::Full, 1)));

        let processed = mgr.process_pending_fills(&mut fc);
        assert_eq!(processed, 2);
        assert!(mgr.get(10).is_none(), "terminal state should drop tracking");
    }

    fn receiver_drain(rx: &crate::persistence::journal::JournalReceiver) -> Vec<JournalEvent> {
        let mut out = Vec::new();
        for _ in 0..50 {
            match rx.try_recv_for_test() {
                Some(evt) => out.push(evt),
                None => break,
            }
        }
        out
    }

    // -------- Story 4.4 new tests --------

    /// Carryover (4-2 S-1): over-fill is rejected as InvalidFillSize.
    #[test]
    fn over_fill_is_rejected() {
        let mut mgr = OrderManager::new(journal());
        mgr.track(make_tracked(1, 1, 2));
        let outcome = mgr.apply_fill(&make_fill(1, 1, FillType::Full, 999));
        assert!(matches!(outcome, FillOutcome::InvalidFillSize { .. }));
        // Tracked order still alive (not corrupted).
        assert!(mgr.get(1).is_some());
        assert_eq!(mgr.get(1).unwrap().remaining_quantity, 2);
    }

    /// Carryover (4-2 S-2): zero-size non-rejected fill is rejected.
    #[test]
    fn zero_size_full_fill_is_rejected() {
        let mut mgr = OrderManager::new(journal());
        mgr.track(make_tracked(1, 1, 2));
        let outcome = mgr.apply_fill(&make_fill(1, 1, FillType::Full, 0));
        assert!(matches!(outcome, FillOutcome::InvalidFillSize { .. }));
        assert_eq!(mgr.get(1).unwrap().state, OrderState::Confirmed);
    }

    /// Carryover (4-2 S-3): PartialFill -> Rejected works through the SM.
    #[test]
    fn partial_then_rejected_terminates_order() {
        let mut mgr = OrderManager::new(journal());
        // Build via the SM path so the SM is wired.
        let (mut op, mut oc, _fp, _fc) = create_order_fill_queues();
        let evt = make_event(1, 11, 3);
        mgr.submit_order(evt, &mut op, None).unwrap();
        // Drain the routing queue.
        let _ = oc.try_pop();

        // Partial fill — tracked state PartialFill.
        let p = mgr.apply_fill(&make_fill(1, 11, FillType::Partial { remaining: 2 }, 1));
        assert!(matches!(p, FillOutcome::Partial { .. }));
        assert_eq!(mgr.get(1).unwrap().state, OrderState::PartialFill);

        // Now reject the remainder — should terminate.
        let r = mgr.apply_fill(&make_fill(
            1,
            11,
            FillType::Rejected {
                reason: RejectReason::ExchangeReject,
            },
            0,
        ));
        assert!(matches!(r, FillOutcome::Rejected { .. }));
        assert!(mgr.get(1).is_none());
        assert!(mgr.state_machine(1).is_none());
    }

    /// Task 7.4 — WAL write-before-submit: order written, "crash", recover.
    #[test]
    fn wal_write_before_submit_recovers_pending_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.db");

        {
            let wal = OrderWal::open(&path).unwrap();
            let mut mgr = OrderManager::with_wal(journal(), wal);
            let (mut op, mut _oc, _fp, _fc) = create_order_fill_queues();
            mgr.submit_order(make_event(42, 4242, 3), &mut op, Some(99))
                .unwrap();
            // Drop without closing — process crash.
        }

        let wal = OrderWal::open(&path).unwrap();
        let pending = wal.recover_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].order_id, 42);
        assert_eq!(pending[0].decision_id, 4242);
        assert_eq!(pending[0].state, OrderState::Submitted);
        assert_eq!(pending[0].bracket_id, Some(99));
    }

    /// Task 7.6 — 5s timeout: order submitted, time advances 5s+, transitions
    /// to Uncertain on tick.
    #[test]
    fn submitted_order_transitions_to_uncertain_on_timeout() {
        let mut mgr = OrderManager::with_wal(journal(), OrderWal::open_in_memory().unwrap());
        let (mut op, mut _oc, _fp, _fc) = create_order_fill_queues();
        let mut evt = make_event(1, 11, 1);
        evt.timestamp = UnixNanos::new(0);
        mgr.submit_order(evt, &mut op, None).unwrap();
        assert_eq!(mgr.get(1).unwrap().state, OrderState::Submitted);
        assert!(!mgr.submissions_paused());

        // Advance 5.1 seconds; tick should flip the order to Uncertain and
        // pause submissions.
        let now = UnixNanos::new(5_100_000_000);
        mgr.tick(now);
        assert_eq!(mgr.state_machine(1).unwrap().state(), OrderState::Uncertain);
        assert!(mgr.submissions_paused());
    }

    /// Task 7.7 — submissions paused during Uncertain — new submit_order rejected.
    #[test]
    fn submissions_paused_rejects_new_submit_order() {
        let mut mgr = OrderManager::with_wal(journal(), OrderWal::open_in_memory().unwrap());
        let (mut op, mut _oc, _fp, _fc) = create_order_fill_queues();
        let mut evt = make_event(1, 11, 1);
        evt.timestamp = UnixNanos::new(0);
        mgr.submit_order(evt, &mut op, None).unwrap();
        mgr.tick(UnixNanos::new(6_000_000_000));
        assert!(mgr.submissions_paused());

        // Second submission must fail.
        let evt2 = make_event(2, 22, 1);
        let result = mgr.submit_order(evt2, &mut op, None);
        assert!(matches!(result, Err(SubmitError::SubmissionsPaused)));
    }

    /// Task 7.8 — resolution: filled with bracket -> no flatten.
    #[test]
    fn resolve_filled_with_bracket_does_not_request_flatten() {
        let mut mgr = OrderManager::with_wal(journal(), OrderWal::open_in_memory().unwrap());
        let (mut op, mut _oc, _fp, _fc) = create_order_fill_queues();
        let mut evt = make_event(1, 11, 1);
        evt.timestamp = UnixNanos::new(0);
        mgr.submit_order(evt, &mut op, Some(7)).unwrap();
        mgr.tick(UnixNanos::new(6_000_000_000));
        mgr.begin_recon(1, UnixNanos::new(6_500_000_000)).unwrap();

        let outcome =
            mgr.resolve_uncertain(1, BrokerOrderStatus::Filled, UnixNanos::new(7_000_000_000));
        assert!(matches!(outcome, ResolveOutcome::FilledBracketProtects));
        // State machine cleared, submissions resumed.
        assert!(mgr.state_machine(1).is_none());
        assert!(!mgr.submissions_paused());
    }

    /// Task 7.8 — resolution: filled without bracket -> flatten triggered.
    #[test]
    fn resolve_filled_without_bracket_requests_flatten() {
        let mut mgr = OrderManager::with_wal(journal(), OrderWal::open_in_memory().unwrap());
        let (mut op, mut _oc, _fp, _fc) = create_order_fill_queues();
        let mut evt = make_event(1, 11, 3);
        evt.timestamp = UnixNanos::new(0);
        mgr.submit_order(evt, &mut op, None).unwrap();
        mgr.tick(UnixNanos::new(6_000_000_000));
        mgr.begin_recon(1, UnixNanos::new(6_500_000_000)).unwrap();

        let outcome =
            mgr.resolve_uncertain(1, BrokerOrderStatus::Filled, UnixNanos::new(7_000_000_000));
        match outcome {
            ResolveOutcome::FilledFlattenRequired {
                symbol_id,
                side,
                quantity,
            } => {
                assert_eq!(symbol_id, 1);
                assert_eq!(side, Side::Sell); // flatten side opposite of Buy entry
                assert_eq!(quantity, 3);
            }
            other => panic!("expected FilledFlattenRequired, got {other:?}"),
        }
        assert!(!mgr.submissions_paused());
    }

    /// Task 7.9 — resolution: broker reports rejected -> Resolved, submissions resume.
    #[test]
    fn resolve_rejected_resumes_submissions() {
        let mut mgr = OrderManager::with_wal(journal(), OrderWal::open_in_memory().unwrap());
        let (mut op, mut _oc, _fp, _fc) = create_order_fill_queues();
        let mut evt = make_event(1, 11, 1);
        evt.timestamp = UnixNanos::new(0);
        mgr.submit_order(evt, &mut op, None).unwrap();
        mgr.tick(UnixNanos::new(6_000_000_000));
        mgr.begin_recon(1, UnixNanos::new(6_500_000_000)).unwrap();
        assert!(mgr.submissions_paused());

        let outcome = mgr.resolve_uncertain(
            1,
            BrokerOrderStatus::Rejected,
            UnixNanos::new(7_000_000_000),
        );
        assert!(matches!(outcome, ResolveOutcome::RejectedNoPosition));
        assert!(!mgr.submissions_paused());
        assert!(mgr.state_machine(1).is_none());
    }

    /// PendingRecon escalation after 30s trips the circuit breaker.
    #[test]
    fn pending_recon_escalates_to_circuit_breaker_after_30s() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let trips = Arc::new(AtomicUsize::new(0));
        let cb_trips = trips.clone();
        let cb: CircuitBreakerCallback = Arc::new(move |_| {
            cb_trips.fetch_add(1, Ordering::SeqCst);
        });

        let mut mgr = OrderManager::with_wal(journal(), OrderWal::open_in_memory().unwrap())
            .with_circuit_breaker(cb);
        let (mut op, mut _oc, _fp, _fc) = create_order_fill_queues();
        let mut evt = make_event(1, 11, 1);
        evt.timestamp = UnixNanos::new(0);
        mgr.submit_order(evt, &mut op, None).unwrap();
        mgr.tick(UnixNanos::new(6_000_000_000));
        mgr.begin_recon(1, UnixNanos::new(6_500_000_000)).unwrap();
        // Tick 30s+ later — escalation fires.
        mgr.tick(UnixNanos::new(36_500_000_001));
        assert!(trips.load(Ordering::SeqCst) >= 1);
    }

    /// Submitting a duplicate order_id is rejected.
    #[test]
    fn duplicate_order_id_is_rejected() {
        let mut mgr = OrderManager::new(journal());
        let (mut op, mut _oc, _fp, _fc) = create_order_fill_queues();
        let evt = make_event(1, 1, 1);
        mgr.submit_order(evt, &mut op, None).unwrap();
        let result = mgr.submit_order(evt, &mut op, None);
        assert!(matches!(result, Err(SubmitError::DuplicateOrderId(1))));
    }
}
