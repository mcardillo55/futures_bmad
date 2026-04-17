# Story 3.4: Structural Price Levels

Status: done

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
- [x] 1.1: Create `crates/engine/src/signals/levels.rs`
- [x] 1.2: Add `pub mod levels;` to `crates/engine/src/signals/mod.rs`

### Task 2: Define StructuralLevel types (AC: level identification)
- [x] 2.1: Define `StructuralLevel` struct: `{ price: FixedPrice, level_type: LevelType, source: LevelSource }`
- [x] 2.2: Define `LevelType` enum: `PriorDayHigh`, `PriorDayLow`, `Vpoc`, `ManualLevel`
- [x] 2.3: Define `LevelSource` enum: `Historical`, `Configured`, `Computed`
- [x] 2.4: Define `LevelProximity` struct: `{ level: StructuralLevel, distance: FixedPrice, at_level: bool }`

### Task 3: Implement LevelEngine struct (AC: initialization, level management, proximity detection)
- [x] 3.1: Create `LevelEngine` struct with all specified fields
- [x] 3.2: Implement `LevelEngine::new(config: LevelConfig) -> Self` with warning logging
- [x] 3.3: Implement `load_historical(&mut self, session: &SessionData)` with VPOC computation and structured logging
- [x] 3.4: Implement `load_from_parquet(&mut self, path: &Path) -> Result<()>` using ParquetDataSource

### Task 4: Implement proximity detection (AC: "at level" flagging)
- [x] 4.1: Implement `check_proximity(&self, current_price: FixedPrice) -> Vec<LevelProximity>`
- [x] 4.2: Implement `is_at_any_level(&self, current_price: FixedPrice) -> bool`

### Task 5: Implement session refresh (AC: session transitions)
- [x] 5.1: Implement `refresh_session(&mut self, new_session: &SessionData)` with rebuild and logging
- [x] 5.2: Session detection deferred to composite evaluation layer (configurable session times)

### Task 6: Define LevelConfig (AC: configurable levels and proximity)
- [x] 6.1: Define `LevelConfig` struct with proximity_threshold and manual_levels
- [x] 6.2: Implement `LevelConfig::new()` constructor with f64-to-FixedPrice conversion
- [x] 6.3: Manual levels converted via `FixedPrice::from_f64()` at load time

### Task 7: Write unit tests (AC: level identification, proximity, session refresh)
- [x] 7.1: Test: manual levels loaded correctly from config
- [x] 7.2: Test: prior day high/low extracted from session data
- [x] 7.3: Test: VPOC computed correctly (price with highest volume wins)
- [x] 7.4: Test: VPOC tiebreak behavior (lower price wins)
- [x] 7.5: Test: proximity detection: price exactly at level returns `at_level = true`
- [x] 7.6: Test: proximity detection: price within threshold returns `at_level = true`
- [x] 7.7: Test: proximity detection: price outside threshold returns `at_level = false`
- [x] 7.8: Test: session refresh updates all historical levels
- [x] 7.9: Test: no historical data — only manual levels active
- [x] 7.10: Test: VPOC with empty volume data returns None
- [x] 7.11: Test: is_at_any_level convenience method

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
Claude Opus 4.6 (1M context)

### Debug Log References
No issues. Minor clippy fix (format! -> to_string()).

### Completion Notes List
- LevelEngine manages structural price levels (PDH, PDL, VPOC, manual) with proximity detection
- SessionData type builds H/L/VPOC from trade iterator with deterministic tiebreak (lower price wins)
- Parquet integration via existing ParquetDataSource for historical data loading
- Integer proximity check in quarter-ticks — no floating point in distance comparison
- Structured tracing for level loading and session refresh
- LevelConfig with f64-to-FixedPrice conversion for manual levels
- 11 tests covering all ACs
- All 192 workspace tests pass, zero clippy warnings

### Change Log
- 2026-04-17: Implemented Story 3.4 — Structural Price Levels (all tasks complete)

### File List
- crates/engine/src/signals/levels.rs (new)
- crates/engine/src/signals/mod.rs (modified — added pub mod levels and re-exports)
- crates/engine/tests/levels_tests.rs (new)
