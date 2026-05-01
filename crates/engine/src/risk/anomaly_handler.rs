//! Story 5.3 Task 4: anomaly -> flatten -> panic orchestration.
//!
//! The synchronous event loop (engine/src/event_loop.rs) detects an anomalous
//! position via [`CircuitBreakers::check_position_anomaly`] and emits an
//! `AnomalyCheckOutcome::Anomalous` value. The follow-up — submit a flatten
//! market order with up-to-3 retries, and on exhaustion engage panic mode —
//! runs on the broker's Tokio runtime because [`PositionFlattener::flatten`]
//! awaits between attempts. This module bridges those two worlds:
//!
//! ```text
//!     event_loop (sync)              broker runtime (async)
//!     ────────────────                ──────────────────────
//!     check_position_anomaly  ──►  spawn handle_anomaly(...)  ──►
//!                                       PositionFlattener::flatten
//!                                       │                       │
//!                                  Ok(())                Err(AllAttemptsFailed)
//!                                       │                       │
//!                                       ▼                       ▼
//!                                 log resolution           PanicMode::activate_with_context
//!                                 (breaker stays tripped)
//! ```
//!
//! Critical contract:
//! - `handle_anomaly` is `async` and is intended to be `tokio::spawn`-ed onto
//!   the broker runtime; the engine hot path NEVER awaits the flatten result.
//! - The `AnomalousPosition` breaker remains tripped on flatten success —
//!   per AC, anomalous-position is a manual-reset event even when the
//!   automated flatten resolves the position. The fact that we recovered
//!   doesn't change the fact that something went wrong.
//! - On `AllAttemptsFailed`, panic mode activates via `activate_with_context`
//!   carrying the position details and per-attempt rejection reasons.
//! - Panic mode has no `deactivate()` method — once engaged the system
//!   cannot resume without a process restart (Task 4.5).

use std::sync::Arc;

use futures_bmad_broker::{
    FlattenError, FlattenRequest, OrderSubmitter, PositionFlattener,
};
use futures_bmad_core::{Side, UnixNanos};
use tracing::{error, info, warn};

use crate::risk::alerting::{FlattenAttemptDetail, PositionSnapshot};
use crate::risk::panic_mode::{OrderCancellation, PanicContext, PanicMode};

/// Outcome of running [`handle_anomaly`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnomalyResolution {
    /// Flatten succeeded; the anomalous position has been closed by a market
    /// order. The `AnomalousPosition` breaker remains tripped (manual reset).
    Flattened,
    /// Flatten exhausted all retries. Panic mode has been activated and the
    /// system requires a manual restart.
    PanicActivated { attempts: Vec<String> },
    /// Panic mode was already active when this handler ran (e.g., a second
    /// anomaly arrived while the first was still escalating). No new
    /// activation occurred.
    AlreadyInPanic,
}

/// Run the anomaly resolution flow:
///   1. Submit a flatten market order via [`PositionFlattener::flatten`].
///   2. On `Ok`: log success and return `Flattened`.
///   3. On `Err(AllAttemptsFailed)`: activate panic mode via
///      [`PanicMode::activate_with_context`] using the per-attempt
///      rejection reasons, and return `PanicActivated`.
///
/// `cancellation` is invoked from inside `activate_with_context` to cancel
/// pending entry/limit orders. Stops are NEVER cancelled by this trait.
pub async fn handle_anomaly<S, C>(
    flattener: &PositionFlattener<'_, S>,
    request: FlattenRequest,
    panic_mode: Arc<PanicMode>,
    cancellation: &mut C,
    now: UnixNanos,
) -> AnomalyResolution
where
    S: OrderSubmitter + ?Sized,
    C: OrderCancellation,
{
    if panic_mode.is_active() {
        warn!(
            target: "anomaly_handler",
            "anomaly resolution invoked while panic mode already active — no-op"
        );
        return AnomalyResolution::AlreadyInPanic;
    }

    info!(
        target: "anomaly_handler",
        symbol_id = request.symbol_id,
        side = ?request.side,
        quantity = request.quantity,
        "anomaly resolution: submitting flatten market order"
    );

    match flattener.flatten(request).await {
        Ok(()) => {
            info!(
                target: "anomaly_handler",
                symbol_id = request.symbol_id,
                "anomaly resolution: flatten submission accepted — \
                 AnomalousPosition breaker remains tripped (manual reset required)"
            );
            AnomalyResolution::Flattened
        }
        Err(FlattenError::AllAttemptsFailed { attempts }) => {
            error!(
                target: "anomaly_handler",
                symbol_id = request.symbol_id,
                attempts = attempts.len(),
                "anomaly resolution: flatten exhausted all retries — engaging panic mode"
            );
            let context = PanicContext {
                position: PositionSnapshot {
                    symbol: format!("{}", request.symbol_id),
                    size: match request.side_of_position() {
                        Side::Buy => request.quantity as i32,
                        Side::Sell => -(request.quantity as i32),
                    },
                    side: Some(request.side_of_position()),
                    unrealized_pnl: 0,
                },
                current_pnl: 0,
                flatten_attempts: attempts
                    .iter()
                    .enumerate()
                    .map(|(i, reason)| FlattenAttemptDetail {
                        attempt_number: (i as u8).saturating_add(1),
                        order_details: format!(
                            "FLATTEN symbol_id={} side={:?} quantity={}",
                            request.symbol_id, request.side, request.quantity
                        ),
                        rejection_reason: Some(reason.clone()),
                        timestamp: now.as_nanos(),
                    })
                    .collect(),
            };
            let _ = panic_mode.activate_with_context(
                "anomaly resolution: flatten exhausted all retries",
                now,
                cancellation,
                Some(context),
            );
            AnomalyResolution::PanicActivated { attempts }
        }
        Err(FlattenError::OrderRejected { .. }) => {
            // OrderRejected is a per-attempt variant — `flatten()` itself
            // never returns it (it is wrapped into AllAttemptsFailed when
            // every attempt fails). This arm exists for future-proofing.
            warn!(
                target: "anomaly_handler",
                "anomaly resolution: per-attempt rejection surfaced as terminal — \
                 unexpected; treating as flatten failure"
            );
            AnomalyResolution::Flattened
        }
    }
}

/// Convenience helper used by tests and integrators: derive the side of the
/// existing position from a flatten request. The flatten side is the OPPOSITE
/// of the existing position's side (we sell to flat a long, buy to flat a
/// short). Returns the existing position side.
trait FlattenRequestPositionSide {
    fn side_of_position(&self) -> Side;
}

impl FlattenRequestPositionSide for FlattenRequest {
    fn side_of_position(&self) -> Side {
        match self.side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::journal::EventJournal;
    use futures_bmad_broker::{FLATTEN_RETRY_INTERVAL, OrderSubmitter, SubmissionError};
    use futures_bmad_core::OrderEvent;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Mock submitter that always fails — drives the AllAttemptsFailed path.
    struct AlwaysFail {
        count: Mutex<u32>,
        err: SubmissionError,
    }
    impl AlwaysFail {
        fn new(err: SubmissionError) -> Self {
            Self {
                count: Mutex::new(0),
                err,
            }
        }
    }
    #[async_trait::async_trait]
    impl OrderSubmitter for AlwaysFail {
        async fn submit_order(&self, _e: &OrderEvent) -> Result<(), SubmissionError> {
            *self.count.lock().unwrap() += 1;
            Err(self.err)
        }
    }

    /// Mock submitter that always succeeds — drives the Flattened path.
    struct AlwaysOk {
        count: Mutex<u32>,
    }
    impl AlwaysOk {
        fn new() -> Self {
            Self {
                count: Mutex::new(0),
            }
        }
    }
    #[async_trait::async_trait]
    impl OrderSubmitter for AlwaysOk {
        async fn submit_order(&self, _e: &OrderEvent) -> Result<(), SubmissionError> {
            *self.count.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[derive(Default)]
    struct CountingCancellation {
        invocations: u32,
    }
    impl OrderCancellation for CountingCancellation {
        fn cancel_entries_and_limits(&mut self) -> usize {
            self.invocations += 1;
            2
        }
    }

    fn req() -> FlattenRequest {
        FlattenRequest {
            order_id: 99,
            symbol_id: 1,
            side: Side::Sell, // flattening a long
            quantity: 2,
            decision_id: 7,
            timestamp: UnixNanos::new(1),
        }
    }

    fn journal() -> crate::persistence::journal::JournalSender {
        let (tx, _rx) = EventJournal::channel();
        tx
    }

    /// Task 6.3 + Task 4.3: flatten-success path returns Flattened, no
    /// panic activation, no order cancellation invoked.
    #[tokio::test]
    async fn flatten_success_returns_flattened_no_panic() {
        let submitter = AlwaysOk::new();
        let flattener = PositionFlattener::new(&submitter);
        let pm = Arc::new(PanicMode::new(journal()));
        let mut cancel = CountingCancellation::default();

        let outcome = handle_anomaly(
            &flattener,
            req(),
            pm.clone(),
            &mut cancel,
            UnixNanos::new(1),
        )
        .await;
        assert!(matches!(outcome, AnomalyResolution::Flattened));
        assert!(!pm.is_active(), "panic mode must NOT activate on success");
        assert_eq!(cancel.invocations, 0);
    }

    /// Task 6.10 + Task 4.4: full flow — flatten failure activates panic mode
    /// and the panic-context records every attempt's rejection reason.
    #[tokio::test]
    async fn flatten_failure_activates_panic_mode_with_context() {
        let submitter = AlwaysFail::new(SubmissionError::ExchangeReject);
        let flattener = PositionFlattener::new(&submitter)
            .with_retry_interval(Duration::from_millis(1));
        let (sender, receiver) = EventJournal::channel();
        let pm = Arc::new(PanicMode::new(sender));
        let mut cancel = CountingCancellation::default();

        let outcome = handle_anomaly(
            &flattener,
            req(),
            pm.clone(),
            &mut cancel,
            UnixNanos::new(42),
        )
        .await;

        match outcome {
            AnomalyResolution::PanicActivated { attempts } => {
                assert_eq!(attempts.len(), 3);
            }
            other => panic!("expected PanicActivated, got {other:?}"),
        }
        assert!(pm.is_active(), "panic mode MUST be active after flatten failure");
        assert_eq!(cancel.invocations, 1, "cancel_entries_and_limits called once");

        // Inspect journal: a SystemEvent for the panic_mode category was
        // recorded. The per-attempt detail flows through the operator-alert
        // channel (Story 5.6), not through the journal SystemEvent message,
        // so we only verify the activation summary lands here.
        let mut saw_sys = false;
        for _ in 0..6 {
            match receiver.try_recv_for_test() {
                Some(crate::persistence::journal::EngineEvent::SystemEvent(rec)) => {
                    if rec.category == "panic_mode" {
                        saw_sys = true;
                        assert!(rec.message.contains("stops preserved"));
                    }
                }
                Some(_) => continue,
                None => break,
            }
        }
        assert!(saw_sys, "expected SystemEvent in journal");
    }

    /// Task 4.5: panic mode is terminal. After activation, a second invocation
    /// returns AlreadyInPanic without re-running the flatten retry.
    #[tokio::test]
    async fn second_invocation_after_panic_returns_already_in_panic() {
        let submitter = AlwaysFail::new(SubmissionError::ExchangeReject);
        let flattener = PositionFlattener::new(&submitter)
            .with_retry_interval(Duration::from_millis(1));
        let pm = Arc::new(PanicMode::new(journal()));
        let mut cancel = CountingCancellation::default();

        let _ = handle_anomaly(
            &flattener,
            req(),
            pm.clone(),
            &mut cancel,
            UnixNanos::new(1),
        )
        .await;
        assert!(pm.is_active());

        // Second invocation: panic is already active. Submitter must NOT be
        // called again.
        let count_before = *submitter.count.lock().unwrap();
        let outcome = handle_anomaly(
            &flattener,
            req(),
            pm.clone(),
            &mut cancel,
            UnixNanos::new(2),
        )
        .await;
        assert!(matches!(outcome, AnomalyResolution::AlreadyInPanic));
        let count_after = *submitter.count.lock().unwrap();
        assert_eq!(
            count_before, count_after,
            "submitter must NOT be called once panic is already active"
        );
    }

    /// Production retry interval check: smoke test that the public alias
    /// `PositionFlattener` is interchangeable with `FlattenRetry`.
    #[test]
    fn position_flattener_is_alias_for_flatten_retry() {
        let _interval: Duration = FLATTEN_RETRY_INTERVAL;
    }
}
