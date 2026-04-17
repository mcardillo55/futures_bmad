# Story 6.2: Strategy Enable/Disable & Regime Transition Logging

Status: ready-for-dev

## Story

As a trader-operator,
I want strategies automatically enabled or disabled based on regime,
So that the system sits out unfavorable conditions without manual intervention.

## Acceptance Criteria (BDD)

- Given a regime-to-strategy mapping in configuration When the regime state changes Then strategies permitted in the new regime are enabled, strategies not permitted are disabled, disabling a strategy does NOT cancel existing stop-loss orders (positions remain protected)
- Given a regime transition occurs When the transition is detected Then a `RegimeTransition` event is emitted with `from`, `to`, `timestamp`, the event is written to the event journal for post-hoc analysis, the transition is included in structured logs
- Given the regime is `Unknown` at startup When sufficient bars accumulate and a regime is classified Then the first transition from `Unknown` to a known regime is logged, strategy enabling follows the new regime's permitted strategy set
- Given rapid regime oscillation (flipping between states) When transitions occur faster than a configurable cooldown period Then the system remains in the more conservative state (does not enable trading on brief regime blips), the oscillation pattern is logged for research analysis

## Tasks / Subtasks

### Task 1: Define RegimeTransition event in core (AC: event with from/to/timestamp)
- 1.1: Add `RegimeTransition` struct to `crates/core/src/events/risk.rs`:
  - `from: RegimeState`
  - `to: RegimeState`
  - `timestamp: UnixNanos`
  - Derive `Debug, Clone, Copy, PartialEq, Eq`
- 1.2: Ensure `RegimeTransition` is re-exported from the events module and `core` lib.rs

### Task 2: Define regime orchestration config types (AC: regime-to-strategy mapping, cooldown)
- 2.1: Create or extend config in `crates/core/src/config/` (e.g. `regime.rs` or within `trading.rs`) with `RegimeOrchestrationConfig`:
  - `regime_strategy_map: HashMap<RegimeState, Vec<String>>` -- maps each regime to list of permitted strategy names
  - `cooldown_period_secs: u64` -- minimum seconds between acting on regime transitions (e.g. 300 = 5 minutes)
  - `conservative_on_oscillation: bool` -- if true, rapid oscillation keeps the more restrictive state (default true)
  - Derive `Debug, Clone, serde::Deserialize`
- 2.2: Add `[regime]` section support to the TOML config structure
- 2.3: Implement `Default` for `RegimeOrchestrationConfig` with sensible defaults (e.g. all strategies permitted in Trending, none in Unknown)

### Task 3: Implement RegimeOrchestrator struct (AC: strategy enable/disable, cooldown, transition detection)
- 3.1: Create `crates/engine/src/regime/orchestrator.rs`
- 3.2: Update `crates/engine/src/regime/mod.rs` to add `pub mod orchestrator;`
- 3.3: Define `RegimeOrchestrator` struct with fields:
  - `config: RegimeOrchestrationConfig`
  - `current_regime: RegimeState` (initialized to `Unknown`)
  - `last_transition_time: Option<UnixNanos>` (tracks when last transition was acted upon)
  - `pending_transitions: Vec<RegimeTransition>` (logged but not acted on during cooldown)
  - `enabled_strategies: HashSet<String>` (currently enabled strategy names)
- 3.4: Implement `RegimeOrchestrator::new(config: RegimeOrchestrationConfig) -> Self` constructor
  - Initialize with `current_regime = Unknown`, empty enabled strategies

### Task 4: Implement regime transition detection and cooldown logic (AC: cooldown prevents oscillation)
- 4.1: Implement `on_regime_update(&mut self, new_regime: RegimeState, timestamp: UnixNanos, clock: &dyn Clock) -> Option<RegimeTransition>`:
  - If `new_regime == self.current_regime`, return `None` (no transition)
  - Create `RegimeTransition { from: self.current_regime, to: new_regime, timestamp }`
  - Check cooldown: if `last_transition_time` is `Some` and `(timestamp - last_transition_time) < cooldown_period`, then:
    - Log the oscillation event (structured log with from/to/timestamp and "cooldown_suppressed" flag)
    - If `conservative_on_oscillation`, do NOT update `current_regime` -- remain in current (more conservative) state
    - Return the transition event (for logging) but do NOT apply strategy changes
  - If cooldown has elapsed or first transition:
    - Update `self.current_regime = new_regime`
    - Update `self.last_transition_time = Some(timestamp)`
    - Apply strategy enable/disable (Task 5)
    - Return the transition event
- 4.2: Implement private method `is_more_conservative(a: RegimeState, b: RegimeState) -> RegimeState`:
  - Conservative ordering: `Unknown` > `Volatile` > `Rotational` > `Trending` (Unknown is most conservative)
  - Used when oscillation detected to pick the safer state

### Task 5: Implement strategy enable/disable logic (AC: permitted strategies enabled, others disabled, stops preserved)
- 5.1: Implement `apply_strategy_permissions(&mut self, regime: RegimeState)`:
  - Look up `config.regime_strategy_map[regime]` to get permitted strategy names
  - Set `self.enabled_strategies` to the permitted set
  - CRITICAL: this method NEVER cancels existing stop-loss orders -- it only sets which strategies may initiate NEW trades
- 5.2: Implement `is_strategy_enabled(&self, strategy_name: &str) -> bool`:
  - Returns whether the named strategy is in `enabled_strategies`
  - Used by the engine's trade decision logic to gate new trade entries
- 5.3: Implement `enabled_strategies(&self) -> &HashSet<String>`:
  - Read-only accessor for current enabled strategy set

### Task 6: Implement regime transition logging (AC: journal and structured logs)
- 6.1: Implement `log_transition(&self, transition: &RegimeTransition)`:
  - Emit structured log via `tracing::info!` with fields: `from`, `to`, `timestamp`, `cooldown_suppressed` (bool)
  - Log target: `engine::regime::orchestrator` for log filtering
- 6.2: Ensure `RegimeTransition` event is compatible with the event journal writer (Story 4.1):
  - Implement `From<RegimeTransition>` for the journal's generic event type if needed
  - Journal table: `regime_transitions` with columns: `id`, `from_state`, `to_state`, `timestamp_nanos`
- 6.3: On first transition from `Unknown` to a known state, log at `INFO` level with message indicating regime detection has initialized

### Task 7: Write unit tests (AC: all transition, cooldown, and strategy gating behavior verified)
- 7.1: Create `crates/engine/tests/regime_orchestrator_tests.rs` (or inline `#[cfg(test)]` module)
- 7.2: Test: regime change from Unknown to Trending emits RegimeTransition event with correct from/to/timestamp
- 7.3: Test: regime change updates enabled strategies to match config mapping
- 7.4: Test: same regime reported twice produces no transition (returns None)
- 7.5: Test: rapid oscillation within cooldown period does NOT change enabled strategies
- 7.6: Test: rapid oscillation is still logged (transition event returned for journaling)
- 7.7: Test: after cooldown elapses, next transition is acted upon normally
- 7.8: Test: `is_strategy_enabled` returns correct values based on current regime
- 7.9: Test: disabling a strategy does not affect any stop-loss state (verify via assertion that the orchestrator has no access to order management)
- 7.10: Test: first transition from Unknown logged correctly
- 7.11: Test: conservative_on_oscillation keeps more restrictive regime during rapid changes
- 7.12: All tests use `testkit::SimClock::new()` for deterministic time

## Dev Notes

### Architecture Patterns & Constraints
- RegimeTransition event struct in `core/src/events/risk.rs`: `from: RegimeState`, `to: RegimeState`, `timestamp: UnixNanos`
- Strategy enable/disable NEVER cancels existing stop-loss orders -- this is a hard safety constraint. The orchestrator only gates new trade entry; it has no reference to order management or position management
- Cooldown period prevents rapid oscillation from whipsawing strategy state. Default to a conservative value (e.g. 300 seconds / 5 minutes)
- Conservative ordering for oscillation: Unknown (most restrictive) > Volatile > Rotational > Trending (least restrictive)
- Config loaded from TOML `[regime]` section; regime-to-strategy mapping is a map of regime names to lists of strategy names
- Structured logging via `tracing` crate with typed span fields for downstream filtering
- Event journal integration: RegimeTransition written to `regime_transitions` table in SQLite journal (Story 4.1)
- MANDATORY: `&dyn Clock` parameter passed through for timestamp consistency (use clock for any time comparisons, not `std::time`)

### Project Structure Notes
```
crates/engine/src/
├── regime/
│   ├── mod.rs              # pub mod threshold; pub mod orchestrator;
│   ├── threshold.rs        # ThresholdRegimeDetector (Story 6.1)
│   └── orchestrator.rs     # RegimeOrchestrator, strategy enable/disable, cooldown

crates/core/src/
├── events/
│   └── risk.rs             # RegimeTransition event struct (+ CircuitBreakerEvent)
├── traits/
│   └── regime.rs           # RegimeDetector trait, RegimeState enum
├── config/
│   └── (regime config or trading.rs)  # RegimeOrchestrationConfig

crates/testkit/src/
├── sim_clock.rs            # SimClock for deterministic tests
```

### References
- Architecture: `_bmad-output/planning-artifacts/architecture.md` -- RegimeTransition event, regime_transitions journal table, strategy orchestration
- Epics: `_bmad-output/planning-artifacts/epics.md` -- Epic 6, Story 6.2
- Dependencies: `core` (RegimeState, RegimeTransition, UnixNanos, Clock, config types), `testkit` (SimClock -- dev-dependency)
- Prerequisite stories: Story 1.5 (traits, Clock), Story 6.1 (ThresholdRegimeDetector provides regime state), Story 4.1 (event journal for persistence)
- Related: Story 9.2 (diagnostic logging consumes regime transition events for detailed diagnostics)

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
