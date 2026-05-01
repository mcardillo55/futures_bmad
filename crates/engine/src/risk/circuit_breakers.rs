//! Unified circuit-breaker / gate framework — story 5.1.
//!
//! [`CircuitBreakers`] is the single source of truth for "can we trade?".
//! Every order submission path in the engine routes through
//! [`CircuitBreakers::permits_trading`] before reaching the broker queue.
//!
//! Vocabulary
//! ----------
//!   * **Breaker** — manual reset. Tripping persists until process restart.
//!     Used for severity-of-state failures (daily-loss cap, runaway loss
//!     streak, anomalous fills, connection loss, buffer overflow).
//!   * **Gate** — auto-clear. The framework re-evaluates the underlying
//!     condition each tick; when it resolves, trading auto-resumes and a
//!     clearance event is emitted to the journal.
//!
//! Hot-path discipline
//! -------------------
//!   * `permits_trading()` is `#[inline]`, branches over a fixed table of
//!     ten field comparisons, and returns `Ok(())` without allocating
//!     when every breaker / gate is `Active`.
//!   * Failure-side construction allocates a `Vec<DenialReason>` capped at
//!     ten entries; that is unavoidable since the caller needs the full
//!     set, but it only fires when at least one breaker is non-`Active`.
//!
//! Stop-loss preservation invariant
//! --------------------------------
//! [`CircuitBreakers::orders_to_cancel`] explicitly filters out
//! `OrderType::Stop` orders. A circuit-breaker trip cancels pending entry
//! market orders and resting limit orders only — leaving an open position
//! unprotected by removing its stop is, per the architecture spec, "the
//! worst possible state". The flatten-then-stop policy is enforced
//! upstream (the panic-mode controller in `panic_mode.rs`); the breaker
//! framework itself never strips stops.

#![deny(unsafe_code)]

use std::collections::VecDeque;
use std::time::Instant;

use crossbeam_channel::{Sender, TrySendError};
use futures_bmad_core::{
    BreakerCategory, BreakerState, BreakerType, CircuitBreakerEvent, OrderEvent, OrderType,
    TradingConfig, UnixNanos,
};
use tracing::{info, warn};

use super::{DenialReason, TradingDenied};

/// Per-state tracking for the ten breakers / gates.
///
/// Field layout follows the [`BreakerType`] variant order. Each
/// `*_state` field is the current [`BreakerState`]; the optional
/// `*_tripped_at` field records when a gate was tripped so we can log the
/// duration of the outage when it auto-clears.
pub struct CircuitBreakers {
    // Breakers (manual reset, restart-clears).
    daily_loss_state: BreakerState,
    consecutive_losses_state: BreakerState,
    max_trades_state: BreakerState,
    anomalous_position_state: BreakerState,
    connection_failure_state: BreakerState,
    buffer_overflow_state: BreakerState,
    malformed_messages_state: BreakerState,

    // Gates (auto-clear when condition resolves).
    max_position_size_state: BreakerState,
    data_quality_state: BreakerState,
    fee_staleness_state: BreakerState,

    // Per-gate trip-time tracking — used for the "duration tripped" log
    // line on auto-clear (Task 8.3). `None` while the gate is `Active`.
    max_position_size_tripped_at: Option<Instant>,
    data_quality_tripped_at: Option<Instant>,
    fee_staleness_tripped_at: Option<Instant>,

    // Cached denial-context fields. The breaker is the source of truth for
    // its tripped state; these fields carry the contextual data we surface
    // back via `DenialReason`.
    daily_loss_current: i64,
    daily_loss_limit: i64,
    consecutive_loss_count: u32,
    consecutive_loss_limit: u32,
    trade_count: u32,
    max_trades_limit: u32,
    requested_position_size: u32,
    max_position_size_limit: u32,
    fee_days_stale: i64,
    buffer_occupancy_pct: f32,
    /// Sliding window of recent malformed-message arrivals; the
    /// malformed-messages breaker (story 5.4) populates this. Carried
    /// here so the framework owns the state, but the breaker logic
    /// itself is the future story's responsibility.
    pub malformed_window: VecDeque<Instant>,

    /// Free-form context fields populated by trip sites — included in
    /// denial-reason display strings.
    anomalous_position_detail: String,
    connection_failure_detail: String,
    data_quality_detail: String,
    malformed_messages_detail: String,

    journal: Sender<CircuitBreakerEvent>,
}

impl CircuitBreakers {
    /// Construct from a trading config. Every breaker / gate starts in
    /// `Active` (i.e. not tripped). `journal` is the crossbeam sender used
    /// to emit `CircuitBreakerEvent` records on every trip / clear; the
    /// receiver lives on the journal worker thread (see `persistence`).
    pub fn new(config: &TradingConfig, journal: Sender<CircuitBreakerEvent>) -> Self {
        Self {
            daily_loss_state: BreakerState::Active,
            consecutive_losses_state: BreakerState::Active,
            max_trades_state: BreakerState::Active,
            anomalous_position_state: BreakerState::Active,
            connection_failure_state: BreakerState::Active,
            buffer_overflow_state: BreakerState::Active,
            malformed_messages_state: BreakerState::Active,
            max_position_size_state: BreakerState::Active,
            data_quality_state: BreakerState::Active,
            fee_staleness_state: BreakerState::Active,

            max_position_size_tripped_at: None,
            data_quality_tripped_at: None,
            fee_staleness_tripped_at: None,

            daily_loss_current: 0,
            daily_loss_limit: config.max_daily_loss_ticks,
            consecutive_loss_count: 0,
            consecutive_loss_limit: config.max_consecutive_losses,
            trade_count: 0,
            max_trades_limit: config.max_trades_per_day,
            requested_position_size: 0,
            max_position_size_limit: config.max_position_size,
            fee_days_stale: 0,
            buffer_occupancy_pct: 0.0,
            malformed_window: VecDeque::new(),

            anomalous_position_detail: String::new(),
            connection_failure_detail: String::new(),
            data_quality_detail: String::new(),
            malformed_messages_detail: String::new(),

            journal,
        }
    }

    /// All breakers green ⇒ `Ok(())` without allocation. Otherwise returns
    /// the full set of denial reasons.
    ///
    /// `#[inline]` because this sits on the per-tick hot path and the
    /// happy path collapses to a single boolean reduction over ten
    /// `BreakerState::Active` checks.
    #[inline]
    pub fn permits_trading(&self) -> Result<(), TradingDenied> {
        // Happy-path early-out: bitwise reduction over ten enum equalities,
        // no allocation, no branching beyond the AND chain.
        if self.all_active() {
            return Ok(());
        }
        Err(self.collect_denials())
    }

    /// Whether every breaker / gate is currently `Active`. Pure read,
    /// kept inline so `permits_trading()` collapses to a single boolean.
    #[inline]
    fn all_active(&self) -> bool {
        matches!(self.daily_loss_state, BreakerState::Active)
            && matches!(self.consecutive_losses_state, BreakerState::Active)
            && matches!(self.max_trades_state, BreakerState::Active)
            && matches!(self.anomalous_position_state, BreakerState::Active)
            && matches!(self.connection_failure_state, BreakerState::Active)
            && matches!(self.buffer_overflow_state, BreakerState::Active)
            && matches!(self.malformed_messages_state, BreakerState::Active)
            && matches!(self.max_position_size_state, BreakerState::Active)
            && matches!(self.data_quality_state, BreakerState::Active)
            && matches!(self.fee_staleness_state, BreakerState::Active)
    }

    /// Collect every active denial reason. Cold-path; only invoked when
    /// at least one breaker / gate is non-`Active`.
    fn collect_denials(&self) -> TradingDenied {
        let mut reasons: Vec<DenialReason> = Vec::with_capacity(2);
        if !matches!(self.daily_loss_state, BreakerState::Active) {
            reasons.push(DenialReason::DailyLossExceeded {
                current: self.daily_loss_current,
                limit: self.daily_loss_limit,
            });
        }
        if !matches!(self.consecutive_losses_state, BreakerState::Active) {
            reasons.push(DenialReason::ConsecutiveLossesExceeded {
                current: self.consecutive_loss_count,
                limit: self.consecutive_loss_limit,
            });
        }
        if !matches!(self.max_trades_state, BreakerState::Active) {
            reasons.push(DenialReason::MaxTradesExceeded {
                current: self.trade_count,
                limit: self.max_trades_limit,
            });
        }
        if !matches!(self.anomalous_position_state, BreakerState::Active) {
            reasons.push(DenialReason::AnomalousPosition {
                detail: self.anomalous_position_detail.clone(),
            });
        }
        if !matches!(self.connection_failure_state, BreakerState::Active) {
            reasons.push(DenialReason::ConnectionFailure {
                detail: self.connection_failure_detail.clone(),
            });
        }
        if !matches!(self.buffer_overflow_state, BreakerState::Active) {
            reasons.push(DenialReason::BufferOverflow {
                occupancy_pct: self.buffer_occupancy_pct,
            });
        }
        if !matches!(self.malformed_messages_state, BreakerState::Active) {
            reasons.push(DenialReason::MalformedMessages {
                detail: self.malformed_messages_detail.clone(),
            });
        }
        if !matches!(self.max_position_size_state, BreakerState::Active) {
            reasons.push(DenialReason::MaxPositionSizeExceeded {
                requested: self.requested_position_size,
                limit: self.max_position_size_limit,
            });
        }
        if !matches!(self.data_quality_state, BreakerState::Active) {
            reasons.push(DenialReason::DataQualityFailure {
                detail: self.data_quality_detail.clone(),
            });
        }
        if !matches!(self.fee_staleness_state, BreakerState::Active) {
            reasons.push(DenialReason::FeeScheduleStale {
                days_old: self.fee_days_stale,
            });
        }
        TradingDenied { reasons }
    }

    /// Trip a breaker or gate.
    ///
    /// If the breaker / gate is already tripped, this is a no-op that returns
    /// the current state on the event for idempotency (the previous and new
    /// states will both be `Tripped` and no journal record is emitted —
    /// callers can detect the no-op by inspecting the returned event).
    pub fn trip_breaker(
        &mut self,
        breaker_type: BreakerType,
        reason: String,
        timestamp: UnixNanos,
    ) -> CircuitBreakerEvent {
        let previous = self.state(breaker_type);
        if matches!(previous, BreakerState::Tripped) {
            // Idempotent — return the current event without re-emitting.
            return CircuitBreakerEvent {
                breaker_type,
                trigger_reason: reason,
                timestamp,
                previous_state: previous,
                new_state: previous,
            };
        }

        // Capture context strings for cold-path denial rendering.
        match breaker_type {
            BreakerType::AnomalousPosition => {
                self.anomalous_position_detail = reason.clone();
            }
            BreakerType::ConnectionFailure => {
                self.connection_failure_detail = reason.clone();
            }
            BreakerType::DataQuality => {
                self.data_quality_detail = reason.clone();
            }
            BreakerType::MalformedMessages => {
                self.malformed_messages_detail = reason.clone();
            }
            _ => {}
        }

        // Stamp gate-trip Instant for duration tracking.
        if matches!(breaker_type.category(), BreakerCategory::Gate) {
            let now = Instant::now();
            match breaker_type {
                BreakerType::MaxPositionSize => self.max_position_size_tripped_at = Some(now),
                BreakerType::DataQuality => self.data_quality_tripped_at = Some(now),
                BreakerType::FeeStaleness => self.fee_staleness_tripped_at = Some(now),
                _ => {}
            }
        }

        self.set_state(breaker_type, BreakerState::Tripped);

        let event = CircuitBreakerEvent {
            breaker_type,
            trigger_reason: reason,
            timestamp,
            previous_state: previous,
            new_state: BreakerState::Tripped,
        };
        self.emit_event(&event);
        event
    }

    /// Clear a gate (auto-clear path). Returns the clearance event so
    /// callers can correlate the resumption in their own state.
    ///
    /// Returns `Err(())` for non-gate types: breakers require an explicit
    /// `reset_breaker()` call (manual reset / restart) and silently
    /// allowing this would defeat the gate-vs-breaker distinction.
    pub fn clear_gate(
        &mut self,
        gate_type: BreakerType,
        timestamp: UnixNanos,
    ) -> Result<CircuitBreakerEvent, GateClearError> {
        if !matches!(gate_type.category(), BreakerCategory::Gate) {
            return Err(GateClearError::NotAGate(gate_type));
        }
        let previous = self.state(gate_type);
        if matches!(previous, BreakerState::Active) {
            // Already clear — idempotent no-op.
            return Ok(CircuitBreakerEvent {
                breaker_type: gate_type,
                trigger_reason: "already-active".to_string(),
                timestamp,
                previous_state: previous,
                new_state: previous,
            });
        }
        // Compute duration the gate was tripped for the info log.
        let tripped_at = match gate_type {
            BreakerType::MaxPositionSize => self.max_position_size_tripped_at.take(),
            BreakerType::DataQuality => self.data_quality_tripped_at.take(),
            BreakerType::FeeStaleness => self.fee_staleness_tripped_at.take(),
            _ => None,
        };
        let duration = tripped_at.map(|t| t.elapsed());

        self.set_state(gate_type, BreakerState::Active);

        info!(
            target: "circuit_breakers",
            gate = gate_type.as_str(),
            tripped_for_ms = duration.map(|d| d.as_millis() as u64),
            "gate auto-cleared, trading resuming"
        );

        let event = CircuitBreakerEvent {
            breaker_type: gate_type,
            trigger_reason: "gate cleared".to_string(),
            timestamp,
            previous_state: previous,
            new_state: BreakerState::Cleared,
        };
        self.emit_event(&event);
        Ok(event)
    }

    /// Manually reset a breaker (V1: only invoked on process restart). For
    /// gate types this is also a valid call but `clear_gate()` is the
    /// preferred path because it logs duration and emits the clearance
    /// event.
    pub fn reset_breaker(&mut self, breaker_type: BreakerType) {
        self.set_state(breaker_type, BreakerState::Active);
        // Drop any cached gate-trip Instant so a subsequent re-trip starts
        // from a clean clock.
        match breaker_type {
            BreakerType::MaxPositionSize => self.max_position_size_tripped_at = None,
            BreakerType::DataQuality => self.data_quality_tripped_at = None,
            BreakerType::FeeStaleness => self.fee_staleness_tripped_at = None,
            _ => {}
        }
    }

    /// Public read accessor for a breaker / gate's current state.
    pub fn state(&self, breaker_type: BreakerType) -> BreakerState {
        match breaker_type {
            BreakerType::DailyLoss => self.daily_loss_state,
            BreakerType::ConsecutiveLosses => self.consecutive_losses_state,
            BreakerType::MaxTrades => self.max_trades_state,
            BreakerType::AnomalousPosition => self.anomalous_position_state,
            BreakerType::ConnectionFailure => self.connection_failure_state,
            BreakerType::BufferOverflow => self.buffer_overflow_state,
            BreakerType::MaxPositionSize => self.max_position_size_state,
            BreakerType::DataQuality => self.data_quality_state,
            BreakerType::FeeStaleness => self.fee_staleness_state,
            BreakerType::MalformedMessages => self.malformed_messages_state,
        }
    }

    /// Internal mutable state dispatch — kept private so the only ways to
    /// move a breaker / gate are `trip_breaker`, `clear_gate`,
    /// `reset_breaker`, or the gate-condition update helpers below.
    fn set_state(&mut self, breaker_type: BreakerType, new_state: BreakerState) {
        match breaker_type {
            BreakerType::DailyLoss => self.daily_loss_state = new_state,
            BreakerType::ConsecutiveLosses => self.consecutive_losses_state = new_state,
            BreakerType::MaxTrades => self.max_trades_state = new_state,
            BreakerType::AnomalousPosition => self.anomalous_position_state = new_state,
            BreakerType::ConnectionFailure => self.connection_failure_state = new_state,
            BreakerType::BufferOverflow => self.buffer_overflow_state = new_state,
            BreakerType::MaxPositionSize => self.max_position_size_state = new_state,
            BreakerType::DataQuality => self.data_quality_state = new_state,
            BreakerType::FeeStaleness => self.fee_staleness_state = new_state,
            BreakerType::MalformedMessages => self.malformed_messages_state = new_state,
        }
    }

    /// Filter `active_orders` to the IDs whose order types are eligible to
    /// be cancelled by a circuit-breaker trip — entry market orders and
    /// resting limit orders. Stop orders are NEVER returned: removing the
    /// stop on an open position is the architecture-spec definition of
    /// "the worst possible state".
    pub fn orders_to_cancel(&self, active_orders: &[OrderEvent]) -> Vec<u64> {
        active_orders
            .iter()
            .filter(|o| match o.order_type {
                OrderType::Market | OrderType::Limit { .. } => true,
                OrderType::Stop { .. } => false,
            })
            .map(|o| o.order_id)
            .collect()
    }

    /// Re-evaluate every gate's underlying condition and auto-clear gates
    /// whose tripping condition no longer holds. Stories 5.2–5.6 supply
    /// the per-gate evaluation closures via [`GateConditions`]; the
    /// framework here only owns the state-machine bookkeeping.
    ///
    /// Each transition `Tripped -> Active` triggers a `CircuitBreakerEvent`
    /// (via `clear_gate()`) with `new_state = Cleared`.
    pub fn update_gate_conditions(&mut self, conditions: &GateConditions, timestamp: UnixNanos) {
        // Each tuple: (breaker_type, condition still active?). When the
        // condition is no longer active (false), call `clear_gate`.
        let evaluations = [
            (
                BreakerType::MaxPositionSize,
                conditions.max_position_size_tripped,
            ),
            (BreakerType::DataQuality, conditions.data_quality_tripped),
            (BreakerType::FeeStaleness, conditions.fee_staleness_tripped),
        ];
        for (gate, still_tripped) in evaluations {
            if !still_tripped && matches!(self.state(gate), BreakerState::Tripped) {
                // We already validated `gate.category() == Gate` via the table above,
                // so `clear_gate` cannot return `NotAGate` here — but we still match
                // on the result rather than unwrap to avoid a panic path on the hot
                // gate-update tick.
                if let Err(e) = self.clear_gate(gate, timestamp) {
                    warn!(target: "circuit_breakers", error = ?e, "clear_gate failed unexpectedly");
                }
            }
        }
    }

    /// Emit a `CircuitBreakerEvent` to the journal channel without ever
    /// blocking the hot path. On `Full` / `Disconnected` we log and drop
    /// (matches the journal-write policy used elsewhere in the engine).
    fn emit_event(&self, event: &CircuitBreakerEvent) {
        match self.journal.try_send(event.clone()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                warn!(
                    target: "circuit_breakers",
                    breaker = event.breaker_type.as_str(),
                    "circuit-breaker journal channel full — event dropped"
                );
            }
            Err(TrySendError::Disconnected(_)) => {
                warn!(
                    target: "circuit_breakers",
                    breaker = event.breaker_type.as_str(),
                    "circuit-breaker journal channel disconnected — event dropped"
                );
            }
        }
    }

    /// Test-only / internal accessor used by gate-update closures and the
    /// breaker stories 5.2–5.6 to update the cached context fields.
    #[doc(hidden)]
    pub fn set_buffer_occupancy(&mut self, pct: f32) {
        self.buffer_occupancy_pct = pct;
    }
}

/// Per-tick gate evaluation snapshot. Stories 5.2–5.6 produce this from
/// their respective monitoring inputs (position tracker, data-quality
/// feed, fee-schedule clock) and pass it to
/// [`CircuitBreakers::update_gate_conditions`]. The framework here is
/// agnostic to *how* a gate's condition was evaluated — only to whether
/// it is still tripped.
#[derive(Debug, Clone, Copy, Default)]
pub struct GateConditions {
    /// Whether the max-position-size gate's condition still holds (i.e.
    /// the requested position would still exceed the cap). `false` ⇒ gate
    /// auto-clears.
    pub max_position_size_tripped: bool,
    pub data_quality_tripped: bool,
    pub fee_staleness_tripped: bool,
}

/// Error returned by `clear_gate` when called on a non-gate type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GateClearError {
    #[error("not a gate type: {0:?}")]
    NotAGate(BreakerType),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{Receiver, unbounded};
    use futures_bmad_core::FixedPrice;

    fn test_config() -> TradingConfig {
        TradingConfig {
            symbol: "ES".into(),
            max_position_size: 2,
            max_daily_loss_ticks: 1000,
            max_consecutive_losses: 3,
            max_trades_per_day: 30,
            edge_multiple_threshold: 1.5,
            session_start: "09:30".into(),
            session_end: "16:00".into(),
            max_spread_threshold: FixedPrice::new(4),
            fee_schedule_date: chrono::Utc::now().date_naive(),
            events: Vec::new(),
        }
    }

    fn fixture() -> (CircuitBreakers, Receiver<CircuitBreakerEvent>) {
        let (tx, rx) = unbounded();
        (CircuitBreakers::new(&test_config(), tx), rx)
    }

    /// Task 9.1 — `permits_trading` is `Ok` when every breaker / gate is `Active`.
    #[test]
    fn permits_trading_ok_when_all_active() {
        let (cb, _rx) = fixture();
        assert!(cb.permits_trading().is_ok());
    }

    /// Task 9.2 — tripping a single breaker yields the matching denial reason.
    #[test]
    fn permits_trading_returns_denial_for_tripped_breaker() {
        let (mut cb, _rx) = fixture();
        cb.trip_breaker(
            BreakerType::DailyLoss,
            "loss exceeded".into(),
            UnixNanos::new(100),
        );
        let err = cb.permits_trading().unwrap_err();
        assert_eq!(err.reasons.len(), 1);
        assert!(err.contains(BreakerType::DailyLoss));
    }

    /// Task 9.3 — `trip_breaker` emits a `CircuitBreakerEvent` to the journal.
    #[test]
    fn trip_breaker_emits_event_to_journal() {
        let (mut cb, rx) = fixture();
        let event = cb.trip_breaker(
            BreakerType::DailyLoss,
            "loss too big".into(),
            UnixNanos::new(42),
        );
        assert_eq!(event.breaker_type, BreakerType::DailyLoss);
        assert_eq!(event.previous_state, BreakerState::Active);
        assert_eq!(event.new_state, BreakerState::Tripped);

        let received = rx.try_recv().expect("event must be on channel");
        assert_eq!(received.breaker_type, BreakerType::DailyLoss);
        assert_eq!(received.trigger_reason, "loss too big");
        assert_eq!(received.timestamp, UnixNanos::new(42));
    }

    /// Task 9.4 — `orders_to_cancel` filters out stops, returns entries+limits.
    #[test]
    fn orders_to_cancel_preserves_stops() {
        let (cb, _rx) = fixture();
        use futures_bmad_core::Side;

        let entry = OrderEvent {
            order_id: 1,
            symbol_id: 1,
            side: Side::Buy,
            quantity: 1,
            order_type: OrderType::Market,
            decision_id: 1,
            timestamp: UnixNanos::new(1),
        };
        let limit = OrderEvent {
            order_id: 2,
            symbol_id: 1,
            side: Side::Sell,
            quantity: 1,
            order_type: OrderType::Limit {
                price: FixedPrice::new(200),
            },
            decision_id: 1,
            timestamp: UnixNanos::new(2),
        };
        let stop = OrderEvent {
            order_id: 3,
            symbol_id: 1,
            side: Side::Sell,
            quantity: 1,
            order_type: OrderType::Stop {
                trigger: FixedPrice::new(50),
            },
            decision_id: 1,
            timestamp: UnixNanos::new(3),
        };

        let to_cancel = cb.orders_to_cancel(&[entry, limit, stop]);
        assert_eq!(to_cancel, vec![1, 2], "stops MUST be preserved");
    }

    /// Task 9.5 — gate auto-clear transitions Tripped -> Active.
    #[test]
    fn gate_auto_clear_transitions_tripped_to_active() {
        let (mut cb, rx) = fixture();
        cb.trip_breaker(
            BreakerType::DataQuality,
            "stale book".into(),
            UnixNanos::new(1),
        );
        // Drain the trip event.
        let _trip = rx.try_recv().unwrap();

        let conditions = GateConditions {
            data_quality_tripped: false,
            ..Default::default()
        };
        cb.update_gate_conditions(&conditions, UnixNanos::new(2));

        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Active);
        let clear_event = rx.try_recv().expect("clear event must be emitted");
        assert_eq!(clear_event.breaker_type, BreakerType::DataQuality);
        assert_eq!(clear_event.previous_state, BreakerState::Tripped);
        assert_eq!(clear_event.new_state, BreakerState::Cleared);
    }

    /// Task 9.6 — breaker (manual-reset) does NOT auto-clear; only an
    /// explicit `reset_breaker` call moves it back to Active.
    #[test]
    fn breaker_manual_reset_required() {
        let (mut cb, _rx) = fixture();
        cb.trip_breaker(BreakerType::DailyLoss, "loss".into(), UnixNanos::new(1));
        // Update gate conditions (a no-op for non-gate types).
        cb.update_gate_conditions(&GateConditions::default(), UnixNanos::new(2));
        assert_eq!(cb.state(BreakerType::DailyLoss), BreakerState::Tripped);

        cb.reset_breaker(BreakerType::DailyLoss);
        assert_eq!(cb.state(BreakerType::DailyLoss), BreakerState::Active);
    }

    /// Task 9.7 — multiple simultaneous breakers each appear as their own
    /// denial reason; ordering is deterministic.
    #[test]
    fn multiple_breakers_yield_multiple_denials() {
        let (mut cb, _rx) = fixture();
        cb.trip_breaker(BreakerType::DailyLoss, "loss".into(), UnixNanos::new(1));
        cb.trip_breaker(BreakerType::MaxTrades, "too many".into(), UnixNanos::new(2));
        cb.trip_breaker(
            BreakerType::ConnectionFailure,
            "lost broker".into(),
            UnixNanos::new(3),
        );
        let denied = cb.permits_trading().unwrap_err();
        assert_eq!(denied.reasons.len(), 3);
        assert!(denied.contains(BreakerType::DailyLoss));
        assert!(denied.contains(BreakerType::MaxTrades));
        assert!(denied.contains(BreakerType::ConnectionFailure));
    }

    /// `clear_gate` rejects non-gate (manual-reset breaker) types.
    #[test]
    fn clear_gate_rejects_breaker_types() {
        let (mut cb, _rx) = fixture();
        cb.trip_breaker(BreakerType::DailyLoss, "loss".into(), UnixNanos::new(1));
        let err = cb
            .clear_gate(BreakerType::DailyLoss, UnixNanos::new(2))
            .unwrap_err();
        assert_eq!(err, GateClearError::NotAGate(BreakerType::DailyLoss));
        // State unchanged.
        assert_eq!(cb.state(BreakerType::DailyLoss), BreakerState::Tripped);
    }

    /// Idempotency — re-tripping an already-tripped breaker is a no-op
    /// (no duplicate journal event).
    #[test]
    fn re_tripping_is_idempotent() {
        let (mut cb, rx) = fixture();
        cb.trip_breaker(BreakerType::DailyLoss, "first".into(), UnixNanos::new(1));
        let _first = rx.try_recv().unwrap();

        cb.trip_breaker(BreakerType::DailyLoss, "second".into(), UnixNanos::new(2));
        // No second event should have been sent.
        assert!(
            rx.try_recv().is_err(),
            "duplicate trip must NOT emit a second journal event"
        );
    }

    /// Hot-path discipline: when every breaker is `Active`, the happy
    /// path returns `Ok(())` without allocating a denial Vec. We can't
    /// observe heap calls directly without instrumentation; the next-best
    /// proxy is to verify the body short-circuits by checking that
    /// `all_active()` matches the public predicate.
    #[test]
    fn happy_path_short_circuits() {
        let (cb, _rx) = fixture();
        assert!(cb.all_active());
        assert!(cb.permits_trading().is_ok());
    }

    /// Hot-path NFR proxy: an order-of-magnitude check that
    /// `permits_trading()` is sub-microsecond on the happy path. This is
    /// a smoke test, not an SLA — the actual NFR10 budget ("within one
    /// tick") is enforced upstream by the event-loop's timing harness.
    #[test]
    fn permits_trading_is_fast_on_happy_path() {
        let (cb, _rx) = fixture();
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            let _ = cb.permits_trading();
        }
        let elapsed = start.elapsed();
        // 100k calls in 100ms => 1us/call. Generous to avoid CI flakes;
        // the real budget is far tighter.
        assert!(
            elapsed.as_millis() < 100,
            "permits_trading too slow: {elapsed:?}"
        );
    }
}
