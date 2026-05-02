//! Three-tier deterministic replay verification — Story 7.2.
//!
//! Replay determinism is verified across three orthogonal tiers, each with
//! its own comparison rule appropriate to the data type:
//!
//! 1. **Tier 1 — Fixed-point (BIT-IDENTICAL).** All
//!    [`futures_bmad_core::FixedPrice`] values (prices, P&L, fees) live as
//!    quarter-tick `i64` integers under the hood. Same binary on same
//!    hardware MUST produce the exact same bytes. Any drift here is a bug —
//!    integer arithmetic is associative and platform-stable.
//!
//! 2. **Tier 2 — Signals (EPSILON).** Signal output is `f64` and subject to
//!    floating-point non-associativity. Same binary on same hardware should
//!    still produce bit-identical `f64` values, but a small epsilon
//!    (`DEFAULT_SIGNAL_EPSILON = 1e-10`) provides a safety margin and
//!    documents the boundary where bit-identity stops being guaranteed
//!    (cross-compiler, cross-arch).
//!
//! 3. **Tier 3 — Regime (SNAPSHOT).** [`RegimeState`] is a discrete enum
//!    with no associated data; comparison is exact variant equality.
//!
//! All three assert helpers panic on mismatch with detailed diagnostics
//! (hex dump for Tier 1, absolute and relative deltas for Tier 2, both
//! variants for Tier 3). The non-panicking [`DeterminismReport`] flow
//! collects every mismatch instead so a single failing run surfaces every
//! divergence rather than just the first.

use futures_bmad_core::{FixedPrice, RegimeState};
use serde::{Deserialize, Serialize};

/// Default epsilon for Tier 2 (signal value) comparisons.
///
/// Calibrated for `f64` values in the typical signal range `[-1.0, 1.0]`
/// (OBI, microprice, VPIN). Same binary on same hardware almost always
/// produces bit-identical `f64`; epsilon is the safety margin documented
/// in the architecture's three-tier model.
pub const DEFAULT_SIGNAL_EPSILON: f64 = 1e-10;

/// Tier 1 — bit-identical [`FixedPrice`] comparison.
///
/// Panics on mismatch with a hex dump of both raw `i64` values so the
/// failure mode is unambiguous (visible bit pattern, not a `Display`-rounded
/// f64). Use this assert for prices, P&L, fees — anything that flows
/// through fixed-point arithmetic.
pub fn assert_fixed_price_identical(a: FixedPrice, b: FixedPrice) {
    if a.raw() != b.raw() {
        panic!(
            "Tier 1 mismatch: FixedPrice values differ\n  \
             a.raw = {a_raw} (0x{a_hex:016x})\n  \
             b.raw = {b_raw} (0x{b_hex:016x})\n  \
             diff  = {diff} qt",
            a_raw = a.raw(),
            a_hex = a.raw() as u64,
            b_raw = b.raw(),
            b_hex = b.raw() as u64,
            diff = a.raw().saturating_sub(b.raw()),
        );
    }
}

/// Tier 2 — `f64` epsilon comparison. Panics with absolute and relative
/// difference on mismatch.
///
/// `epsilon` should typically be [`DEFAULT_SIGNAL_EPSILON`] (1e-10).
/// Special-cases NaN: any NaN on either side is treated as a hard
/// mismatch (NaN never equals itself, and signal pipelines should never
/// emit NaN — see the `is_finite` guards in the OBI/VPIN/microprice
/// implementations).
pub fn assert_signal_epsilon(a: f64, b: f64, epsilon: f64) {
    if a.is_nan() || b.is_nan() {
        panic!(
            "Tier 2 mismatch: signal value is NaN\n  a = {a}\n  b = {b}",
            a = a,
            b = b,
        );
    }
    let abs_diff = (a - b).abs();
    if abs_diff > epsilon {
        let denom = a.abs().max(b.abs()).max(f64::MIN_POSITIVE);
        let rel_diff = abs_diff / denom;
        panic!(
            "Tier 2 mismatch: signal values differ beyond epsilon {eps:e}\n  \
             a       = {a}\n  \
             b       = {b}\n  \
             |a-b|   = {abs:e}\n  \
             rel     = {rel:e}",
            eps = epsilon,
            a = a,
            b = b,
            abs = abs_diff,
            rel = rel_diff,
        );
    }
}

/// Tier 3 — discrete [`RegimeState`] equality. Panics with both variants on
/// mismatch.
pub fn assert_regime_identical(a: &RegimeState, b: &RegimeState) {
    if a != b {
        panic!(
            "Tier 3 mismatch: RegimeState differs\n  \
             a = {a:?}\n  \
             b = {b:?}",
        );
    }
}

/// Detail of a single Tier 1 (fixed-point) mismatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixedPriceMismatch {
    /// Index into the captured value vector where the mismatch occurred.
    pub index: usize,
    /// Raw `i64` value from the first run.
    pub expected_raw: i64,
    /// Raw `i64` value from the second run.
    pub actual_raw: i64,
}

impl FixedPriceMismatch {
    pub fn diff_qt(&self) -> i64 {
        self.expected_raw.saturating_sub(self.actual_raw)
    }
}

/// Detail of a single Tier 2 (signal) mismatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalMismatch {
    pub index: usize,
    pub expected: f64,
    pub actual: f64,
    pub abs_diff: f64,
}

/// Detail of a single Tier 3 (regime) mismatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeMismatch {
    pub index: usize,
    pub expected: RegimeState,
    pub actual: RegimeState,
}

/// Snapshot-comparison mismatch reported per-snapshot, per-field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMismatch {
    /// Which snapshot pair (0-indexed) the mismatch came from.
    pub snapshot_index: usize,
    /// Signal name (e.g. `"obi"`, `"vpin"`, `"microprice"`).
    pub signal: String,
    /// Field of the snapshot that differed (`"value"`, `"valid"`, `"timestamp"`).
    pub field: String,
    /// Human-readable description of the divergence.
    pub detail: String,
}

/// Aggregate result of a [`super::ReplayResult::compare`] call.
///
/// `is_deterministic` is true ONLY when all three tiers report zero
/// mismatches. The detail vectors retain every individual divergence so
/// reviewers see the full picture rather than just the first failure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeterminismReport {
    pub tier1_mismatches: Vec<FixedPriceMismatch>,
    pub tier2_mismatches: Vec<SignalMismatch>,
    pub tier3_mismatches: Vec<RegimeMismatch>,
    pub snapshot_mismatches: Vec<SnapshotMismatch>,
    /// True iff every tier passed (i.e. all four mismatch lists are empty).
    pub is_deterministic: bool,
}

impl DeterminismReport {
    /// Total mismatch count across all tiers.
    pub fn total_mismatches(&self) -> usize {
        self.tier1_mismatches.len()
            + self.tier2_mismatches.len()
            + self.tier3_mismatches.len()
            + self.snapshot_mismatches.len()
    }

    /// Recompute `is_deterministic` after mutation. Idempotent.
    pub fn finalize(&mut self) {
        self.is_deterministic = self.total_mismatches() == 0;
    }
}

impl std::fmt::Display for DeterminismReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const SHOW_FIRST_N: usize = 5;
        if self.is_deterministic {
            return write!(f, "DeterminismReport: deterministic ✓ (no mismatches)");
        }
        writeln!(
            f,
            "DeterminismReport: non-deterministic ({total} mismatches)",
            total = self.total_mismatches(),
        )?;
        writeln!(
            f,
            "  Tier 1 (fixed-point):   {n}",
            n = self.tier1_mismatches.len()
        )?;
        for m in self.tier1_mismatches.iter().take(SHOW_FIRST_N) {
            writeln!(
                f,
                "    [{idx}] expected={exp} actual={act} (diff={diff} qt)",
                idx = m.index,
                exp = m.expected_raw,
                act = m.actual_raw,
                diff = m.diff_qt(),
            )?;
        }
        if self.tier1_mismatches.len() > SHOW_FIRST_N {
            writeln!(
                f,
                "    ... +{n} more",
                n = self.tier1_mismatches.len() - SHOW_FIRST_N
            )?;
        }

        writeln!(
            f,
            "  Tier 2 (signals @ ε):    {n}",
            n = self.tier2_mismatches.len()
        )?;
        for m in self.tier2_mismatches.iter().take(SHOW_FIRST_N) {
            writeln!(
                f,
                "    [{idx}] expected={exp} actual={act} |diff|={d:e}",
                idx = m.index,
                exp = m.expected,
                act = m.actual,
                d = m.abs_diff,
            )?;
        }
        if self.tier2_mismatches.len() > SHOW_FIRST_N {
            writeln!(
                f,
                "    ... +{n} more",
                n = self.tier2_mismatches.len() - SHOW_FIRST_N
            )?;
        }

        writeln!(
            f,
            "  Tier 3 (regime):         {n}",
            n = self.tier3_mismatches.len()
        )?;
        for m in self.tier3_mismatches.iter().take(SHOW_FIRST_N) {
            writeln!(
                f,
                "    [{idx}] expected={exp:?} actual={act:?}",
                idx = m.index,
                exp = m.expected,
                act = m.actual,
            )?;
        }
        if self.tier3_mismatches.len() > SHOW_FIRST_N {
            writeln!(
                f,
                "    ... +{n} more",
                n = self.tier3_mismatches.len() - SHOW_FIRST_N
            )?;
        }

        writeln!(
            f,
            "  Snapshots (signals):    {n}",
            n = self.snapshot_mismatches.len()
        )?;
        for m in self.snapshot_mismatches.iter().take(SHOW_FIRST_N) {
            writeln!(
                f,
                "    [{idx}] {sig}.{field}: {detail}",
                idx = m.snapshot_index,
                sig = m.signal,
                field = m.field,
                detail = m.detail,
            )?;
        }
        if self.snapshot_mismatches.len() > SHOW_FIRST_N {
            writeln!(
                f,
                "    ... +{n} more",
                n = self.snapshot_mismatches.len() - SHOW_FIRST_N
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_price_identical_passes_on_equal_values() {
        assert_fixed_price_identical(FixedPrice::new(17_929), FixedPrice::new(17_929));
    }

    #[test]
    #[should_panic(expected = "Tier 1 mismatch")]
    fn fixed_price_identical_panics_on_diff() {
        assert_fixed_price_identical(FixedPrice::new(17_929), FixedPrice::new(17_930));
    }

    #[test]
    fn signal_epsilon_passes_on_equal_values() {
        assert_signal_epsilon(0.123_456_789_012_3, 0.123_456_789_012_3, 1e-10);
    }

    #[test]
    fn signal_epsilon_passes_within_tolerance() {
        // Difference is ~1e-12, well below 1e-10.
        assert_signal_epsilon(0.5, 0.5 + 1e-12, DEFAULT_SIGNAL_EPSILON);
    }

    #[test]
    #[should_panic(expected = "Tier 2 mismatch")]
    fn signal_epsilon_panics_outside_tolerance() {
        assert_signal_epsilon(0.5, 0.5 + 1e-3, DEFAULT_SIGNAL_EPSILON);
    }

    #[test]
    #[should_panic(expected = "is NaN")]
    fn signal_epsilon_panics_on_nan() {
        assert_signal_epsilon(f64::NAN, 0.5, DEFAULT_SIGNAL_EPSILON);
    }

    #[test]
    fn regime_identical_passes_on_equal_variants() {
        assert_regime_identical(&RegimeState::Trending, &RegimeState::Trending);
    }

    #[test]
    #[should_panic(expected = "Tier 3 mismatch")]
    fn regime_identical_panics_on_diff() {
        assert_regime_identical(&RegimeState::Trending, &RegimeState::Volatile);
    }

    #[test]
    fn report_is_deterministic_when_empty() {
        let mut r = DeterminismReport::default();
        r.finalize();
        assert!(r.is_deterministic);
        assert_eq!(r.total_mismatches(), 0);
    }

    #[test]
    fn report_is_non_deterministic_with_mismatches() {
        let mut r = DeterminismReport::default();
        r.tier1_mismatches.push(FixedPriceMismatch {
            index: 0,
            expected_raw: 1,
            actual_raw: 2,
        });
        r.finalize();
        assert!(!r.is_deterministic);
        assert_eq!(r.total_mismatches(), 1);
    }

    #[test]
    fn report_display_shows_summary_when_deterministic() {
        let mut r = DeterminismReport::default();
        r.finalize();
        let s = format!("{r}");
        assert!(s.contains("deterministic"));
    }

    #[test]
    fn report_display_lists_first_mismatches_when_failing() {
        let mut r = DeterminismReport::default();
        for i in 0..10 {
            r.tier1_mismatches.push(FixedPriceMismatch {
                index: i,
                expected_raw: 100 + i as i64,
                actual_raw: 100 + i as i64 + 1,
            });
        }
        r.finalize();
        let s = format!("{r}");
        assert!(s.contains("non-deterministic"));
        assert!(s.contains("10 mismatches") || s.contains("(10"));
        assert!(s.contains("+5 more"));
    }
}
