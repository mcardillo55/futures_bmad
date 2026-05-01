use chrono::NaiveDate;
use serde::Deserialize;

use crate::types::FixedPrice;

/// Trading-side configuration loaded from TOML.
///
/// Story 5.1 introduced the explicit risk-limit fields used by the circuit-
/// breaker framework: `max_daily_loss_ticks` (negative-going integer cap on
/// session P&L), `max_trades_per_day` (count cap), and `fee_schedule_date`
/// (drives the staleness gate). These are kept on the trading config — not
/// on a separate "risk config" — because they are inherently tied to the
/// trading session: the same TOML stanza describes "what we trade" and
/// "what makes us stop trading".
#[derive(Debug, Clone, Deserialize)]
pub struct TradingConfig {
    pub symbol: String,
    pub max_position_size: u32,

    /// Maximum acceptable session-cumulative loss in ticks (positive integer
    /// representing the absolute size of the loss cap; the daily-loss
    /// breaker trips when the realised loss reaches or exceeds this).
    pub max_daily_loss_ticks: i64,

    pub max_consecutive_losses: u32,

    /// Maximum number of round-trip trades permitted per session before the
    /// max-trades breaker trips. `0` is reserved as "no cap" by convention
    /// but config validation rejects it (a real cap is mandatory).
    pub max_trades_per_day: u32,

    pub edge_multiple_threshold: f64,
    pub session_start: String,
    pub session_end: String,
    pub max_spread_threshold: FixedPrice,

    /// Fee schedule effective date — used by the fee-staleness gate. Stored
    /// as a chrono `NaiveDate` so config consumers do not have to re-parse
    /// the string at every gate evaluation.
    pub fee_schedule_date: NaiveDate,
}
