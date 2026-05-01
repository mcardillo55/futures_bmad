//! Regime orchestration configuration (story 6.2).
//!
//! [`RegimeOrchestrationConfig`] defines the regime-to-strategy mapping and
//! the cooldown/oscillation policy consumed by the engine's regime
//! orchestrator. It is loaded from the TOML `[regime]` stanza alongside the
//! existing `[trading]`, `[broker]`, and `[fees]` sections — a separate
//! stanza is intentional: regime orchestration policy belongs to its own
//! configuration surface so trading/risk operators can tune it without
//! touching the trading config.
//!
//! Conservative-on-oscillation is the safety hedge for the
//! "rapid-flipping" failure mode: when the classifier oscillates faster
//! than the cooldown, the orchestrator stays in the more conservative of
//! the two regimes (per the ordering documented at
//! `RegimeOrchestrationConfig::conservative_on_oscillation`) so a brief
//! blip into a permissive regime cannot enable trading.

use std::collections::HashMap;

use serde::Deserialize;

use crate::traits::RegimeState;

/// Default cooldown — five minutes is conservative enough to absorb short
/// classification flutters near a threshold boundary while still allowing
/// the orchestrator to react to genuine regime changes within a single
/// trading session.
pub const DEFAULT_REGIME_COOLDOWN_SECS: u64 = 300;

/// Configuration for the regime orchestrator.
///
/// Loaded from the TOML `[regime]` stanza. Example:
/// ```toml
/// [regime]
/// cooldown_period_secs = 300
/// conservative_on_oscillation = true
///
/// [regime.regime_strategy_map]
/// Trending   = ["obi", "vpin"]
/// Rotational = ["microprice"]
/// Volatile   = []
/// Unknown    = []
/// ```
///
/// `regime_strategy_map` is keyed on the [`RegimeState`] variant name
/// exactly (case-sensitive) — the variants are
/// `Trending`, `Rotational`, `Volatile`, `Unknown`.
#[derive(Debug, Clone, Deserialize)]
pub struct RegimeOrchestrationConfig {
    /// Strategies permitted while each regime is the acted-upon regime. The
    /// orchestrator gates only **new** trade entries with this mapping;
    /// existing positions and their stop-loss orders are NEVER cancelled
    /// when a strategy is disabled.
    #[serde(default = "default_regime_strategy_map")]
    pub regime_strategy_map: HashMap<RegimeState, Vec<String>>,

    /// Minimum seconds between acting on regime transitions. Transitions
    /// observed sooner than this after the previous acted-upon transition
    /// are treated as oscillations: they are still surfaced as
    /// [`RegimeTransition`](crate::RegimeTransition) events for journaling,
    /// but the strategy permission set is not updated.
    #[serde(default = "default_cooldown_period_secs")]
    pub cooldown_period_secs: u64,

    /// When `true` and an oscillation is detected, the orchestrator remains
    /// in the more conservative of the current and proposed regime —
    /// conservative ordering is `Unknown > Volatile > Rotational > Trending`
    /// (most-restrictive first). When `false`, the cooldown still
    /// suppresses the strategy update but the current regime is left
    /// unchanged regardless of relative conservativeness.
    #[serde(default = "default_conservative_on_oscillation")]
    pub conservative_on_oscillation: bool,
}

impl Default for RegimeOrchestrationConfig {
    fn default() -> Self {
        Self {
            regime_strategy_map: default_regime_strategy_map(),
            cooldown_period_secs: DEFAULT_REGIME_COOLDOWN_SECS,
            conservative_on_oscillation: true,
        }
    }
}

/// Default mapping: trending allows the OBI-class strategies, rotational
/// allows microprice mean-reversion, volatile and unknown sit out.
///
/// These names are placeholders that downstream wiring (story 8.x signal
/// orchestration) will match against actual strategy identifiers; the
/// orchestrator itself treats them as opaque strings.
fn default_regime_strategy_map() -> HashMap<RegimeState, Vec<String>> {
    let mut map = HashMap::new();
    map.insert(
        RegimeState::Trending,
        vec!["obi".to_string(), "vpin".to_string()],
    );
    map.insert(RegimeState::Rotational, vec!["microprice".to_string()]);
    map.insert(RegimeState::Volatile, Vec::new());
    map.insert(RegimeState::Unknown, Vec::new());
    map
}

const fn default_cooldown_period_secs() -> u64 {
    DEFAULT_REGIME_COOLDOWN_SECS
}

const fn default_conservative_on_oscillation() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_blocks_trading_in_unknown_and_volatile() {
        let cfg = RegimeOrchestrationConfig::default();
        assert!(
            cfg.regime_strategy_map
                .get(&RegimeState::Unknown)
                .map(|v| v.is_empty())
                .unwrap_or(false),
            "Unknown must permit no strategies by default"
        );
        assert!(
            cfg.regime_strategy_map
                .get(&RegimeState::Volatile)
                .map(|v| v.is_empty())
                .unwrap_or(false),
            "Volatile must permit no strategies by default"
        );
    }

    #[test]
    fn default_permits_some_strategies_in_trending() {
        let cfg = RegimeOrchestrationConfig::default();
        let trending = cfg
            .regime_strategy_map
            .get(&RegimeState::Trending)
            .expect("trending entry");
        assert!(!trending.is_empty());
    }

    #[test]
    fn default_cooldown_is_five_minutes() {
        let cfg = RegimeOrchestrationConfig::default();
        assert_eq!(cfg.cooldown_period_secs, 300);
    }

    #[test]
    fn default_is_conservative_on_oscillation() {
        let cfg = RegimeOrchestrationConfig::default();
        assert!(cfg.conservative_on_oscillation);
    }

    #[test]
    fn deserialises_from_toml() {
        let src = r#"
            cooldown_period_secs = 120
            conservative_on_oscillation = false

            [regime_strategy_map]
            Trending = ["obi"]
            Rotational = []
            Volatile = []
            Unknown = []
        "#;
        let cfg: RegimeOrchestrationConfig = toml::from_str(src).expect("config parses");
        assert_eq!(cfg.cooldown_period_secs, 120);
        assert!(!cfg.conservative_on_oscillation);
        assert_eq!(
            cfg.regime_strategy_map.get(&RegimeState::Trending),
            Some(&vec!["obi".to_string()])
        );
        assert_eq!(
            cfg.regime_strategy_map.get(&RegimeState::Volatile),
            Some(&Vec::<String>::new())
        );
    }

    #[test]
    fn deserialises_with_all_defaults() {
        let src = "";
        let cfg: RegimeOrchestrationConfig = toml::from_str(src).expect("empty parses");
        assert_eq!(cfg.cooldown_period_secs, DEFAULT_REGIME_COOLDOWN_SECS);
        assert!(cfg.conservative_on_oscillation);
        assert!(
            cfg.regime_strategy_map
                .get(&RegimeState::Unknown)
                .map(|v| v.is_empty())
                .unwrap_or(false)
        );
    }
}
