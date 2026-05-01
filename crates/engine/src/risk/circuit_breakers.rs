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

use super::alerting::{Alert, AlertSender, PositionSnapshot};
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

    /// Operator-alert sender (Story 5.6). When `Some`, every `trip_breaker`
    /// for a `BreakerCategory::Breaker` type also emits an [`Alert`] with
    /// severity `Error`. Gates explicitly do NOT route through this path —
    /// gate transitions remain routine structured-log events.
    alerts: Option<AlertSender>,
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
            alerts: None,
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
            let snapshot = position.clone().unwrap_or_else(PositionSnapshot::flat_unknown);
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

    // ----- Story 5.6: alerting integration -----

    fn alerting_fixture() -> (
        CircuitBreakers,
        Receiver<CircuitBreakerEvent>,
        super::super::alerting::AlertReceiver,
    ) {
        use super::super::alerting::alert_channel;
        let (tx, rx) = unbounded();
        let (alerts_tx, alerts_rx) = alert_channel();
        let cb = CircuitBreakers::new(&test_config(), tx).with_alerts(alerts_tx);
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
        cb.trip_breaker(
            BreakerType::DailyLoss,
            "loss".into(),
            UnixNanos::new(1),
        );
        // Compiles and runs without panicking.
    }

    /// Idempotent re-trip does not re-emit an alert.
    #[test]
    fn idempotent_trip_does_not_double_alert() {
        let (mut cb, _journal, alerts_rx) = alerting_fixture();
        cb.trip_breaker(
            BreakerType::DailyLoss,
            "first".into(),
            UnixNanos::new(1),
        );
        cb.trip_breaker(
            BreakerType::DailyLoss,
            "second".into(),
            UnixNanos::new(2),
        );
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
        let mut cb = CircuitBreakers::new(&config, tx);

        // Position size: 4 OK, 5 trips.
        assert!(cb.check_position_size(4, UnixNanos::new(1)).is_none());
        assert!(cb.check_position_size(5, UnixNanos::new(2)).is_some());

        // Daily loss: -200 OK, -300 trips against the 250 cap.
        let (tx2, _rx2) = unbounded();
        let mut cb2 = CircuitBreakers::new(&config, tx2);
        assert!(cb2.update_daily_loss(-200, 0, UnixNanos::new(1)).is_none());
        assert!(cb2.update_daily_loss(-300, 0, UnixNanos::new(2)).is_some());

        // Consecutive losses: 6 not trip, 7 trips.
        let (tx3, _rx3) = unbounded();
        let mut cb3 = CircuitBreakers::new(&config, tx3);
        for i in 0..6 {
            assert!(
                cb3.record_trade_result(false, UnixNanos::new(i as u64))
                    .is_none()
            );
        }
        assert!(cb3.record_trade_result(false, UnixNanos::new(99)).is_some());

        // Max trades: 100 OK, 101 trips.
        let (tx4, _rx4) = unbounded();
        let mut cb4 = CircuitBreakers::new(&config, tx4);
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
}
