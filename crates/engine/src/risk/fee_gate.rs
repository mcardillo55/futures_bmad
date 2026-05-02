use chrono::NaiveDate;
use futures_bmad_core::{FeeConfig, FixedPrice};

/// Reason why the fee gate blocked or permitted a trade.
///
/// Pre-Epic-6 cleanup D-3: the `StaleSchedule` variant has been removed.
/// Fee-staleness is now owned exclusively by
/// [`crate::risk::CircuitBreakers::check_fee_staleness`] / the
/// `FeeStaleness` gate. `FeeGate` is pure economics (edge vs fee).
#[derive(Debug, Clone, PartialEq)]
pub enum FeeGateReason {
    Permitted,
    EdgeBelowThreshold {
        edge: FixedPrice,
        threshold: FixedPrice,
    },
}

/// Fee-aware trade gating.
///
/// Pure economics: blocks trades when expected edge does not exceed a
/// configurable multiple of total round-trip cost. Pre-Epic-6 cleanup D-3
/// removed the `>60 days` staleness branch from this gate; staleness is
/// now owned by [`crate::risk::CircuitBreakers::check_fee_staleness`] —
/// both the gate (`BreakerType::FeeStaleness`) and the optional
/// `tracing::warn!` at the >30-day threshold live there. This gate keeps
/// the `fee_schedule_date` field for cost computation and FYI only.
///
/// CRITICAL: Flattening / safety orders must always use
/// [`Self::permits_flatten`].
pub struct FeeGate {
    pub exchange_fee_per_side: FixedPrice,
    pub commission_per_side: FixedPrice,
    pub api_fee_per_side: FixedPrice,
    pub slippage_model: FixedPrice,
    pub minimum_edge_multiple: f64,
    pub fee_schedule_date: NaiveDate,
}

impl FeeGate {
    /// Construct from core FeeConfig.
    pub fn from_config(config: &FeeConfig, slippage_qticks: i64, min_edge_multiple: f64) -> Self {
        let exchange =
            FixedPrice::from_f64(config.exchange_fee + config.clearing_fee + config.nfa_fee)
                .unwrap_or(FixedPrice::new(0));
        let commission =
            FixedPrice::from_f64(config.broker_commission).unwrap_or(FixedPrice::new(0));
        let date = NaiveDate::parse_from_str(&config.effective_date, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());

        Self {
            exchange_fee_per_side: exchange,
            commission_per_side: commission,
            api_fee_per_side: FixedPrice::new(0),
            slippage_model: FixedPrice::new(slippage_qticks),
            minimum_edge_multiple: min_edge_multiple,
            fee_schedule_date: date,
        }
    }

    /// Total round-trip cost: `2 * (exchange + commission + api) + slippage`
    pub fn total_round_trip_cost(&self) -> FixedPrice {
        let per_side = self
            .exchange_fee_per_side
            .saturating_add(self.commission_per_side)
            .saturating_add(self.api_fee_per_side);
        per_side
            .saturating_mul(2)
            .saturating_add(self.slippage_model)
    }

    /// Check if a trade with the given expected edge is permitted on
    /// pure economics: returns `Ok(true)` if `expected_edge >
    /// minimum_edge_multiple * total_round_trip_cost`, `Ok(false)`
    /// otherwise.
    ///
    /// Pre-Epic-6 cleanup D-3: this method NO LONGER consults fee
    /// staleness. Staleness lives on
    /// [`crate::risk::CircuitBreakers::check_fee_staleness`] /
    /// `BreakerType::FeeStaleness`. The previously-retained `clock`
    /// parameter has been dropped — there is no economics-time-of-day
    /// branch in this gate today.
    pub fn permits_trade(&self, expected_edge: FixedPrice) -> Result<bool, FeeGateReason> {
        // Compute minimum edge threshold
        let cost = self.total_round_trip_cost();
        let threshold_f64 = self.minimum_edge_multiple * cost.to_f64();
        let threshold = FixedPrice::from_f64(threshold_f64).unwrap_or(FixedPrice::new(0));

        if expected_edge.raw() > threshold.raw() {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Flattening and safety orders are NEVER gated by edge threshold.
    pub fn permits_flatten(&self) -> bool {
        true
    }
}
