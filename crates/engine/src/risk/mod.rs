//! Risk module — the circuit-breaker / gate framework that gates every
//! order submission.
//!
//! Story 5.1 introduces the unified [`CircuitBreakers`] struct (see
//! [`circuit_breakers`]). Stories 5.2–5.6 layer the per-breaker logic on
//! top, but the API surface is fixed here and must not be reshaped without
//! coordinated downstream updates.
//!
//! Story 5.6 adds the operator-alerting plumbing in [`alerting`]: a
//! dedicated JSON-lines alert log, a bounded crossbeam channel, and an
//! optional fire-and-forget external script invocation. Producers are
//! [`circuit_breakers::CircuitBreakers`] (on `trip_breaker`) and
//! [`panic_mode::PanicMode`] (on `activate`); gate transitions explicitly
//! do NOT route through alerting (they remain routine structured-log
//! events).
//!
//! The hard rules from the architecture spec:
//!
//!   * `unsafe` is forbidden in this module (see workspace `clippy.toml`
//!     and the per-file `#![deny(unsafe_code)]` declarations below).
//!   * `permits_trading()` is on the hot path: no heap allocation on the
//!     happy path (every breaker `Active` ⇒ `Ok(())` returned without
//!     allocating the failure-side `Vec`).
//!   * Stop-loss orders are NEVER cancelled by a circuit-breaker trip.
//!     Only entry market orders and resting limit orders are. The
//!     `orders_to_cancel()` helper enforces this.

#![deny(unsafe_code)]

pub mod alerting;
pub mod anomaly_handler;
pub mod circuit_breakers;
pub mod event_windows;
pub mod fee_gate;
pub mod panic_mode;

use std::fmt;

use futures_bmad_core::BreakerType;

pub use alerting::{
    ALERT_CHANNEL_CAPACITY, Alert, AlertManager, AlertReceiver, AlertSender, AlertSeverity,
    BreakerKind, DEFAULT_SCRIPT_TIMEOUT, FlattenAttemptDetail, PositionSnapshot, SharedAlertSender,
    alert_channel,
};
pub use anomaly_handler::{AnomalyResolution, handle_anomaly};
// Re-export the broker-side `FlattenRequest` so producers (Story 5.3
// pre-Epic-6 cleanup, in `EventLoop`) and consumers (Story 8.2) share a
// single import path. The struct itself remains owned by
// `futures_bmad_broker::position_flatten` — re-exporting keeps the seam
// stable across crate boundaries.
pub use circuit_breakers::{AnomalyCheckOutcome, CircuitBreakers, ConnectionState};
pub use event_windows::{ActiveEvent, EventWindowManager, TradingRestriction};
pub use fee_gate::{FeeGate, FeeGateReason};
pub use futures_bmad_broker::FlattenRequest;
pub use panic_mode::{ActivationOutcome, OrderCancellation, PanicContext, PanicMode, PanicState};

/// One specific reason `permits_trading()` denied a trade.
///
/// Each variant maps 1:1 to a [`BreakerType`]. The variant carries the
/// minimum context an operator needs to make sense of the denial in a
/// log line or alert: current vs limit values where applicable, or a
/// terse contextual string where structured fields are not natural.
///
/// Kept `Clone` (rather than `Copy`) because some variants carry small
/// `String` context fields. The hot path never constructs a `DenialReason`
/// — the happy-path `permits_trading()` short-circuits to `Ok(())`.
#[derive(Debug, Clone, PartialEq)]
pub enum DenialReason {
    /// Panic mode is active — trading is disabled until process restart. This
    /// variant has NO 1:1 [`BreakerType`] mapping (panic mode lives outside
    /// the breaker table) and is always reported FIRST in
    /// [`TradingDenied::reasons`] when present so operator alerts attribute
    /// the denial to panic mode rather than secondary breaker triggers panic
    /// mode's flatten path likely caused.
    PanicModeActive,
    /// Daily-loss breaker tripped — session P&L hit the configured cap.
    DailyLossExceeded { current: i64, limit: i64 },
    /// Consecutive-losses breaker tripped.
    ConsecutiveLossesExceeded { current: u32, limit: u32 },
    /// Max-trades-per-day breaker tripped.
    MaxTradesExceeded { current: u32, limit: u32 },
    /// Anomalous-position breaker tripped (panic-mode tier).
    AnomalousPosition { detail: String },
    /// Connection failure breaker tripped.
    ConnectionFailure { detail: String },
    /// Buffer-overflow breaker tripped (queue saturation).
    BufferOverflow { occupancy_pct: f32 },
    /// Max-position-size gate tripped (would exceed cap on next fill).
    MaxPositionSizeExceeded { requested: u32, limit: u32 },
    /// Data-quality gate tripped.
    DataQualityFailure { detail: String },
    /// Fee-staleness gate tripped (>60 day fee schedule).
    FeeScheduleStale { days_old: i64 },
    /// Malformed-message rate breaker tripped.
    MalformedMessages { detail: String },
}

impl DenialReason {
    /// The breaker/gate type this denial corresponds to, if any.
    ///
    /// [`DenialReason::PanicModeActive`] returns `None` — panic mode is
    /// orthogonal to the breaker table (it lives in
    /// [`crate::risk::panic_mode`]).
    pub const fn breaker_type(&self) -> Option<BreakerType> {
        match self {
            DenialReason::PanicModeActive => None,
            DenialReason::DailyLossExceeded { .. } => Some(BreakerType::DailyLoss),
            DenialReason::ConsecutiveLossesExceeded { .. } => Some(BreakerType::ConsecutiveLosses),
            DenialReason::MaxTradesExceeded { .. } => Some(BreakerType::MaxTrades),
            DenialReason::AnomalousPosition { .. } => Some(BreakerType::AnomalousPosition),
            DenialReason::ConnectionFailure { .. } => Some(BreakerType::ConnectionFailure),
            DenialReason::BufferOverflow { .. } => Some(BreakerType::BufferOverflow),
            DenialReason::MaxPositionSizeExceeded { .. } => Some(BreakerType::MaxPositionSize),
            DenialReason::DataQualityFailure { .. } => Some(BreakerType::DataQuality),
            DenialReason::FeeScheduleStale { .. } => Some(BreakerType::FeeStaleness),
            DenialReason::MalformedMessages { .. } => Some(BreakerType::MalformedMessages),
        }
    }
}

impl fmt::Display for DenialReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DenialReason::PanicModeActive => {
                write!(f, "panic mode active — manual restart required")
            }
            DenialReason::DailyLossExceeded { current, limit } => {
                write!(f, "daily loss {current} ticks reached limit {limit}")
            }
            DenialReason::ConsecutiveLossesExceeded { current, limit } => {
                write!(f, "{current} consecutive losses reached limit {limit}")
            }
            DenialReason::MaxTradesExceeded { current, limit } => {
                write!(f, "{current} trades reached daily limit {limit}")
            }
            DenialReason::AnomalousPosition { detail } => {
                write!(f, "anomalous position: {detail}")
            }
            DenialReason::ConnectionFailure { detail } => {
                write!(f, "connection failure: {detail}")
            }
            DenialReason::BufferOverflow { occupancy_pct } => {
                write!(f, "buffer overflow at {occupancy_pct:.1}% occupancy")
            }
            DenialReason::MaxPositionSizeExceeded { requested, limit } => {
                write!(f, "requested position {requested} exceeds max {limit}")
            }
            DenialReason::DataQualityFailure { detail } => {
                write!(f, "data quality: {detail}")
            }
            DenialReason::FeeScheduleStale { days_old } => {
                write!(f, "fee schedule {days_old} days stale")
            }
            DenialReason::MalformedMessages { detail } => {
                write!(f, "malformed messages: {detail}")
            }
        }
    }
}

/// Aggregated reason `permits_trading()` returned `Err`.
///
/// Carries every active denial reason in a single struct so the caller
/// gets the complete picture in one place instead of having to retry the
/// check after fixing the first failure. Construction is pushed off the
/// hot path: only built when at least one breaker / gate is non-`Active`.
#[derive(Debug, Clone, PartialEq)]
pub struct TradingDenied {
    pub reasons: Vec<DenialReason>,
}

impl TradingDenied {
    /// Construct from an iterator of denial reasons. Empty iterators are
    /// permitted but indicate a bug at the caller — they should produce
    /// `Ok(())` instead. We do not panic here because that would put the
    /// hot path into a worse state.
    pub fn from_reasons(reasons: impl IntoIterator<Item = DenialReason>) -> Self {
        Self {
            reasons: reasons.into_iter().collect(),
        }
    }

    /// Whether a denial of the given type is present in this set.
    pub fn contains(&self, ty: BreakerType) -> bool {
        self.reasons.iter().any(|r| r.breaker_type() == Some(ty))
    }

    /// Whether the panic-mode denial is present in this set. Convenience
    /// helper because [`DenialReason::PanicModeActive`] has no
    /// [`BreakerType`] (so it can't be queried via [`Self::contains`]).
    pub fn contains_panic_mode(&self) -> bool {
        self.reasons
            .iter()
            .any(|r| matches!(r, DenialReason::PanicModeActive))
    }
}

impl fmt::Display for TradingDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "trading denied:")?;
        for (i, reason) in self.reasons.iter().enumerate() {
            if i == 0 {
                write!(f, " {reason}")?;
            } else {
                write!(f, "; {reason}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for TradingDenied {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denial_reason_breaker_type_mapping() {
        let pairs = [
            (
                DenialReason::DailyLossExceeded {
                    current: 1,
                    limit: 1,
                },
                BreakerType::DailyLoss,
            ),
            (
                DenialReason::ConsecutiveLossesExceeded {
                    current: 1,
                    limit: 1,
                },
                BreakerType::ConsecutiveLosses,
            ),
            (
                DenialReason::MaxTradesExceeded {
                    current: 1,
                    limit: 1,
                },
                BreakerType::MaxTrades,
            ),
            (
                DenialReason::AnomalousPosition { detail: "x".into() },
                BreakerType::AnomalousPosition,
            ),
            (
                DenialReason::ConnectionFailure { detail: "x".into() },
                BreakerType::ConnectionFailure,
            ),
            (
                DenialReason::BufferOverflow { occupancy_pct: 1.0 },
                BreakerType::BufferOverflow,
            ),
            (
                DenialReason::MaxPositionSizeExceeded {
                    requested: 1,
                    limit: 1,
                },
                BreakerType::MaxPositionSize,
            ),
            (
                DenialReason::DataQualityFailure { detail: "x".into() },
                BreakerType::DataQuality,
            ),
            (
                DenialReason::FeeScheduleStale { days_old: 100 },
                BreakerType::FeeStaleness,
            ),
            (
                DenialReason::MalformedMessages { detail: "x".into() },
                BreakerType::MalformedMessages,
            ),
        ];
        for (reason, expected_type) in pairs {
            assert_eq!(reason.breaker_type(), Some(expected_type));
        }
    }

    #[test]
    fn panic_mode_active_has_no_breaker_type() {
        assert_eq!(DenialReason::PanicModeActive.breaker_type(), None);
    }

    #[test]
    fn panic_mode_active_display_is_descriptive() {
        let s = format!("{}", DenialReason::PanicModeActive);
        assert!(s.contains("panic mode"));
    }

    #[test]
    fn trading_denied_contains_panic_mode_only_via_helper() {
        let denied = TradingDenied::from_reasons([DenialReason::PanicModeActive]);
        assert!(denied.contains_panic_mode());
        // Default `contains` is keyed on BreakerType — panic mode has no
        // mapping, so every BreakerType query returns false.
        for bt in [
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
        ] {
            assert!(!denied.contains(bt));
        }
    }

    #[test]
    fn trading_denied_display_lists_all_reasons() {
        let denied = TradingDenied::from_reasons([
            DenialReason::DailyLossExceeded {
                current: 1200,
                limit: 1000,
            },
            DenialReason::MaxTradesExceeded {
                current: 31,
                limit: 30,
            },
        ]);
        let s = format!("{denied}");
        assert!(s.contains("daily loss 1200"));
        assert!(s.contains("31 trades"));
        // Multi-reason rendering: separated by `;`
        assert!(s.contains(';'));
    }

    #[test]
    fn trading_denied_contains_finds_type() {
        let denied =
            TradingDenied::from_reasons([DenialReason::FeeScheduleStale { days_old: 100 }]);
        assert!(denied.contains(BreakerType::FeeStaleness));
        assert!(!denied.contains(BreakerType::DailyLoss));
    }
}
