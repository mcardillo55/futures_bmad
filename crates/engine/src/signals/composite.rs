use futures_bmad_core::{Clock, FixedPrice, Side, Signal};
use tracing::info;

use super::SignalPipeline;
use crate::risk::fee_gate::{FeeGate, FeeGateReason};

/// Configuration for composite signal weighting.
#[derive(Debug, Clone)]
pub struct CompositeConfig {
    pub obi_weight: f64,
    pub vpin_weight: f64,
    pub microprice_weight: f64,
    /// Historical edge per unit of composite score (in dollars).
    pub historical_edge_per_unit: f64,
}

/// Reason for a trade/no-trade decision.
#[derive(Debug, Clone, PartialEq)]
pub enum DecisionReason {
    Trade,
    NoTradeSignalInvalid(String),
    NoTradeEdgeBelowThreshold,
    NoTradeFeeGateBlocked,
    NoTradeNotAtLevel,
}

/// A complete trade decision with causality tracing.
#[derive(Debug, Clone)]
pub struct TradeDecision {
    pub decision_id: u64,
    pub direction: Side,
    pub expected_edge: FixedPrice,
    pub composite_score: f64,
    pub obi_value: f64,
    pub vpin_value: f64,
    pub microprice_value: f64,
    pub reason: DecisionReason,
}

/// Evaluates composite signal and produces trade decisions.
pub struct CompositeEvaluator {
    config: CompositeConfig,
    decision_counter: u64,
}

impl CompositeEvaluator {
    pub fn new(config: CompositeConfig) -> Self {
        Self {
            config,
            decision_counter: 0,
        }
    }

    /// Evaluate all signals and fee gate to produce a trade decision.
    pub fn evaluate(
        &mut self,
        pipeline: &SignalPipeline,
        fee_gate: &FeeGate,
        clock: &dyn Clock,
    ) -> TradeDecision {
        self.decision_counter += 1;
        let decision_id = self.decision_counter;

        // Default signal values for no-trade decisions
        let obi_val = pipeline.obi.snapshot().value.unwrap_or(0.0);
        let vpin_val = pipeline.vpin.snapshot().value.unwrap_or(0.0);
        let mp_val = pipeline.microprice.snapshot().value.unwrap_or(0.0);

        // Check all signals valid
        if !pipeline.all_valid() {
            let mut invalid = Vec::new();
            if !pipeline.obi.is_valid() {
                invalid.push("obi");
            }
            if !pipeline.vpin.is_valid() {
                invalid.push("vpin");
            }
            if !pipeline.microprice.is_valid() {
                invalid.push("microprice");
            }
            let reason = DecisionReason::NoTradeSignalInvalid(invalid.join(", "));
            info!(
                decision_id,
                reason = ?reason,
                "no-trade decision"
            );
            return TradeDecision {
                decision_id,
                direction: Side::Buy,
                expected_edge: FixedPrice::new(0),
                composite_score: 0.0,
                obi_value: obi_val,
                vpin_value: vpin_val,
                microprice_value: mp_val,
                reason,
            };
        }

        // Compute weighted composite score
        let score = obi_val * self.config.obi_weight
            + vpin_val * self.config.vpin_weight
            + mp_val * self.config.microprice_weight;

        if !score.is_finite() {
            let reason = DecisionReason::NoTradeSignalInvalid("non-finite composite score".into());
            info!(decision_id, reason = ?reason, "no-trade decision");
            return TradeDecision {
                decision_id,
                direction: Side::Buy,
                expected_edge: FixedPrice::new(0),
                composite_score: 0.0,
                obi_value: obi_val,
                vpin_value: vpin_val,
                microprice_value: mp_val,
                reason,
            };
        }

        let direction = if score >= 0.0 {
            Side::Buy
        } else {
            Side::Sell
        };

        // Compute expected edge
        let edge_f64 = score.abs() * self.config.historical_edge_per_unit;
        let expected_edge = FixedPrice::from_f64(edge_f64).unwrap_or(FixedPrice::new(0));

        // Check fee gate
        match fee_gate.permits_trade(expected_edge, clock) {
            Err(FeeGateReason::StaleSchedule { days }) => {
                let reason = DecisionReason::NoTradeFeeGateBlocked;
                info!(
                    decision_id,
                    stale_days = days,
                    reason = ?reason,
                    "no-trade decision"
                );
                return TradeDecision {
                    decision_id,
                    direction,
                    expected_edge,
                    composite_score: score,
                    obi_value: obi_val,
                    vpin_value: vpin_val,
                    microprice_value: mp_val,
                    reason,
                };
            }
            Ok(false) => {
                let reason = DecisionReason::NoTradeEdgeBelowThreshold;
                info!(
                    decision_id,
                    edge = %expected_edge,
                    cost = %fee_gate.total_round_trip_cost(),
                    reason = ?reason,
                    "no-trade decision"
                );
                return TradeDecision {
                    decision_id,
                    direction,
                    expected_edge,
                    composite_score: score,
                    obi_value: obi_val,
                    vpin_value: vpin_val,
                    microprice_value: mp_val,
                    reason,
                };
            }
            Ok(true) => {}
            Err(_) => {} // EdgeBelowThreshold/Permitted — not returned by permits_trade
        }

        // All conditions met — trade
        info!(
            decision_id,
            direction = ?direction,
            edge = %expected_edge,
            score,
            obi = obi_val,
            vpin = vpin_val,
            microprice = mp_val,
            "trade decision"
        );

        TradeDecision {
            decision_id,
            direction,
            expected_edge,
            composite_score: score,
            obi_value: obi_val,
            vpin_value: vpin_val,
            microprice_value: mp_val,
            reason: DecisionReason::Trade,
        }
    }
}
