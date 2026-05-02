use serde::{Deserialize, Serialize};

use crate::traits::clock::Clock;
use crate::types::Bar;

/// Market regime classification used by the regime detector and orchestrator.
///
/// `Hash` is derived so [`RegimeState`] can key the regime-to-strategy
/// mapping in [`RegimeOrchestrationConfig`](crate::config::RegimeOrchestrationConfig).
/// `serde::{Serialize, Deserialize}` are derived so the same map can be loaded
/// directly from a TOML inline table keyed on the variant names (`Trending`,
/// `Rotational`, `Volatile`, `Unknown`) and so [`RegimeState`] values can be
/// captured into a `ReplayResult` (Story 7.2) for snapshot-based determinism
/// verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize, Serialize)]
pub enum RegimeState {
    Trending,
    Rotational,
    Volatile,
    #[default]
    Unknown,
}

impl RegimeState {
    /// Stable string identifier used for journal records and structured logs.
    pub const fn as_str(&self) -> &'static str {
        match self {
            RegimeState::Trending => "Trending",
            RegimeState::Rotational => "Rotational",
            RegimeState::Volatile => "Volatile",
            RegimeState::Unknown => "Unknown",
        }
    }
}

pub trait RegimeDetector: Send {
    fn update(&mut self, bar: &Bar, clock: &dyn Clock) -> RegimeState;
    fn current(&self) -> RegimeState;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unknown() {
        assert_eq!(RegimeState::default(), RegimeState::Unknown);
    }
}
