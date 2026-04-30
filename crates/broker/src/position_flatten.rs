//! Position flatten retry — the broker-side mechanism that drives a market
//! flatten order through the [`OrderSubmitter`] trait, retrying up to
//! `FLATTEN_MAX_ATTEMPTS` times with a `FLATTEN_RETRY_INTERVAL` delay between
//! attempts (per architecture spec).
//!
//! ```text
//!   submit market flatten ─► success ─► FlattenOutcome::Success
//!         │
//!         ▼
//!     submission error
//!         │
//!         ▼
//!     wait FLATTEN_RETRY_INTERVAL (1s)
//!         │
//!         ▼
//!     retry, up to FLATTEN_MAX_ATTEMPTS (3) total
//!         │
//!         ▼
//!     all attempts failed ─► FlattenOutcome::Failed { attempts }
//!                            (caller engages PanicMode)
//! ```
//!
//! Critical invariants:
//! - The flatten order is always a Market order — a stop or limit could fail to
//!   fill at all. Speed beats slippage when the position is unprotected.
//! - The retry sleep runs on the Tokio runtime; the engine hot path is notified
//!   of the outcome via the FillQueue (the engine never blocks on this).
//! - This module does NOT activate panic mode itself. It only reports
//!   `FlattenOutcome::Failed { attempts }` after exhausting retries. The
//!   engine-side risk module (`engine::risk::panic_mode`) decides what to do.

use std::time::Duration;

use futures_bmad_core::{OrderEvent, OrderType, Side, UnixNanos};
use tracing::{error, info, warn};

use crate::order_routing::{OrderSubmitter, SubmissionError};

/// Total flatten attempts before declaring failure (per architecture spec
/// "Flatten retry count + interval | 3 attempts, 1s between").
pub const FLATTEN_MAX_ATTEMPTS: u8 = 3;

/// Wall-clock pause between flatten retries.
pub const FLATTEN_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// Result of a flatten retry sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlattenOutcome {
    /// Flatten market order accepted by the broker. The actual fill arrives via
    /// the normal FillEvent stream — this outcome is just the submission ack.
    Success {
        /// Engine-side `order_id` of the flatten market order.
        order_id: u64,
        /// Number of attempts taken (1..=FLATTEN_MAX_ATTEMPTS).
        attempts: u8,
    },
    /// All attempts failed. Caller (engine) should engage panic mode.
    Failed {
        attempts: u8,
        last_error: SubmissionError,
    },
}

/// Parameters for a single flatten request.
#[derive(Debug, Clone, Copy)]
pub struct FlattenRequest {
    /// Engine-side `order_id` to assign to the flatten market order. The
    /// submission path expects the caller to pre-allocate this so the engine's
    /// order tracker can wire it up before the first attempt.
    pub order_id: u64,
    pub symbol_id: u32,
    /// Side of the *flatten* order (opposite of the position side: Buy to flat
    /// a short, Sell to flat a long).
    pub side: Side,
    pub quantity: u32,
    /// Originating trade decision (for causality tracing — NFR17).
    pub decision_id: u64,
    pub timestamp: UnixNanos,
}

/// Broker-side flatten retry orchestrator.
///
/// Holds an `OrderSubmitter` (typically the live `RithmicSubmitter`) and exposes
/// `flatten_with_retry` which drives the retry loop.
///
/// Tokio sleeps are used between attempts; the orchestrator is `async`-only so
/// it MUST run on the broker's Tokio runtime, never on the hot path.
pub struct FlattenRetry<'a, S: OrderSubmitter + ?Sized> {
    submitter: &'a S,
    /// Override for the per-retry delay — set to a small value in tests so the
    /// retry loop completes within the test timeout. Production uses
    /// [`FLATTEN_RETRY_INTERVAL`].
    retry_interval: Duration,
    max_attempts: u8,
}

impl<'a, S: OrderSubmitter + ?Sized> FlattenRetry<'a, S> {
    /// Construct with production retry parameters.
    pub fn new(submitter: &'a S) -> Self {
        Self {
            submitter,
            retry_interval: FLATTEN_RETRY_INTERVAL,
            max_attempts: FLATTEN_MAX_ATTEMPTS,
        }
    }

    /// Override the retry interval. Intended for tests; production code should
    /// always use the default.
    pub fn with_retry_interval(mut self, interval: Duration) -> Self {
        self.retry_interval = interval;
        self
    }

    /// Override the max attempts (test-only). Production keeps the
    /// architecture-spec value of 3.
    pub fn with_max_attempts(mut self, attempts: u8) -> Self {
        self.max_attempts = attempts.max(1);
        self
    }

    /// Execute the flatten retry loop.
    ///
    /// Returns when either (a) a submission attempt succeeds or (b) all
    /// `max_attempts` have failed. The caller observes the outcome via the
    /// returned [`FlattenOutcome`] AND via the regular FillEvent stream once
    /// the broker reports the flatten order's fill (or rejection).
    pub async fn flatten_with_retry(&self, req: FlattenRequest) -> FlattenOutcome {
        let event = OrderEvent {
            order_id: req.order_id,
            symbol_id: req.symbol_id,
            side: req.side,
            quantity: req.quantity,
            order_type: OrderType::Market,
            decision_id: req.decision_id,
            timestamp: req.timestamp,
        };

        let mut last_error = SubmissionError::Unknown;
        for attempt in 1..=self.max_attempts {
            info!(
                target: "flatten_retry",
                order_id = req.order_id,
                decision_id = req.decision_id,
                attempt,
                max_attempts = self.max_attempts,
                "submitting flatten market order"
            );
            match self.submitter.submit_order(&event).await {
                Ok(()) => {
                    info!(
                        target: "flatten_retry",
                        order_id = req.order_id,
                        decision_id = req.decision_id,
                        attempts = attempt,
                        "flatten submission accepted"
                    );
                    return FlattenOutcome::Success {
                        order_id: req.order_id,
                        attempts: attempt,
                    };
                }
                Err(err) => {
                    last_error = err;
                    warn!(
                        target: "flatten_retry",
                        order_id = req.order_id,
                        decision_id = req.decision_id,
                        symbol_id = req.symbol_id,
                        attempt,
                        max_attempts = self.max_attempts,
                        error = %err,
                        "flatten submission rejected"
                    );
                    if attempt < self.max_attempts {
                        tokio::time::sleep(self.retry_interval).await;
                    }
                }
            }
        }

        error!(
            target: "flatten_retry",
            order_id = req.order_id,
            decision_id = req.decision_id,
            symbol_id = req.symbol_id,
            attempts = self.max_attempts,
            last_error = %last_error,
            "flatten retry exhausted — caller must engage panic mode"
        );
        FlattenOutcome::Failed {
            attempts: self.max_attempts,
            last_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mock submitter that fails its first N attempts and succeeds afterwards.
    struct ScriptedSubmitter {
        // Number of failures to emit before succeeding.
        fail_count: Mutex<u8>,
        fail_with: SubmissionError,
        // Total submissions attempted (for assertions).
        attempts: Mutex<u32>,
    }

    impl ScriptedSubmitter {
        fn new(fails: u8, err: SubmissionError) -> Self {
            Self {
                fail_count: Mutex::new(fails),
                fail_with: err,
                attempts: Mutex::new(0),
            }
        }

        fn attempts(&self) -> u32 {
            *self.attempts.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl OrderSubmitter for ScriptedSubmitter {
        async fn submit_order(&self, _event: &OrderEvent) -> Result<(), SubmissionError> {
            *self.attempts.lock().unwrap() += 1;
            let mut remaining = self.fail_count.lock().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                Err(self.fail_with)
            } else {
                Ok(())
            }
        }
    }

    fn req() -> FlattenRequest {
        FlattenRequest {
            order_id: 42,
            symbol_id: 1,
            side: Side::Sell,
            quantity: 2,
            decision_id: 7,
            timestamp: UnixNanos::new(1),
        }
    }

    /// Task 6.4 — first attempt rejected, second succeeds. Verifies the retry
    /// path is exercised and that `attempts == 2` is reported.
    #[tokio::test]
    async fn first_attempt_fails_second_succeeds() {
        let submitter = ScriptedSubmitter::new(1, SubmissionError::ExchangeReject);
        let retry = FlattenRetry::new(&submitter)
            // 1ms in tests so the test wall-clock stays bounded.
            .with_retry_interval(Duration::from_millis(1));

        let outcome = retry.flatten_with_retry(req()).await;
        assert!(matches!(
            outcome,
            FlattenOutcome::Success {
                order_id: 42,
                attempts: 2
            }
        ));
        assert_eq!(submitter.attempts(), 2);
    }

    /// Task 6.5 — all 3 attempts fail -> `Failed { attempts: 3 }`. Caller is
    /// expected to activate panic mode.
    #[tokio::test]
    async fn three_failures_returns_failed_outcome() {
        let submitter = ScriptedSubmitter::new(10, SubmissionError::ConnectionLost);
        let retry = FlattenRetry::new(&submitter).with_retry_interval(Duration::from_millis(1));

        let outcome = retry.flatten_with_retry(req()).await;
        assert!(matches!(
            outcome,
            FlattenOutcome::Failed {
                attempts: 3,
                last_error: SubmissionError::ConnectionLost
            }
        ));
        assert_eq!(submitter.attempts(), 3);
    }

    /// First attempt succeeds — no retries.
    #[tokio::test]
    async fn first_attempt_succeeds_no_retry() {
        let submitter = ScriptedSubmitter::new(0, SubmissionError::Unknown);
        let retry = FlattenRetry::new(&submitter);
        let outcome = retry.flatten_with_retry(req()).await;
        assert!(matches!(
            outcome,
            FlattenOutcome::Success {
                order_id: 42,
                attempts: 1
            }
        ));
        assert_eq!(submitter.attempts(), 1);
    }

    /// Verify the submitted event is a Market order on the requested side.
    #[tokio::test]
    async fn flatten_submits_market_order_on_requested_side() {
        struct CapturingSubmitter {
            captured: Mutex<Option<OrderEvent>>,
        }
        #[async_trait::async_trait]
        impl OrderSubmitter for CapturingSubmitter {
            async fn submit_order(&self, event: &OrderEvent) -> Result<(), SubmissionError> {
                *self.captured.lock().unwrap() = Some(*event);
                Ok(())
            }
        }
        let submitter = CapturingSubmitter {
            captured: Mutex::new(None),
        };
        let retry = FlattenRetry::new(&submitter);
        let mut request = req();
        request.side = Side::Buy;
        request.quantity = 5;
        retry.flatten_with_retry(request).await;

        let evt = submitter.captured.lock().unwrap().unwrap();
        assert!(matches!(evt.order_type, OrderType::Market));
        assert_eq!(evt.side, Side::Buy);
        assert_eq!(evt.quantity, 5);
        assert_eq!(evt.order_id, 42);
        assert_eq!(evt.decision_id, 7);
    }
}
