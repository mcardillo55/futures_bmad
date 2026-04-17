# Story 6.1: Threshold-Based Regime Detector

Status: ready-for-dev

## Story

As a trader-operator,
I want the market regime classified in real-time,
So that the system only trades when conditions match the active strategy's edge.

## Acceptance Criteria (BDD)

- Given `engine/src/regime/threshold.rs` When `ThresholdRegimeDetector` implemented Then implements `RegimeDetector` trait from `core/src/traits/regime.rs`, processes 1-min or 5-min `Bar` data (configurable interval, not every tick), classifies regime as `Trending`, `Rotational`, `Volatile`, or `Unknown`, classification uses configurable thresholds on: volatility (ATR-based), directional persistence, and range-to-body ratio
- Given startup or insufficient data When fewer bars than the warmup period have been processed Then `current()` returns `RegimeState::Unknown`, `Unknown` regime blocks trading by default (configurable)
- Given known market data sequences (via testkit scenarios) When the regime detector processes them Then a clear trending sequence (monotonically increasing bars) classifies as `Trending`, a choppy sideways sequence classifies as `Rotational`, a high-ATR sequence with no direction classifies as `Volatile`, all tests use `SimClock`

## Tasks / Subtasks

### Task 1: Create regime module structure in engine crate (AC: module exists)
- 1.1: Create `crates/engine/src/regime/mod.rs` with `pub mod threshold;` declaration
- 1.2: Update `crates/engine/src/lib.rs` to declare `pub mod regime`

### Task 2: Implement ThresholdRegimeDetector config struct (AC: configurable thresholds and interval)
- 2.1: Create `crates/engine/src/regime/threshold.rs` with `ThresholdRegimeConfig` struct containing:
  - `warmup_period: usize` (number of bars before classification begins)
  - `bar_interval_minutes: u64` (1 or 5, determines which bars to consume)
  - `atr_period: usize` (lookback for ATR calculation, e.g. 14)
  - `atr_trending_threshold: f64` (ATR below this = not volatile)
  - `atr_volatile_threshold: f64` (ATR above this = volatile)
  - `directional_persistence_threshold: f64` (fraction of bars in same direction to qualify as trending, e.g. 0.7)
  - `range_body_ratio_threshold: f64` (high range-to-body ratio = rotational/choppy, e.g. 3.0)
  - `unknown_blocks_trading: bool` (default true)
- 2.2: Implement `Default` for `ThresholdRegimeConfig` with sensible starting values
- 2.3: Derive `Debug, Clone, serde::Deserialize` on config for TOML loading

### Task 3: Implement ThresholdRegimeDetector struct (AC: RegimeDetector trait, bar-based processing)
- 3.1: Define `ThresholdRegimeDetector` struct with fields:
  - `config: ThresholdRegimeConfig`
  - `current_state: RegimeState` (initialized to `Unknown`)
  - `bar_buffer: VecDeque<Bar>` (ring buffer of recent bars, capacity = max of atr_period and warmup_period)
  - `bars_processed: usize`
  - `atr_values: VecDeque<f64>` (rolling ATR values for smoothing)
- 3.2: Implement `ThresholdRegimeDetector::new(config: ThresholdRegimeConfig) -> Self` constructor
- 3.3: Implement private method `compute_atr(&self) -> f64`:
  - Calculate True Range for each bar pair: `max(high-low, |high-prev_close|, |low-prev_close|)`
  - Use FixedPrice `to_f64()` for the computation (ATR is a signal-domain value, f64 is permitted)
  - Average over `config.atr_period` bars
- 3.4: Implement private method `compute_directional_persistence(&self) -> f64`:
  - Count fraction of recent bars where `close > open` (up bars) vs `close < open` (down bars)
  - Persistence = `max(up_fraction, down_fraction)` over the lookback window
  - High persistence (e.g. > 0.7) suggests trending
- 3.5: Implement private method `compute_avg_range_body_ratio(&self) -> f64`:
  - For each bar: range = `(high - low).to_f64()`, body = `(close - open).to_f64().abs()`
  - Ratio = `range / body` (guard: if body < epsilon, treat ratio as very high)
  - Average over lookback window
  - High ratio = indecisive/rotational candles
- 3.6: Implement private method `classify(&self) -> RegimeState`:
  - If `bars_processed < config.warmup_period`, return `Unknown`
  - Compute ATR, directional persistence, range-body ratio
  - Decision logic (V1 threshold-based):
    - If ATR > `atr_volatile_threshold` AND persistence < `directional_persistence_threshold`: `Volatile`
    - If persistence >= `directional_persistence_threshold` AND ATR <= `atr_volatile_threshold`: `Trending`
    - Otherwise: `Rotational`

### Task 4: Implement RegimeDetector trait (AC: trait contract satisfied)
- 4.1: Implement `RegimeDetector` for `ThresholdRegimeDetector`:
  - `update(&mut self, bar: &Bar, clock: &dyn Clock) -> RegimeState`:
    - Push bar into `bar_buffer` (evict oldest if at capacity)
    - Increment `bars_processed`
    - Call `classify()` and store result in `current_state`
    - Return `current_state`
    - Note: `clock` parameter accepted per trait contract; V1 may not use it directly but it is available for future time-based logic
  - `current(&self) -> RegimeState`:
    - Return `self.current_state`

### Task 5: Write unit tests (AC: known sequences produce correct classifications, SimClock used)
- 5.1: Create `crates/engine/tests/regime_tests.rs` (or inline `#[cfg(test)]` module in threshold.rs)
- 5.2: Test: fewer bars than warmup period -> `current()` returns `Unknown`
- 5.3: Test: trending sequence (monotonically increasing closes, moderate ATR) -> classifies as `Trending`
  - Build bars with `testkit` helpers: each bar has `close > open`, consistent upward movement
- 5.4: Test: choppy sideways sequence (alternating up/down bars, narrow range) -> classifies as `Rotational`
  - Build bars that oscillate around a mean price with large range-to-body ratios
- 5.5: Test: volatile sequence (large ATR, no directional persistence) -> classifies as `Volatile`
  - Build bars with wide ranges and random direction
- 5.6: Test: transition from `Unknown` to a classified state after warmup bars processed
- 5.7: Test: `unknown_blocks_trading` config flag is accessible and defaults to true
- 5.8: Test: different `bar_interval_minutes` config values are accepted (1 and 5)
- 5.9: All tests use `testkit::SimClock::new()` for deterministic time
- 5.10: All test bars built using testkit bar construction helpers (or manual Bar construction with known FixedPrice values)

## Dev Notes

### Architecture Patterns & Constraints
- RegimeDetector trait defined in `core/src/traits/regime.rs`: `update(&mut self, bar: &Bar, clock: &dyn Clock) -> RegimeState` and `current(&self) -> RegimeState`
- RegimeState enum defined in `core/src/traits/regime.rs`: `Trending`, `Rotational`, `Volatile`, `Unknown`
- Bar type from `core/src/types/bar.rs`: `open`, `high`, `low`, `close` as `FixedPrice`, `volume` as `u64`, `timestamp` as `UnixNanos`
- ATR and indicator values are `f64` (signal-domain) -- `FixedPrice::to_f64()` used for computation, this is acceptable per architecture rules (f64 permitted for signal output values)
- V1 is threshold-based. HMM-based regime detection is explicitly deferred until training data is available
- Clock injected via `&dyn Clock` parameter per trait signature; V1 implementation may not use time directly but must accept it
- `VecDeque<Bar>` for ring buffer is the one heap allocation allowed at initialization; no per-update heap allocation
- Concrete type (not `Box<dyn RegimeDetector>`) used as named field in engine orchestration
- MANDATORY: `Unknown` at startup blocks trading by default (configurable via `unknown_blocks_trading`)

### Project Structure Notes
```
crates/engine/src/
├── regime/
│   ├── mod.rs              # pub mod threshold;
│   └── threshold.rs        # ThresholdRegimeDetector, ThresholdRegimeConfig

crates/core/src/
├── traits/
│   └── regime.rs           # RegimeDetector trait, RegimeState enum (defined in Story 1.5)
├── types/
│   └── bar.rs              # Bar struct (defined in Story 1.2)

crates/testkit/src/
├── sim_clock.rs            # SimClock for deterministic tests
```

### References
- Architecture: `_bmad-output/planning-artifacts/architecture.md` -- RegimeDetector trait, RegimeState enum, regime module placement
- Epics: `_bmad-output/planning-artifacts/epics.md` -- Epic 6, Story 6.1
- Dependencies: `core` (RegimeDetector trait, RegimeState enum, Bar, FixedPrice, UnixNanos, Clock), `testkit` (SimClock -- dev-dependency)
- Prerequisite stories: Story 1.2 (Bar, FixedPrice, UnixNanos types), Story 1.5 (RegimeDetector trait, Clock trait)
- Related: Story 6.2 (regime transition events and strategy enable/disable consume this detector)

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
