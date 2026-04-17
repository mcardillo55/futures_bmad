# Story 3.4: Structural Price Levels

Status: ready-for-dev

## Story

As a trader-operator,
I want key structural price levels identified automatically,
So that signals are evaluated at meaningful market levels rather than continuously.

## Acceptance Criteria (BDD)

- Given the level engine initialized Then identifies prior day high/low from historical data (previous session's Parquet or configured values), identifies volume point of control (VPOC) — the price with highest volume in the prior session, supports manually configured levels from config file
- Given structural levels computed When current price approaches a level (within configurable proximity in quarter-ticks) Then flags "at level", triggers signal evaluation (signals fire at levels, not continuously)
- Given new trading session When level engine refreshes Then prior day H/L updates to most recent completed session, VPOC recalculates from most recent session data, refresh logged with new level values
- Given no historical data (first run) When initialized Then operates with manual levels only, logs warning that automatic levels are unavailable

## Tasks / Subtasks

### Task 1: Create structural levels module (AC: module structure)
- 1.1: Create `crates/engine/src/signals/levels.rs` (or `crates/engine/src/signals/levels/mod.rs` if submodules needed)
- 1.2: Add `pub mod levels;` to `crates/engine/src/signals/mod.rs`

### Task 2: Define StructuralLevel types (AC: level identification)
- 2.1: Define `StructuralLevel` struct: `{ price: FixedPrice, level_type: LevelType, source: LevelSource }`
- 2.2: Define `LevelType` enum: `PriorDayHigh`, `PriorDayLow`, `Vpoc`, `ManualLevel`
- 2.3: Define `LevelSource` enum: `Historical` (from Parquet), `Configured` (from config file), `Computed` (derived at runtime)
- 2.4: Define `LevelProximity` struct or result: `{ level: StructuralLevel, distance: FixedPrice, at_level: bool }`

### Task 3: Implement LevelEngine struct (AC: initialization, level management, proximity detection)
- 3.1: Create `LevelEngine` struct with fields:
  - `levels: Vec<StructuralLevel>` — all active structural levels
  - `proximity_threshold: FixedPrice` — configurable distance in quarter-ticks to trigger "at level"
  - `prior_day_high: Option<FixedPrice>`
  - `prior_day_low: Option<FixedPrice>`
  - `vpoc: Option<FixedPrice>`
  - `manual_levels: Vec<FixedPrice>` — from config
- 3.2: Implement `LevelEngine::new(config: LevelConfig) -> Self`:
  - Load manual levels from config
  - Initialize with no historical levels if data unavailable
  - Log warning if no historical data: `tracing::warn!("No historical data available, operating with manual levels only")`
- 3.3: Implement `load_historical(&mut self, prior_session: &SessionData) -> Result<()>`:
  - Extract prior day high and low from session data
  - Compute VPOC: iterate price-volume data, find price with highest volume
  - Build StructuralLevel entries and add to `levels`
  - Log new level values: `tracing::info!("Structural levels loaded: PDH={}, PDL={}, VPOC={}", ...)`
- 3.4: Implement `load_from_parquet(&mut self, path: &Path) -> Result<()>`:
  - Read prior session Parquet file
  - Extract OHLC for prior day high/low
  - Aggregate volume by price for VPOC computation
  - Fall back to configured values if Parquet unavailable

### Task 4: Implement proximity detection (AC: "at level" flagging)
- 4.1: Implement `check_proximity(&self, current_price: FixedPrice) -> Vec<LevelProximity>`:
  - For each structural level, compute absolute distance in quarter-ticks
  - Flag `at_level = true` if `distance <= proximity_threshold`
  - Return all levels with proximity info
- 4.2: Implement `is_at_any_level(&self, current_price: FixedPrice) -> bool`:
  - Convenience method: returns true if any level is within proximity threshold
  - Used by composite evaluation to gate signal processing

### Task 5: Implement session refresh (AC: session transitions)
- 5.1: Implement `refresh_session(&mut self, new_session: &SessionData)`:
  - Replace prior day H/L with data from the newly completed session
  - Recompute VPOC from new session volume data
  - Rebuild `levels` vec with updated historical + existing manual levels
  - Log: `tracing::info!("Session refresh: new PDH={}, PDL={}, VPOC={}", ...)`
- 5.2: Session detection: determine when a new session starts (CME session boundaries — configurable session times)

### Task 6: Define LevelConfig (AC: configurable levels and proximity)
- 6.1: Define `LevelConfig` struct: `{ proximity_threshold_qticks: i64, manual_levels: Vec<f64>, session_times: SessionTimeConfig }`
- 6.2: Implement `From<&config::Config>` or deserialization for LevelConfig
- 6.3: Manual levels in config as f64 prices, converted to FixedPrice via `FixedPrice::from_f64()` at load time

### Task 7: Write unit tests (AC: level identification, proximity, session refresh)
- 7.1: Test: manual levels loaded correctly from config
- 7.2: Test: prior day high/low extracted from session data
- 7.3: Test: VPOC computed correctly (price with highest volume wins)
- 7.4: Test: VPOC tiebreak behavior (deterministic — e.g., lower price wins)
- 7.5: Test: proximity detection: price exactly at level returns `at_level = true`
- 7.6: Test: proximity detection: price within threshold returns `at_level = true`
- 7.7: Test: proximity detection: price outside threshold returns `at_level = false`
- 7.8: Test: session refresh updates all historical levels
- 7.9: Test: no historical data — only manual levels active, warning logged
- 7.10: Test: VPOC with empty volume data returns None
- 7.11: All tests use `testkit::SimClock`

## Dev Notes

### Architecture Patterns & Constraints
- Signals fire at levels, not continuously — this is a key architectural decision that reduces signal noise
- LevelEngine is NOT a Signal (does not implement Signal trait) — it gates when signals are evaluated
- Proximity threshold is in quarter-ticks (FixedPrice) for integer comparison — no floating point in proximity checks
- VPOC = Volume Point of Control = price with highest traded volume in prior session
- Prior day data sources: (1) Parquet files in `data/market/{SYMBOL}/{YYYY-MM-DD}.parquet`, (2) configured fallback values
- CME session boundaries vary by instrument — make session times configurable
- Level refresh must be logged for audit trail (structured tracing)
- Manual levels from config: `[signals.levels]` section in TOML config files
- This module does NOT import from `risk/` — it is consumed by composite evaluation

### Project Structure Notes
```
crates/engine/src/
├── signals/
│   ├── mod.rs          # pub mod obi; pub mod vpin; pub mod microprice; pub mod levels;
│   ├── obi.rs          # Story 3.1
│   ├── vpin.rs         # Story 3.2
│   ├── microprice.rs   # Story 3.3
│   └── levels.rs       # LevelEngine, StructuralLevel, proximity detection

data/market/{SYMBOL}/
└── {YYYY-MM-DD}.parquet   # Historical market data (source for prior day H/L, VPOC)
```

### References
- Architecture: Parquet partitioning layout, Signal & Strategy Architecture (signals fire at levels), config structure
- Epics: Epic 3, Story 3.4
- Dependencies: `core` (FixedPrice, UnixNanos), `testkit` (SimClock — dev-dependency)
- Data: Parquet files via `databento` crate or standard parquet reader
- Related: Story 3.5 (composite evaluation checks LevelEngine before evaluating signals)

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
