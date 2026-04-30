//! Panic mode — the terminal safety state engaged when flatten retry exhausts.
//!
//! Activation effects (per architecture spec):
//!   1. All trading is disabled — every subsequent `is_trading_enabled()` query
//!      returns `false`. Signal evaluation must short-circuit on this flag.
//!   2. All pending entry orders and limit orders are cancelled via the
//!      [`OrderCancellation`] trait the engine implements.
//!   3. **Resting stop-loss orders are NEVER cancelled.** An unprotected
//!      position is the worst possible state. The cancellation path explicitly
//!      filters out stops; never bypass this.
//!   4. An operator alert is logged at `error` with structured fields so the
//!      external alerting integration can pick it up.
//!   5. A `CircuitBreakerEvent` is journaled so the audit trail records the
//!      activation timestamp and reason.
//!   6. Panic state PERSISTS until the process restarts. There is no
//!      `deactivate()` method — manual operator restart is required.

use std::sync::atomic::{AtomicBool, Ordering};

use futures_bmad_core::UnixNanos;
use tracing::error;

use crate::persistence::journal::{
    CircuitBreakerEventRecord, EngineEvent as JournalEvent, JournalSender, SystemEventRecord,
};

/// Two-state panic discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanicState {
    Normal,
    PanicActive,
}

/// Trait the engine event-loop implements so [`PanicMode`] can drive
/// "cancel all pending entries and limits — preserve stops" without taking a
/// hard dependency on the order manager's concrete shape.
///
/// Implementations MUST NOT cancel resting stop-loss orders (the entire reason
/// for tiered safety is that an unprotected position is the worst state).
pub trait OrderCancellation {
    /// Cancel every pending entry market order and every resting limit order.
    /// Stops are left untouched.
    ///
    /// Returns the number of orders for which cancellation was issued.
    fn cancel_entries_and_limits(&mut self) -> usize;
}

/// Panic-mode controller.
///
/// The state is held in an [`AtomicBool`] (rather than a plain field) so the
/// "is trading enabled?" check is safe to read from any thread (signal
/// evaluators, the order manager, the buffer monitor, etc.) without touching
/// the controller itself. This avoids a system-wide `Mutex<PanicMode>` that
/// would otherwise sit on the hot path.
pub struct PanicMode {
    panic_active: AtomicBool,
    journal: JournalSender,
}

impl PanicMode {
    pub fn new(journal: JournalSender) -> Self {
        Self {
            panic_active: AtomicBool::new(false),
            journal,
        }
    }

    /// Current panic state.
    pub fn state(&self) -> PanicState {
        if self.panic_active.load(Ordering::Acquire) {
            PanicState::PanicActive
        } else {
            PanicState::Normal
        }
    }

    /// Whether new trades are allowed. Returns `false` once panic activates.
    /// Must be checked synchronously before every signal evaluation /
    /// order submission.
    pub fn is_trading_enabled(&self) -> bool {
        !self.panic_active.load(Ordering::Acquire)
    }

    /// Whether panic mode is currently active.
    pub fn is_active(&self) -> bool {
        self.panic_active.load(Ordering::Acquire)
    }

    /// Activate panic mode.
    ///
    /// Idempotent: a second call is a no-op (still `PanicActive`, no double
    /// cancellation, no double journal record). The first call:
    ///   1. Flips the trading flag,
    ///   2. Logs a structured `error!` for the operator alerting integration,
    ///   3. Cancels pending entries and limits via `cancellation` (stops
    ///      preserved),
    ///   4. Writes a `CircuitBreakerEvent` to the journal,
    ///   5. Writes a `SystemEvent` summary to the journal.
    pub fn activate<C: OrderCancellation>(
        &self,
        reason: &str,
        now: UnixNanos,
        cancellation: &mut C,
    ) -> ActivationOutcome {
        // Compare-exchange so a concurrent or duplicate activate() doesn't
        // double-cancel or double-journal.
        let was_normal = self
            .panic_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if !was_normal {
            return ActivationOutcome::AlreadyActive;
        }

        // 1. Operator alert — structured for downstream alerting integrations.
        error!(
            target: "panic_mode",
            reason,
            timestamp_nanos = now.as_nanos(),
            alert = true,
            "PANIC MODE ACTIVATED — all trading disabled, manual restart required"
        );

        // 2. Cancel pending entries and limits (stops preserved).
        let cancelled = cancellation.cancel_entries_and_limits();

        // 3. Journal: circuit-breaker event.
        let cb_record = CircuitBreakerEventRecord {
            timestamp: now,
            breaker_type: "panic_mode".to_string(),
            triggered: true,
            reason: reason.to_string(),
        };
        self.journal
            .send(JournalEvent::CircuitBreakerEvent(cb_record));

        // 4. Journal: system event summary.
        let sys_record = SystemEventRecord {
            timestamp: now,
            category: "panic_mode".to_string(),
            message: format!(
                "panic activated: {reason}; cancelled {cancelled} entries/limits, stops preserved"
            ),
        };
        self.journal.send(JournalEvent::SystemEvent(sys_record));

        ActivationOutcome::JustActivated {
            cancelled_entries_and_limits: cancelled,
        }
    }
}

/// Result of an activation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationOutcome {
    /// Activation succeeded; panic mode is now active. `cancelled` is the
    /// number of entry/limit orders cancelled (stops are intentionally not
    /// counted because they were not touched).
    JustActivated { cancelled_entries_and_limits: usize },
    /// Panic mode was already active; this call was a no-op.
    AlreadyActive,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::journal::EventJournal;

    /// Mock cancellation that records what was asked of it.
    #[derive(Default)]
    struct MockCancellation {
        invocations: u32,
        // Track that we never get a request to cancel a stop. (The mock
        // doesn't have stops — the contract is that the trait method itself
        // is "non-stop only", and we verify call shape here.)
        last_count: usize,
    }

    impl OrderCancellation for MockCancellation {
        fn cancel_entries_and_limits(&mut self) -> usize {
            self.invocations += 1;
            // Pretend we cancelled 4 orders.
            self.last_count = 4;
            4
        }
    }

    fn journal() -> JournalSender {
        let (tx, _rx) = EventJournal::channel();
        tx
    }

    /// Task 6.6 — panic mode disables trading, cancels entries, persists.
    #[test]
    fn activate_disables_trading_and_cancels_entries() {
        let pm = PanicMode::new(journal());
        let mut mock = MockCancellation::default();
        assert!(pm.is_trading_enabled());
        assert_eq!(pm.state(), PanicState::Normal);

        let outcome = pm.activate("flatten exhausted", UnixNanos::new(100), &mut mock);
        assert!(matches!(
            outcome,
            ActivationOutcome::JustActivated {
                cancelled_entries_and_limits: 4
            }
        ));
        assert!(pm.is_active());
        assert_eq!(pm.state(), PanicState::PanicActive);
        assert!(!pm.is_trading_enabled(), "trading must be disabled");
        assert_eq!(mock.invocations, 1);
    }

    /// Repeated activation is a no-op (no double-cancellation, no double-log).
    #[test]
    fn activate_is_idempotent() {
        let pm = PanicMode::new(journal());
        let mut mock = MockCancellation::default();
        let _ = pm.activate("first", UnixNanos::new(1), &mut mock);
        let outcome = pm.activate("second", UnixNanos::new(2), &mut mock);
        assert!(matches!(outcome, ActivationOutcome::AlreadyActive));
        assert_eq!(mock.invocations, 1, "must NOT cancel twice");
    }

    /// PanicMode persists — there is no public deactivate API. Reading state
    /// repeatedly after activation must always report PanicActive.
    #[test]
    fn panic_state_persists() {
        let pm = PanicMode::new(journal());
        let mut mock = MockCancellation::default();
        let _ = pm.activate("test", UnixNanos::new(1), &mut mock);
        for _ in 0..10 {
            assert!(pm.is_active());
            assert!(!pm.is_trading_enabled());
        }
    }

    /// Task 6.5 — composition: flatten exhaustion -> panic mode activation.
    ///
    /// Mirrors the engine event-loop path that wires `FlattenOutcome::Failed`
    /// into `PanicMode::activate`. Asserted separately from the broker-side
    /// flatten-retry tests so the policy boundary stays explicit.
    #[tokio::test]
    async fn flatten_exhaustion_triggers_panic_mode() {
        use futures_bmad_broker::{
            FlattenOutcome as BFO, FlattenRequest, FlattenRetry, OrderSubmitter, SubmissionError,
        };
        use futures_bmad_core::{OrderEvent, Side, UnixNanos};
        use std::sync::Mutex;
        use std::time::Duration;

        struct AlwaysFail {
            count: Mutex<u32>,
        }
        #[async_trait::async_trait]
        impl OrderSubmitter for AlwaysFail {
            async fn submit_order(&self, _e: &OrderEvent) -> Result<(), SubmissionError> {
                *self.count.lock().unwrap() += 1;
                Err(SubmissionError::ExchangeReject)
            }
        }
        let submitter = AlwaysFail {
            count: Mutex::new(0),
        };
        let retry = FlattenRetry::new(&submitter).with_retry_interval(Duration::from_millis(1));
        let outcome = retry
            .flatten_with_retry(FlattenRequest {
                order_id: 1,
                symbol_id: 1,
                side: Side::Sell,
                quantity: 1,
                decision_id: 7,
                timestamp: UnixNanos::new(1),
            })
            .await;
        assert!(matches!(
            outcome,
            BFO::Failed {
                attempts: 3,
                last_error: SubmissionError::ExchangeReject
            }
        ));

        // Now apply the policy: exhausted flatten -> activate panic mode.
        let pm = PanicMode::new(journal());
        let mut mock = MockCancellation::default();
        let activation = pm.activate("flatten retry exhausted", UnixNanos::new(2), &mut mock);
        assert!(matches!(
            activation,
            ActivationOutcome::JustActivated { .. }
        ));
        assert!(pm.is_active());
        assert!(!pm.is_trading_enabled());
    }

    /// Activation writes a CircuitBreakerEvent and a SystemEvent to the journal.
    #[test]
    fn activate_writes_journal_records() {
        let (sender, receiver) = EventJournal::channel();
        let pm = PanicMode::new(sender);
        let mut mock = MockCancellation::default();
        let _ = pm.activate("flatten exhausted", UnixNanos::new(42), &mut mock);

        let mut saw_cb = false;
        let mut saw_sys = false;
        for _ in 0..4 {
            match receiver.try_recv_for_test() {
                Some(JournalEvent::CircuitBreakerEvent(rec)) => {
                    saw_cb = true;
                    assert_eq!(rec.breaker_type, "panic_mode");
                    assert!(rec.triggered);
                    assert_eq!(rec.reason, "flatten exhausted");
                    assert_eq!(rec.timestamp, UnixNanos::new(42));
                }
                Some(JournalEvent::SystemEvent(rec)) => {
                    saw_sys = true;
                    assert_eq!(rec.category, "panic_mode");
                    assert!(rec.message.contains("flatten exhausted"));
                    assert!(rec.message.contains("stops preserved"));
                }
                Some(other) => panic!("unexpected event: {other:?}"),
                None => break,
            }
        }
        assert!(saw_cb, "circuit-breaker event must be journaled");
        assert!(saw_sys, "system event must be journaled");
    }
}
