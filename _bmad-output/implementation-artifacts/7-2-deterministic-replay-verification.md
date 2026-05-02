# Story 7.2: Deterministic Replay Verification

Status: done

## Story

As a trader-operator,
I want identical data to produce identical results every time,
So that I can trust replay as a validation tool for signal and strategy changes.

## Acceptance Criteria (BDD)

- Given same Parquet data and config When replay run twice Then all FixedPrice values bit-identical, signal values identical within epsilon (1e-10), RegimeState identical at every bar boundary
- Given Signal::reset() called before replay When runs Then no residual state affects results
- Given Signal::snapshot() called at intervals When snapshots from two runs compared Then match exactly (three-tier determinism: fixed-point=bitwise, signals=epsilon, regime=snapshot)
- Given integration tests in `engine/tests/replay_determinism.rs` When known dataset replayed Then results compared against committed snapshots (insta crate), regression = test failure

## Tasks / Subtasks

### Task 1: Define the three-tier determinism model and comparison utilities (AC: bit-identical, epsilon, snapshot)
- [x] 1.1: Create `crates/engine/src/replay/determinism.rs` with comparison functions for each tier
- [x] 1.2: Implement `assert_fixed_price_identical(a: FixedPrice, b: FixedPrice)` — bitwise comparison, panic with hex dump on mismatch
- [x] 1.3: Implement `assert_signal_epsilon(a: f64, b: f64, epsilon: f64)` — default epsilon 1e-10, reports absolute and relative difference on failure
- [x] 1.4: Implement `assert_regime_identical(a: &RegimeState, b: &RegimeState)` — exact enum variant and field comparison
- [x] 1.5: Update `crates/engine/src/replay/mod.rs` to declare `pub mod determinism`

### Task 2: Implement ReplayResult capture for comparison (AC: all values captured for comparison)
- [x] 2.1: Define `ReplayResult` struct containing trade P&L values (raw quarter-ticks for bit-identical Tier 1), signal values per event, regime states per bar boundary, and summary stats
- [x] 2.2: Implement `ReplayOrchestrator::run_with_capture() -> ReplayResult` that runs replay and collects all comparison-relevant data
- [x] 2.3: Implement `Serialize`/`Deserialize` for `ReplayResult` (serde) to enable snapshot persistence (raw `i64` for Tier 1, snapshot wrapper for `SignalSnapshot`'s `&'static str` field)
- [x] 2.4: Implement `ReplayResult::compare(&self, other: &ReplayResult) -> DeterminismReport` that applies three-tier comparison and collects all mismatches

### Task 3: Implement Signal::reset() verification (AC: no residual state)
- [x] 3.1: Ensure `ReplayOrchestrator` calls `Signal::reset()` on ALL signal instances before beginning replay — this is mandatory, not optional (`run_with_capture` always calls `pipeline.reset()`)
- [x] 3.2: Add a pre-replay assertion that verifies signal state is at initial values after reset (`assert_initial_pipeline_state` compares post-reset snapshots against a freshly constructed reference pipeline, panicking on any field divergence)
- [x] 3.3: Test that running replay, then running again WITHOUT reset produces different results (proving reset is necessary)
- [x] 3.4: Test that running replay, then running again WITH reset produces identical results (proving reset works)

### Task 4: Implement Signal::snapshot() interval capture (AC: snapshots at configurable intervals)
- [x] 4.1: Add `snapshot_interval: Option<u64>` to `ReplayConfig` — number of events between snapshots (None = no snapshots, Some(N) = every N events)
- [x] 4.2: During replay, call `Signal::snapshot()` at the configured interval and store snapshots in `ReplayResult`
- [x] 4.3: Implement snapshot comparison in `ReplayResult::compare()` — apply three-tier model: f64 fields epsilon, valid/timestamp/name exact
- [x] 4.4: Log snapshot capture count at replay completion

### Task 5: Implement DeterminismReport (AC: detailed mismatch reporting)
- [x] 5.1: Define `DeterminismReport` struct with fields: `tier1_mismatches: Vec<FixedPriceMismatch>`, `tier2_mismatches: Vec<SignalMismatch>`, `tier3_mismatches: Vec<RegimeMismatch>`, `snapshot_mismatches: Vec<SnapshotMismatch>`, `is_deterministic: bool`
- [x] 5.2: Each mismatch type includes: event index, expected value, actual value, difference (for numeric types)
- [x] 5.3: Implement `Display` for `DeterminismReport` — human-readable summary with first N mismatches shown in detail (N=5)
- [x] 5.4: `is_deterministic` is true only when all four mismatch lists (three tiers + snapshots) are empty

### Task 6: Create integration test with insta snapshots (AC: regression testing)
- [x] 6.1: Create `crates/engine/tests/replay_determinism.rs`
- [x] 6.2: Create a small, committed test dataset in `crates/engine/tests/data/market/DETERMINISM/2026-04-18.parquet` — 1000 events covering OBI/VPIN updates, trade-flow imbalance, and ~16 1-minute regime bars. The `MarketDataWriter`-produced layout (`market/<symbol>/<date>.parquet`) is honoured for parity with live recording. The committed `.gitignore` is updated with a `!crates/engine/tests/data/` allowlist exception so the file ships with the repo despite the project-wide `data/` ignore rule.
- [x] 6.3: Create a committed test config in `crates/engine/tests/data/determinism_test.toml`
- [x] 6.4: Test `replay_produces_identical_results`: run replay twice with same data/config, assert `ReplayResult::compare()` reports `is_deterministic == true`
- [x] 6.5: Test `replay_determinism_snapshot`: run replay once, compare `ReplayResult` against insta snapshot — any change in output fails the test
- [x] 6.6: Test `signal_reset_required`: run replay twice WITHOUT reset (via `run_with_capture_no_reset`), assert results differ; run twice WITH reset, assert results match
- [x] 6.7: Test `snapshot_interval_comparison`: run replay twice with snapshot_interval=250, compare all snapshots (interval 250 yields 4 snapshots over 1000 events — enough to span warmup→fully-warm without bloating the committed snapshot)

### Task 7: Add insta dependency and configure snapshot storage (AC: insta crate integration)
- [x] 7.1: Add `insta` crate as a dev-dependency in `crates/engine/Cargo.toml` (workspace dependency 1.46.3, `yaml` feature for stable diff output)
- [x] 7.2: Configure insta snapshot storage directory (`crates/engine/tests/snapshots/replay_determinism__determinism_baseline.snap`)
- [x] 7.3: Committed snapshots serve as the regression baseline — any change to replay output requires explicit snapshot update (`cargo insta review`)
- [x] 7.4: Document in dev notes that snapshot updates require manual review to verify the change is intentional (see Completion Notes for the developer-facing note)

## Dev Notes

### Architecture Patterns & Constraints
- The three-tier determinism model is an architectural decision documented in `architecture.md`:
  1. **Tier 1 (Fixed-point):** All `FixedPrice` values (prices, P&L, fees) must be BIT-IDENTICAL between runs. This is guaranteed by integer arithmetic on quarter-tick representation — no floating-point involved.
  2. **Tier 2 (Signals):** Signal values are f64 and subject to floating-point non-associativity. Epsilon tolerance of 1e-10 accounts for this. Same binary on same hardware should produce bitwise-identical f64, but epsilon provides safety margin.
  3. **Tier 3 (Regime):** RegimeState is a discrete enum with associated data. Snapshot-based comparison at bar boundaries verifies regime transitions are deterministic.
- **Same binary on same hardware = determinism guarantee.** Cross-platform or cross-compiler comparisons use epsilon tolerance, not bitwise equality. This is explicitly documented as a boundary.
- `Signal::reset()` must clear ALL internal state — EMA accumulators, ring buffers, counters. If any signal stores state that persists across reset, determinism breaks.
- `Signal::snapshot()` returns an opaque snapshot that can be serialized. The snapshot format is signal-specific but must be `Serialize + Deserialize + PartialEq`.
- The `insta` crate provides snapshot testing with review workflow — `cargo insta test` runs tests, `cargo insta review` shows diffs for changed snapshots. This is the regression mechanism.

### Project Structure Notes
```
crates/engine/
├── src/
│   └── replay/
│       ├── mod.rs               (ReplayOrchestrator — adds run_with_capture)
│       └── determinism.rs       (comparison utilities, DeterminismReport)
├── tests/
│   ├── replay_determinism.rs    (integration tests)
│   ├── data/
│   │   ├── determinism_test.parquet  (committed test dataset)
│   │   └── determinism_test.toml    (committed test config)
│   └── snapshots/
│       └── replay_determinism/  (insta snapshots, committed)
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Three-tier determinism model, Signal trait (reset/snapshot)
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 7, Story 7.2
- Dependencies: insta (dev-dependency), serde (for ReplayResult serialization)
- Depends on: Story 7.1 (ReplayOrchestrator), Story 1.5 (Signal trait with reset/snapshot), Story 3.x (Signal implementations)
- NFR15: Deterministic replay — bit-identical results on identical input
- FR30: Deterministic reproducible replay

## Dev Agent Record

### Agent Model Used
Claude Opus 4.7 (1M context) acting as the BMad dev agent.

### Debug Log References
- `cargo build --workspace` — clean (no warnings).
- `cargo clippy --workspace --all-targets` — clean (one collapsible-if warning fixed mid-implementation).
- `cargo test --workspace` — 294 lib tests + all integration tests pass. The pre-existing `risk::alerting::tests::notification_script_receives_json_on_stdin` test was observed to be flaky once under full parallelism but passes on rerun and with `--test-threads=2`; behaviour is unchanged from baseline (Story 7.1) and unrelated to Story 7.2.
- `cargo test --test replay_determinism` — all 5 new integration tests pass.

### Completion Notes List
- **Three-tier determinism model implemented as documented in `architecture.md`.** Tier 1 (FixedPrice) compares raw quarter-tick `i64` values; Tier 2 (signals) uses `DEFAULT_SIGNAL_EPSILON = 1e-10`; Tier 3 (regime) uses exact enum equality. A fourth axis — pipeline `Signal::snapshot()` payloads at configurable intervals — applies the same per-field tier rules.
- **`ReplayOrchestrator` extended with `run_with_capture()`** that wires the existing `SignalPipeline` and an optional `ThresholdRegimeDetector` into the replay loop. The orchestrator now owns the pipeline and detector across calls so the no-reset test path (`run_with_capture_no_reset`) actually observes residual state — the production entry point always calls `pipeline.reset()` and asserts the post-reset state matches a freshly-constructed reference (Task 3.1, 3.2).
- **Signal output limitation acknowledged in test docs.** With the engine's L1-only `apply_market_event`, the order book never holds ≥3 levels per side, so OBI and microprice always report `None` — they participate in the determinism check via consistent `None` sequences. VPIN is the meaningful Tier 2 signal in V1; the test generator is explicitly tuned to a 2:1 buy:sell aggressor ratio so VPIN values are non-zero and would diverge across runs without `reset()`. Full L2 book replay is an Epic-2 follow-up.
- **Test dataset is regenerated from a committed deterministic generator if missing**, then committed at `crates/engine/tests/data/market/DETERMINISM/2026-04-18.parquet`. The `MarketDataWriter`-driven directory layout is identical to live recording. The repo's `.gitignore` was updated with a `!crates/engine/tests/data/` allowlist exception so the parquet file ships with the repo despite the project-wide `data/` rule.
- **Snapshot updates require explicit `cargo insta review`.** Any change to replay output (signal logic, regime classifier, generator, or test config TOML) shifts `replay_determinism__determinism_baseline.snap` and fails CI; the developer must inspect the diff and re-accept it. The snapshot IS the regression gate.
- **`PipelineSnapshotSerde` / `SignalSnapshotSerde` wrappers**: the core `SignalSnapshot` carries `name: &'static str` which serde can't own through a `Deserialize` round-trip. The wrapper holds an owned `String` instead so insta YAML round-trips cleanly without changing the core trait surface.
- **`RegimeState` gained a `Serialize` derive** (it already had `Deserialize`). This is the minimum surface change in `core` required for the new `ReplayResult`'s insta snapshot.

### File List
- `crates/core/src/traits/regime.rs` — added `Serialize` derive on `RegimeState`.
- `crates/engine/Cargo.toml` — added `insta` dev-dependency with `yaml` feature.
- `crates/engine/src/main.rs` — populated the new `ReplayConfig` instrumentation fields with defaults so the binary entry point still compiles.
- `crates/engine/src/replay/mod.rs` — re-exports the new determinism API.
- `crates/engine/src/replay/determinism.rs` — NEW. Three-tier assert helpers, `DeterminismReport`, and per-tier mismatch detail structs.
- `crates/engine/src/replay/orchestrator.rs` — extended `ReplayConfig` with snapshot/instrumentation knobs, added `ReplayResult` with `compare()`, added owned-pipeline + `run_with_capture()` / `run_with_capture_no_reset()` entry points, added `BarBuilder` helper for synthesising regime bars from trade events, and added the `assert_initial_pipeline_state` post-reset invariant check.
- `crates/engine/tests/replay_determinism.rs` — NEW. The five integration tests (`replay_produces_identical_results`, `replay_determinism_snapshot`, `signal_reset_required`, `snapshot_interval_comparison`, `assert_helpers_are_publicly_callable`).
- `crates/engine/tests/replay_orchestrator.rs` — kept passing by populating new `ReplayConfig` fields.
- `crates/engine/tests/data/determinism_test.toml` — NEW. Committed test config consumed by the integration tests.
- `crates/engine/tests/data/market/DETERMINISM/2026-04-18.parquet` — NEW. Committed test dataset (1000 events).
- `crates/engine/tests/snapshots/replay_determinism__determinism_baseline.snap` — NEW. Committed insta snapshot of the baseline `ReplayResult`.
- `.gitignore` — added an allowlist exception for `crates/engine/tests/data/`.

### Change Log
- 2026-05-01 — Story 7.2 implemented end-to-end. Replay orchestrator gains `run_with_capture()`, three-tier `DeterminismReport`, optional periodic `Signal::snapshot()` capture, and a committed insta-backed regression gate at `crates/engine/tests/snapshots/replay_determinism__determinism_baseline.snap`.
