//! [`TradeSource`] — discriminator for the origin of a journal entry.
//!
//! Story 7.4 introduces a single source-of-truth tag that flows through every
//! trade-related event written to the SQLite event journal. This makes it
//! possible to compute P&L, win rate, max drawdown, and trade count
//! per-source from a single shared schema — paper, replay, and live runs all
//! land in the same tables, distinguished only by the `source` column.
//!
//! Per Story 7.4 Dev Notes: the tag is set at the [`crate::BrokerAdapter`]
//! layer (or, in practice, at orchestrator construction — see
//! `engine::paper::PaperTradingOrchestrator` and
//! `engine::replay::ReplayOrchestrator`). The event loop, signal pipeline,
//! risk subsystem, and journal writer remain mode-agnostic.
//!
//! ## Wire format
//!
//! Serialized as the lowercase variant name (`"live"`, `"paper"`, `"replay"`)
//! when written to SQLite — the journal stores it as a TEXT column. The
//! `as_str()` method returns the same canonical token used by structured-log
//! output so operators can grep both with one search term.

use serde::{Deserialize, Serialize};

/// Origin tag attached to every trade-related journal entry.
///
/// `Live` is the default for backwards compatibility — every existing
/// constructor that does not opt in to a non-live source is treated as live.
/// Paper and replay are explicitly selected by their respective orchestrators
/// at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeSource {
    /// Real exchange order routing (Rithmic OrderPlant). Default.
    Live,
    /// Live market data, simulated execution via
    /// [`crate::BrokerAdapter`] mock. No orders ever leave the host.
    Paper,
    /// Recorded market data + simulated execution. Used for deterministic
    /// replay regression tests.
    Replay,
}

impl Default for TradeSource {
    /// Default is [`TradeSource::Live`] — see module docs. This default is
    /// chosen so callers that have not migrated to source-aware sender
    /// construction continue to record under the live tag, never silently
    /// landing under paper or replay.
    fn default() -> Self {
        TradeSource::Live
    }
}

impl TradeSource {
    /// Stable lowercase string representation used by both the journal
    /// (TEXT column value) and structured-log output.
    pub const fn as_str(self) -> &'static str {
        match self {
            TradeSource::Live => "live",
            TradeSource::Paper => "paper",
            TradeSource::Replay => "replay",
        }
    }

    /// Parse a TEXT value read back from the journal. Returns `None` for any
    /// unknown token so callers can decide whether to surface a corrupted-
    /// journal warning vs. silently coerce to live.
    ///
    /// Named [`Self::parse`] (not `from_str`) deliberately — `from_str` would
    /// shadow [`std::str::FromStr::from_str`] on the type, and we don't want
    /// to commit to that trait's `Err` machinery for what is genuinely an
    /// "unknown variant ⇒ None" semantic.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "live" => Some(TradeSource::Live),
            "paper" => Some(TradeSource::Paper),
            "replay" => Some(TradeSource::Replay),
            _ => None,
        }
    }
}

impl std::fmt::Display for TradeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_live() {
        assert_eq!(TradeSource::default(), TradeSource::Live);
    }

    #[test]
    fn as_str_round_trip() {
        for src in [TradeSource::Live, TradeSource::Paper, TradeSource::Replay] {
            let s = src.as_str();
            assert_eq!(TradeSource::parse(s), Some(src));
        }
    }

    #[test]
    fn parse_unknown_is_none() {
        assert_eq!(TradeSource::parse("LIVE"), None);
        assert_eq!(TradeSource::parse(""), None);
        assert_eq!(TradeSource::parse("backtest"), None);
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(format!("{}", TradeSource::Paper), "paper");
        assert_eq!(format!("{}", TradeSource::Live), "live");
        assert_eq!(format!("{}", TradeSource::Replay), "replay");
    }

    #[test]
    fn deserializes_lowercase_from_toml() {
        #[derive(Deserialize)]
        struct Wrap {
            source: TradeSource,
        }
        let parsed: Wrap = toml::from_str(r#"source = "paper""#).unwrap();
        assert_eq!(parsed.source, TradeSource::Paper);
        let parsed: Wrap = toml::from_str(r#"source = "live""#).unwrap();
        assert_eq!(parsed.source, TradeSource::Live);
        let parsed: Wrap = toml::from_str(r#"source = "replay""#).unwrap();
        assert_eq!(parsed.source, TradeSource::Replay);
    }
}
