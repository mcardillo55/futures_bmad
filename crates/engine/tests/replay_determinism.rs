//! Integration tests for Story 7.2 — Deterministic Replay Verification.
//!
//! Drives [`futures_bmad_engine::replay::ReplayOrchestrator::run_with_capture`]
//! against the committed test dataset
//! (`tests/data/determinism_test.parquet`, regenerated automatically from the
//! deterministic [`generate_test_events`] helper if missing) and asserts:
//!
//! 1. Two runs of the same data + config produce a `ReplayResult` whose
//!    `compare()` reports `is_deterministic == true` (Task 6.4).
//! 2. The captured `ReplayResult` matches the committed insta snapshot
//!    `replay_determinism__determinism_baseline.snap` — any drift fails CI
//!    (Task 6.5, 7.3). Snapshot updates require explicit
//!    `cargo insta review`.
//! 3. Running `run_with_capture_no_reset` back-to-back yields divergent
//!    results, while `run_with_capture` (which DOES reset) yields identical
//!    results — proving the reset call is load-bearing (Task 6.6).
//! 4. With `snapshot_interval = 250`, both runs capture the same number of
//!    pipeline snapshots and every field of every snapshot agrees within
//!    the three-tier model (Task 6.7).
//!
//! The test config is loaded from `tests/data/determinism_test.toml` so that
//! reviewers can see which knobs drive the expected output without reading
//! the integration test source. Any change to the TOML will shift the
//! snapshot — that is the regression mechanism.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType, Side, UnixNanos};
use futures_bmad_engine::persistence::MarketDataWriter;
use futures_bmad_engine::regime::ThresholdRegimeConfig;
use futures_bmad_engine::replay::{
    RegimeInstrumentationConfig, ReplayConfig, ReplayOrchestrator, SignalInstrumentationConfig,
};
use serde::Deserialize;

/// Test data directory under `crates/engine/tests/`.
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

const SYMBOL: &str = "DETERMINISM";
const TEST_DATE: (i32, u32, u32) = (2026, 4, 18);
const NUM_EVENTS: usize = 1000;

/// Deterministic event generator.
///
/// Events are interleaved bid/ask/trade updates with a slow upward drift,
/// designed to:
///   * Cycle several VPIN buckets (bucket size 50, varying trade volumes,
///     ~333 trades over 1000 events).
///   * Produce a non-zero VPIN by skewing the buy-aggressor / sell-aggressor
///     ratio (~2:1). A perfectly balanced trade flow yields `VPIN = 0`
///     regardless of internal bucket state, which would mask the
///     Story 7.2 Task 6.6 demonstration that `signal_reset_required` —
///     two runs without reset would still produce all-zero output and
///     spuriously pass the "deterministic" check.
///   * Generate enough trade prints over a 1000-second span (1 event per
///     second) to fill ~16 one-minute regime bars with a clear upward drift,
///     so the regime classifier reliably returns Trending after warmup.
///
/// Note on OBI / microprice: with the engine's L1-only `apply_market_event`
/// implementation, the order book never holds more than one bid and one ask
/// level. `OrderBook::is_tradeable` requires `bid_count >= 3 && ask_count >=
/// 3`, so OBI and microprice always report `None` here. They participate in
/// the determinism check by virtue of the captured `None` series — `None ==
/// None` across runs — but the meaningful signal-state determinism gate is
/// VPIN. This is a known V1 limitation: full L2 book replay is an Epic-2
/// follow-up.
///
/// Producing the same byte sequence on every call is the ENTIRE source of
/// determinism for this test file — any nondeterminism here would cascade
/// into nondeterminism in the snapshot.
pub fn generate_test_events() -> Vec<MarketEvent> {
    let ts0 = 1_776_470_400_000_000_000u64; // 2026-04-18 00:00:00 UTC
    let mut events = Vec::with_capacity(NUM_EVENTS);
    let mut bid_price: i64 = 18_000;
    let mut ask_price: i64 = 18_004;
    for i in 0..NUM_EVENTS {
        let ts = ts0 + (i as u64) * 1_000_000_000; // one event per second
        let kind = i % 3;
        let event = match kind {
            0 => MarketEvent {
                timestamp: UnixNanos::new(ts),
                symbol_id: 0,
                sequence: i as u64,
                event_type: MarketEventType::BidUpdate,
                price: FixedPrice::new(bid_price),
                size: 100 + (i as u32 % 16),
                side: Some(Side::Buy),
            },
            1 => MarketEvent {
                timestamp: UnixNanos::new(ts),
                symbol_id: 0,
                sequence: i as u64,
                event_type: MarketEventType::AskUpdate,
                price: FixedPrice::new(ask_price),
                size: 100 + (i as u32 % 16),
                side: Some(Side::Sell),
            },
            _ => {
                // 2:1 buy:sell aggressor ratio — Sell every third trade.
                // The VPIN imbalance therefore settles around 1/3.
                let trade_idx = i / 3;
                let aggressor = if trade_idx.is_multiple_of(3) {
                    Side::Sell
                } else {
                    Side::Buy
                };
                let price = if aggressor == Side::Buy {
                    FixedPrice::new(ask_price)
                } else {
                    FixedPrice::new(bid_price)
                };
                // Vary per-trade volume so VPIN bucket boundaries fall at
                // different positions within a sequence — without varying
                // sizes a back-to-back replay can land identical bucket
                // contents purely by accident.
                let size = 1 + (trade_idx as u32 % 7);
                MarketEvent {
                    timestamp: UnixNanos::new(ts),
                    symbol_id: 0,
                    sequence: i as u64,
                    event_type: MarketEventType::Trade,
                    price,
                    size,
                    side: Some(aggressor),
                }
            }
        };
        events.push(event);

        // Slow upward drift every 10 events so the regime detector classifies
        // as Trending after the warmup window. The drift is in raw quarter-
        // ticks (each tick is 4 raw units, so price advances 1 tick every 10
        // events).
        if i.is_multiple_of(10) {
            bid_price += 1;
            ask_price += 1;
        }
    }
    events
}

/// Ensure the committed parquet test dataset exists. If missing (e.g. in a
/// fresh worktree), regenerate it from [`generate_test_events`]. This keeps
/// the test self-contained while honouring Task 6.2's directive that the
/// dataset live at a stable path under `tests/data/`.
fn ensure_dataset() -> PathBuf {
    let dir = data_dir();
    let date = NaiveDate::from_ymd_opt(TEST_DATE.0, TEST_DATE.1, TEST_DATE.2).unwrap();
    let target = futures_bmad_engine::persistence::parquet_writer::file_path_for(
        &dir, SYMBOL, date,
    );
    if !target.exists() {
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let mut writer = MarketDataWriter::new(dir.clone(), SYMBOL.to_string());
        for ev in generate_test_events() {
            writer.write_event(&ev).unwrap();
        }
        writer.flush().unwrap();
        assert!(
            target.exists(),
            "expected MarketDataWriter to produce {} but it was not created",
            target.display(),
        );
    }
    target
}

#[derive(Debug, Deserialize)]
struct TestSignalsConfig {
    obi_warmup: u64,
    max_spread_qt: i64,
    vpin_bucket_size: u64,
    vpin_num_buckets: usize,
}

#[derive(Debug, Deserialize)]
struct TestRegimeConfig {
    bar_interval_nanos: u64,
    warmup_period: usize,
    bar_interval_minutes: u64,
    atr_period: usize,
    atr_trending_threshold: f64,
    atr_volatile_threshold: f64,
    directional_persistence_threshold: f64,
    range_body_ratio_threshold: f64,
    unknown_blocks_trading: bool,
}

#[derive(Debug, Deserialize)]
struct TestSnapshotsConfig {
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct TestConfigFile {
    signals: TestSignalsConfig,
    regime: TestRegimeConfig,
    snapshots: TestSnapshotsConfig,
}

/// Load the committed `determinism_test.toml` and translate it into a
/// fully populated [`ReplayConfig`].
fn load_test_config(parquet_path: PathBuf) -> ReplayConfig {
    let toml_path = data_dir().join("determinism_test.toml");
    let toml_src = std::fs::read_to_string(&toml_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", toml_path.display()));
    let parsed: TestConfigFile = toml::from_str(&toml_src)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", toml_path.display()));

    ReplayConfig {
        parquet_path,
        fill_model: futures_bmad_engine::replay::FillModel::ImmediateAtMarket,
        summary_output: false,
        market_event_capacity: None,
        snapshot_interval: Some(parsed.snapshots.interval),
        signal_instrumentation: SignalInstrumentationConfig {
            obi_warmup: parsed.signals.obi_warmup,
            max_spread_qt: parsed.signals.max_spread_qt,
            vpin_bucket_size: parsed.signals.vpin_bucket_size,
            vpin_num_buckets: parsed.signals.vpin_num_buckets,
        },
        regime_instrumentation: Some(RegimeInstrumentationConfig {
            bar_interval_nanos: parsed.regime.bar_interval_nanos,
            detector: ThresholdRegimeConfig {
                warmup_period: parsed.regime.warmup_period,
                bar_interval_minutes: parsed.regime.bar_interval_minutes,
                atr_period: parsed.regime.atr_period,
                atr_trending_threshold: parsed.regime.atr_trending_threshold,
                atr_volatile_threshold: parsed.regime.atr_volatile_threshold,
                directional_persistence_threshold: parsed.regime.directional_persistence_threshold,
                range_body_ratio_threshold: parsed.regime.range_body_ratio_threshold,
                unknown_blocks_trading: parsed.regime.unknown_blocks_trading,
            },
        }),
    }
}

/// 6.4 — running replay twice with the same data + config produces an
/// identical [`ReplayResult`] under the three-tier comparison.
#[test]
fn replay_produces_identical_results() {
    let parquet = ensure_dataset();
    let cfg = load_test_config(parquet);

    // Two independent orchestrators, each calling `run_with_capture` once,
    // mirror the real "did the pipeline drift between two operator-driven
    // re-runs?" question.
    let mut orch_a = ReplayOrchestrator::new(cfg.clone()).unwrap();
    let result_a = orch_a.run_with_capture();
    let mut orch_b = ReplayOrchestrator::new(cfg).unwrap();
    let result_b = orch_b.run_with_capture();

    let report = result_a.compare(&result_b);
    assert!(
        report.is_deterministic,
        "expected deterministic replay; got\n{report}"
    );
    assert_eq!(report.total_mismatches(), 0);
    assert!(
        result_a.total_events > 0,
        "test would silently pass if no events flowed"
    );
}

/// 6.5 — committed snapshot of a single replay's output. Any change in
/// signals, regime, P&L, or snapshot intervals will change this artefact and
/// require explicit `cargo insta review`. The review IS the regression
/// gate.
///
/// Because per-run wall clock and elapsed time fields are absent from
/// `ReplayResult` (we deliberately excluded them), this snapshot only covers
/// the deterministic state that should never drift between runs.
#[test]
fn replay_determinism_snapshot() {
    let parquet = ensure_dataset();
    let cfg = load_test_config(parquet);
    let mut orch = ReplayOrchestrator::new(cfg).unwrap();
    let result = orch.run_with_capture();

    // Sanity: the dataset must produce some non-trivial output, otherwise
    // an empty snapshot would falsely pass.
    assert_eq!(result.total_events, NUM_EVENTS as u64);
    assert!(
        !result.obi_values.is_empty(),
        "OBI series should be non-empty"
    );
    assert!(
        !result.regime_states.is_empty(),
        "regime series should be non-empty"
    );
    assert!(
        !result.snapshots.is_empty(),
        "snapshots series should be non-empty (interval was set)"
    );

    // The committed snapshot lives at
    // `crates/engine/tests/snapshots/replay_determinism__determinism_baseline.snap`.
    insta::assert_yaml_snapshot!("determinism_baseline", result);
}

/// 6.6 — `run_with_capture` calls `pipeline.reset()`, while
/// `run_with_capture_no_reset` does not. Running BACK-TO-BACK on the SAME
/// orchestrator must therefore yield divergent results when reset is
/// skipped, and identical results when reset is honoured.
#[test]
fn signal_reset_required() {
    let parquet = ensure_dataset();
    let cfg = load_test_config(parquet);

    // --- Scenario A: no reset between runs ⇒ second run is contaminated by
    // residual signal state from the first run, so values must differ.
    let mut orch = ReplayOrchestrator::new(cfg.clone()).unwrap();
    let first = orch.run_with_capture_no_reset();
    let second = orch.run_with_capture_no_reset();
    let dirty_report = first.compare(&second);
    assert!(
        !dirty_report.is_deterministic,
        "without reset, leftover state should produce different signal values; \
         got deterministic=true (this means reset is no longer load-bearing or \
         the test data does not exercise stateful signals)\n{dirty_report}"
    );

    // --- Scenario B: with reset (the production code path), back-to-back
    // runs are bit-for-bit identical (Tier 1) and epsilon-identical
    // (Tier 2) and snapshot-identical (Tier 3).
    let mut orch_clean = ReplayOrchestrator::new(cfg).unwrap();
    let clean_a = orch_clean.run_with_capture();
    let clean_b = orch_clean.run_with_capture();
    let clean_report = clean_a.compare(&clean_b);
    assert!(
        clean_report.is_deterministic,
        "with reset, back-to-back runs must be deterministic; \
         got\n{clean_report}"
    );
}

/// 6.7 — periodic `Signal::snapshot()` capture. With `snapshot_interval` set
/// from the TOML, both runs must capture the same number of snapshots and
/// each snapshot must be identical field-by-field across the two runs.
#[test]
fn snapshot_interval_comparison() {
    let parquet = ensure_dataset();
    let cfg = load_test_config(parquet);

    let mut orch_a = ReplayOrchestrator::new(cfg.clone()).unwrap();
    let result_a = orch_a.run_with_capture();
    let mut orch_b = ReplayOrchestrator::new(cfg).unwrap();
    let result_b = orch_b.run_with_capture();

    assert_eq!(
        result_a.snapshots.len(),
        result_b.snapshots.len(),
        "snapshot count diverged: a={} b={}",
        result_a.snapshots.len(),
        result_b.snapshots.len(),
    );
    assert!(
        !result_a.snapshots.is_empty(),
        "test config snapshot interval should produce at least one snapshot"
    );

    let report = result_a.compare(&result_b);
    assert!(
        report.snapshot_mismatches.is_empty(),
        "expected zero snapshot mismatches; got {} mismatches\n{report}",
        report.snapshot_mismatches.len(),
    );
    // Defence in depth: assert the raw vectors match, not just compare()'s
    // verdict. Catches a future bug where `compare` accidentally short-
    // circuits without iterating.
    assert_eq!(
        result_a.snapshots, result_b.snapshots,
        "snapshot vectors must be PartialEq across deterministic runs"
    );
}

/// Cross-check on the determinism helpers (Story 7.2 Task 1). These mirror
/// the unit tests in `determinism.rs` but exercise them through the public
/// re-export to confirm the API surface is consumer-usable.
#[test]
fn assert_helpers_are_publicly_callable() {
    use futures_bmad_engine::replay::{
        DEFAULT_SIGNAL_EPSILON, assert_fixed_price_identical, assert_regime_identical,
        assert_signal_epsilon,
    };
    use futures_bmad_core::RegimeState;

    assert_fixed_price_identical(FixedPrice::new(42), FixedPrice::new(42));
    assert_signal_epsilon(0.5, 0.5, DEFAULT_SIGNAL_EPSILON);
    assert_regime_identical(&RegimeState::Trending, &RegimeState::Trending);
}
