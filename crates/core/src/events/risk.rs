//! Circuit-breaker event types — the cross-crate vocabulary for the risk
//! framework introduced by story 5.1.
//!
//! `BreakerType` enumerates every gate and breaker the engine knows about
//! (10 total — 4 gates that auto-clear and 6 breakers that require manual
//! reset). Each variant is classified via [`BreakerType::category`] so the
//! risk framework can decide whether a re-evaluated condition should clear
//! the gate (`Gate`) or whether only an explicit operator restart can clear
//! it (`Breaker`).
//!
//! `BreakerState` is the per-type lifecycle: `Active` is the healthy state
//! (no denial), `Tripped` is the post-activation state, `Cleared` is the
//! transient post-clearance state used for the audit trail (the field is
//! returned to `Active` immediately after the clearance event is emitted).
//!
//! `CircuitBreakerEvent` is the audit-trail record sent through the journal
//! channel each time a breaker trips or a gate clears.

use crate::types::UnixNanos;

/// Whether a breaker auto-clears when its underlying condition resolves
/// (`Gate`) or whether it persists until the operator restarts the process
/// (`Breaker`). The architecture spec is explicit: gates are recoverable
/// (e.g. a transient data-quality glitch), breakers are terminal for the
/// session (e.g. a daily-loss cap was hit — coming back online without a
/// human in the loop would invite repeating the loss).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BreakerCategory {
    /// Auto-clears when the underlying condition resolves.
    Gate,
    /// Requires a manual reset (process restart in V1).
    Breaker,
}

/// All circuit-breaker / gate types the risk framework tracks.
///
/// Variants are classified via [`BreakerType::category`]:
///   - Breakers (manual reset): `DailyLoss`, `ConsecutiveLosses`, `MaxTrades`,
///     `AnomalousPosition`, `ConnectionFailure`, `BufferOverflow`,
///     `MalformedMessages`.
///   - Gates (auto-clear): `MaxPositionSize`, `DataQuality`, `FeeStaleness`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BreakerType {
    DailyLoss,
    ConsecutiveLosses,
    MaxTrades,
    AnomalousPosition,
    ConnectionFailure,
    BufferOverflow,
    MaxPositionSize,
    DataQuality,
    FeeStaleness,
    MalformedMessages,
}

impl BreakerType {
    /// Classify this breaker/gate type.
    ///
    /// `const` so the categorisation is a pure compile-time table —
    /// `permits_trading()` and gate-clearance logic can branch on this
    /// without a runtime allocation or virtual dispatch.
    pub const fn category(&self) -> BreakerCategory {
        match self {
            BreakerType::DailyLoss
            | BreakerType::ConsecutiveLosses
            | BreakerType::MaxTrades
            | BreakerType::AnomalousPosition
            | BreakerType::ConnectionFailure
            | BreakerType::BufferOverflow
            | BreakerType::MalformedMessages => BreakerCategory::Breaker,
            BreakerType::MaxPositionSize | BreakerType::DataQuality | BreakerType::FeeStaleness => {
                BreakerCategory::Gate
            }
        }
    }

    /// Stable string identifier used for journal records and structured logs.
    pub const fn as_str(&self) -> &'static str {
        match self {
            BreakerType::DailyLoss => "daily_loss",
            BreakerType::ConsecutiveLosses => "consecutive_losses",
            BreakerType::MaxTrades => "max_trades",
            BreakerType::AnomalousPosition => "anomalous_position",
            BreakerType::ConnectionFailure => "connection_failure",
            BreakerType::BufferOverflow => "buffer_overflow",
            BreakerType::MaxPositionSize => "max_position_size",
            BreakerType::DataQuality => "data_quality",
            BreakerType::FeeStaleness => "fee_staleness",
            BreakerType::MalformedMessages => "malformed_messages",
        }
    }
}

/// Per-type lifecycle state of a breaker or gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BreakerState {
    /// Healthy state — no denial.
    Active,
    /// Tripped: trading is denied for this breaker/gate.
    Tripped,
    /// Transient post-clearance marker used in audit-trail events. The
    /// runtime field is reset to `Active` immediately after the clearance
    /// event is emitted, so the in-memory `BreakerState` field never holds
    /// `Cleared` between calls; it is carried only on the emitted event.
    Cleared,
}

/// Audit-trail event emitted on every trip or gate-clear.
///
/// `previous_state -> new_state` lets the journal record reconstruct the
/// transition without the consumer needing to track prior state. The
/// `trigger_reason` is a free-form human-readable string captured at the
/// trip site (e.g. `"daily loss 1200 ticks exceeds 1000 limit"`) so the
/// audit trail does not require interpreting a machine code.
#[derive(Debug, Clone)]
pub struct CircuitBreakerEvent {
    pub breaker_type: BreakerType,
    pub trigger_reason: String,
    pub timestamp: UnixNanos,
    pub previous_state: BreakerState,
    pub new_state: BreakerState,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Task 1.5 — every variant has a stable category, with the split exactly
    /// matching the architecture spec (3 gates / 7 breakers).
    #[test]
    fn breaker_type_categorisation() {
        let breakers = [
            BreakerType::DailyLoss,
            BreakerType::ConsecutiveLosses,
            BreakerType::MaxTrades,
            BreakerType::AnomalousPosition,
            BreakerType::ConnectionFailure,
            BreakerType::BufferOverflow,
            BreakerType::MalformedMessages,
        ];
        let gates = [
            BreakerType::MaxPositionSize,
            BreakerType::DataQuality,
            BreakerType::FeeStaleness,
        ];
        for b in breakers {
            assert_eq!(b.category(), BreakerCategory::Breaker, "{b:?}");
        }
        for g in gates {
            assert_eq!(g.category(), BreakerCategory::Gate, "{g:?}");
        }
        assert_eq!(breakers.len() + gates.len(), 10);
    }

    /// `as_str()` round-trip — every type has a unique stable identifier.
    #[test]
    fn breaker_type_as_str_unique() {
        let all = [
            BreakerType::DailyLoss,
            BreakerType::ConsecutiveLosses,
            BreakerType::MaxTrades,
            BreakerType::AnomalousPosition,
            BreakerType::ConnectionFailure,
            BreakerType::BufferOverflow,
            BreakerType::MaxPositionSize,
            BreakerType::DataQuality,
            BreakerType::FeeStaleness,
            BreakerType::MalformedMessages,
        ];
        let mut seen = std::collections::HashSet::new();
        for t in all {
            assert!(seen.insert(t.as_str()), "duplicate identifier for {t:?}");
        }
    }
}
