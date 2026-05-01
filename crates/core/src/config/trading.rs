use chrono::{NaiveDate, NaiveDateTime};
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
///
/// Story 5.5 added the `events` array — wall-clock-scheduled trading
/// restrictions for high-impact news releases (FOMC, CPI, NFP). Event
/// windows are a separate restriction layer from circuit breakers; they
/// do not trip breakers, they only restrict what new trades may execute
/// during their active interval.
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

    /// Wall-clock-scheduled event windows (FOMC, CPI, NFP, etc.).
    ///
    /// Loaded from the TOML `[[events]]` array-of-tables. Empty by default
    /// so existing configs without an `events` stanza continue to load
    /// unchanged.
    #[serde(default)]
    pub events: Vec<EventWindowConfig>,
}

/// Action to apply when an event window is active.
///
/// Hierarchy from least-to-most restrictive:
///   1. `DisableStrategies` — signal evaluation is skipped; existing
///      positions remain with their stops.
///   2. `ReduceExposure` — position size cap is reduced to a configured
///      minimum; excess exposure must be flattened.
///   3. `SitOut` — `DisableStrategies` plus flatten any open positions
///      before the window starts (most restrictive).
///
/// Serialised as TOML lower-snake-case strings (`disable_strategies`,
/// `reduce_exposure`, `sit_out`) per the example config in the story spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventAction {
    DisableStrategies,
    ReduceExposure,
    SitOut,
}

impl EventAction {
    /// Numeric severity ranking — higher means more restrictive.
    /// Used to pick the most restrictive action when multiple events overlap.
    pub const fn severity(self) -> u8 {
        match self {
            EventAction::DisableStrategies => 1,
            EventAction::ReduceExposure => 2,
            EventAction::SitOut => 3,
        }
    }
}

/// One event window: a wall-clock interval during which a trading
/// restriction applies.
///
/// Either `end` (explicit end timestamp) or `duration_minutes` (computed
/// end = `start + duration`) must be provided — never both, never neither.
/// Validation is performed by [`validate_event_window_config`].
///
/// TOML example:
/// ```toml
/// [[events]]
/// name = "FOMC"
/// start = "2026-04-16T14:00:00"
/// duration_minutes = 120
/// action = "sit_out"
/// ```
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct EventWindowConfig {
    /// Human-readable identifier used in logs ("FOMC", "CPI Release").
    pub name: String,

    /// Window start as a wall-clock UTC timestamp (TOML's RFC 3339 partial
    /// form is parsed as `NaiveDateTime` and treated as UTC by the
    /// [`EventWindowManager`] — keep TOML strings in UTC to avoid surprises).
    pub start: NaiveDateTime,

    /// Optional explicit end timestamp. Mutually exclusive with
    /// `duration_minutes`. Validation rejects configs that supply both
    /// or neither.
    #[serde(default)]
    pub end: Option<NaiveDateTime>,

    /// Optional duration in minutes (`end = start + duration_minutes`).
    /// Mutually exclusive with `end`.
    #[serde(default)]
    pub duration_minutes: Option<u32>,

    /// Trading restriction to apply while this window is active.
    pub action: EventAction,
}
