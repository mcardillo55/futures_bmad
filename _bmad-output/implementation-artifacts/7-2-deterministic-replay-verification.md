# Story 7.2: Deterministic Replay Verification

Status: ready-for-dev

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
- 1.1: Create `crates/engine/src/replay/determinism.rs` with comparison functions for each tier
- 1.2: Implement `assert_fixed_price_identical(a: FixedPrice, b: FixedPrice)` — bitwise comparison, panic with hex dump on mismatch
- 1.3: Implement `assert_signal_epsilon(a: f64, b: f64, epsilon: f64)` — default epsilon 1e-10, reports absolute and relative difference on failure
- 1.4: Implement `assert_regime_identical(a: &RegimeState, b: &RegimeState)` — exact enum variant and field comparison
- 1.5: Update `crates/engine/src/replay/mod.rs` to declare `pub mod determinism`

### Task 2: Implement ReplayResult capture for comparison (AC: all values captured for comparison)
- 2.1: Define `ReplayResult` struct containing: `Vec<FixedPrice>` (all trade P&L values), `Vec<f64>` (signal values at each bar), `Vec<RegimeState>` (regime at each bar boundary), summary stats (total events, trade count, net P&L)
- 2.2: Implement `ReplayOrchestrator::run_with_capture() -> ReplayResult` that runs replay and collects all comparison-relevant data
- 2.3: Implement `Serialize`/`Deserialize` for `ReplayResult` (serde) to enable snapshot persistence
- 2.4: Implement `ReplayResult::compare(&self, other: &ReplayResult) -> DeterminismReport` that applies three-tier comparison and collects all mismatches

### Task 3: Implement Signal::reset() verification (AC: no residual state)
- 3.1: Ensure `ReplayOrchestrator` calls `Signal::reset()` on ALL signal instances before beginning replay — this is mandatory, not optional
- 3.2: Add a pre-replay assertion that verifies signal state is at initial values after reset (use `Signal::snapshot()` and compare to known initial state)
- 3.3: Test that running replay, then running again WITHOUT reset produces different results (proving reset is necessary)
- 3.4: Test that running replay, then running again WITH reset produces identical results (proving reset works)

### Task 4: Implement Signal::snapshot() interval capture (AC: snapshots at configurable intervals)
- 4.1: Add `snapshot_interval: Option<u64>` to `ReplayConfig` — number of events between snapshots (None = no snapshots, Some(1000) = every 1000 events)
- 4.2: During replay, call `Signal::snapshot()` at the configured interval and store snapshots in `ReplayResult`
- 4.3: Implement snapshot comparison in `ReplayResult::compare()` — apply three-tier model: FixedPrice fields bitwise, f64 fields epsilon, RegimeState exact
- 4.4: Log snapshot capture count at replay completion

### Task 5: Implement DeterminismReport (AC: detailed mismatch reporting)
- 5.1: Define `DeterminismReport` struct with fields: `tier1_mismatches: Vec<FixedPriceMismatch>`, `tier2_mismatches: Vec<SignalMismatch>`, `tier3_mismatches: Vec<RegimeMismatch>`, `is_deterministic: bool`
- 5.2: Each mismatch type includes: event index, expected value, actual value, difference (for numeric types)
- 5.3: Implement `Display` for `DeterminismReport` — human-readable summary with first N mismatches shown in detail
- 5.4: `is_deterministic` is true only when all three tiers pass

### Task 6: Create integration test with insta snapshots (AC: regression testing)
- 6.1: Create `crates/engine/tests/replay_determinism.rs`
- 6.2: Create a small, committed test dataset in `crates/engine/tests/data/determinism_test.parquet` — known market data with enough events to exercise signals, regime, and trades (suggest 500-1000 events)
- 6.3: Create a committed test config in `crates/engine/tests/data/determinism_test.toml`
- 6.4: Test `replay_produces_identical_results`: run replay twice with same data/config, assert `ReplayResult::compare()` reports `is_deterministic == true`
- 6.5: Test `replay_determinism_snapshot`: run replay once, compare `ReplayResult` against insta snapshot — any change in output fails the test
- 6.6: Test `signal_reset_required`: run replay twice WITHOUT reset, assert results differ; run twice WITH reset, assert results match
- 6.7: Test `snapshot_interval_comparison`: run replay twice with snapshot_interval=100, compare all snapshots

### Task 7: Add insta dependency and configure snapshot storage (AC: insta crate integration)
- 7.1: Add `insta` crate as a dev-dependency in `crates/engine/Cargo.toml`
- 7.2: Configure insta snapshot storage directory (`crates/engine/tests/snapshots/`)
- 7.3: Committed snapshots serve as the regression baseline — any change to replay output requires explicit snapshot update (`cargo insta review`)
- 7.4: Document in dev notes that snapshot updates require manual review to verify the change is intentional

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
### Debug Log References
### Completion Notes List
### File List
