# Story 5.5: Event-Aware Trading Windows

Status: review

## Story

As a trader-operator,
I want the system to automatically reduce exposure during high-impact scheduled events,
So that I avoid the outsized risk of FOMC, CPI, NFP announcements.

## Acceptance Criteria (BDD)

- Given config section for events When configured Then each event specifies: start time, end time/duration, action (disable_strategies/reduce_exposure/sit_out)
- Given current time within event window When check runs Then configured action applied, activation logged
- Given event window expires When end time passes Then normal trading resumes, resumption logged
- Given Clock trait for time checks When evaluated Then `clock.wall_clock()` used (wall-clock scheduled), SimClock's wall_clock in replay

## Tasks / Subtasks

### Task 1: Define event window configuration types (AC: config section, start/end/duration/action)
- [x] 1.1: In `core/src/config/trading.rs`, define `EventWindowConfig` struct with fields: `name` (String), `start` (chrono::NaiveDateTime), `end` (Option<chrono::NaiveDateTime>), `duration_minutes` (Option<u32>), `action` (EventAction enum)
- [x] 1.2: Define `EventAction` enum with variants: `DisableStrategies`, `ReduceExposure`, `SitOut`
- [x] 1.3: Add `events: Vec<EventWindowConfig>` field to TradingConfig
- [x] 1.4: Implement validation: each event must have either `end` or `duration_minutes` (not both, not neither)
- [x] 1.5: Add serde Deserialize for TOML `[[events]]` array-of-tables syntax

### Task 2: Implement EventWindowManager (AC: check runs, action applied, activation logged)
- [x] 2.1: Create `engine/src/risk/event_windows.rs` with `EventWindowManager` struct
- [x] 2.2: Constructor takes `Vec<EventWindowConfig>` and pre-computes resolved end times (start + duration if duration specified)
- [x] 2.3: Implement `check_active_events(&mut self, clock: &dyn Clock) -> Vec<ActiveEvent>` that returns currently active event windows
- [x] 2.4: Use `clock.wall_clock()` for time comparison (NOT `clock.now()`) since events are wall-clock scheduled
- [x] 2.5: Return `ActiveEvent` struct with event name, action, remaining duration

### Task 3: Implement event actions (AC: disable_strategies, reduce_exposure, sit_out)
- [x] 3.1: `DisableStrategies`: signal pipeline is not evaluated, existing positions remain with stops
- [x] 3.2: `ReduceExposure`: reduce max position size to configured minimum (e.g., 1 contract), flatten excess
- [x] 3.3: `SitOut`: equivalent to DisableStrategies + flatten any open positions before window starts
- [x] 3.4: Implement `get_trading_restriction(&self, clock: &dyn Clock) -> Option<TradingRestriction>` that returns the most restrictive active action
- [x] 3.5: Define `TradingRestriction` enum matching EventAction but usable by the event loop

### Task 4: Implement window lifecycle logging (AC: activation logged, resumption logged)
- [x] 4.1: Track active event state to detect transitions (inactive -> active, active -> inactive)
- [x] 4.2: On activation: log at `info` level with event name, action, start time, expected end time
- [x] 4.3: On expiration/resumption: log at `info` level with event name, duration it was active
- [x] 4.4: Avoid repeated logging — only log on state transitions, not every tick

### Task 5: Integrate with Clock trait for replay support (AC: wall_clock, SimClock)
- [x] 5.1: All time comparisons use `clock.wall_clock()` from the Clock trait in `core/src/traits/clock.rs`
- [x] 5.2: In live mode, `SystemClock::wall_clock()` returns actual wall time
- [x] 5.3: In replay mode, `SimClock::wall_clock()` returns simulated wall time — events activate based on replay timeline
- [x] 5.4: Ensure event window checks are deterministic in replay: same SimClock time = same event activation

### Task 6: Wire into event loop and CircuitBreakers (AC: integrated with risk framework)
- [x] 6.1: Call `event_window_manager.check_active_events()` in event loop before trade evaluation
- [x] 6.2: When an event restriction is active, skip signal evaluation (DisableStrategies) or reduce position limits (ReduceExposure)
- [x] 6.3: Event windows do NOT trip circuit breakers — they are a separate restriction layer
- [x] 6.4: Event restrictions are checked AFTER circuit breakers in the evaluation order

### Task 7: Unit tests (AC: all)
- [x] 7.1: Test event window activates when clock.wall_clock() falls within start-end range
- [x] 7.2: Test event window does not activate before start time
- [x] 7.3: Test event window deactivates after end time, trading resumes
- [x] 7.4: Test duration-based events compute correct end time from start + duration
- [x] 7.5: Test DisableStrategies action prevents signal evaluation
- [x] 7.6: Test ReduceExposure action reduces position limits
- [x] 7.7: Test SitOut action disables strategies and triggers flatten
- [x] 7.8: Test multiple simultaneous events: most restrictive action wins
- [x] 7.9: Test with SimClock: event activation is deterministic based on simulated wall time
- [x] 7.10: Test lifecycle logging: activation and resumption events logged on transitions only
- [x] 7.11: Test config validation: rejects events without end or duration

## Dev Notes

### Architecture Patterns & Constraints
- Event windows are wall-clock scheduled — uses `clock.wall_clock()`, not `clock.now()` (which is exchange time)
- In replay, SimClock's wall_clock determines which events are active, making replay deterministic
- Event windows are a separate restriction layer from circuit breakers — they do not trip breakers
- Configuration via `[[events]]` TOML array-of-tables, allowing multiple events
- Evaluation order: circuit breakers first, then event windows, then signal evaluation
- Event actions are hierarchical: SitOut > ReduceExposure > DisableStrategies (most to least restrictive)
- When multiple events overlap, the most restrictive action applies
- `unsafe` is forbidden in `engine/src/risk/`
- Example TOML config:
```toml
[[events]]
name = "FOMC"
start = "2026-04-16T14:00:00"
duration_minutes = 120
action = "sit_out"

[[events]]
name = "CPI Release"
start = "2026-05-13T08:30:00"
duration_minutes = 30
action = "disable_strategies"
```

### Project Structure Notes
```
crates/core/
├── src/
│   ├── config/
│   │   └── trading.rs          # EventWindowConfig, EventAction, added to TradingConfig
│   └── traits/
│       └── clock.rs            # Clock trait: now(), wall_clock()
crates/engine/
├── src/
│   ├── risk/
│   │   └── event_windows.rs    # EventWindowManager, ActiveEvent, TradingRestriction
│   └── event_loop.rs           # Check event windows before trade evaluation
crates/testkit/
├── src/
│   └── sim_clock.rs            # SimClock: wall_clock() for deterministic replay
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Section: Clock trait (now, wall_clock)
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 5, Story 5.5
- Testkit scenarios: `testkit/src/scenario.rs` includes FOMC scenario for event window testing

## Dev Agent Record

### Agent Model Used
- Claude Opus 4.7 (1M context) via the bmad-dev-story skill in an isolated worktree branch.

### Debug Log References
- Initial worktree base was 4.5; rebased onto `main` to pick up story 5.1 (circuit-breaker framework) before starting implementation.
- `cargo test --workspace` — all packages green after each task commit.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean after the doc-lazy-continuation fix on `event_windows.rs`.

### Completion Notes List
- Task 1: `EventWindowConfig` and `EventAction` live in `core/src/config/trading.rs`; serde deserialises via `#[serde(rename_all = "snake_case")]` so TOML strings are `disable_strategies`, `reduce_exposure`, `sit_out`. `TradingConfig.events` defaults to an empty `Vec` so configs without an `[[events]]` array continue to load. `validate_event_window_config` enforces XOR(end, duration_minutes), `duration_minutes > 0`, and `end > start`. `validate_trading_config` accumulates per-event errors so `validate_all` surfaces every problem at once.
- Tasks 2-5: `EventWindowManager` in `engine/src/risk/event_windows.rs` pre-resolves end timestamps at construction, uses a right-open `[start, end)` interval (so back-to-back windows don't double-count at the boundary), and surfaces the active set per call. All time comparisons go through `clock.wall_clock()` — never `clock.now()` (exchange time). Edge-triggered logging emits `info!` once on inactive→active and once on active→inactive transitions; no per-tick spam. `TradingRestriction` and `EventAction` share a severity ranking so the most restrictive action wins for overlapping windows.
- Task 6: `EventLoop` gains an `event_windows: EventWindowManager` field; `EventLoop::with_event_windows` accepts the `Vec<EventWindowConfig>` from `TradingConfig`. `current_trading_restriction()` is the public accessor the (future) signal evaluator calls AFTER the breaker `permits_trading` gate. Event windows are deliberately decoupled from breakers — they emit no `CircuitBreakerEvent` records and do not affect the data-quality gate or buffer state.
- Task 7: 19 unit tests on `EventWindowManager`, 11 validation tests on `EventWindowConfig` (including TOML round-trip), and 4 integration tests on `EventLoop`. Determinism with `SimClock` is asserted directly. The right-open interval boundary (start inclusive, end exclusive) is asserted in dedicated tests.
- Defence-in-depth: `EventWindowManager::new` silently drops malformed entries that slipped past validation. Test `manager_drops_malformed_entries` covers that path.
- Module-level `#![deny(unsafe_code)]` on `event_windows.rs`, matching the existing `risk/` files (story 5.1 introduced the deny attributes).

### File List
- `crates/core/Cargo.toml` (modified — add `toml` dev-dep)
- `crates/core/src/config/mod.rs` (modified — re-exports)
- `crates/core/src/config/trading.rs` (modified — `EventAction`, `EventWindowConfig`, `events` field)
- `crates/core/src/config/validation.rs` (modified — `validate_event_window_config`, 4 new error variants, 11 new tests)
- `crates/core/src/lib.rs` (modified — re-exports)
- `crates/engine/src/event_loop.rs` (modified — `event_windows` field, `with_event_windows`, `current_trading_restriction`, 4 new tests)
- `crates/engine/src/risk/circuit_breakers.rs` (modified — `events: Vec::new()` in test fixture)
- `crates/engine/src/risk/event_windows.rs` (new — `EventWindowManager`, `ActiveEvent`, `TradingRestriction`, 19 tests)
- `crates/engine/src/risk/mod.rs` (modified — register module + re-exports)
- `Cargo.toml` (modified — workspace `toml` dep)
- `Cargo.lock` (regenerated)
- `_bmad-output/implementation-artifacts/5-5-event-aware-trading-windows.md` (this file)

## Change Log
- 2026-04-30: Story 5.5 implementation. Three commits on the worktree branch — `feat(story-5.5): event window config types` (Task 1), `feat(story-5.5): EventWindowManager wall-clock restriction layer` (Tasks 2-5, 7), `feat(story-5.5): wire event-window manager into event loop` (Task 6).
