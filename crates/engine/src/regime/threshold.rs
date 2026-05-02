//! Threshold-based regime detector (V1).
//!
//! Classifies the prevailing market regime as one of
//! [`RegimeState::Trending`], [`RegimeState::Rotational`],
//! [`RegimeState::Volatile`], or [`RegimeState::Unknown`] based on a small
//! set of configurable thresholds applied to OHLCV bars:
//!   * **ATR (Average True Range)** — volatility proxy.
//!   * **Directional persistence** — fraction of recent bars whose close
//!     moved in the same direction (max of up- and down-fractions).
//!   * **Range-to-body ratio** — average of `(high − low) / |close − open|`,
//!     a proxy for indecisive / rotational candles.
//!
//! The detector consumes whichever bar interval is configured (1-minute or
//! 5-minute, see [`ThresholdRegimeConfig::bar_interval_minutes`]). It does
//! NOT subdivide ticks itself — the caller is expected to feed completed
//! bars at the configured cadence.
//!
//! Memory model: a single `VecDeque<Bar>` ring buffer is allocated at
//! [`ThresholdRegimeDetector::new`]. No further heap allocations occur on
//! the per-update hot path.
//!
//! `Unknown` is returned during the warmup window (`bars_processed <
//! config.warmup_period`) and indicates trading should be blocked by
//! default — see [`ThresholdRegimeConfig::unknown_blocks_trading`].

use std::collections::VecDeque;

use futures_bmad_core::{Bar, Clock, RegimeDetector, RegimeState};
use serde::Deserialize;

/// Configuration for [`ThresholdRegimeDetector`].
///
/// All thresholds are in signal-domain `f64` units — converted from
/// fixed-point [`FixedPrice`](futures_bmad_core::FixedPrice) values via
/// `to_f64()` per the architecture rule that signal computations may use
/// floating-point.
#[derive(Debug, Clone, Deserialize)]
pub struct ThresholdRegimeConfig {
    /// Number of bars that must be observed before classification begins.
    /// While `bars_processed < warmup_period`, the detector reports
    /// [`RegimeState::Unknown`].
    pub warmup_period: usize,
    /// Bar cadence the detector consumes. Expected values are 1 or 5; the
    /// detector itself does not enforce the value but downstream
    /// orchestration uses it to decide which bar stream to forward.
    pub bar_interval_minutes: u64,
    /// Lookback window (in bars) over which ATR, directional persistence,
    /// and range-to-body ratio are computed.
    pub atr_period: usize,
    /// ATR strictly below this value is considered low-volatility (so a
    /// trending market is permitted). Currently informational; the V1
    /// classifier uses [`Self::atr_volatile_threshold`] for the upper
    /// bound and treats anything below it as non-volatile.
    pub atr_trending_threshold: f64,
    /// ATR at-or-above this value is considered volatile.
    pub atr_volatile_threshold: f64,
    /// Minimum same-direction-bar fraction (0.0..=1.0) required to qualify
    /// the window as a trend. E.g. `0.7` means at least 70 % of the bars
    /// in the lookback closed in the same direction.
    pub directional_persistence_threshold: f64,
    /// Threshold on the average range-to-body ratio above which the
    /// classifier leans toward [`RegimeState::Rotational`]. Currently
    /// informational in the V1 decision tree but exposed for tuning and
    /// future use.
    pub range_body_ratio_threshold: f64,
    /// Whether the `Unknown` regime should block trading. Defaults to
    /// `true` (mandatory per architecture: `Unknown` at startup blocks
    /// trading until classification is established).
    pub unknown_blocks_trading: bool,
}

impl Default for ThresholdRegimeConfig {
    fn default() -> Self {
        Self {
            warmup_period: 30,
            bar_interval_minutes: 1,
            atr_period: 14,
            atr_trending_threshold: 1.0,
            atr_volatile_threshold: 4.0,
            directional_persistence_threshold: 0.7,
            range_body_ratio_threshold: 3.0,
            unknown_blocks_trading: true,
        }
    }
}

/// Threshold-based [`RegimeDetector`] implementation. See module-level docs.
pub struct ThresholdRegimeDetector {
    config: ThresholdRegimeConfig,
    current_state: RegimeState,
    /// Ring buffer of recent bars; capacity = `max(atr_period+1,
    /// warmup_period)` so that True-Range computation always has access
    /// to a previous close even at the smallest lookback boundary.
    bar_buffer: VecDeque<Bar>,
    bars_processed: usize,
    /// Rolling ATR values (one per update past the warmup point). Sized
    /// at `atr_period` so smoothing is bounded; primarily for diagnostic
    /// access in future stories.
    atr_values: VecDeque<f64>,
}

impl ThresholdRegimeDetector {
    /// Construct a detector. Allocates the ring buffer up-front so the
    /// hot path stays allocation-free.
    pub fn new(config: ThresholdRegimeConfig) -> Self {
        let buffer_capacity = config
            .atr_period
            .saturating_add(1)
            .max(config.warmup_period)
            .max(1);
        let atr_capacity = config.atr_period.max(1);
        Self {
            current_state: RegimeState::Unknown,
            bar_buffer: VecDeque::with_capacity(buffer_capacity),
            bars_processed: 0,
            atr_values: VecDeque::with_capacity(atr_capacity),
            config,
        }
    }

    /// Read-only access to the active configuration.
    pub fn config(&self) -> &ThresholdRegimeConfig {
        &self.config
    }

    /// Number of bars consumed since construction.
    pub fn bars_processed(&self) -> usize {
        self.bars_processed
    }

    /// Compute average True Range over the most recent `atr_period` bars.
    ///
    /// True Range for each bar pair is
    /// `max(high − low, |high − prev_close|, |low − prev_close|)`. With
    /// fewer than two bars in the buffer the ATR is `0.0` (caller is
    /// responsible for honouring `warmup_period`).
    fn compute_atr(&self) -> f64 {
        let n_bars = self.bar_buffer.len();
        if n_bars < 2 {
            return 0.0;
        }
        // Walk the most recent `atr_period` pairs (clamped to what we have).
        let pair_count = self.config.atr_period.min(n_bars - 1);
        if pair_count == 0 {
            return 0.0;
        }
        let start_idx = n_bars - pair_count - 1;
        let mut sum = 0.0_f64;
        for i in 0..pair_count {
            let prev = &self.bar_buffer[start_idx + i];
            let curr = &self.bar_buffer[start_idx + i + 1];
            let high = curr.high.to_f64();
            let low = curr.low.to_f64();
            let prev_close = prev.close.to_f64();
            let r1 = (high - low).abs();
            let r2 = (high - prev_close).abs();
            let r3 = (low - prev_close).abs();
            let tr = r1.max(r2).max(r3);
            sum += tr;
        }
        sum / (pair_count as f64)
    }

    /// Fraction of recent bars in the dominant direction:
    /// `max(up_fraction, down_fraction)` over the lookback.
    fn compute_directional_persistence(&self) -> f64 {
        let n_bars = self.bar_buffer.len();
        if n_bars == 0 {
            return 0.0;
        }
        let lookback = self.config.atr_period.min(n_bars).max(1);
        let start_idx = n_bars - lookback;
        let mut up = 0_usize;
        let mut down = 0_usize;
        for bar in self.bar_buffer.iter().skip(start_idx) {
            let close = bar.close.to_f64();
            let open = bar.open.to_f64();
            if close > open {
                up += 1;
            } else if close < open {
                down += 1;
            }
            // Equal close/open -> doji, contributes to neither tally but
            // still counts toward the lookback denominator.
        }
        let dominant = up.max(down) as f64;
        dominant / (lookback as f64)
    }

    /// Average range-to-body ratio over the lookback window. Bodies smaller
    /// than `BODY_EPSILON` are treated as a large ratio (`MAX_RATIO`) to
    /// avoid divide-by-zero and to flag indecisive doji-style candles.
    fn compute_avg_range_body_ratio(&self) -> f64 {
        const BODY_EPSILON: f64 = 1e-9;
        const MAX_RATIO: f64 = 1e6;

        let n_bars = self.bar_buffer.len();
        if n_bars == 0 {
            return 0.0;
        }
        let lookback = self.config.atr_period.min(n_bars).max(1);
        let start_idx = n_bars - lookback;
        let mut sum = 0.0_f64;
        for bar in self.bar_buffer.iter().skip(start_idx) {
            let range = (bar.high.to_f64() - bar.low.to_f64()).abs();
            let body = (bar.close.to_f64() - bar.open.to_f64()).abs();
            let ratio = if body < BODY_EPSILON {
                MAX_RATIO
            } else {
                range / body
            };
            sum += ratio;
        }
        sum / (lookback as f64)
    }

    /// V1 classifier. Returns [`RegimeState::Unknown`] until warmup
    /// completes; otherwise applies the threshold decision tree.
    fn classify(&self) -> RegimeState {
        if self.bars_processed < self.config.warmup_period {
            return RegimeState::Unknown;
        }
        let atr = self.compute_atr();
        let persistence = self.compute_directional_persistence();
        // Range-body ratio is computed but reserved for future tuning;
        // touch it so it remains live and benchmarked.
        let _range_body = self.compute_avg_range_body_ratio();

        let above_volatile = atr > self.config.atr_volatile_threshold;
        let strong_persistence = persistence >= self.config.directional_persistence_threshold;

        if above_volatile && !strong_persistence {
            RegimeState::Volatile
        } else if strong_persistence && !above_volatile {
            RegimeState::Trending
        } else {
            RegimeState::Rotational
        }
    }

    fn push_bar(&mut self, bar: Bar) {
        if self.bar_buffer.len() == self.bar_buffer.capacity() && self.bar_buffer.capacity() > 0 {
            self.bar_buffer.pop_front();
        }
        self.bar_buffer.push_back(bar);
    }

    fn record_atr_sample(&mut self, atr: f64) {
        if self.atr_values.len() == self.atr_values.capacity() && self.atr_values.capacity() > 0 {
            self.atr_values.pop_front();
        }
        self.atr_values.push_back(atr);
    }
}

impl RegimeDetector for ThresholdRegimeDetector {
    fn update(&mut self, bar: &Bar, _clock: &dyn Clock) -> RegimeState {
        // V1 does not consume the clock directly but accepts it per the
        // trait contract for future time-based logic.
        self.push_bar(*bar);
        self.bars_processed = self.bars_processed.saturating_add(1);

        // Track the rolling ATR (post-warmup it informs classification;
        // pre-warmup we still record samples to keep the deque warm).
        if self.bar_buffer.len() >= 2 {
            let atr = self.compute_atr();
            self.record_atr_sample(atr);
        }

        self.current_state = self.classify();
        self.current_state
    }

    fn current(&self) -> RegimeState {
        self.current_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_bmad_core::{Bar, FixedPrice, UnixNanos};
    use futures_bmad_testkit::SimClock;

    /// Tiny helper to build a bar from f64 values without dragging FixedPrice
    /// arithmetic into every test.
    fn mk_bar(open: f64, high: f64, low: f64, close: f64, ts_nanos: u64) -> Bar {
        Bar {
            open: FixedPrice::from_f64(open).unwrap(),
            high: FixedPrice::from_f64(high).unwrap(),
            low: FixedPrice::from_f64(low).unwrap(),
            close: FixedPrice::from_f64(close).unwrap(),
            volume: 100,
            timestamp: UnixNanos::new(ts_nanos),
        }
    }

    /// Fast-warmup config so classification kicks in after a handful of bars.
    fn fast_warmup_config() -> ThresholdRegimeConfig {
        ThresholdRegimeConfig {
            warmup_period: 5,
            bar_interval_minutes: 1,
            atr_period: 5,
            atr_trending_threshold: 1.0,
            atr_volatile_threshold: 5.0,
            directional_persistence_threshold: 0.7,
            range_body_ratio_threshold: 3.0,
            unknown_blocks_trading: true,
        }
    }

    #[test]
    fn defaults_block_trading_on_unknown() {
        let cfg = ThresholdRegimeConfig::default();
        assert!(cfg.unknown_blocks_trading);
        assert_eq!(cfg.bar_interval_minutes, 1);
        assert_eq!(cfg.warmup_period, 30);
    }

    #[test]
    fn unknown_blocks_trading_flag_is_accessible() {
        let mut cfg = ThresholdRegimeConfig::default();
        assert!(cfg.unknown_blocks_trading);
        cfg.unknown_blocks_trading = false;
        let detector = ThresholdRegimeDetector::new(cfg);
        assert!(!detector.config().unknown_blocks_trading);
    }

    #[test]
    fn accepts_one_minute_and_five_minute_intervals() {
        for interval in [1u64, 5u64] {
            let cfg = ThresholdRegimeConfig {
                bar_interval_minutes: interval,
                ..ThresholdRegimeConfig::default()
            };
            let detector = ThresholdRegimeDetector::new(cfg);
            assert_eq!(detector.config().bar_interval_minutes, interval);
        }
    }

    #[test]
    fn fewer_bars_than_warmup_returns_unknown() {
        let clock = SimClock::new(0);
        let mut detector = ThresholdRegimeDetector::new(fast_warmup_config());
        // Push fewer than warmup_period bars (5 in fast_warmup_config).
        let mut price = 100.0_f64;
        for i in 0..3 {
            let bar = mk_bar(price, price + 0.5, price - 0.25, price + 0.25, i * 60);
            detector.update(&bar, &clock);
            price += 0.25;
        }
        assert_eq!(detector.current(), RegimeState::Unknown);
    }

    #[test]
    fn transitions_from_unknown_after_warmup() {
        let clock = SimClock::new(0);
        let cfg = fast_warmup_config();
        let warmup = cfg.warmup_period;
        let mut detector = ThresholdRegimeDetector::new(cfg);

        // Pre-warmup: should remain Unknown.
        let mut price = 100.0_f64;
        for i in 0..(warmup - 1) {
            let bar = mk_bar(
                price,
                price + 0.5,
                price - 0.25,
                price + 0.25,
                i as u64 * 60,
            );
            let state = detector.update(&bar, &clock);
            assert_eq!(state, RegimeState::Unknown);
            price += 0.25;
        }
        // The Nth bar completes warmup -> should leave Unknown.
        let bar = mk_bar(price, price + 0.5, price - 0.25, price + 0.25, 999);
        let state = detector.update(&bar, &clock);
        assert_ne!(state, RegimeState::Unknown);
        assert_eq!(detector.current(), state);
    }

    #[test]
    fn trending_sequence_classifies_as_trending() {
        let clock = SimClock::new(0);
        let cfg = fast_warmup_config();
        let mut detector = ThresholdRegimeDetector::new(cfg);

        // Monotonically increasing closes, modest ATR (low volatility),
        // every bar closes well above its open -> high persistence.
        let mut price = 100.0_f64;
        let step = 0.5_f64; // small drift per bar
        for i in 0..20 {
            let open = price;
            let close = price + step;
            let high = close + 0.25;
            let low = open - 0.25;
            let bar = mk_bar(open, high, low, close, i as u64 * 60);
            detector.update(&bar, &clock);
            price = close;
        }
        assert_eq!(detector.current(), RegimeState::Trending);
    }

    #[test]
    fn choppy_sideways_sequence_classifies_as_rotational() {
        let clock = SimClock::new(0);
        let cfg = fast_warmup_config();
        let mut detector = ThresholdRegimeDetector::new(cfg);

        // Alternating up/down bars with moderate ATR around a flat mean ->
        // persistence ~= 0.5, well below the 0.7 trend threshold; ATR
        // stays below the volatile threshold so the result is Rotational.
        let center = 100.0_f64;
        for i in 0..20 {
            let (open, close) = if i % 2 == 0 {
                (center, center + 0.25)
            } else {
                (center + 0.25, center)
            };
            let high = open.max(close) + 0.5;
            let low = open.min(close) - 0.5;
            let bar = mk_bar(open, high, low, close, i as u64 * 60);
            detector.update(&bar, &clock);
        }
        assert_eq!(detector.current(), RegimeState::Rotational);
    }

    #[test]
    fn volatile_sequence_classifies_as_volatile() {
        let clock = SimClock::new(0);
        let cfg = fast_warmup_config();
        let mut detector = ThresholdRegimeDetector::new(cfg);

        // Wide-range bars with no clear direction:
        //   - ranges of ~10 (high - low) -> ATR > atr_volatile_threshold (5.0)
        //   - alternating up/down opens/closes -> persistence ~= 0.5 < 0.7
        let center = 100.0_f64;
        for i in 0..20 {
            let (open, close) = if i % 2 == 0 {
                (center - 0.5, center + 0.25)
            } else {
                (center + 0.5, center - 0.25)
            };
            let high = center + 6.0;
            let low = center - 6.0;
            let bar = mk_bar(open, high, low, close, i as u64 * 60);
            detector.update(&bar, &clock);
        }
        assert_eq!(detector.current(), RegimeState::Volatile);
    }

    #[test]
    fn ring_buffer_does_not_grow_unbounded() {
        let clock = SimClock::new(0);
        let cfg = fast_warmup_config();
        let cap_hint = cfg.atr_period.saturating_add(1).max(cfg.warmup_period);
        let mut detector = ThresholdRegimeDetector::new(cfg);
        for i in 0..1_000 {
            let p = 100.0 + (i as f64) * 0.01;
            let bar = mk_bar(p, p + 0.25, p - 0.25, p, i as u64 * 60);
            detector.update(&bar, &clock);
        }
        // Buffer length should never exceed its initial capacity hint.
        assert!(detector.bar_buffer.len() <= cap_hint);
        assert_eq!(detector.bars_processed(), 1_000);
    }

    /// Malformed bars where `high < low` must contribute `|high - low|` to
    /// True Range, not the negative raw difference. Real CME data never
    /// produces this state, but fuzz/replay corpora (Story 7.2) may not
    /// enforce that invariant — a missing `.abs()` would silently understate
    /// True Range when `r1` should dominate.
    ///
    /// Constructed so `prev_close` sits between the swapped `high` and
    /// `low`, making `|h-prev|` and `|l-prev|` both small — `r1` is the
    /// only term that should produce the correct TR magnitude.
    #[test]
    fn compute_atr_handles_malformed_bar_with_low_above_high() {
        let cfg = ThresholdRegimeConfig {
            atr_period: 1,
            ..fast_warmup_config()
        };
        let mut detector = ThresholdRegimeDetector::new(cfg);
        // prev bar: close = 99 (sandwiched between the malformed bar's
        // swapped high=95 / low=100 so r2 = 4 and r3 = 1).
        detector.push_bar(mk_bar(99.0, 99.5, 98.5, 99.0, 0));
        // Malformed bar: high < low. |high - low| = 5.
        detector.push_bar(mk_bar(98.0, 95.0, 100.0, 97.0, 60));
        let atr = detector.compute_atr();
        // With pair_count = 1, ATR == TR of the single pair.
        // r1 = |95 - 100| = 5, r2 = |95 - 99| = 4, r3 = |100 - 99| = 1.
        // max = 5. Without `.abs()` on r1 the test would observe 4
        // (r2 dominates), revealing the silent understatement.
        assert!((atr - 5.0).abs() < f64::EPSILON, "expected TR=5.0, got {atr}");
    }

    #[test]
    fn current_returns_last_classification_without_update() {
        let clock = SimClock::new(0);
        let cfg = fast_warmup_config();
        let mut detector = ThresholdRegimeDetector::new(cfg);
        // Empty: starts Unknown.
        assert_eq!(detector.current(), RegimeState::Unknown);
        // Drive past warmup with a clear trend.
        let mut price = 100.0_f64;
        for i in 0..15 {
            let open = price;
            let close = price + 0.5;
            let bar = mk_bar(open, close + 0.25, open - 0.25, close, i as u64 * 60);
            detector.update(&bar, &clock);
            price = close;
        }
        let last = detector.current();
        assert_ne!(last, RegimeState::Unknown);
        // Without further updates, current() must continue to report the
        // last classification.
        assert_eq!(detector.current(), last);
    }

    #[test]
    fn config_deserialises_from_toml() {
        // Round-trip via toml so the serde::Deserialize derive is exercised.
        let src = r#"
            warmup_period = 12
            bar_interval_minutes = 5
            atr_period = 7
            atr_trending_threshold = 0.5
            atr_volatile_threshold = 3.5
            directional_persistence_threshold = 0.65
            range_body_ratio_threshold = 2.5
            unknown_blocks_trading = false
        "#;
        let cfg: ThresholdRegimeConfig = toml::from_str(src).expect("config parses");
        assert_eq!(cfg.warmup_period, 12);
        assert_eq!(cfg.bar_interval_minutes, 5);
        assert_eq!(cfg.atr_period, 7);
        assert!((cfg.atr_volatile_threshold - 3.5).abs() < f64::EPSILON);
        assert!(!cfg.unknown_blocks_trading);
    }
}
