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
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use crossbeam_channel::{Sender, TrySendError};
use futures_bmad_core::{
    BreakerCategory, BreakerState, BreakerType, CircuitBreakerEvent, OrderEvent, OrderType,
    TradingConfig, UnixNanos,
};
use tracing::{error, info, warn};

use super::alerting::{Alert, AlertSender, PositionSnapshot};
use super::panic_mode::PanicMode;
use super::{DenialReason, TradingDenied};

/// Reconnection FSM state surfaced by `engine/src/connection/fsm.rs`.
///
/// Story 5.4 only needs to react to the [`ConnectionState::CircuitBreak`]
/// terminal state — entered when reconciliation fails (timeout or position
/// mismatch). All other states are transient and do not trip the
/// connection-failure breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionState {
    /// Connected and reconciled — normal operation.
    Connected,
    /// Disconnected, attempting reconnect.
    Reconnecting,
    /// Reconnected, replaying broker state.
    Reconciling,
    /// Reconciliation failed — manual intervention required.
    CircuitBreak,
}

/// Story 5.3 — outcome of [`CircuitBreakers::check_position_anomaly`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnomalyCheckOutcome {
    /// Position matches the active strategy expectation. No action taken.
    Consistent,
    /// Position lies outside any active strategy context — the
    /// `AnomalousPosition` breaker has been tripped (manual reset only) and
    /// the caller should engage the flatten path.
    Anomalous {
        current_position: i32,
        strategy_expected_position: i32,
    },
}

/// Sliding-window threshold for malformed Rithmic messages — `>10` arrivals
/// in any 60s window trips the malformed-messages breaker (architecture
/// spec, Implementation Specifications).
const MALFORMED_WINDOW: Duration = Duration::from_secs(60);
const MALFORMED_THRESHOLD: usize = 10;

/// Buffer-occupancy thresholds for the tiered SPSC saturation response.
const BUFFER_WARN_PCT: f32 = 50.0;
const BUFFER_GATE_PCT: f32 = 80.0;
const BUFFER_BREAK_PCT: f32 = 95.0;
const BUFFER_FULL_PCT: f32 = 100.0;

/// Fee schedule staleness threshold (days). Anything strictly greater than
/// this trips the fee-staleness gate.
const FEE_STALENESS_DAYS: i64 = 60;

/// Fee schedule staleness warn threshold (days). Anything strictly greater
/// than this but less than [`FEE_STALENESS_DAYS`] emits a `tracing::warn!`
/// from [`CircuitBreakers::check_fee_staleness`] without tripping the gate.
/// Pre-Epic-6 cleanup D-3: relocated from `FeeGate::permits_trade`.
const FEE_STALENESS_WARN_DAYS: i64 = 30;

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
    /// Capacity of the SPSC buffer the framework is monitoring. Captured
    /// from `update_buffer_occupancy` calls so the 100%-full log line can
    /// report absolute counts, not just the percentage.
    buffer_capacity: usize,
    /// Sliding window of recent malformed-message arrivals; populated by
    /// `record_malformed_message`. Front-pruned to the most recent 60s
    /// window on each call, so `len()` is the live count.
    pub malformed_window: VecDeque<Instant>,

    /// Whether the buffer-high condition (>=80%) is active. The
    /// `data_quality_state` slot is shared between buffer pressure and
    /// data-quality issues; tracking these as separate sources lets the
    /// gate auto-clear only when BOTH have resolved.
    buffer_high_condition: bool,
    /// Whether a data-quality issue (`!is_tradeable`, gap, stale) is
    /// active. Counterpart to `buffer_high_condition`.
    data_quality_condition: bool,

    /// Free-form context fields populated by trip sites — included in
    /// denial-reason display strings.
    anomalous_position_detail: String,
    connection_failure_detail: String,
    data_quality_detail: String,
    malformed_messages_detail: String,

    journal: Sender<CircuitBreakerEvent>,

    /// Operator-alert sender (Story 5.6). When `Some`, every `trip_breaker`
    /// for a `BreakerCategory::Breaker` type also emits an [`Alert`] with
    /// severity `Error`. Gates explicitly do NOT route through this path —
    /// gate transitions remain routine structured-log events.
    alerts: Option<AlertSender>,

    /// Panic-mode controller (D-1 fix from pre-Epic-6 cleanup spec).
    /// `permits_trading` and `permits_trade_evaluation` consult this FIRST;
    /// when `is_trading_enabled()` returns `false`,
    /// [`DenialReason::PanicModeActive`] is prepended to the returned
    /// `reasons` list so operator alerts attribute the denial to panic mode
    /// rather than secondary breakers panic mode's flatten path likely
    /// caused. Held as an `Arc` so the same controller can be shared with
    /// the (future) consumer task that drives `handle_anomaly` in Story
    /// 8.2.
    panic_mode: Arc<PanicMode>,
}

impl CircuitBreakers {
    /// Construct from a trading config. Every breaker / gate starts in
    /// `Active` (i.e. not tripped). `journal` is the crossbeam sender used
    /// to emit `CircuitBreakerEvent` records on every trip / clear; the
    /// receiver lives on the journal worker thread (see `persistence`).
    ///
    /// `panic_mode` is the shared [`PanicMode`] controller. When the
    /// controller's `is_trading_enabled()` returns `false`,
    /// [`Self::permits_trading`] / [`Self::permits_trade_evaluation`]
    /// short-circuit with [`DenialReason::PanicModeActive`] as the FIRST
    /// reason. Pass an `Arc::new(PanicMode::new(journal_clone))` here.
    pub fn new(
        config: &TradingConfig,
        journal: Sender<CircuitBreakerEvent>,
        panic_mode: Arc<PanicMode>,
    ) -> Self {
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
            buffer_capacity: 0,
            malformed_window: VecDeque::new(),
            buffer_high_condition: false,
            data_quality_condition: false,

            anomalous_position_detail: String::new(),
            connection_failure_detail: String::new(),
            data_quality_detail: String::new(),
            malformed_messages_detail: String::new(),

            journal,
            alerts: None,
            panic_mode,
        }
    }

    /// Wire an [`AlertSender`] in for breaker (not gate) trips. Story 5.6
    /// — see module docs in `alerting.rs`. Gates are deliberately excluded
    /// from alerting; their routine activations are structured-log events
    /// only.
    pub fn with_alerts(mut self, alerts: AlertSender) -> Self {
        self.alerts = Some(alerts);
        self
    }

    /// Read access to the shared [`PanicMode`] controller. Used by
    /// integrators (engine event loop, future Story 8.2 anomaly consumer)
    /// that need to share the same controller without re-constructing.
    pub fn panic_mode(&self) -> &Arc<PanicMode> {
        &self.panic_mode
    }

    /// All breakers green ⇒ `Ok(())` without allocation. Otherwise returns
    /// the full set of denial reasons.
    ///
    /// Trade-evaluation-only gates (today: `FeeStaleness`) are deliberately
    /// excluded from this check — see [`permits_trade_evaluation`]. This
    /// is the gate safety / flatten orders go through, so a stale fee
    /// schedule never blocks a stop-loss or panic-mode flatten.
    ///
    /// **Panic-mode precedence (D-1):** the panic-mode controller is
    /// consulted FIRST. When `panic_mode.is_trading_enabled() == false`,
    /// the call returns `Err` with [`DenialReason::PanicModeActive`] as
    /// `reasons[0]` even if every breaker is otherwise `Active`. This
    /// ensures operator alerts attribute the denial to panic mode rather
    /// than to secondary breakers that panic mode's flatten path likely
    /// caused.
    ///
    /// `#[inline]` because this sits on the per-tick hot path and the
    /// happy path collapses to a single boolean reduction over nine
    /// `BreakerState::Active` checks.
    ///
    /// Single-snapshot consultation of panic_mode — TOCTOU-safe within a call.
    #[inline]
    pub fn permits_trading(&self) -> Result<(), TradingDenied> {
        // D-1: panic-mode first. The atomic load is single-instruction
        // hot-path-clean; on the happy path (panic inactive) we fall
        // through to the breaker reduction unchanged.
        let panic_active = !self.panic_mode.is_trading_enabled();
        // Happy-path early-out: bitwise reduction over nine enum equalities,
        // no allocation, no branching beyond the AND chain.
        if !panic_active && self.all_active_excluding_eval_only() {
            return Ok(());
        }
        let mut denied = self.collect_denials_excluding_eval_only();
        if panic_active {
            denied.reasons.insert(0, DenialReason::PanicModeActive);
        }
        Err(denied)
    }

    /// Whether every non-eval-only breaker / gate is currently `Active`.
    /// Pure read, kept inline so `permits_trading()` collapses to a single
    /// boolean.
    #[inline]
    fn all_active_excluding_eval_only(&self) -> bool {
        matches!(self.daily_loss_state, BreakerState::Active)
            && matches!(self.consecutive_losses_state, BreakerState::Active)
            && matches!(self.max_trades_state, BreakerState::Active)
            && matches!(self.anomalous_position_state, BreakerState::Active)
            && matches!(self.connection_failure_state, BreakerState::Active)
            && matches!(self.buffer_overflow_state, BreakerState::Active)
            && matches!(self.malformed_messages_state, BreakerState::Active)
            && matches!(self.max_position_size_state, BreakerState::Active)
            && matches!(self.data_quality_state, BreakerState::Active)
        // FeeStaleness intentionally excluded — see permits_trading docs.
    }

    /// Whether every breaker / gate (including eval-only) is currently
    /// `Active`. Used by `permits_trade_evaluation`.
    #[inline]
    fn all_active(&self) -> bool {
        self.all_active_excluding_eval_only()
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

    /// Like `collect_denials` but omits the eval-only `FeeStaleness`
    /// gate. Used by `permits_trading()` so safety / flatten orders are
    /// never blocked by a stale fee schedule.
    fn collect_denials_excluding_eval_only(&self) -> TradingDenied {
        let mut denied = self.collect_denials();
        denied
            .reasons
            .retain(|r| r.breaker_type() != Some(BreakerType::FeeStaleness));
        denied
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
        self.trip_breaker_with_context(breaker_type, reason, timestamp, None, 0)
    }

    /// Trip a breaker and additionally produce an operator alert payload
    /// containing the supplied position snapshot and current P&L (Story 5.6,
    /// Task 6.1). For gate-category trips no alert is emitted regardless of
    /// the supplied context — gates do not route through alerting.
    pub fn trip_breaker_with_context(
        &mut self,
        breaker_type: BreakerType,
        reason: String,
        timestamp: UnixNanos,
        position: Option<PositionSnapshot>,
        current_pnl: i64,
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

        // Story 5.6: emit operator alert for breaker-category trips ONLY.
        // Gates are routine recoverable events and stay in structured logs.
        if let Some(sender) = &self.alerts
            && matches!(breaker_type.category(), BreakerCategory::Breaker)
        {
            let snapshot = position
                .clone()
                .unwrap_or_else(PositionSnapshot::flat_unknown);
            sender.send(Alert::for_breaker(
                breaker_type,
                reason.clone(),
                timestamp,
                snapshot,
                current_pnl,
            ));
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

    // ---------------------------------------------------------------
    // Story 5.2 — position & loss-limit breakers / gate
    // ---------------------------------------------------------------
    //
    // Four pieces of state, four entry points. All four are integer-only
    // (quarter-ticks for P&L, raw counts for everything else) and on the
    // hot path so they avoid heap allocation outside of the (cold)
    // breaker-trip path.

    /// Re-evaluate the max-position-size **gate** against the current net
    /// position. Called every time the position changes (after a fill) or
    /// any time the engine wants the gate to re-check.
    ///
    /// Behaviour:
    ///   * `abs(current_position) >= max_position_size_limit` ⇒ trip the
    ///     gate (no-op if already tripped).
    ///   * `abs(current_position) <  max_position_size_limit` ⇒ if the gate
    ///     was previously tripped, auto-clear it. The clearance flows
    ///     through `clear_gate()` so the journal records a `Cleared` event
    ///     and the duration of the outage is logged.
    ///
    /// Returns the emitted [`CircuitBreakerEvent`] when the call caused a
    /// state transition, `None` otherwise (no transition ⇒ no journal
    /// record). The on-the-wire side of `requested_position_size` is also
    /// updated so the [`DenialReason::MaxPositionSizeExceeded`] context
    /// reflects the size we last observed, even when the call is a no-op
    /// against an already-tripped gate.
    pub fn check_position_size(
        &mut self,
        current_position: i32,
        timestamp: UnixNanos,
    ) -> Option<CircuitBreakerEvent> {
        let abs_pos = current_position.unsigned_abs();
        // Always update the contextual field so denial messages stay
        // current even when this call doesn't transition state.
        self.requested_position_size = abs_pos;

        let limit = self.max_position_size_limit;
        let already_tripped = matches!(self.max_position_size_state, BreakerState::Tripped);

        if abs_pos >= limit {
            if already_tripped {
                return None;
            }
            let evt = self.trip_breaker(
                BreakerType::MaxPositionSize,
                format!("position {abs_pos} >= max {limit}"),
                timestamp,
            );
            return Some(evt);
        }

        if already_tripped {
            // Gate condition resolved — auto-clear via `clear_gate` so the
            // duration log line and the `Cleared` event flow through the
            // canonical path.
            return self
                .clear_gate(BreakerType::MaxPositionSize, timestamp)
                .ok();
        }
        None
    }

    /// Update the cumulative session P&L tracked by the daily-loss
    /// **breaker**. Both arguments are quarter-tick integers.
    ///
    /// `daily_loss_current = realized_pnl + unrealized_pnl`. The breaker
    /// trips when the current loss is at or beyond the configured cap:
    /// since `max_daily_loss_ticks` is stored as a positive integer
    /// representing the absolute size of the loss budget, the trip
    /// condition is `daily_loss_current <= -max_daily_loss_ticks`.
    ///
    /// Once tripped, the breaker NEVER auto-clears — even if the P&L
    /// recovers later in the same session. This is the manual-reset rule:
    /// breakers persist until process restart. Subsequent calls update
    /// the cached `daily_loss_current` (so denial messages stay current)
    /// but do not move the breaker state.
    ///
    /// Returns the emitted trip event when this call caused the trip,
    /// `None` otherwise.
    pub fn update_daily_loss(
        &mut self,
        realized_pnl: i64,
        unrealized_pnl: i64,
        timestamp: UnixNanos,
    ) -> Option<CircuitBreakerEvent> {
        let total = realized_pnl.saturating_add(unrealized_pnl);
        self.daily_loss_current = total;

        // Already tripped? Update context only — never auto-clear.
        if matches!(self.daily_loss_state, BreakerState::Tripped) {
            return None;
        }

        // `max_daily_loss_ticks` is stored as a positive integer (the
        // absolute size of the cap). The cap is reached when the cumulative
        // P&L falls to or below `-limit`.
        let trip = total <= -self.daily_loss_limit;
        if trip {
            let evt = self.trip_breaker(
                BreakerType::DailyLoss,
                format!(
                    "daily P&L {total} ticks reached loss cap of {} ticks",
                    self.daily_loss_limit
                ),
                timestamp,
            );
            return Some(evt);
        }
        None
    }

    /// Record a completed trade's win/loss outcome and update the
    /// consecutive-losses **breaker**.
    ///
    /// Reset rule (architecture spec, Implementation Specifications table):
    ///   * On a winning trade the counter resets to 0 — REGARDLESS of the
    ///     breaker's current state. If the operator manually reset the
    ///     breaker mid-session, the next loss will continue from 0 because
    ///     the previous winning trade already cleared the counter.
    ///   * On a losing trade the counter increments. When it reaches the
    ///     configured limit the breaker trips.
    ///
    /// The deliberate consequence (per architecture.md): a manual breaker
    /// reset does NOT zero the counter. If an operator restarts and the
    /// next trade is also a loss, the counter continues from where the
    /// last winner left it — preventing a "reset and immediately blow
    /// through the limit again" scenario.
    pub fn record_trade_result(
        &mut self,
        is_winner: bool,
        timestamp: UnixNanos,
    ) -> Option<CircuitBreakerEvent> {
        if is_winner {
            self.consecutive_loss_count = 0;
            return None;
        }

        // Loss: increment the counter.
        self.consecutive_loss_count = self.consecutive_loss_count.saturating_add(1);

        if matches!(self.consecutive_losses_state, BreakerState::Tripped) {
            // Already tripped — keep counting for the contextual field
            // but don't re-trip.
            return None;
        }

        if self.consecutive_loss_count >= self.consecutive_loss_limit {
            let evt = self.trip_breaker(
                BreakerType::ConsecutiveLosses,
                format!(
                    "{} consecutive losses reached limit {}",
                    self.consecutive_loss_count, self.consecutive_loss_limit
                ),
                timestamp,
            );
            return Some(evt);
        }
        None
    }

    /// Increment the per-session trade counter and trip the
    /// max-trades-per-day **breaker** when the count exceeds the limit.
    ///
    /// "Exceeds" means strictly greater than: the threshold itself (e.g.
    /// `max_trades_per_day = 30`) is the LAST permitted trade count. The
    /// trip happens on the (limit + 1)th trade.
    ///
    /// Like all breakers, this requires manual reset (= process restart).
    /// The count itself never resets within a session.
    pub fn record_trade(&mut self, timestamp: UnixNanos) -> Option<CircuitBreakerEvent> {
        self.trade_count = self.trade_count.saturating_add(1);

        if matches!(self.max_trades_state, BreakerState::Tripped) {
            return None;
        }

        if self.trade_count > self.max_trades_limit {
            let evt = self.trip_breaker(
                BreakerType::MaxTrades,
                format!(
                    "{} trades exceeds daily limit {}",
                    self.trade_count, self.max_trades_limit
                ),
                timestamp,
            );
            return Some(evt);
        }
        None
    }

    /// Test-only / forensic accessor for the cached daily-loss value.
    #[doc(hidden)]
    pub fn daily_loss_current(&self) -> i64 {
        self.daily_loss_current
    }

    /// Test-only / forensic accessor for the consecutive-loss counter.
    #[doc(hidden)]
    pub fn consecutive_loss_count(&self) -> u32 {
        self.consecutive_loss_count
    }

    /// Test-only / forensic accessor for the per-session trade count.
    #[doc(hidden)]
    pub fn trade_count(&self) -> u32 {
        self.trade_count
    }

    // ---------------------------------------------------------------
    // Story 5.3 — anomalous-position detection
    // ---------------------------------------------------------------

    /// Detect a position that lies outside any active strategy context.
    ///
    /// `current_position` is the engine's net signed position for the symbol
    /// (positive long, negative short). `strategy_expected_position` is the
    /// sum of every active strategy's expected position; callers pass `0`
    /// when no strategy currently claims this symbol.
    ///
    /// Detection rules (per Story 5.3 ACs):
    ///   - Both flat ⇒ `Consistent`.
    ///   - Both equal ⇒ `Consistent` (strategy owns the position).
    ///   - Otherwise ⇒ `Anomalous`, trips the
    ///     [`BreakerType::AnomalousPosition`] breaker, journals a
    ///     `CircuitBreakerEvent` with the position delta in the reason
    ///     string. Manual reset only.
    pub fn check_position_anomaly(
        &mut self,
        symbol_id: u32,
        current_position: i32,
        strategy_expected_position: i32,
        timestamp: UnixNanos,
    ) -> AnomalyCheckOutcome {
        if current_position == strategy_expected_position {
            return AnomalyCheckOutcome::Consistent;
        }
        let delta = current_position.saturating_sub(strategy_expected_position);
        let reason = format!(
            "anomalous position on symbol {symbol_id}: current={current_position} \
             strategy_expected={strategy_expected_position} delta={delta}"
        );
        if matches!(self.anomalous_position_state, BreakerState::Active) {
            self.trip_breaker(BreakerType::AnomalousPosition, reason, timestamp);
        }
        AnomalyCheckOutcome::Anomalous {
            current_position,
            strategy_expected_position,
        }
    }

    // -------------------------------------------------------------------
    // Story 5.4 — infrastructure breakers.
    //
    // The methods below implement the five infrastructure-level safety
    // checks that catch system-state problems (queue saturation,
    // degraded data, stale fee schedules, malformed feed messages,
    // failed reconnects). They share the existing per-breaker state
    // machine (trip / gate-clear / manual-reset) introduced by
    // story 5.1; their job is to translate raw input signals into the
    // appropriate trip / clear actions.
    // -------------------------------------------------------------------

    /// Update the buffer-occupancy metric and trip / clear the tiered
    /// response.
    ///
    /// Tier table (architecture spec, SPSC Buffer Strategy):
    ///   * `>= 50%` — log warning (no breaker / gate action)
    ///   * `>= 80%` — trip the [`BreakerType::DataQuality`] gate (auto-clears
    ///     when occupancy falls below 80% AND no other data-quality fault
    ///     is active). Trade evaluation is paused but the engine continues
    ///     consuming events to drain the queue.
    ///   * `>= 95%` — trip the [`BreakerType::BufferOverflow`] breaker
    ///     (manual reset). Distinct from the 80% gate — both states can be
    ///     active simultaneously.
    ///   * `>= 100%` — log the saturation explicitly at `error!` level and
    ///     trip the BufferOverflow breaker (covers the 95% path with an
    ///     explicit "never silently drop" guarantee).
    ///
    /// `current_len` is the number of slots currently in use; `capacity`
    /// is the total queue size. Both are passed in so the framework is
    /// not tied to the SPSC implementation.
    pub fn update_buffer_occupancy(
        &mut self,
        current_len: usize,
        capacity: usize,
        timestamp: UnixNanos,
    ) {
        // Defensive: a zero-capacity queue would yield NaN/Inf percentage.
        // Treat it as unmonitored — log once at error level and bail.
        if capacity == 0 {
            error!(
                target: "circuit_breakers",
                "buffer occupancy update with capacity=0 — ignored"
            );
            return;
        }
        self.buffer_capacity = capacity;
        let pct = (current_len as f32 / capacity as f32) * 100.0;
        self.buffer_occupancy_pct = pct;

        // Tier 4: 100% full — explicit log, never silent. The 95% breaker
        // path also fires below; this branch is the audit-trail guarantee
        // that we never drop without explicit notice.
        if pct >= BUFFER_FULL_PCT {
            error!(
                target: "circuit_breakers",
                occupancy_pct = pct,
                current_len,
                capacity,
                "SPSC buffer 100% full — events will be dropped, tripping BufferOverflow"
            );
        }

        // Tier 3: 95% — trip the BufferOverflow breaker (manual reset).
        if pct >= BUFFER_BREAK_PCT && matches!(self.buffer_overflow_state, BreakerState::Active) {
            self.trip_breaker(
                BreakerType::BufferOverflow,
                format!("buffer occupancy {pct:.1}% reached 95% threshold"),
                timestamp,
            );
        }

        // Tier 2: 80% — trip the DataQuality gate due to buffer pressure.
        // The DataQuality gate slot is shared with the data-quality
        // sources (Task 2); we track the buffer-high condition separately
        // so the gate auto-clears only when both sources are clear.
        if pct >= BUFFER_GATE_PCT {
            if !self.buffer_high_condition {
                self.buffer_high_condition = true;
                if matches!(self.data_quality_state, BreakerState::Active) {
                    self.trip_breaker(
                        BreakerType::DataQuality,
                        format!("buffer occupancy {pct:.1}% reached 80% threshold"),
                        timestamp,
                    );
                }
            }
        } else if self.buffer_high_condition {
            // Buffer pressure resolved.
            self.buffer_high_condition = false;
            self.maybe_clear_data_quality_gate(timestamp);
        }

        // Tier 1: 50% — warning only.
        if (BUFFER_WARN_PCT..BUFFER_GATE_PCT).contains(&pct) {
            warn!(
                target: "circuit_breakers",
                occupancy_pct = pct,
                current_len,
                capacity,
                "SPSC buffer >=50% occupancy"
            );
        }
    }

    /// Update the data-quality state from order-book / feed signals.
    ///
    /// Any of the three flags being "bad" (`!is_tradeable`, `has_gap`,
    /// `is_stale`) trips the [`BreakerType::DataQuality`] gate. When all
    /// three flags clear AND no buffer-pressure source remains, the gate
    /// auto-clears.
    pub fn update_data_quality(
        &mut self,
        is_tradeable: bool,
        has_gap: bool,
        is_stale: bool,
        timestamp: UnixNanos,
    ) {
        let bad = !is_tradeable || has_gap || is_stale;
        if bad {
            if !self.data_quality_condition {
                self.data_quality_condition = true;
                if matches!(self.data_quality_state, BreakerState::Active) {
                    let detail = format!(
                        "is_tradeable={is_tradeable} has_gap={has_gap} is_stale={is_stale}",
                    );
                    self.trip_breaker(BreakerType::DataQuality, detail, timestamp);
                }
            }
        } else if self.data_quality_condition {
            self.data_quality_condition = false;
            self.maybe_clear_data_quality_gate(timestamp);
        }
    }

    /// Helper used by both buffer-pressure and data-quality clear paths:
    /// auto-clear the DataQuality gate only when neither source remains
    /// active. Idempotent.
    fn maybe_clear_data_quality_gate(&mut self, timestamp: UnixNanos) {
        if !self.buffer_high_condition
            && !self.data_quality_condition
            && matches!(self.data_quality_state, BreakerState::Tripped)
        {
            // `clear_gate` is the canonical path — emits the duration log
            // and audit-trail event.
            if let Err(e) = self.clear_gate(BreakerType::DataQuality, timestamp) {
                warn!(target: "circuit_breakers", error = ?e, "data_quality clear_gate failed");
            }
        }
    }

    /// Check the fee-schedule staleness gate.
    ///
    /// `current_date - fee_schedule_date > 60 days` ⇒ trip the
    /// [`BreakerType::FeeStaleness`] gate. When the difference falls back
    /// to ≤60 days (e.g. operator pushed a fresh config), the gate
    /// auto-clears.
    ///
    /// CRITICAL: the FeeStaleness gate ONLY blocks trade evaluation. Use
    /// [`CircuitBreakers::permits_trade_evaluation`] for entry decisions
    /// and the regular [`CircuitBreakers::permits_trading`] (which
    /// excludes FeeStaleness) for safety / flatten orders. See the
    /// distinction enforced in `permits_trade_evaluation` below.
    pub fn check_fee_staleness(
        &mut self,
        fee_schedule_date: NaiveDate,
        current_date: NaiveDate,
        timestamp: UnixNanos,
    ) {
        let days_old = (current_date - fee_schedule_date).num_days();
        self.fee_days_stale = days_old;

        if days_old > FEE_STALENESS_DAYS {
            if matches!(self.fee_staleness_state, BreakerState::Active) {
                self.trip_breaker(
                    BreakerType::FeeStaleness,
                    format!("fee schedule {days_old} days stale (threshold {FEE_STALENESS_DAYS})"),
                    timestamp,
                );
            }
        } else {
            // Pre-Epic-6 cleanup D-3: the `>30 days` warn was relocated
            // from `FeeGate::permits_trade` to here so the staleness
            // surface lives in exactly one place.
            if days_old > FEE_STALENESS_WARN_DAYS {
                warn!(
                    target: "circuit_breakers",
                    days = days_old,
                    "fee schedule >30 days stale (warning only; gate trips at >60)"
                );
            }
            if matches!(self.fee_staleness_state, BreakerState::Tripped) {
                // Operator pushed a fresh config — auto-clear.
                if let Err(e) = self.clear_gate(BreakerType::FeeStaleness, timestamp) {
                    warn!(target: "circuit_breakers", error = ?e, "fee_staleness clear_gate failed");
                }
            }
        }
    }

    /// Strict variant of `permits_trading` for trade-evaluation decisions
    /// (signal → entry-order pipeline).
    ///
    /// Returns `Err` if any of the nine `permits_trading()` breakers /
    /// gates have tripped *or* the FeeStaleness gate is tripped. Safety /
    /// flatten orders MUST use `permits_trading()` — they are NEVER
    /// blocked by the FeeStaleness gate (an open position with a stale
    /// fee schedule still needs its stop honored).
    ///
    /// `#[inline]` because the entry-order decision pipeline is also
    /// hot-path; the happy-path short-circuits to `Ok(())` without
    /// allocation.
    ///
    /// **Panic-mode precedence (D-1):** like [`Self::permits_trading`],
    /// this consults the panic-mode controller FIRST. When panic mode is
    /// active, `reasons[0] == DenialReason::PanicModeActive`.
    ///
    /// Single-snapshot consultation of panic_mode — TOCTOU-safe within a call.
    #[inline]
    pub fn permits_trade_evaluation(&self) -> Result<(), TradingDenied> {
        let panic_active = !self.panic_mode.is_trading_enabled();
        if !panic_active && self.all_active() {
            return Ok(());
        }
        let mut denied = self.collect_denials();
        if panic_active {
            denied.reasons.insert(0, DenialReason::PanicModeActive);
        }
        Err(denied)
    }

    /// Record a malformed Rithmic message arrival.
    ///
    /// Maintains a sliding 60-second window via front-pruning of the
    /// internal `VecDeque<Instant>`. When the window holds more than 10
    /// entries (i.e. 11+ malformed arrivals in any 60s span), trips the
    /// [`BreakerType::MalformedMessages`] breaker (manual reset).
    ///
    /// Note: individual malformed messages are already logged and skipped
    /// upstream by `broker/src/message_validator.rs` — this breaker
    /// detects the *pattern* that the feed is corrupt enough to warrant
    /// pulling the trading plug.
    pub fn record_malformed_message(&mut self, timestamp: Instant, journal_ts: UnixNanos) {
        // Front-prune entries older than 60s.
        let cutoff = timestamp.checked_sub(MALFORMED_WINDOW);
        if let Some(cutoff) = cutoff {
            while let Some(&front) = self.malformed_window.front() {
                if front < cutoff {
                    self.malformed_window.pop_front();
                } else {
                    break;
                }
            }
        }
        self.malformed_window.push_back(timestamp);

        if self.malformed_window.len() > MALFORMED_THRESHOLD
            && matches!(self.malformed_messages_state, BreakerState::Active)
        {
            let count = self.malformed_window.len();
            self.trip_breaker(
                BreakerType::MalformedMessages,
                format!(
                    "{count} malformed messages in 60s window (threshold {MALFORMED_THRESHOLD})"
                ),
                journal_ts,
            );
        }
    }

    /// React to a reconnection-FSM state transition.
    ///
    /// Only the [`ConnectionState::CircuitBreak`] state trips the
    /// [`BreakerType::ConnectionFailure`] breaker (manual reset). The
    /// breaker is appropriate, not a gate, because in-flight broker state
    /// may be corrupt after a failed reconciliation — the operator must
    /// confirm the world is consistent before resuming.
    pub fn on_connection_state_change(&mut self, new_state: ConnectionState, timestamp: UnixNanos) {
        if matches!(new_state, ConnectionState::CircuitBreak)
            && matches!(self.connection_failure_state, BreakerState::Active)
        {
            self.trip_breaker(
                BreakerType::ConnectionFailure,
                "reconnection FSM entered CircuitBreak".to_string(),
                timestamp,
            );
        }
    }

    /// Read the current buffer occupancy percentage. Test/observability.
    pub fn buffer_occupancy_pct(&self) -> f32 {
        self.buffer_occupancy_pct
    }

    /// Read the current malformed-window length. Test/observability.
    pub fn malformed_window_len(&self) -> usize {
        self.malformed_window.len()
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
        let panic_mode = Arc::new(PanicMode::new(panic_journal()));
        (CircuitBreakers::new(&test_config(), tx, panic_mode), rx)
    }

    /// Stand-alone panic-mode-only fixture for tests that need to
    /// activate panic mode and observe the resulting denial path.
    fn fixture_with_panic() -> (
        CircuitBreakers,
        Receiver<CircuitBreakerEvent>,
        Arc<PanicMode>,
    ) {
        let (tx, rx) = unbounded();
        let panic_mode = Arc::new(PanicMode::new(panic_journal()));
        (
            CircuitBreakers::new(&test_config(), tx, panic_mode.clone()),
            rx,
            panic_mode,
        )
    }

    fn panic_journal() -> crate::persistence::journal::JournalSender {
        let (tx, _rx) = crate::persistence::journal::EventJournal::channel();
        tx
    }

    /// Mock cancellation used to drive `PanicMode::activate` from D-1 tests.
    #[derive(Default)]
    struct NoopCancel;
    impl crate::risk::panic_mode::OrderCancellation for NoopCancel {
        fn cancel_entries_and_limits(&mut self) -> usize {
            0
        }
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

    // ----- Story 5.6: alerting integration -----

    fn alerting_fixture() -> (
        CircuitBreakers,
        Receiver<CircuitBreakerEvent>,
        super::super::alerting::AlertReceiver,
    ) {
        use super::super::alerting::alert_channel;
        let (tx, rx) = unbounded();
        let (alerts_tx, alerts_rx) = alert_channel();
        let panic_mode = Arc::new(PanicMode::new(panic_journal()));
        let cb = CircuitBreakers::new(&test_config(), tx, panic_mode).with_alerts(alerts_tx);
        (cb, rx, alerts_rx)
    }

    /// Task 7.1 + 6.1: tripping a breaker (not gate) emits an alert with all
    /// the required fields.
    #[test]
    fn breaker_trip_emits_alert_with_all_fields() {
        use super::super::alerting::{AlertSeverity, BreakerKind, PositionSnapshot};
        use futures_bmad_core::Side;

        let (mut cb, _journal, alerts_rx) = alerting_fixture();
        let snapshot = PositionSnapshot {
            symbol: "ESM6".to_string(),
            size: 2,
            side: Some(Side::Buy),
            unrealized_pnl: -42,
        };
        cb.trip_breaker_with_context(
            BreakerType::DailyLoss,
            "session loss limit hit".into(),
            UnixNanos::new(1_700_000_000),
            Some(snapshot),
            -250,
        );
        let alert = alerts_rx.try_recv().expect("breaker trip emits alert");
        assert_eq!(alert.severity, AlertSeverity::Error);
        assert!(matches!(
            alert.breaker_type,
            BreakerKind::Circuit(BreakerType::DailyLoss)
        ));
        assert_eq!(alert.trigger_reason, "session loss limit hit");
        assert_eq!(alert.timestamp, 1_700_000_000);
        assert_eq!(alert.position_state.symbol, "ESM6");
        assert_eq!(alert.current_pnl, -250);
    }

    /// Task 6.3 + 7.2: gate trips do NOT emit alerts.
    #[test]
    fn gate_trip_does_not_emit_alert() {
        let (mut cb, _journal, alerts_rx) = alerting_fixture();
        cb.trip_breaker(
            BreakerType::DataQuality,
            "stale tick".into(),
            UnixNanos::new(1),
        );
        cb.trip_breaker(
            BreakerType::FeeStaleness,
            "schedule old".into(),
            UnixNanos::new(2),
        );
        cb.trip_breaker(
            BreakerType::MaxPositionSize,
            "would exceed cap".into(),
            UnixNanos::new(3),
        );
        // Channel must be empty — no alerts emitted for gates.
        assert!(alerts_rx.try_recv().is_err());
    }

    /// Without `with_alerts`, no alerts are emitted even on breaker trips —
    /// preserves backwards compatibility with sites that haven't opted in.
    #[test]
    fn breaker_trip_without_alert_sender_is_silent() {
        let (mut cb, _journal) = fixture();
        cb.trip_breaker(BreakerType::DailyLoss, "loss".into(), UnixNanos::new(1));
        // Compiles and runs without panicking.
    }

    /// Idempotent re-trip does not re-emit an alert.
    #[test]
    fn idempotent_trip_does_not_double_alert() {
        let (mut cb, _journal, alerts_rx) = alerting_fixture();
        cb.trip_breaker(BreakerType::DailyLoss, "first".into(), UnixNanos::new(1));
        cb.trip_breaker(BreakerType::DailyLoss, "second".into(), UnixNanos::new(2));
        // First trip emits, second is a no-op.
        assert!(alerts_rx.try_recv().is_ok());
        assert!(alerts_rx.try_recv().is_err());
    }

    // ================================================================
    // Story 5.2 — position & loss-limit breakers / gate
    // ================================================================

    /// Task 6.1 — max-position-size gate trips at the limit and auto-clears
    /// when the position drops back inside the budget.
    #[test]
    fn max_position_size_gate_trips_and_auto_clears() {
        // Limit = 2 from `test_config()`.
        let (mut cb, rx) = fixture();

        // At the limit ⇒ trip.
        let trip = cb
            .check_position_size(2, UnixNanos::new(1))
            .expect("expected trip at limit");
        assert_eq!(trip.breaker_type, BreakerType::MaxPositionSize);
        assert_eq!(trip.new_state, BreakerState::Tripped);
        assert_eq!(
            cb.state(BreakerType::MaxPositionSize),
            BreakerState::Tripped
        );
        assert!(cb.permits_trading().is_err());
        // Drain the trip event.
        let _ = rx.try_recv().unwrap();

        // Reduce the position below the limit ⇒ auto-clear.
        let clear = cb
            .check_position_size(1, UnixNanos::new(2))
            .expect("expected auto-clear when within limits");
        assert_eq!(clear.breaker_type, BreakerType::MaxPositionSize);
        assert_eq!(clear.new_state, BreakerState::Cleared);
        assert_eq!(cb.state(BreakerType::MaxPositionSize), BreakerState::Active);
        assert!(cb.permits_trading().is_ok());

        // The clearance event was journaled.
        let evt = rx.try_recv().expect("clearance event must be on channel");
        assert_eq!(evt.new_state, BreakerState::Cleared);
    }

    /// Max-position-size: short side mirrors long side (abs() comparison).
    #[test]
    fn max_position_size_uses_absolute_value() {
        let (mut cb, _rx) = fixture();
        // Short of 2 contracts ⇒ |−2| = 2 == limit ⇒ trip.
        let evt = cb.check_position_size(-2, UnixNanos::new(1));
        assert!(evt.is_some());
        assert_eq!(
            cb.state(BreakerType::MaxPositionSize),
            BreakerState::Tripped
        );
    }

    // -------------------------------------------------------------------
    // Story 5.4 — infrastructure breaker tests (Task 7).
    //
    // Each test maps 1:1 to a subtask in Task 7 of the story.
    // -------------------------------------------------------------------

    const CAPACITY: usize = 1000;
    fn drain(rx: &Receiver<CircuitBreakerEvent>) -> Vec<CircuitBreakerEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// 7.1 — buffer overflow breaker trips at 95%, distinct from 80% gate.
    #[test]
    fn buffer_overflow_at_95_distinct_from_80_gate() {
        let (mut cb, rx) = fixture();
        // Cross 95% — this also crosses 80%, so BOTH should activate
        // simultaneously per the AC.
        cb.update_buffer_occupancy(950, CAPACITY, UnixNanos::new(1));

        assert_eq!(cb.state(BreakerType::BufferOverflow), BreakerState::Tripped);
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Tripped);

        let events = drain(&rx);
        // Two trips: BufferOverflow + DataQuality (in this order via
        // update_buffer_occupancy: 95% block runs first, then 80% block).
        let kinds: Vec<_> = events.iter().map(|e| e.breaker_type).collect();
        assert!(kinds.contains(&BreakerType::BufferOverflow));
        assert!(kinds.contains(&BreakerType::DataQuality));
    }

    /// 7.2 — 80% buffer gate auto-clears when occupancy drops below 80%.
    #[test]
    fn buffer_80_gate_auto_clears_below_threshold() {
        let (mut cb, _rx) = fixture();
        // Cross 80% (but not 95%) so only the gate trips.
        cb.update_buffer_occupancy(820, CAPACITY, UnixNanos::new(1));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Tripped);
        assert_eq!(cb.state(BreakerType::BufferOverflow), BreakerState::Active);

        // Drop below 80% — gate auto-clears.
        cb.update_buffer_occupancy(700, CAPACITY, UnixNanos::new(2));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Active);
    }

    /// 7.3 — 100% full triggers immediate circuit break with explicit log.
    /// We can't capture the tracing output without a subscriber; we
    /// instead verify the breaker IS tripped at 100%, satisfying the
    /// "never silently drop" requirement at the API level.
    #[test]
    fn buffer_100_percent_circuit_breaks() {
        let (mut cb, _rx) = fixture();
        cb.update_buffer_occupancy(CAPACITY, CAPACITY, UnixNanos::new(1));
        assert_eq!(cb.state(BreakerType::BufferOverflow), BreakerState::Tripped);
        assert!((cb.buffer_occupancy_pct() - 100.0).abs() < 1e-3);
    }

    /// 7.4 — data-quality gate trips on `!is_tradeable`, auto-clears on restore.
    #[test]
    fn data_quality_gate_trips_on_not_tradeable_and_clears() {
        let (mut cb, _rx) = fixture();
        cb.update_data_quality(false, false, false, UnixNanos::new(1));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Tripped);

        cb.update_data_quality(true, false, false, UnixNanos::new(2));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Active);
    }

    /// 7.5 — data-quality gate trips on stale data, clears on fresh data.
    #[test]
    fn data_quality_gate_trips_on_stale_and_clears() {
        let (mut cb, _rx) = fixture();
        cb.update_data_quality(true, false, true, UnixNanos::new(1));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Tripped);

        cb.update_data_quality(true, false, false, UnixNanos::new(2));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Active);
    }

    /// 7.5b — data-quality gate trips on sequence gap (separate to 7.5).
    #[test]
    fn data_quality_gate_trips_on_gap_and_clears() {
        let (mut cb, _rx) = fixture();
        cb.update_data_quality(true, true, false, UnixNanos::new(1));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Tripped);

        cb.update_data_quality(true, false, false, UnixNanos::new(2));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Active);
    }

    /// 7.6 — fee staleness gate trips at 61 days, NOT at 60 days.
    #[test]
    fn fee_staleness_threshold_at_61_days_not_60() {
        let (mut cb, _rx) = fixture();
        let today = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();

        // 60 days back — boundary, must NOT trip.
        let sched_60 = today - chrono::Duration::days(60);
        cb.check_fee_staleness(sched_60, today, UnixNanos::new(1));
        assert_eq!(
            cb.state(BreakerType::FeeStaleness),
            BreakerState::Active,
            "60 days must not trip (threshold is >60)"
        );

        // 61 days back — must trip.
        let sched_61 = today - chrono::Duration::days(61);
        cb.check_fee_staleness(sched_61, today, UnixNanos::new(2));
        assert_eq!(cb.state(BreakerType::FeeStaleness), BreakerState::Tripped);
    }

    /// 7.6b — fee staleness gate auto-clears when config is freshened.
    #[test]
    fn fee_staleness_auto_clears_on_fresh_config() {
        let (mut cb, _rx) = fixture();
        let today = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();

        let sched_old = today - chrono::Duration::days(90);
        cb.check_fee_staleness(sched_old, today, UnixNanos::new(1));
        assert_eq!(cb.state(BreakerType::FeeStaleness), BreakerState::Tripped);

        // Fresh config (today's date) — gate auto-clears.
        cb.check_fee_staleness(today, today, UnixNanos::new(2));
        assert_eq!(cb.state(BreakerType::FeeStaleness), BreakerState::Active);
    }

    /// D-3 (relocated from FeeGate): a 31-day-old fee schedule emits a
    /// `tracing::warn!` from `check_fee_staleness` but DOES NOT trip the
    /// gate. This test verifies the gate stays Active; the warn is
    /// observed only via a tracing subscriber so we don't assert on it
    /// directly.
    #[test]
    fn fee_staleness_warn_at_31_days_does_not_trip_gate() {
        let (mut cb, _rx) = fixture();
        let today = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let sched_31 = today - chrono::Duration::days(31);
        cb.check_fee_staleness(sched_31, today, UnixNanos::new(1));
        assert_eq!(
            cb.state(BreakerType::FeeStaleness),
            BreakerState::Active,
            "31-day staleness must warn-only, never trip the gate"
        );
    }

    /// D-3 (sole owner): `CircuitBreakers::check_fee_staleness` is now
    /// the single source for the >60d trip — the duplicate branch in
    /// `FeeGate::permits_trade` was removed in pre-Epic-6 cleanup.
    /// Confirms the trip + the `permits_trade_evaluation` denial
    /// continue to fire here.
    #[test]
    fn fee_staleness_sole_owner_after_d3() {
        let (mut cb, _rx) = fixture();
        let today = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let sched = today - chrono::Duration::days(70);
        cb.check_fee_staleness(sched, today, UnixNanos::new(1));
        assert_eq!(cb.state(BreakerType::FeeStaleness), BreakerState::Tripped);

        // Trade evaluation denied with FeeStaleness reason.
        let denied = cb.permits_trade_evaluation().unwrap_err();
        assert!(denied.contains(BreakerType::FeeStaleness));

        // Safety/flatten path — STILL permitted (FeeStaleness is eval-only).
        assert!(cb.permits_trading().is_ok());
    }

    /// 7.7 — fee staleness gate does NOT block safety/flatten orders;
    /// `permits_trading()` (the safety-permitting check) returns Ok even
    /// when FeeStaleness is tripped, while `permits_trade_evaluation()`
    /// rejects it.
    #[test]
    fn fee_staleness_blocks_eval_only_not_safety() {
        let (mut cb, _rx) = fixture();
        let today = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let sched_old = today - chrono::Duration::days(90);
        cb.check_fee_staleness(sched_old, today, UnixNanos::new(1));

        // Safety / flatten path — must be permitted.
        assert!(cb.permits_trading().is_ok());

        // Trade-evaluation path — must be denied with FeeStaleness reason.
        let denied = cb.permits_trade_evaluation().unwrap_err();
        assert!(denied.contains(BreakerType::FeeStaleness));
    }

    /// 7.8 — malformed-message sliding window prunes entries older than 60s.
    #[test]
    fn malformed_window_prunes_old_entries() {
        let (mut cb, _rx) = fixture();
        let t0 = Instant::now();

        // First arrival.
        cb.record_malformed_message(t0, UnixNanos::new(1));
        assert_eq!(cb.malformed_window_len(), 1);

        // Arrival 61 seconds later — the t0 entry must be pruned, leaving
        // only the new one.
        let t1 = t0 + Duration::from_secs(61);
        cb.record_malformed_message(t1, UnixNanos::new(2));
        assert_eq!(
            cb.malformed_window_len(),
            1,
            "old entry must be pruned outside 60s window"
        );
    }

    /// 7.9 — malformed-message breaker trips at 11th message within 60s window.
    #[test]
    fn malformed_breaker_trips_on_eleventh_in_window() {
        let (mut cb, _rx) = fixture();
        let t0 = Instant::now();

        // 10 arrivals — must NOT trip (threshold is `>10`).
        for i in 0..10u64 {
            cb.record_malformed_message(t0 + Duration::from_millis(i * 100), UnixNanos::new(i));
        }
        assert_eq!(
            cb.state(BreakerType::MalformedMessages),
            BreakerState::Active
        );

        // 11th arrival — must trip.
        cb.record_malformed_message(t0 + Duration::from_millis(1100), UnixNanos::new(99));
        assert_eq!(
            cb.state(BreakerType::MalformedMessages),
            BreakerState::Tripped
        );
    }

    /// `update_gate_conditions(...)` continues to integrate with the
    /// max-position-size gate when callers pre-evaluate the condition
    /// themselves (the framework's pre-existing API surface).
    #[test]
    fn max_position_size_clears_via_update_gate_conditions() {
        let (mut cb, _rx) = fixture();
        cb.trip_breaker(
            BreakerType::MaxPositionSize,
            "external trip".into(),
            UnixNanos::new(1),
        );
        cb.update_gate_conditions(
            &GateConditions {
                max_position_size_tripped: false,
                ..Default::default()
            },
            UnixNanos::new(2),
        );
        assert_eq!(cb.state(BreakerType::MaxPositionSize), BreakerState::Active);
    }

    /// Task 6.2 — daily-loss breaker trips when cumulative loss exceeds
    /// the configured cap.
    #[test]
    fn daily_loss_breaker_trips_at_cap() {
        // `test_config()` has max_daily_loss_ticks = 1000.
        let (mut cb, _rx) = fixture();

        // Loss of 999 ticks ⇒ within budget.
        assert!(cb.update_daily_loss(-999, 0, UnixNanos::new(1)).is_none());
        assert!(cb.permits_trading().is_ok());

        // Loss of 1001 ticks ⇒ trip.
        let evt = cb
            .update_daily_loss(-1001, 0, UnixNanos::new(2))
            .expect("expected trip");
        assert_eq!(evt.breaker_type, BreakerType::DailyLoss);
        assert_eq!(cb.state(BreakerType::DailyLoss), BreakerState::Tripped);
    }

    /// Daily-loss trips at exact-equal threshold (≤ −limit).
    #[test]
    fn daily_loss_trips_at_exact_threshold() {
        let (mut cb, _rx) = fixture();
        let evt = cb.update_daily_loss(-1000, 0, UnixNanos::new(1));
        assert!(evt.is_some(), "trip on exact equal to cap");
    }

    /// Task 6.3 — daily-loss breaker does NOT auto-clear if P&L recovers.
    #[test]
    fn daily_loss_does_not_auto_clear_on_recovery() {
        let (mut cb, _rx) = fixture();

        // Trip the breaker.
        cb.update_daily_loss(-1500, 0, UnixNanos::new(1)).unwrap();
        assert_eq!(cb.state(BreakerType::DailyLoss), BreakerState::Tripped);

        // Big positive P&L recovery — breaker MUST stay tripped.
        cb.update_daily_loss(500, 200, UnixNanos::new(2));
        assert_eq!(
            cb.state(BreakerType::DailyLoss),
            BreakerState::Tripped,
            "daily loss is a breaker; recovery must NOT auto-clear"
        );
        assert!(cb.permits_trading().is_err());

        // Even the cached `daily_loss_current` is updated for context...
        assert_eq!(cb.daily_loss_current(), 700);
        // ...but the breaker stays tripped until manual reset.
        cb.reset_breaker(BreakerType::DailyLoss);
        assert_eq!(cb.state(BreakerType::DailyLoss), BreakerState::Active);
    }

    /// Task 6.4 — daily-loss includes both realized and unrealized
    /// components.
    #[test]
    fn daily_loss_aggregates_realized_and_unrealized() {
        let (mut cb, _rx) = fixture();

        // Realized -700 + unrealized -400 = total -1100 ⇒ over the 1000 cap.
        let evt = cb
            .update_daily_loss(-700, -400, UnixNanos::new(1))
            .expect("expected trip when sum exceeds cap");
        assert_eq!(evt.breaker_type, BreakerType::DailyLoss);
        assert_eq!(cb.daily_loss_current(), -1100);
    }

    /// Task 6.5 — consecutive-loss counter increments on losses, resets
    /// on a winning trade.
    #[test]
    fn consecutive_loss_counter_increments_then_resets_on_win() {
        let (mut cb, _rx) = fixture();

        // Two losses, no trip yet (limit = 3).
        cb.record_trade_result(false, UnixNanos::new(1));
        cb.record_trade_result(false, UnixNanos::new(2));
        assert_eq!(cb.consecutive_loss_count(), 2);
        assert_eq!(
            cb.state(BreakerType::ConsecutiveLosses),
            BreakerState::Active
        );

        // A winner resets the counter to 0.
        cb.record_trade_result(true, UnixNanos::new(3));
        assert_eq!(cb.consecutive_loss_count(), 0);
        assert!(cb.permits_trading().is_ok());
    }

    /// Task 6.6 — consecutive-loss breaker trips at exactly the configured
    /// threshold (3rd consecutive loss when limit = 3).
    #[test]
    fn consecutive_loss_breaker_trips_at_limit() {
        let (mut cb, _rx) = fixture();
        // 3rd loss in a row should trip (limit = 3).
        assert!(cb.record_trade_result(false, UnixNanos::new(1)).is_none());
        assert!(cb.record_trade_result(false, UnixNanos::new(2)).is_none());
        let evt = cb
            .record_trade_result(false, UnixNanos::new(3))
            .expect("expected trip at limit");
        assert_eq!(evt.breaker_type, BreakerType::ConsecutiveLosses);
        assert_eq!(
            cb.state(BreakerType::ConsecutiveLosses),
            BreakerState::Tripped
        );
    }

    /// Task 6.7 — counter resets on a winning trade EVEN when the breaker
    /// is tripped. The counter and the breaker state are independent: a
    /// manual reset moves the breaker but NOT the counter; a winner
    /// always zeroes the counter.
    #[test]
    fn consecutive_loss_counter_resets_on_win_even_when_tripped() {
        let (mut cb, _rx) = fixture();

        // Trip the breaker via 3 consecutive losses.
        cb.record_trade_result(false, UnixNanos::new(1));
        cb.record_trade_result(false, UnixNanos::new(2));
        cb.record_trade_result(false, UnixNanos::new(3));
        assert_eq!(
            cb.state(BreakerType::ConsecutiveLosses),
            BreakerState::Tripped
        );

        // A winner — the breaker stays tripped (manual-reset rule), but the
        // counter resets.
        cb.record_trade_result(true, UnixNanos::new(4));
        assert_eq!(cb.consecutive_loss_count(), 0);
        assert_eq!(
            cb.state(BreakerType::ConsecutiveLosses),
            BreakerState::Tripped
        );

        // Now, if the operator manually resets the breaker, subsequent
        // losses count from 0 (because the winner already zeroed the
        // counter).
        cb.reset_breaker(BreakerType::ConsecutiveLosses);
        assert_eq!(cb.consecutive_loss_count(), 0);
        cb.record_trade_result(false, UnixNanos::new(5));
        assert_eq!(cb.consecutive_loss_count(), 1);
        assert_eq!(
            cb.state(BreakerType::ConsecutiveLosses),
            BreakerState::Active
        );
    }

    /// 7.10 — malformed breaker does NOT trip when 10 messages spread over >60s.
    #[test]
    fn malformed_breaker_no_trip_when_spread_over_window() {
        let (mut cb, _rx) = fixture();
        let t0 = Instant::now();

        // 11 arrivals, each 7 seconds apart — total 70s span. Window is
        // 60s, so no point in time has more than ~9 in window.
        for i in 0..11u64 {
            cb.record_malformed_message(t0 + Duration::from_secs(i * 7), UnixNanos::new(i));
            assert_eq!(
                cb.state(BreakerType::MalformedMessages),
                BreakerState::Active,
                "iteration {i}: must not trip when spread"
            );
        }
    }

    /// 7.11 — connection failure breaker trips on CircuitBreak FSM state.
    #[test]
    fn connection_failure_trips_on_circuit_break_state() {
        let (mut cb, _rx) = fixture();

        // Benign transitions are no-ops.
        cb.on_connection_state_change(ConnectionState::Reconnecting, UnixNanos::new(1));
        cb.on_connection_state_change(ConnectionState::Reconciling, UnixNanos::new(2));
        cb.on_connection_state_change(ConnectionState::Connected, UnixNanos::new(3));
        assert_eq!(
            cb.state(BreakerType::ConnectionFailure),
            BreakerState::Active
        );

        // CircuitBreak trips the breaker.
        cb.on_connection_state_change(ConnectionState::CircuitBreak, UnixNanos::new(4));
        assert_eq!(
            cb.state(BreakerType::ConnectionFailure),
            BreakerState::Tripped
        );
    }

    /// 7.12 — all infrastructure breakers can coexist (multi-trip).
    #[test]
    fn all_infrastructure_breakers_coexist() {
        let (mut cb, _rx) = fixture();
        let today = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let t0 = Instant::now();

        // Trip BufferOverflow + DataQuality (95%).
        cb.update_buffer_occupancy(950, CAPACITY, UnixNanos::new(1));
        // Trip data-quality with explicit fault (additional source).
        cb.update_data_quality(false, true, true, UnixNanos::new(2));
        // Trip FeeStaleness (90 days).
        let sched_old = today - chrono::Duration::days(90);
        cb.check_fee_staleness(sched_old, today, UnixNanos::new(3));
        // Trip MalformedMessages (11 in window).
        for i in 0..11 {
            cb.record_malformed_message(t0 + Duration::from_millis(i * 100), UnixNanos::new(i + 4));
        }
        // Trip ConnectionFailure.
        cb.on_connection_state_change(ConnectionState::CircuitBreak, UnixNanos::new(20));

        // All five infrastructure breakers / gates tripped simultaneously.
        assert_eq!(cb.state(BreakerType::BufferOverflow), BreakerState::Tripped);
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Tripped);
        assert_eq!(cb.state(BreakerType::FeeStaleness), BreakerState::Tripped);
        assert_eq!(
            cb.state(BreakerType::MalformedMessages),
            BreakerState::Tripped
        );
        assert_eq!(
            cb.state(BreakerType::ConnectionFailure),
            BreakerState::Tripped
        );

        // Trade-evaluation must be denied with all five reasons present.
        let denied = cb.permits_trade_evaluation().unwrap_err();
        assert!(denied.contains(BreakerType::BufferOverflow));
        assert!(denied.contains(BreakerType::DataQuality));
        assert!(denied.contains(BreakerType::FeeStaleness));
        assert!(denied.contains(BreakerType::MalformedMessages));
        assert!(denied.contains(BreakerType::ConnectionFailure));

        // permits_trading() denies all but FeeStaleness.
        let denied_safety = cb.permits_trading().unwrap_err();
        assert!(denied_safety.contains(BreakerType::BufferOverflow));
        assert!(denied_safety.contains(BreakerType::DataQuality));
        assert!(!denied_safety.contains(BreakerType::FeeStaleness));
        assert!(denied_safety.contains(BreakerType::MalformedMessages));
        assert!(denied_safety.contains(BreakerType::ConnectionFailure));
    }

    /// Edge: data-quality gate stays tripped while buffer-pressure source
    /// remains active. Verifies the dual-source auto-clear logic.
    #[test]
    fn data_quality_gate_dual_source_auto_clear() {
        let (mut cb, _rx) = fixture();

        // Trip via buffer pressure first.
        cb.update_buffer_occupancy(820, CAPACITY, UnixNanos::new(1));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Tripped);

        // Add a separate data-quality fault.
        cb.update_data_quality(false, false, false, UnixNanos::new(2));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Tripped);

        // Clear buffer pressure — gate must STAY tripped (data-quality
        // source still active).
        cb.update_buffer_occupancy(500, CAPACITY, UnixNanos::new(3));
        assert_eq!(
            cb.state(BreakerType::DataQuality),
            BreakerState::Tripped,
            "gate must remain tripped while data-quality source is active"
        );

        // Clear data-quality fault — gate auto-clears (no sources active).
        cb.update_data_quality(true, false, false, UnixNanos::new(4));
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Active);
    }

    /// Edge: 95% breaker is manual-reset; tripping persists across
    /// further `update_buffer_occupancy` calls even when occupancy
    /// recedes.
    #[test]
    fn buffer_overflow_breaker_requires_manual_reset() {
        let (mut cb, _rx) = fixture();
        cb.update_buffer_occupancy(960, CAPACITY, UnixNanos::new(1));
        assert_eq!(cb.state(BreakerType::BufferOverflow), BreakerState::Tripped);

        // Drain back down — breaker must NOT auto-clear.
        cb.update_buffer_occupancy(100, CAPACITY, UnixNanos::new(2));
        assert_eq!(cb.state(BreakerType::BufferOverflow), BreakerState::Tripped);

        // Manual reset clears it.
        cb.reset_breaker(BreakerType::BufferOverflow);
        assert_eq!(cb.state(BreakerType::BufferOverflow), BreakerState::Active);
    }

    /// Edge: malformed-messages breaker is manual-reset; once tripped, an
    /// idle 60s+ window does not bring it back.
    #[test]
    fn malformed_breaker_requires_manual_reset() {
        let (mut cb, _rx) = fixture();
        let t0 = Instant::now();
        for i in 0..11 {
            cb.record_malformed_message(t0 + Duration::from_millis(i * 100), UnixNanos::new(i));
        }
        assert_eq!(
            cb.state(BreakerType::MalformedMessages),
            BreakerState::Tripped
        );

        // Window goes idle — but breaker stays tripped.
        cb.record_malformed_message(t0 + Duration::from_secs(120), UnixNanos::new(99));
        assert_eq!(
            cb.state(BreakerType::MalformedMessages),
            BreakerState::Tripped
        );

        cb.reset_breaker(BreakerType::MalformedMessages);
        assert_eq!(
            cb.state(BreakerType::MalformedMessages),
            BreakerState::Active
        );
    }

    /// Counter survives a manual breaker reset that is NOT preceded by
    /// a winning trade. This is the "reset and immediately blow through"
    /// guard from the architecture spec: if the operator resets after the
    /// breaker tripped at 3, and the very next trade is a loss, the
    /// counter continues from 3 ⇒ 4 ⇒ trip immediately.
    #[test]
    fn manual_reset_does_not_zero_consecutive_counter() {
        let (mut cb, _rx) = fixture();
        // Trip via 3 losses in a row.
        cb.record_trade_result(false, UnixNanos::new(1));
        cb.record_trade_result(false, UnixNanos::new(2));
        cb.record_trade_result(false, UnixNanos::new(3));
        assert_eq!(
            cb.state(BreakerType::ConsecutiveLosses),
            BreakerState::Tripped
        );
        assert_eq!(cb.consecutive_loss_count(), 3);

        // Operator resets the breaker but NO winning trade has occurred.
        cb.reset_breaker(BreakerType::ConsecutiveLosses);
        assert_eq!(
            cb.consecutive_loss_count(),
            3,
            "manual reset must NOT zero the loss counter"
        );

        // The next loss takes us back over the threshold ⇒ trip again.
        let evt = cb
            .record_trade_result(false, UnixNanos::new(4))
            .expect("expected immediate re-trip");
        assert_eq!(evt.breaker_type, BreakerType::ConsecutiveLosses);
    }

    /// Task 6.8 — max-trades-per-day breaker trips when the count exceeds
    /// the configured limit.
    #[test]
    fn max_trades_breaker_trips_when_exceeded() {
        // limit = 30 from `test_config()`.
        let (mut cb, _rx) = fixture();
        for i in 1..=30u32 {
            assert!(
                cb.record_trade(UnixNanos::new(i as u64)).is_none(),
                "trip too early at {i}"
            );
        }
        // 31st trade pushes us over the cap.
        let evt = cb
            .record_trade(UnixNanos::new(31))
            .expect("expected trip on (limit+1)th trade");
        assert_eq!(evt.breaker_type, BreakerType::MaxTrades);
        assert_eq!(cb.state(BreakerType::MaxTrades), BreakerState::Tripped);
        assert_eq!(cb.trade_count(), 31);
    }

    /// Max-trades count never auto-resets within a session even after a
    /// manual breaker reset (the count itself is by-design a session-life
    /// counter; the breaker state is what manual reset clears).
    #[test]
    fn max_trades_count_persists_across_manual_reset() {
        let (mut cb, _rx) = fixture();
        for i in 1..=31u32 {
            cb.record_trade(UnixNanos::new(i as u64));
        }
        assert_eq!(cb.state(BreakerType::MaxTrades), BreakerState::Tripped);

        cb.reset_breaker(BreakerType::MaxTrades);
        assert_eq!(cb.state(BreakerType::MaxTrades), BreakerState::Active);
        // Count is preserved.
        assert_eq!(cb.trade_count(), 31);
        // Next trade re-trips immediately.
        let evt = cb.record_trade(UnixNanos::new(32));
        assert!(evt.is_some());
    }

    /// Task 6.9 — all four 5.2 breakers/gate can be tripped simultaneously
    /// and `permits_trading()` reports every denial reason.
    #[test]
    fn all_four_breakers_trip_simultaneously() {
        let (mut cb, _rx) = fixture();

        // 1) Position size gate (limit 2).
        cb.check_position_size(2, UnixNanos::new(1));
        // 2) Daily loss (limit 1000 ticks).
        cb.update_daily_loss(-2000, 0, UnixNanos::new(2));
        // 3) Consecutive losses (limit 3).
        cb.record_trade_result(false, UnixNanos::new(3));
        cb.record_trade_result(false, UnixNanos::new(4));
        cb.record_trade_result(false, UnixNanos::new(5));
        // 4) Max trades (limit 30).
        for i in 0..31u32 {
            cb.record_trade(UnixNanos::new(100 + i as u64));
        }

        let denied = cb.permits_trading().unwrap_err();
        assert!(denied.contains(BreakerType::MaxPositionSize));
        assert!(denied.contains(BreakerType::DailyLoss));
        assert!(denied.contains(BreakerType::ConsecutiveLosses));
        assert!(denied.contains(BreakerType::MaxTrades));
        assert_eq!(denied.reasons.len(), 4);
    }

    /// Task 6.10 — config thresholds drive the limits. Different config
    /// values produce different trip thresholds.
    #[test]
    fn config_thresholds_drive_trip_points() {
        let (tx, _rx) = unbounded();
        let config = TradingConfig {
            symbol: "ES".into(),
            max_position_size: 5,
            max_daily_loss_ticks: 250,
            max_consecutive_losses: 7,
            max_trades_per_day: 100,
            edge_multiple_threshold: 1.5,
            session_start: "09:30".into(),
            session_end: "16:00".into(),
            max_spread_threshold: FixedPrice::new(4),
            fee_schedule_date: chrono::Utc::now().date_naive(),
            events: Vec::new(),
        };
        let pm = Arc::new(PanicMode::new(panic_journal()));
        let mut cb = CircuitBreakers::new(&config, tx, pm.clone());

        // Position size: 4 OK, 5 trips.
        assert!(cb.check_position_size(4, UnixNanos::new(1)).is_none());
        assert!(cb.check_position_size(5, UnixNanos::new(2)).is_some());

        // Daily loss: -200 OK, -300 trips against the 250 cap.
        let (tx2, _rx2) = unbounded();
        let mut cb2 = CircuitBreakers::new(&config, tx2, pm.clone());
        assert!(cb2.update_daily_loss(-200, 0, UnixNanos::new(1)).is_none());
        assert!(cb2.update_daily_loss(-300, 0, UnixNanos::new(2)).is_some());

        // Consecutive losses: 6 not trip, 7 trips.
        let (tx3, _rx3) = unbounded();
        let mut cb3 = CircuitBreakers::new(&config, tx3, pm.clone());
        for i in 0..6 {
            assert!(
                cb3.record_trade_result(false, UnixNanos::new(i as u64))
                    .is_none()
            );
        }
        assert!(cb3.record_trade_result(false, UnixNanos::new(99)).is_some());

        // Max trades: 100 OK, 101 trips.
        let (tx4, _rx4) = unbounded();
        let mut cb4 = CircuitBreakers::new(&config, tx4, pm);
        for i in 0..100u32 {
            assert!(cb4.record_trade(UnixNanos::new(i as u64)).is_none());
        }
        assert!(cb4.record_trade(UnixNanos::new(200)).is_some());
    }

    /// Task 5 integration: simulate the full event-loop call sequence
    /// (decision → permits_trading → submit → fill → record outcome →
    /// position update). The test verifies that the breaker hooks are
    /// composable in the expected order without surprising state
    /// interactions.
    #[test]
    fn event_loop_call_sequence_integration() {
        let (mut cb, _rx) = fixture();
        let mut now = 0u64;
        let mut tick = || -> UnixNanos {
            now += 1;
            UnixNanos::new(now)
        };

        // Trade #1: pre-flight checks pass.
        assert!(cb.permits_trading().is_ok());
        // Submit + fill, position +1.
        cb.record_trade(tick());
        cb.check_position_size(1, tick());
        // Mark to market, P&L still healthy.
        cb.update_daily_loss(-50, 10, tick());
        // Trade resolves as a winner.
        cb.record_trade_result(true, tick());

        // Trade #2: same flow, position +2 reaches the gate.
        assert!(cb.permits_trading().is_ok());
        cb.record_trade(tick());
        let trip = cb.check_position_size(2, tick());
        assert!(trip.is_some(), "position-size gate must trip at limit");

        // Now `permits_trading()` MUST deny.
        let denied = cb.permits_trading().unwrap_err();
        assert!(denied.contains(BreakerType::MaxPositionSize));

        // After flatten back to 0, the gate auto-clears.
        let clear = cb.check_position_size(0, tick());
        assert!(clear.is_some());
        assert!(cb.permits_trading().is_ok());
    }

    /// Re-tripping the position-size gate while already tripped is a no-op
    /// (no duplicate journal events).
    #[test]
    fn position_size_re_trip_is_idempotent() {
        let (mut cb, rx) = fixture();
        cb.check_position_size(2, UnixNanos::new(1)).unwrap();
        let _first = rx.try_recv().unwrap();

        // Still over the limit on the next call — no duplicate event.
        let evt = cb.check_position_size(3, UnixNanos::new(2));
        assert!(evt.is_none(), "duplicate trip must not emit a new event");
        assert!(rx.try_recv().is_err());
    }

    /// `update_daily_loss` saturating-add does not overflow on extreme
    /// inputs (defence against arithmetic accidents from upstream code).
    #[test]
    fn update_daily_loss_saturates() {
        let (mut cb, _rx) = fixture();
        // i64::MIN + i64::MIN would overflow naive addition; saturating
        // arithmetic clamps to i64::MIN which is well past any realistic cap.
        cb.update_daily_loss(i64::MIN, i64::MIN, UnixNanos::new(1));
        assert_eq!(cb.state(BreakerType::DailyLoss), BreakerState::Tripped);
    }

    /// Edge: connection-failure breaker is manual-reset; subsequent
    /// `Connected` transitions don't clear it.
    #[test]
    fn connection_failure_breaker_requires_manual_reset() {
        let (mut cb, _rx) = fixture();
        cb.on_connection_state_change(ConnectionState::CircuitBreak, UnixNanos::new(1));
        assert_eq!(
            cb.state(BreakerType::ConnectionFailure),
            BreakerState::Tripped
        );

        cb.on_connection_state_change(ConnectionState::Connected, UnixNanos::new(2));
        assert_eq!(
            cb.state(BreakerType::ConnectionFailure),
            BreakerState::Tripped
        );

        cb.reset_breaker(BreakerType::ConnectionFailure);
        assert_eq!(
            cb.state(BreakerType::ConnectionFailure),
            BreakerState::Active
        );
    }

    /// Edge: zero-capacity buffer update is a no-op (defensive guard).
    #[test]
    fn buffer_occupancy_zero_capacity_is_noop() {
        let (mut cb, _rx) = fixture();
        cb.update_buffer_occupancy(0, 0, UnixNanos::new(1));
        // No state change, no panic.
        assert_eq!(cb.state(BreakerType::BufferOverflow), BreakerState::Active);
        assert_eq!(cb.state(BreakerType::DataQuality), BreakerState::Active);
    }

    /// Edge: hot-path discipline — `permits_trade_evaluation` short-circuits
    /// on the happy path just like `permits_trading`.
    #[test]
    fn permits_trade_evaluation_fast_on_happy_path() {
        let (cb, _rx) = fixture();
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            let _ = cb.permits_trade_evaluation();
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "permits_trade_evaluation too slow: {elapsed:?}"
        );
    }

    // ===================================================================
    // D-1 — panic-mode integration tests (pre-Epic-6 cleanup spec).
    // ===================================================================

    /// D-1: panic mode active → `permits_trading` denies even when every
    /// breaker is `Active`. `reasons[0]` is `PanicModeActive`.
    #[test]
    fn permits_trading_denies_when_panic_active_with_panic_first() {
        let (cb, _rx, panic_mode) = fixture_with_panic();
        let mut cancel = NoopCancel;
        panic_mode.activate("test", UnixNanos::new(1), &mut cancel);

        let denied = cb
            .permits_trading()
            .expect_err("panic active must deny trading");
        assert!(
            matches!(denied.reasons.first(), Some(DenialReason::PanicModeActive)),
            "PanicModeActive must be FIRST in reasons; got {:?}",
            denied.reasons
        );
        assert!(denied.contains_panic_mode());
    }

    /// D-1: panic mode active AND a breaker tripped → `PanicModeActive`
    /// still leads, the breaker reason follows.
    #[test]
    fn permits_trading_panic_mode_precedes_breaker_reason() {
        let (mut cb, _rx, panic_mode) = fixture_with_panic();
        cb.trip_breaker(BreakerType::DailyLoss, "loss".into(), UnixNanos::new(1));
        let mut cancel = NoopCancel;
        panic_mode.activate("test", UnixNanos::new(2), &mut cancel);

        let denied = cb.permits_trading().unwrap_err();
        assert!(matches!(denied.reasons[0], DenialReason::PanicModeActive));
        assert!(denied.contains(BreakerType::DailyLoss));
    }

    /// D-1: panic mode inactive → `permits_trading` is `Ok` when every
    /// breaker is `Active` (regression: panic-mode integration must not
    /// break the happy path).
    #[test]
    fn permits_trading_ok_when_panic_inactive_and_breakers_active() {
        let (cb, _rx, _panic_mode) = fixture_with_panic();
        assert!(cb.permits_trading().is_ok());
    }

    /// D-1: `permits_trade_evaluation` follows the same panic-mode-first
    /// ordering rule as `permits_trading`.
    #[test]
    fn permits_trade_evaluation_panic_mode_first() {
        let (cb, _rx, panic_mode) = fixture_with_panic();
        let mut cancel = NoopCancel;
        panic_mode.activate("test", UnixNanos::new(1), &mut cancel);

        let denied = cb.permits_trade_evaluation().unwrap_err();
        assert!(matches!(denied.reasons[0], DenialReason::PanicModeActive));
    }

    /// D-1: `panic_mode()` accessor returns the same `Arc` we constructed
    /// with — confirms shared ownership for Story 8.2's consumer task.
    #[test]
    fn panic_mode_accessor_returns_shared_arc() {
        let (cb, _rx, panic_mode) = fixture_with_panic();
        assert!(Arc::ptr_eq(cb.panic_mode(), &panic_mode));
    }
}
