//! Broker-mode selector — picks which [`crate::BrokerAdapter`] implementation
//! is wired in at startup.
//!
//! Story 7.3 introduces three logical run modes:
//!
//! | Mode    | Clock         | BrokerAdapter        | Data Source     |
//! |---------|---------------|----------------------|-----------------|
//! | Live    | `SystemClock` | Rithmic `OrderPlant` | Rithmic live    |
//! | Paper   | `SystemClock` | `MockBrokerAdapter`  | Rithmic live    |
//! | Replay  | `SimClock`    | `MockBrokerAdapter`  | Parquet files   |
//!
//! The [`BrokerMode`] enum is the single config knob that switches Live ↔
//! Paper. Replay is selected by the `--replay` CLI flag and uses its own
//! orchestrator (`crates/engine/src/replay/orchestrator.rs`).
//!
//! By design, code paths NEVER branch on `BrokerMode` outside the engine's
//! startup sequence. Once the right adapter is constructed and injected,
//! all downstream code (event loop, signals, risk, regime, order manager,
//! journal) is adapter-agnostic — see Story 7.3 AC.

use serde::Deserialize;

/// Which broker implementation the engine wires in at startup.
///
/// `Live` connects to Rithmic for both market data AND order routing.
/// `Paper` still connects to Rithmic for market data (TickerPlant) but
/// routes orders to an in-process [`futures_bmad_testkit::MockBrokerAdapter`]
/// — no orders ever reach the exchange.
///
/// Serialized as TOML lower-snake-case strings (`live`, `paper`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerMode {
    /// Real Rithmic OrderPlant. Used in production / live trading.
    Live,
    /// Live market data, simulated execution. Used to validate the system in
    /// real market conditions without risking capital (Story 7.3).
    Paper,
}

impl Default for BrokerMode {
    /// Default is [`BrokerMode::Live`]. This is a fail-closed default for
    /// the config loader: an operator who forgets the `mode` key does NOT
    /// silently drop into paper. (Live mode is independently gated by the
    /// `--live` CLI flag, so the default cannot route real orders by
    /// accident.)
    fn default() -> Self {
        BrokerMode::Live
    }
}

impl BrokerMode {
    /// Stable string label used in startup banners and structured-log fields
    /// (`live` / `paper`). Matches the TOML serialisation form so log
    /// readers can grep both with the same token.
    pub const fn as_str(self) -> &'static str {
        match self {
            BrokerMode::Live => "live",
            BrokerMode::Paper => "paper",
        }
    }

    /// `true` when no orders are routed to a real exchange. Equivalent to
    /// `matches!(self, BrokerMode::Paper)` today; kept as a method so future
    /// non-exchange modes (e.g. backtest) can extend the predicate without
    /// touching every call site.
    pub const fn is_simulated(self) -> bool {
        matches!(self, BrokerMode::Paper)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Story 7.3 Task 7.1 — `BrokerMode::Paper` config deserializes
    /// correctly from TOML.
    #[test]
    fn deserializes_paper_from_toml_string() {
        #[derive(Deserialize)]
        struct Wrap {
            mode: BrokerMode,
        }
        let parsed: Wrap = toml::from_str(r#"mode = "paper""#).unwrap();
        assert_eq!(parsed.mode, BrokerMode::Paper);
    }

    #[test]
    fn deserializes_live_from_toml_string() {
        #[derive(Deserialize)]
        struct Wrap {
            mode: BrokerMode,
        }
        let parsed: Wrap = toml::from_str(r#"mode = "live""#).unwrap();
        assert_eq!(parsed.mode, BrokerMode::Live);
    }

    /// Anything other than the two snake_case variants is a deserialization
    /// error — typos must surface loudly at config-load time, not silently
    /// fall through to a default mode.
    #[test]
    fn unknown_variant_is_rejected() {
        #[derive(Debug, Deserialize)]
        struct Wrap {
            #[allow(dead_code)]
            mode: BrokerMode,
        }
        let err = toml::from_str::<Wrap>(r#"mode = "PAPER""#).unwrap_err();
        let msg = err.to_string();
        // Surface the offending value so operators see what they typed.
        assert!(
            msg.contains("unknown variant") || msg.contains("PAPER"),
            "expected variant-rejection diagnostic, got: {msg}"
        );
    }

    #[test]
    fn as_str_matches_serde_variant_names() {
        assert_eq!(BrokerMode::Live.as_str(), "live");
        assert_eq!(BrokerMode::Paper.as_str(), "paper");
    }

    #[test]
    fn is_simulated_only_for_paper() {
        assert!(BrokerMode::Paper.is_simulated());
        assert!(!BrokerMode::Live.is_simulated());
    }
}
