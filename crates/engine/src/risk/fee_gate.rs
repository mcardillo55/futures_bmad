use chrono::NaiveDate;
use futures_bmad_core::{Clock, FeeConfig, FixedPrice};
use tracing::{error, warn};

/// Reason why the fee gate blocked or permitted a trade.
#[derive(Debug, Clone, PartialEq)]
pub enum FeeGateReason {
    Permitted,
    EdgeBelowThreshold {
        edge: FixedPrice,
        threshold: FixedPrice,
    },
    StaleSchedule {
        days: u64,
    },
}

/// Fee-aware trade gating.
///
/// Computes total round-trip cost and blocks trades unless expected edge
/// exceeds a configurable multiple of total cost. Staleness gate blocks
/// all trade evaluations when fee schedule is >60 days old, warns at >30 days.
///
/// CRITICAL: Staleness gate never blocks flattening or safety orders.
/// Callers must use `permits_flatten()` for safety orders.
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
        let exchange = FixedPrice::from_f64(config.exchange_fee + config.clearing_fee + config.nfa_fee)
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
        per_side.saturating_mul(2).saturating_add(self.slippage_model)
    }

    /// Check if a trade with the given expected edge is permitted.
    ///
    /// Returns `Ok(true)` if permitted, `Ok(false)` if edge below threshold,
    /// or `Err(FeeGateReason::StaleSchedule)` if fee schedule is >60 days stale.
    pub fn permits_trade(
        &self,
        expected_edge: FixedPrice,
        clock: &dyn Clock,
    ) -> Result<bool, FeeGateReason> {
        // Check staleness
        let today = clock.wall_clock().date_naive();
        let days_since = (today - self.fee_schedule_date).num_days();

        if days_since > 60 {
            error!(
                days = days_since,
                "Fee schedule >60 days stale, blocking trades"
            );
            return Err(FeeGateReason::StaleSchedule {
                days: days_since as u64,
            });
        }

        if days_since > 30 {
            warn!(days = days_since, "Fee schedule >30 days stale");
        }

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

    /// Flattening and safety orders are NEVER gated by staleness or edge threshold.
    pub fn permits_flatten(&self) -> bool {
        true
    }
}
