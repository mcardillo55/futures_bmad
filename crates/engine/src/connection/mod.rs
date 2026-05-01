//! Reconnection FSM and connection-lifecycle wiring.
//!
//! Story 5.4 introduces the minimal [`fsm::ConnectionFsm`] that owns the
//! [`crate::risk::ConnectionState`] for the broker session and notifies
//! the circuit-breaker framework on transitions. Other transports (broker
//! adapter, market-data plant) feed events into this FSM; the only
//! transition we react to today is the terminal `CircuitBreak` state,
//! which trips the connection-failure breaker (manual reset).
//!
//! Future stories may flesh out richer transitions (back-off schedules,
//! reconciliation phases) — the FSM API here is intentionally minimal so
//! the breaker plumbing is exercised end-to-end without prematurely
//! committing to a reconnection strategy.

pub mod fsm;

pub use fsm::ConnectionFsm;
