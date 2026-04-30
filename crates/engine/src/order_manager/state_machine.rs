//! Per-order state machine — story 4.4.
//!
//! Wraps the canonical `core::OrderState` with a trigger-driven, exhaustively
//! matched transition table. Every transition is validated by an explicit
//! `match` on `(state, trigger)` with **no wildcard arms**: adding a new state
//! or trigger is a compile error until every combination is handled. Invalid
//! transitions return `Err(InvalidTransition)` and trip a circuit breaker via
//! the optional callback registered on [`OrderStateMachine`].
//!
//! The trigger-keyed API (`Submit`, `Confirm`, `PartiallyFill`, `Fill`,
//! `Reject`, `Timeout`, `BeginRecon`, `Resolve`) is the contract with the
//! [`OrderManager`]; downstream callers never poke at the underlying
//! `OrderState` enum directly. This insulates the manager from later additions
//! to `core::OrderState` (e.g., `Cancelled`/`PendingCancel`) — those land via
//! their own triggers in subsequent stories without rewriting existing arms.
//!
//! The valid arcs match the story 4.4 spec (Task 1.4):
//!
//! ```text
//!   Idle ──Submit──> Submitted ──Confirm──> Confirmed ──Fill──> Filled
//!                       │                       │
//!                       ├──Reject──> Rejected   ├──PartiallyFill──> PartialFill ──Fill──> Filled
//!                       │                                              │ │
//!                       │                                              │ └─PartiallyFill─> PartialFill
//!                       │                                              │
//!                       │                                              └──Reject──> Rejected   ★
//!                       │
//!                       └──Timeout──> Uncertain ──BeginRecon──> PendingRecon ──Resolve──> Resolved
//! ```
//!
//! ★ The `PartialFill -> Rejected` arc is added to address the carryover review
//! finding (4-2 S-3): a real-world "broker accepts entry, fills 1-of-3, then
//! rejects the remainder" sequence would otherwise strand the order in
//! `PartialFill`. The `Reject` trigger from `PartialFill` flows into the same
//! terminal `Rejected` state.

use core::fmt;

use futures_bmad_core::{OrderState, UnixNanos};
use tracing::{debug, error};

/// Triggers driving the per-order state machine.
///
/// The trigger set is intentionally narrower than the underlying `OrderState`
/// graph: each trigger is named after the *event* (broker confirmed the order,
/// fill arrived, timeout elapsed, ...), not the destination state. This keeps
/// the call sites in [`OrderManager`] readable — `transition(Confirm)` is
/// clearer than `transition_to(OrderState::Confirmed)` and makes invalid combos
/// like `transition(Confirm)` from `Filled` express the actual error mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderTransition {
    /// Order is being routed to the broker (Idle -> Submitted).
    Submit,
    /// Broker confirmed receipt of the order (Submitted -> Confirmed).
    Confirm,
    /// Partial fill arrived (Confirmed/PartialFill -> PartialFill).
    PartiallyFill,
    /// Full fill arrived (Confirmed/PartialFill -> Filled).
    Fill,
    /// Order rejected by exchange (Submitted/PartialFill -> Rejected).
    Reject,
    /// Submission watchdog elapsed (Submitted -> Uncertain).
    Timeout,
    /// Reconciliation query dispatched (Uncertain -> PendingRecon).
    BeginRecon,
    /// Reconciliation answer applied (PendingRecon -> Resolved).
    Resolve,
}

/// Error returned when a `(state, trigger)` combination has no valid arc.
///
/// `from` carries the originating state so the caller can journal the offending
/// transition exactly. `trigger` is the rejected trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub struct InvalidTransition {
    pub from: OrderState,
    pub trigger: OrderTransition,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid order state transition: {:?} via {:?}",
            self.from, self.trigger
        )
    }
}

/// Compute the destination state for an `(OrderState, OrderTransition)` pair.
///
/// **No wildcard arms.** Every reachable `(state, trigger)` pair is listed
/// explicitly; combinations not listed return `Err(InvalidTransition)`. We use
/// a layered match-and-fall-through structure (outer match on state, each arm
/// has its own inner match on trigger) so the exhaustiveness checker enforces
/// per-state coverage.
pub fn try_transition(
    state: OrderState,
    trigger: OrderTransition,
) -> Result<OrderState, InvalidTransition> {
    let invalid = || {
        Err(InvalidTransition {
            from: state,
            trigger,
        })
    };
    match state {
        OrderState::Idle => match trigger {
            OrderTransition::Submit => Ok(OrderState::Submitted),
            OrderTransition::Confirm
            | OrderTransition::PartiallyFill
            | OrderTransition::Fill
            | OrderTransition::Reject
            | OrderTransition::Timeout
            | OrderTransition::BeginRecon
            | OrderTransition::Resolve => invalid(),
        },
        OrderState::Submitted => match trigger {
            OrderTransition::Confirm => Ok(OrderState::Confirmed),
            OrderTransition::Reject => Ok(OrderState::Rejected),
            OrderTransition::Timeout => Ok(OrderState::Uncertain),
            OrderTransition::Submit
            | OrderTransition::PartiallyFill
            | OrderTransition::Fill
            | OrderTransition::BeginRecon
            | OrderTransition::Resolve => invalid(),
        },
        OrderState::Confirmed => match trigger {
            OrderTransition::PartiallyFill => Ok(OrderState::PartialFill),
            OrderTransition::Fill => Ok(OrderState::Filled),
            OrderTransition::Submit
            | OrderTransition::Confirm
            | OrderTransition::Reject
            | OrderTransition::Timeout
            | OrderTransition::BeginRecon
            | OrderTransition::Resolve => invalid(),
        },
        OrderState::PartialFill => match trigger {
            OrderTransition::PartiallyFill => Ok(OrderState::PartialFill),
            OrderTransition::Fill => Ok(OrderState::Filled),
            // Carryover (4-2 S-3): the broker can fill 1-of-N and then reject
            // the remainder. Without this arc, the order would strand in
            // PartialFill indefinitely.
            OrderTransition::Reject => Ok(OrderState::Rejected),
            OrderTransition::Submit
            | OrderTransition::Confirm
            | OrderTransition::Timeout
            | OrderTransition::BeginRecon
            | OrderTransition::Resolve => invalid(),
        },
        OrderState::Uncertain => match trigger {
            OrderTransition::BeginRecon => Ok(OrderState::PendingRecon),
            OrderTransition::Submit
            | OrderTransition::Confirm
            | OrderTransition::PartiallyFill
            | OrderTransition::Fill
            | OrderTransition::Reject
            | OrderTransition::Timeout
            | OrderTransition::Resolve => invalid(),
        },
        OrderState::PendingRecon => match trigger {
            OrderTransition::Resolve => Ok(OrderState::Resolved),
            OrderTransition::Submit
            | OrderTransition::Confirm
            | OrderTransition::PartiallyFill
            | OrderTransition::Fill
            | OrderTransition::Reject
            | OrderTransition::Timeout
            | OrderTransition::BeginRecon => invalid(),
        },
        // Terminal states: nothing leaves them. Listing each trigger
        // explicitly preserves the no-wildcard discipline.
        OrderState::Filled
        | OrderState::Rejected
        | OrderState::Cancelled
        | OrderState::Resolved => match trigger {
            OrderTransition::Submit
            | OrderTransition::Confirm
            | OrderTransition::PartiallyFill
            | OrderTransition::Fill
            | OrderTransition::Reject
            | OrderTransition::Timeout
            | OrderTransition::BeginRecon
            | OrderTransition::Resolve => invalid(),
        },
        // PendingCancel is part of the canonical core::OrderState graph but is
        // not driven by any story 4.4 trigger. Any trigger arriving here is a
        // 4.4-side bug — surface it as an invalid transition rather than
        // silently ignoring (per Task 1.5: no silent drops).
        OrderState::PendingCancel => match trigger {
            OrderTransition::Submit
            | OrderTransition::Confirm
            | OrderTransition::PartiallyFill
            | OrderTransition::Fill
            | OrderTransition::Reject
            | OrderTransition::Timeout
            | OrderTransition::BeginRecon
            | OrderTransition::Resolve => invalid(),
        },
    }
}

/// Callback fired exactly once per order whenever an invalid transition is
/// attempted. The implementation is expected to flip a circuit breaker (per
/// NFR16) — the callback indirection keeps the state machine free of any hard
/// dependency on the engine's risk module.
///
/// The closure must be `Send + Sync` because the state machine is held inside
/// the (single-threaded) [`OrderManager`] but the breaker may be observed from
/// other threads.
pub type CircuitBreakerCallback = std::sync::Arc<dyn Fn(&InvalidTransition) + Send + Sync>;

/// Per-order state machine.
///
/// Carries the per-order metadata the [`OrderManager`] needs alongside the
/// state itself: the engine-allocated `order_id`, the originating `decision_id`
/// (for NFR17 causality tracing), and the `submitted_at` timestamp recorded
/// when the order transitioned into `Submitted` (used by the timeout
/// watchdog).
pub struct OrderStateMachine {
    state: OrderState,
    order_id: u64,
    decision_id: u64,
    submitted_at: Option<UnixNanos>,
    circuit_breaker: Option<CircuitBreakerCallback>,
}

impl fmt::Debug for OrderStateMachine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OrderStateMachine")
            .field("state", &self.state)
            .field("order_id", &self.order_id)
            .field("decision_id", &self.decision_id)
            .field("submitted_at", &self.submitted_at)
            .field(
                "circuit_breaker",
                &self.circuit_breaker.as_ref().map(|_| "<callback>"),
            )
            .finish()
    }
}

impl OrderStateMachine {
    /// Construct a fresh state machine in `Idle` for a newly-allocated
    /// `order_id` / `decision_id`.
    pub fn new(order_id: u64, decision_id: u64) -> Self {
        Self {
            state: OrderState::Idle,
            order_id,
            decision_id,
            submitted_at: None,
            circuit_breaker: None,
        }
    }

    /// Override the starting state. Useful for tests that want to drive a
    /// particular arc without first walking `Idle -> Submitted`.
    pub fn with_state(mut self, state: OrderState) -> Self {
        self.state = state;
        self
    }

    /// Register a circuit-breaker callback to be invoked on invalid
    /// transitions. `OrderManager` wires this to the engine's
    /// `BreakerTripper` (or a noop in tests).
    pub fn with_circuit_breaker(mut self, cb: CircuitBreakerCallback) -> Self {
        self.circuit_breaker = Some(cb);
        self
    }

    pub fn state(&self) -> OrderState {
        self.state
    }

    pub fn order_id(&self) -> u64 {
        self.order_id
    }

    pub fn decision_id(&self) -> u64 {
        self.decision_id
    }

    pub fn submitted_at(&self) -> Option<UnixNanos> {
        self.submitted_at
    }

    /// Whether the order is in a terminal state (Filled / Rejected / Resolved).
    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    /// Apply a trigger. On valid transition: update the state, capture the
    /// `submitted_at` timestamp on first entry into `Submitted`, log at
    /// `debug`, and return the new state. On invalid transition: log at
    /// `error`, fire the circuit-breaker callback, and return `Err`.
    ///
    /// `now` is supplied by the caller (rather than read from a clock here)
    /// so the manager can drive the machine deterministically from a
    /// `SimClock` in tests.
    pub fn transition(
        &mut self,
        trigger: OrderTransition,
        now: UnixNanos,
    ) -> Result<OrderState, InvalidTransition> {
        match try_transition(self.state, trigger) {
            Ok(new_state) => {
                let from = self.state;
                self.state = new_state;
                if matches!(new_state, OrderState::Submitted) {
                    self.submitted_at = Some(now);
                }
                debug!(
                    target: "order_state_machine",
                    order_id = self.order_id,
                    decision_id = self.decision_id,
                    from = ?from,
                    to = ?new_state,
                    ?trigger,
                    "order state transition"
                );
                Ok(new_state)
            }
            Err(err) => {
                error!(
                    target: "order_state_machine",
                    order_id = self.order_id,
                    decision_id = self.decision_id,
                    from = ?err.from,
                    ?trigger,
                    "invalid order state transition — tripping circuit breaker"
                );
                if let Some(cb) = self.circuit_breaker.as_ref() {
                    cb(&err);
                }
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn now() -> UnixNanos {
        UnixNanos::new(1_700_000_000_000_000_000)
    }

    /// Task 7.1 — every valid arc transitions to the expected destination.
    #[test]
    fn valid_transitions_match_spec_graph() {
        let cases: &[(OrderState, OrderTransition, OrderState)] = &[
            (
                OrderState::Idle,
                OrderTransition::Submit,
                OrderState::Submitted,
            ),
            (
                OrderState::Submitted,
                OrderTransition::Confirm,
                OrderState::Confirmed,
            ),
            (
                OrderState::Submitted,
                OrderTransition::Reject,
                OrderState::Rejected,
            ),
            (
                OrderState::Submitted,
                OrderTransition::Timeout,
                OrderState::Uncertain,
            ),
            (
                OrderState::Confirmed,
                OrderTransition::PartiallyFill,
                OrderState::PartialFill,
            ),
            (
                OrderState::Confirmed,
                OrderTransition::Fill,
                OrderState::Filled,
            ),
            (
                OrderState::PartialFill,
                OrderTransition::PartiallyFill,
                OrderState::PartialFill,
            ),
            (
                OrderState::PartialFill,
                OrderTransition::Fill,
                OrderState::Filled,
            ),
            (
                OrderState::PartialFill,
                OrderTransition::Reject,
                OrderState::Rejected,
            ),
            (
                OrderState::Uncertain,
                OrderTransition::BeginRecon,
                OrderState::PendingRecon,
            ),
            (
                OrderState::PendingRecon,
                OrderTransition::Resolve,
                OrderState::Resolved,
            ),
        ];
        for (from, trigger, to) in cases {
            let got = try_transition(*from, *trigger).unwrap_or_else(|_| {
                panic!("expected valid transition {:?} via {:?}", from, trigger)
            });
            assert_eq!(got, *to, "{:?} via {:?}", from, trigger);
        }
    }

    /// Task 7.2 — exhaustive: every NOT-listed `(state, trigger)` pair returns Err.
    #[test]
    fn every_invalid_transition_returns_error() {
        let states = [
            OrderState::Idle,
            OrderState::Submitted,
            OrderState::Confirmed,
            OrderState::PartialFill,
            OrderState::Filled,
            OrderState::Rejected,
            OrderState::Cancelled,
            OrderState::PendingCancel,
            OrderState::Uncertain,
            OrderState::PendingRecon,
            OrderState::Resolved,
        ];
        let triggers = [
            OrderTransition::Submit,
            OrderTransition::Confirm,
            OrderTransition::PartiallyFill,
            OrderTransition::Fill,
            OrderTransition::Reject,
            OrderTransition::Timeout,
            OrderTransition::BeginRecon,
            OrderTransition::Resolve,
        ];
        // The valid pairs from above.
        let valid: std::collections::HashSet<(OrderState, OrderTransition)> = [
            (OrderState::Idle, OrderTransition::Submit),
            (OrderState::Submitted, OrderTransition::Confirm),
            (OrderState::Submitted, OrderTransition::Reject),
            (OrderState::Submitted, OrderTransition::Timeout),
            (OrderState::Confirmed, OrderTransition::PartiallyFill),
            (OrderState::Confirmed, OrderTransition::Fill),
            (OrderState::PartialFill, OrderTransition::PartiallyFill),
            (OrderState::PartialFill, OrderTransition::Fill),
            (OrderState::PartialFill, OrderTransition::Reject),
            (OrderState::Uncertain, OrderTransition::BeginRecon),
            (OrderState::PendingRecon, OrderTransition::Resolve),
        ]
        .into_iter()
        .collect();

        for s in states {
            for t in triggers {
                let result = try_transition(s, t);
                if valid.contains(&(s, t)) {
                    assert!(result.is_ok(), "expected ok for {:?}/{:?}", s, t);
                } else {
                    let err = result.expect_err(&format!("expected err for {:?}/{:?}", s, t));
                    assert_eq!(err.from, s);
                    assert_eq!(err.trigger, t);
                }
            }
        }
    }

    /// Task 7.3 — invalid transition triggers the circuit-breaker callback.
    #[test]
    fn invalid_transition_invokes_circuit_breaker() {
        let trips = std::sync::Arc::new(AtomicUsize::new(0));
        let cb_trips = trips.clone();
        let cb: CircuitBreakerCallback = std::sync::Arc::new(move |_invalid| {
            cb_trips.fetch_add(1, Ordering::SeqCst);
        });

        let mut sm = OrderStateMachine::new(1, 11)
            .with_state(OrderState::Filled)
            .with_circuit_breaker(cb);
        let result = sm.transition(OrderTransition::Confirm, now());
        assert!(result.is_err());
        assert_eq!(trips.load(Ordering::SeqCst), 1);
    }

    /// `submitted_at` populates exactly when the machine first enters Submitted.
    #[test]
    fn submitted_at_recorded_on_submission() {
        let mut sm = OrderStateMachine::new(7, 77);
        assert!(sm.submitted_at().is_none());
        sm.transition(OrderTransition::Submit, UnixNanos::new(123_456))
            .unwrap();
        assert_eq!(sm.submitted_at(), Some(UnixNanos::new(123_456)));
        // Subsequent transitions don't reset it.
        sm.transition(OrderTransition::Confirm, UnixNanos::new(999_999))
            .unwrap();
        assert_eq!(sm.submitted_at(), Some(UnixNanos::new(123_456)));
    }

    /// Successful transitions return Ok and update internal state.
    #[test]
    fn successful_transition_updates_state_and_returns_new_state() {
        let mut sm = OrderStateMachine::new(1, 1);
        let new = sm.transition(OrderTransition::Submit, now()).unwrap();
        assert_eq!(new, OrderState::Submitted);
        assert_eq!(sm.state(), OrderState::Submitted);
    }
}
