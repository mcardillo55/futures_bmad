use tracing::warn;

/// State of the SPSC buffer based on fill thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    /// Buffer is within normal operating range.
    Normal,
    /// Buffer is >= 50% full. Warning has been logged.
    Warning,
    /// Buffer is >= 80% full. Trade evaluation should be disabled.
    TradingDisabled,
    /// Buffer is >= 95% full. Full circuit break.
    CircuitBreak,
}

/// Hysteresis margin: threshold deactivates when fill drops below threshold - 5%.
const HYSTERESIS_MARGIN: f64 = 0.05;

const THRESHOLD_WARNING: f64 = 0.50;
const THRESHOLD_TRADING_DISABLED: f64 = 0.80;
const THRESHOLD_CIRCUIT_BREAK: f64 = 0.95;

/// Monitors SPSC buffer fill level and returns appropriate state.
/// Uses hysteresis to avoid flapping: threshold activates on crossing up,
/// deactivates when dropping below threshold minus 5%.
pub struct BufferMonitor {
    state: BufferState,
}

impl Default for BufferMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferMonitor {
    pub fn new() -> Self {
        Self {
            state: BufferState::Normal,
        }
    }

    /// Update the buffer state based on current fill fraction (0.0 to 1.0).
    /// Returns the new state and whether it changed.
    pub fn update(&mut self, fill_fraction: f64) -> BufferState {
        let new_state = self.compute_state(fill_fraction);

        if new_state != self.state {
            match new_state {
                BufferState::Normal => {}
                BufferState::Warning => {
                    warn!(
                        fill_pct = format!("{:.1}%", fill_fraction * 100.0),
                        "SPSC buffer warning threshold crossed"
                    );
                }
                BufferState::TradingDisabled => {
                    warn!(
                        fill_pct = format!("{:.1}%", fill_fraction * 100.0),
                        "SPSC buffer high — trading evaluation disabled"
                    );
                }
                BufferState::CircuitBreak => {
                    warn!(
                        fill_pct = format!("{:.1}%", fill_fraction * 100.0),
                        "SPSC buffer critical — circuit break"
                    );
                }
            }
            self.state = new_state;
        }

        self.state
    }

    pub fn state(&self) -> BufferState {
        self.state
    }

    fn compute_state(&self, fill: f64) -> BufferState {
        // When escalating (going up), use the raw thresholds
        // When de-escalating (going down), apply hysteresis margin
        match self.state {
            BufferState::Normal => {
                if fill >= THRESHOLD_CIRCUIT_BREAK {
                    BufferState::CircuitBreak
                } else if fill >= THRESHOLD_TRADING_DISABLED {
                    BufferState::TradingDisabled
                } else if fill >= THRESHOLD_WARNING {
                    BufferState::Warning
                } else {
                    BufferState::Normal
                }
            }
            BufferState::Warning => {
                if fill >= THRESHOLD_CIRCUIT_BREAK {
                    BufferState::CircuitBreak
                } else if fill >= THRESHOLD_TRADING_DISABLED {
                    BufferState::TradingDisabled
                } else if fill < THRESHOLD_WARNING - HYSTERESIS_MARGIN {
                    BufferState::Normal
                } else {
                    BufferState::Warning
                }
            }
            BufferState::TradingDisabled => {
                if fill >= THRESHOLD_CIRCUIT_BREAK {
                    BufferState::CircuitBreak
                } else if fill < THRESHOLD_TRADING_DISABLED - HYSTERESIS_MARGIN {
                    if fill >= THRESHOLD_WARNING {
                        BufferState::Warning
                    } else if fill < THRESHOLD_WARNING - HYSTERESIS_MARGIN {
                        BufferState::Normal
                    } else {
                        BufferState::Warning
                    }
                } else {
                    BufferState::TradingDisabled
                }
            }
            BufferState::CircuitBreak => {
                if fill < THRESHOLD_CIRCUIT_BREAK - HYSTERESIS_MARGIN {
                    if fill >= THRESHOLD_TRADING_DISABLED {
                        BufferState::TradingDisabled
                    } else if fill >= THRESHOLD_WARNING {
                        BufferState::Warning
                    } else if fill < THRESHOLD_WARNING - HYSTERESIS_MARGIN {
                        BufferState::Normal
                    } else {
                        BufferState::Warning
                    }
                } else {
                    BufferState::CircuitBreak
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_normal() {
        let monitor = BufferMonitor::new();
        assert_eq!(monitor.state(), BufferState::Normal);
    }

    #[test]
    fn escalates_through_thresholds() {
        let mut monitor = BufferMonitor::new();

        assert_eq!(monitor.update(0.10), BufferState::Normal);
        assert_eq!(monitor.update(0.50), BufferState::Warning);
        assert_eq!(monitor.update(0.80), BufferState::TradingDisabled);
        assert_eq!(monitor.update(0.95), BufferState::CircuitBreak);
    }

    #[test]
    fn hysteresis_prevents_flapping() {
        let mut monitor = BufferMonitor::new();

        // Escalate to warning
        assert_eq!(monitor.update(0.55), BufferState::Warning);

        // Drop slightly below threshold but within hysteresis — stays Warning
        assert_eq!(monitor.update(0.48), BufferState::Warning);

        // Drop below hysteresis margin (50% - 5% = 45%) — goes Normal
        assert_eq!(monitor.update(0.44), BufferState::Normal);
    }

    #[test]
    fn hysteresis_on_trading_disabled() {
        let mut monitor = BufferMonitor::new();

        assert_eq!(monitor.update(0.85), BufferState::TradingDisabled);

        // Drop to 76% — within hysteresis (80% - 5% = 75%), stays TradingDisabled
        assert_eq!(monitor.update(0.76), BufferState::TradingDisabled);

        // Drop to 74% — below hysteresis, goes to Warning
        assert_eq!(monitor.update(0.74), BufferState::Warning);
    }

    #[test]
    fn circuit_break_deescalation() {
        let mut monitor = BufferMonitor::new();

        assert_eq!(monitor.update(0.96), BufferState::CircuitBreak);

        // Still above circuit break hysteresis (95% - 5% = 90%)
        assert_eq!(monitor.update(0.91), BufferState::CircuitBreak);

        // Drop below — goes to TradingDisabled
        assert_eq!(monitor.update(0.85), BufferState::TradingDisabled);
    }

    #[test]
    fn jump_from_normal_to_circuit_break() {
        let mut monitor = BufferMonitor::new();
        assert_eq!(monitor.update(0.96), BufferState::CircuitBreak);
    }
}
