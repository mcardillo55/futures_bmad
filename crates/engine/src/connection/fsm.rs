//! Reconnection FSM that drives the connection-failure circuit breaker.
//!
//! The FSM tracks the lifecycle states defined in
//! [`crate::risk::ConnectionState`] and notifies the breaker framework on
//! every transition. Story 5.4's contract: when the FSM enters
//! `CircuitBreak`, [`crate::risk::CircuitBreakers::on_connection_state_change`]
//! must be invoked so the connection-failure breaker (manual reset) trips.
//!
//! The FSM never blocks the I/O thread; transitions are pure state
//! changes. Higher-level reconnect logic (timeouts, back-off) lives in
//! the broker adapter and feeds events here.

#![deny(unsafe_code)]

use futures_bmad_core::UnixNanos;

use crate::risk::{CircuitBreakers, ConnectionState};

/// Errors a transition request may produce. Today the FSM accepts any
/// transition, but the type is here so future stories can layer
/// invariants without breaking callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionError {
    /// Reserved for future use (e.g. illegal transition such as
    /// `Reconnecting -> Reconciling` without an intervening `Connected`).
    /// Kept private-marker today by virtue of never being constructed.
    #[allow(dead_code)]
    Illegal,
}

/// Tracks the live connection state and forwards transitions to the
/// circuit-breaker framework. Owned by the broker adapter (one per
/// session).
pub struct ConnectionFsm {
    state: ConnectionState,
}

impl Default for ConnectionFsm {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionFsm {
    pub fn new() -> Self {
        Self {
            state: ConnectionState::Connected,
        }
    }

    /// Current FSM state.
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    /// Transition to a new state and notify the circuit-breaker framework.
    ///
    /// `breakers.on_connection_state_change` is invoked for every
    /// transition; its job is to decide whether to trip the breaker (only
    /// `CircuitBreak` does today) or not.
    pub fn transition(
        &mut self,
        new_state: ConnectionState,
        breakers: &mut CircuitBreakers,
        timestamp: UnixNanos,
    ) {
        self.state = new_state;
        breakers.on_connection_state_change(new_state, timestamp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use futures_bmad_core::{
        BreakerState, BreakerType, CircuitBreakerEvent, FixedPrice, TradingConfig,
    };

    fn config() -> TradingConfig {
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

    fn fixture() -> (
        ConnectionFsm,
        CircuitBreakers,
        crossbeam_channel::Receiver<CircuitBreakerEvent>,
    ) {
        use std::sync::Arc;
        use crate::persistence::journal::EventJournal;
        use crate::risk::panic_mode::PanicMode;

        let (tx, rx) = unbounded();
        let (panic_tx, _panic_rx) = EventJournal::channel();
        let panic_mode = Arc::new(PanicMode::new(panic_tx));
        (
            ConnectionFsm::new(),
            CircuitBreakers::new(&config(), tx, panic_mode),
            rx,
        )
    }

    #[test]
    fn fsm_starts_connected() {
        let fsm = ConnectionFsm::new();
        assert_eq!(fsm.state(), ConnectionState::Connected);
    }

    #[test]
    fn benign_transitions_do_not_trip_breaker() {
        let (mut fsm, mut cb, rx) = fixture();
        fsm.transition(
            ConnectionState::Reconnecting,
            &mut cb,
            UnixNanos::new(1),
        );
        fsm.transition(
            ConnectionState::Reconciling,
            &mut cb,
            UnixNanos::new(2),
        );
        fsm.transition(ConnectionState::Connected, &mut cb, UnixNanos::new(3));

        assert_eq!(
            cb.state(BreakerType::ConnectionFailure),
            BreakerState::Active
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn circuit_break_transition_trips_breaker() {
        let (mut fsm, mut cb, rx) = fixture();
        fsm.transition(
            ConnectionState::CircuitBreak,
            &mut cb,
            UnixNanos::new(42),
        );

        assert_eq!(
            cb.state(BreakerType::ConnectionFailure),
            BreakerState::Tripped
        );
        let event = rx.try_recv().expect("breaker event must be emitted");
        assert_eq!(event.breaker_type, BreakerType::ConnectionFailure);
        assert_eq!(event.timestamp, UnixNanos::new(42));
    }
}
