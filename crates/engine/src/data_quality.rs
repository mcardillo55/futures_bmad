use std::collections::HashMap;

use futures_bmad_core::{Clock, UnixNanos};
use tracing::{info, warn};

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Reason the data quality gate was activated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateReason {
    StaleData {
        gap_nanos: u64,
    },
    SequenceGap {
        symbol_id: u32,
        expected: u64,
        received: u64,
    },
}

/// State of the data quality gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateState {
    /// Data is fresh, trading evaluation allowed.
    Open,
    /// Data quality suspect, trading evaluation blocked.
    Gated,
}

/// Detects stale market data by tracking time since last tick.
/// Only triggers during market hours to avoid false positives.
pub struct StaleDataDetector {
    last_tick_nanos: u64,
    threshold_nanos: u64,
}

impl StaleDataDetector {
    pub fn new(threshold_secs: f64) -> Self {
        Self {
            last_tick_nanos: 0,
            threshold_nanos: (threshold_secs * NANOS_PER_SEC as f64) as u64,
        }
    }

    /// Record a tick arrival.
    pub fn on_tick(&mut self, now_nanos: u64) {
        self.last_tick_nanos = now_nanos;
    }

    /// Check if data is stale. Returns gap duration in nanos if stale during market hours.
    pub fn check_stale(&self, now_nanos: u64, market_open: bool) -> Option<u64> {
        if !market_open {
            return None;
        }
        if self.last_tick_nanos == 0 {
            return None; // No tick received yet
        }
        let elapsed = now_nanos.saturating_sub(self.last_tick_nanos);
        if elapsed > self.threshold_nanos {
            Some(elapsed)
        } else {
            None
        }
    }

    pub fn threshold_nanos(&self) -> u64 {
        self.threshold_nanos
    }
}

/// Detects gaps in sequence numbers per symbol.
pub struct SequenceGapDetector {
    last_seq: HashMap<u32, u64>,
}

impl SequenceGapDetector {
    pub fn new() -> Self {
        Self {
            last_seq: HashMap::new(),
        }
    }

    /// Check a sequence number for a symbol. Returns the gap range if a gap is detected.
    pub fn check_sequence(&mut self, symbol_id: u32, seq: u64) -> Option<(u64, u64)> {
        match self.last_seq.get(&symbol_id) {
            Some(&last) => {
                let expected = last + 1;
                self.last_seq.insert(symbol_id, seq);
                if seq > expected {
                    warn!(symbol_id, expected, received = seq, "sequence gap detected");
                    Some((expected, seq))
                } else {
                    None
                }
            }
            None => {
                // First sequence for this symbol
                self.last_seq.insert(symbol_id, seq);
                None
            }
        }
    }
}

/// Data quality gate — auto-clears when fresh data arrives.
/// Distinct from circuit breaker: gate auto-clears on recovery.
pub struct DataQualityGate {
    state: GateState,
    gated_since_nanos: u64,
}

impl DataQualityGate {
    pub fn new() -> Self {
        Self {
            state: GateState::Open,
            gated_since_nanos: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.state == GateState::Open
    }

    pub fn state(&self) -> GateState {
        self.state
    }

    /// Activate the gate due to a data quality issue.
    pub fn activate(&mut self, reason: &GateReason, now_nanos: u64) {
        if self.state == GateState::Open {
            self.gated_since_nanos = now_nanos;
            warn!(?reason, "data quality gate activated");
        }
        self.state = GateState::Gated;
    }

    /// Clear the gate when fresh data resumes. Logs the gap duration.
    pub fn clear(&mut self, now_nanos: u64) {
        if self.state == GateState::Gated {
            let gap_duration_nanos = now_nanos.saturating_sub(self.gated_since_nanos);
            let gap_ms = gap_duration_nanos / 1_000_000;
            info!(gap_duration_ms = gap_ms, "data quality gate cleared");
            self.state = GateState::Open;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NANOS_3S: u64 = 3 * NANOS_PER_SEC;

    // --- StaleDataDetector tests ---

    #[test]
    fn stale_detector_triggers_after_threshold() {
        let mut detector = StaleDataDetector::new(3.0);
        let base = 1_000_000_000_000u64;

        detector.on_tick(base);
        // 2 seconds later — not stale
        assert!(
            detector
                .check_stale(base + 2 * NANOS_PER_SEC, true)
                .is_none()
        );
        // 4 seconds later — stale
        let gap = detector.check_stale(base + 4 * NANOS_PER_SEC, true);
        assert!(gap.is_some());
        assert_eq!(gap.unwrap(), 4 * NANOS_PER_SEC);
    }

    #[test]
    fn stale_detector_no_trigger_outside_market_hours() {
        let mut detector = StaleDataDetector::new(3.0);
        let base = 1_000_000_000_000u64;

        detector.on_tick(base);
        // 10 seconds later but market closed
        assert!(
            detector
                .check_stale(base + 10 * NANOS_PER_SEC, false)
                .is_none()
        );
    }

    #[test]
    fn stale_detector_no_trigger_before_first_tick() {
        let detector = StaleDataDetector::new(3.0);
        assert!(detector.check_stale(1_000_000_000_000, true).is_none());
    }

    // --- SequenceGapDetector tests ---

    #[test]
    fn sequence_gap_detects_missing_range() {
        let mut detector = SequenceGapDetector::new();

        assert!(detector.check_sequence(0, 1).is_none()); // first
        assert!(detector.check_sequence(0, 2).is_none()); // contiguous
        let gap = detector.check_sequence(0, 5); // gap: expected 3, got 5
        assert_eq!(gap, Some((3, 5)));
    }

    #[test]
    fn sequence_gap_passes_contiguous() {
        let mut detector = SequenceGapDetector::new();
        for seq in 1..=100 {
            assert!(detector.check_sequence(0, seq).is_none());
        }
    }

    #[test]
    fn sequence_gap_per_symbol() {
        let mut detector = SequenceGapDetector::new();

        // Symbol 0: 1, 2, 3
        detector.check_sequence(0, 1);
        detector.check_sequence(0, 2);
        assert!(detector.check_sequence(0, 3).is_none());

        // Symbol 1: 1, 5 — gap
        detector.check_sequence(1, 1);
        assert!(detector.check_sequence(1, 5).is_some());

        // Symbol 0 still contiguous
        assert!(detector.check_sequence(0, 4).is_none());
    }

    // --- DataQualityGate tests ---

    #[test]
    fn gate_starts_open() {
        let gate = DataQualityGate::new();
        assert!(gate.is_open());
    }

    #[test]
    fn gate_activates_and_clears() {
        let mut gate = DataQualityGate::new();
        let now = 1_000_000_000_000u64;

        gate.activate(
            &GateReason::StaleData {
                gap_nanos: NANOS_3S,
            },
            now,
        );
        assert!(!gate.is_open());
        assert_eq!(gate.state(), GateState::Gated);

        // Clear after 1 second
        gate.clear(now + NANOS_PER_SEC);
        assert!(gate.is_open());
    }

    #[test]
    fn gate_does_not_flap() {
        let mut gate = DataQualityGate::new();
        let now = 1_000_000_000_000u64;

        // Activate
        gate.activate(
            &GateReason::StaleData {
                gap_nanos: NANOS_3S,
            },
            now,
        );
        assert!(!gate.is_open());

        // Re-activate (shouldn't reset gated_since)
        gate.activate(
            &GateReason::SequenceGap {
                symbol_id: 0,
                expected: 5,
                received: 10,
            },
            now + NANOS_PER_SEC,
        );
        assert!(!gate.is_open());

        // Clear
        gate.clear(now + 2 * NANOS_PER_SEC);
        assert!(gate.is_open());
    }

    #[test]
    fn gate_clear_when_already_open_is_noop() {
        let mut gate = DataQualityGate::new();
        gate.clear(1_000_000_000_000); // Should not panic or log
        assert!(gate.is_open());
    }

    // --- Integration test ---

    #[test]
    fn stale_and_sequence_both_gate() {
        let mut stale = StaleDataDetector::new(3.0);
        let mut seq = SequenceGapDetector::new();
        let mut gate = DataQualityGate::new();
        let base = 1_000_000_000_000u64;

        // Normal operation
        stale.on_tick(base);
        seq.check_sequence(0, 1);
        assert!(gate.is_open());

        // Stale data triggers gate
        let gap = stale.check_stale(base + 4 * NANOS_PER_SEC, true).unwrap();
        gate.activate(
            &GateReason::StaleData { gap_nanos: gap },
            base + 4 * NANOS_PER_SEC,
        );
        assert!(!gate.is_open());

        // Fresh tick clears gate
        stale.on_tick(base + 5 * NANOS_PER_SEC);
        assert!(stale.check_stale(base + 5 * NANOS_PER_SEC, true).is_none());
        gate.clear(base + 5 * NANOS_PER_SEC);
        assert!(gate.is_open());

        // Sequence gap triggers gate
        seq.check_sequence(0, 2);
        let gap = seq.check_sequence(0, 10); // gap
        assert!(gap.is_some());
        gate.activate(
            &GateReason::SequenceGap {
                symbol_id: 0,
                expected: 3,
                received: 10,
            },
            base + 6 * NANOS_PER_SEC,
        );
        assert!(!gate.is_open());

        // Contiguous sequence clears gate
        seq.check_sequence(0, 11);
        gate.clear(base + 7 * NANOS_PER_SEC);
        assert!(gate.is_open());
    }
}
