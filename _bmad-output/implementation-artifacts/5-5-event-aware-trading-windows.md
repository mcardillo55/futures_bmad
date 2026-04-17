# Story 5.5: Event-Aware Trading Windows

Status: ready-for-dev

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
- 1.1: In `core/src/config/trading.rs`, define `EventWindowConfig` struct with fields: `name` (String), `start` (chrono::NaiveDateTime), `end` (Option<chrono::NaiveDateTime>), `duration_minutes` (Option<u32>), `action` (EventAction enum)
- 1.2: Define `EventAction` enum with variants: `DisableStrategies`, `ReduceExposure`, `SitOut`
- 1.3: Add `events: Vec<EventWindowConfig>` field to TradingConfig
- 1.4: Implement validation: each event must have either `end` or `duration_minutes` (not both, not neither)
- 1.5: Add serde Deserialize for TOML `[[events]]` array-of-tables syntax

### Task 2: Implement EventWindowManager (AC: check runs, action applied, activation logged)
- 2.1: Create `engine/src/risk/event_windows.rs` with `EventWindowManager` struct
- 2.2: Constructor takes `Vec<EventWindowConfig>` and pre-computes resolved end times (start + duration if duration specified)
- 2.3: Implement `check_active_events(&mut self, clock: &dyn Clock) -> Vec<ActiveEvent>` that returns currently active event windows
- 2.4: Use `clock.wall_clock()` for time comparison (NOT `clock.now()`) since events are wall-clock scheduled
- 2.5: Return `ActiveEvent` struct with event name, action, remaining duration

### Task 3: Implement event actions (AC: disable_strategies, reduce_exposure, sit_out)
- 3.1: `DisableStrategies`: signal pipeline is not evaluated, existing positions remain with stops
- 3.2: `ReduceExposure`: reduce max position size to configured minimum (e.g., 1 contract), flatten excess
- 3.3: `SitOut`: equivalent to DisableStrategies + flatten any open positions before window starts
- 3.4: Implement `get_trading_restriction(&self, clock: &dyn Clock) -> Option<TradingRestriction>` that returns the most restrictive active action
- 3.5: Define `TradingRestriction` enum matching EventAction but usable by the event loop

### Task 4: Implement window lifecycle logging (AC: activation logged, resumption logged)
- 4.1: Track active event state to detect transitions (inactive -> active, active -> inactive)
- 4.2: On activation: log at `info` level with event name, action, start time, expected end time
- 4.3: On expiration/resumption: log at `info` level with event name, duration it was active
- 4.4: Avoid repeated logging — only log on state transitions, not every tick

### Task 5: Integrate with Clock trait for replay support (AC: wall_clock, SimClock)
- 5.1: All time comparisons use `clock.wall_clock()` from the Clock trait in `core/src/traits/clock.rs`
- 5.2: In live mode, `SystemClock::wall_clock()` returns actual wall time
- 5.3: In replay mode, `SimClock::wall_clock()` returns simulated wall time — events activate based on replay timeline
- 5.4: Ensure event window checks are deterministic in replay: same SimClock time = same event activation

### Task 6: Wire into event loop and CircuitBreakers (AC: integrated with risk framework)
- 6.1: Call `event_window_manager.check_active_events()` in event loop before trade evaluation
- 6.2: When an event restriction is active, skip signal evaluation (DisableStrategies) or reduce position limits (ReduceExposure)
- 6.3: Event windows do NOT trip circuit breakers — they are a separate restriction layer
- 6.4: Event restrictions are checked AFTER circuit breakers in the evaluation order

### Task 7: Unit tests (AC: all)
- 7.1: Test event window activates when clock.wall_clock() falls within start-end range
- 7.2: Test event window does not activate before start time
- 7.3: Test event window deactivates after end time, trading resumes
- 7.4: Test duration-based events compute correct end time from start + duration
- 7.5: Test DisableStrategies action prevents signal evaluation
- 7.6: Test ReduceExposure action reduces position limits
- 7.7: Test SitOut action disables strategies and triggers flatten
- 7.8: Test multiple simultaneous events: most restrictive action wins
- 7.9: Test with SimClock: event activation is deterministic based on simulated wall time
- 7.10: Test lifecycle logging: activation and resumption events logged on transitions only
- 7.11: Test config validation: rejects events without end or duration

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
### Debug Log References
### Completion Notes List
### File List
