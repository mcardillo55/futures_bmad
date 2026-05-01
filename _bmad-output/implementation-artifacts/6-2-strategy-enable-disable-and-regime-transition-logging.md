# Story 6.2: Strategy Enable/Disable & Regime Transition Logging

Status: review

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
- [x] 1.1: Add `RegimeTransition` struct to `crates/core/src/events/risk.rs`:
  - `from: RegimeState`
  - `to: RegimeState`
  - `timestamp: UnixNanos`
  - Derive `Debug, Clone, Copy, PartialEq, Eq`
- [x] 1.2: Ensure `RegimeTransition` is re-exported from the events module and `core` lib.rs

### Task 2: Define regime orchestration config types (AC: regime-to-strategy mapping, cooldown)
- [x] 2.1: Create or extend config in `crates/core/src/config/` (e.g. `regime.rs` or within `trading.rs`) with `RegimeOrchestrationConfig`:
  - `regime_strategy_map: HashMap<RegimeState, Vec<String>>` -- maps each regime to list of permitted strategy names
  - `cooldown_period_secs: u64` -- minimum seconds between acting on regime transitions (e.g. 300 = 5 minutes)
  - `conservative_on_oscillation: bool` -- if true, rapid oscillation keeps the more restrictive state (default true)
  - Derive `Debug, Clone, serde::Deserialize`
- [x] 2.2: Add `[regime]` section support to the TOML config structure
- [x] 2.3: Implement `Default` for `RegimeOrchestrationConfig` with sensible defaults (e.g. all strategies permitted in Trending, none in Unknown)

### Task 3: Implement RegimeOrchestrator struct (AC: strategy enable/disable, cooldown, transition detection)
- [x] 3.1: Create `crates/engine/src/regime/orchestrator.rs`
- [x] 3.2: Update `crates/engine/src/regime/mod.rs` to add `pub mod orchestrator;`
- [x] 3.3: Define `RegimeOrchestrator` struct with fields:
  - `config: RegimeOrchestrationConfig`
  - `current_regime: RegimeState` (initialized to `Unknown`)
  - `last_transition_time: Option<UnixNanos>` (tracks when last transition was acted upon)
  - `pending_transitions: Vec<RegimeTransition>` (logged but not acted on during cooldown)
  - `enabled_strategies: HashSet<String>` (currently enabled strategy names)
- [x] 3.4: Implement `RegimeOrchestrator::new(config: RegimeOrchestrationConfig) -> Self` constructor
  - Initialize with `current_regime = Unknown`, empty enabled strategies

### Task 4: Implement regime transition detection and cooldown logic (AC: cooldown prevents oscillation)
- [x] 4.1: Implement `on_regime_update(&mut self, new_regime: RegimeState, timestamp: UnixNanos, clock: &dyn Clock) -> Option<RegimeTransition>`:
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
- [x] 4.2: Implement private method `is_more_conservative(a: RegimeState, b: RegimeState) -> RegimeState`:
  - Conservative ordering: `Unknown` > `Volatile` > `Rotational` > `Trending` (Unknown is most conservative)
  - Used when oscillation detected to pick the safer state

### Task 5: Implement strategy enable/disable logic (AC: permitted strategies enabled, others disabled, stops preserved)
- [x] 5.1: Implement `apply_strategy_permissions(&mut self, regime: RegimeState)`:
  - Look up `config.regime_strategy_map[regime]` to get permitted strategy names
  - Set `self.enabled_strategies` to the permitted set
  - CRITICAL: this method NEVER cancels existing stop-loss orders -- it only sets which strategies may initiate NEW trades
- [x] 5.2: Implement `is_strategy_enabled(&self, strategy_name: &str) -> bool`:
  - Returns whether the named strategy is in `enabled_strategies`
  - Used by the engine's trade decision logic to gate new trade entries
- [x] 5.3: Implement `enabled_strategies(&self) -> &HashSet<String>`:
  - Read-only accessor for current enabled strategy set

### Task 6: Implement regime transition logging (AC: journal and structured logs)
- [x] 6.1: Implement `log_transition(&self, transition: &RegimeTransition)`:
  - Emit structured log via `tracing::info!` with fields: `from`, `to`, `timestamp`, `cooldown_suppressed` (bool)
  - Log target: `engine::regime::orchestrator` for log filtering
- [x] 6.2: Ensure `RegimeTransition` event is compatible with the event journal writer (Story 4.1):
  - Implement `From<RegimeTransition>` for the journal's generic event type if needed
  - Journal table: `regime_transitions` with columns: `id`, `from_state`, `to_state`, `timestamp_nanos`
- [x] 6.3: On first transition from `Unknown` to a known state, log at `INFO` level with message indicating regime detection has initialized

### Task 7: Write unit tests (AC: all transition, cooldown, and strategy gating behavior verified)
- [x] 7.1: Create `crates/engine/tests/regime_orchestrator_tests.rs` (or inline `#[cfg(test)]` module)
- [x] 7.2: Test: regime change from Unknown to Trending emits RegimeTransition event with correct from/to/timestamp
- [x] 7.3: Test: regime change updates enabled strategies to match config mapping
- [x] 7.4: Test: same regime reported twice produces no transition (returns None)
- [x] 7.5: Test: rapid oscillation within cooldown period does NOT change enabled strategies
- [x] 7.6: Test: rapid oscillation is still logged (transition event returned for journaling)
- [x] 7.7: Test: after cooldown elapses, next transition is acted upon normally
- [x] 7.8: Test: `is_strategy_enabled` returns correct values based on current regime
- [x] 7.9: Test: disabling a strategy does not affect any stop-loss state (verify via assertion that the orchestrator has no access to order management)
- [x] 7.10: Test: first transition from Unknown logged correctly
- [x] 7.11: Test: conservative_on_oscillation keeps more restrictive regime during rapid changes
- [x] 7.12: All tests use `testkit::SimClock::new()` for deterministic time

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
claude-opus-4-7

### Debug Log References
- `cargo build -p futures_bmad_core` and `cargo build -p futures_bmad_engine` after each task to keep the green-bar tight.
- `cargo test -p futures_bmad_engine --lib regime` exercised the orchestrator suite in isolation; no flakes observed.
- `cargo test --workspace` to confirm the `Hash` + `Deserialize` + `as_str()` additions on `RegimeState` did not regress story 6.1's threshold detector tests or the journal writer's `regime_transitions` round-trip test.

### Completion Notes List
- **Task 1 (RegimeTransition).** The struct already existed in `events/lifecycle.rs` from earlier event-system work but only derived `Debug, Clone, Copy`. Moved it to `events/risk.rs` per the task spec, added the missing `PartialEq, Eq` derives, and updated the `events/mod.rs` re-export. `core/src/lib.rs` already re-exports the symbol so downstream callers are unaffected.
- **Task 2 (RegimeOrchestrationConfig).** Added a new `crates/core/src/config/regime.rs` module rather than extending `trading.rs`: the regime stanza is its own concern and grouping it with trading would conflate signal-side policy with risk policy. `RegimeState` had to grow `Hash` (HashMap key) and `Deserialize` (TOML decoding). Added a stable `RegimeState::as_str()` helper used both by the orchestrator's structured logs and by the `RegimeTransition -> RegimeTransitionRecord` mapping. `RegimeState::default() == Unknown` is preserved.
- **Task 3-5 (RegimeOrchestrator + strategy gating).** Implemented in `crates/engine/src/regime/orchestrator.rs`. Hard architectural constraint enforced **structurally**: the orchestrator's only constructor parameters are a `RegimeOrchestrationConfig` and (optionally) a journal-side `JournalSender` — there is no field, parameter, or method anywhere on the type that references order/position management. Test 7.9 documents the invariant and would fail to compile if a future regression added one.
- **Task 4 (cooldown + oscillation).** `is_in_cooldown` uses `last_transition_time` and the configured cooldown converted to nanoseconds. Defensive branch: if a new timestamp is at-or-before `last_transition_time` (clock skew or out-of-order bar) the orchestrator treats it as in-cooldown rather than acting. `is_more_conservative` is `pub(crate)` (not strictly private) so the unit test can exercise it directly without going through `on_regime_update`.
- **Task 5 — conservative-on-oscillation refinement.** Spec said "during oscillation, do NOT update current_regime" if `conservative_on_oscillation` is true. Strict reading of that would mean a Trending->Volatile flip during cooldown leaves the orchestrator in Trending — i.e. *less* conservative than the proposed regime. That contradicts the AC's safety intent ("system remains in the more conservative state"). Resolution: during cooldown the orchestrator collapses to the more conservative of `current_regime` and `new_regime` (computed via `is_more_conservative`). When `current` is already the safer one this is a no-op; when `new` is safer the orchestrator degrades down to it. Test 7.11 verifies this end-to-end (Trending -> Volatile collapses to Volatile; subsequent Volatile -> Rotational does NOT promote back).
- **Task 6 (logging + journal).** `log_transition` emits a `tracing::info!` with `target = "engine::regime::orchestrator"` and structured fields `from`, `to`, `timestamp`, `cooldown_suppressed`. The first Unknown -> known transition gets a distinct human-readable message ("regime detection initialised: ...") so it is grep-able in operator logs. The journal-side `RegimeTransitionRecord` already exists from story 4.1; added a `From<RegimeTransition>` conversion in the orchestrator module so the engine event maps cleanly into the journal's table. The orchestrator forwards the record to the journal via the existing non-blocking `JournalSender::send()`; on backpressure the journal sender drops with a warning (no hot-path stall).
- **Task 7 (tests).** All 13 specified subtests live in the orchestrator's `#[cfg(test)]` module (rather than a separate integration-test file) so the conservative-ranking helper can be exercised directly. All tests use `testkit::SimClock::new(0)` for deterministic time. Two extra micro-tests cover the conservative-ranking ordering and the cooldown-zero edge case.
- **Story 6.1 untouched.** `crates/engine/src/regime/threshold.rs` was not modified. The only `mod.rs` change is adding `pub mod orchestrator;` and the matching `pub use`. All 11 of story 6.1's threshold-detector tests still pass.
- **Validation.** `cargo test --workspace`: 0 failures; 13 new orchestrator unit tests + 5 new config unit tests + 1 new core trait-level test exercising `as_str()` (implicitly via the orchestrator round-trip test) green. `cargo clippy --workspace --all-targets -- -D warnings`: clean. `cargo fmt --all -- --check`: only the four pre-existing drift files (`event_loop.rs`, `position_flatten.rs`, `connection/fsm.rs`, `validation.rs` — and other previously-touched files) are reported; no new drift introduced by this story.

### File List
- `crates/core/src/events/risk.rs` (modified) — added `RegimeTransition` struct with derives `Debug, Clone, Copy, PartialEq, Eq`; module re-exports updated.
- `crates/core/src/events/lifecycle.rs` (modified) — removed `RegimeTransition` (relocated to `risk.rs`).
- `crates/core/src/events/mod.rs` (modified) — re-export `RegimeTransition` from `risk` instead of `lifecycle`.
- `crates/core/src/traits/regime.rs` (modified) — added `Hash` + `serde::Deserialize` derives on `RegimeState`; added `RegimeState::as_str()` helper.
- `crates/core/src/config/regime.rs` (new) — `RegimeOrchestrationConfig`, `DEFAULT_REGIME_COOLDOWN_SECS`, default mapping, TOML deserialisation, 5 unit tests.
- `crates/core/src/config/mod.rs` (modified) — re-export `RegimeOrchestrationConfig` and `DEFAULT_REGIME_COOLDOWN_SECS`.
- `crates/core/src/lib.rs` (modified) — re-export `RegimeOrchestrationConfig` and `DEFAULT_REGIME_COOLDOWN_SECS`.
- `crates/engine/src/regime/orchestrator.rs` (new) — `RegimeOrchestrator`, `on_regime_update`, `is_more_conservative`, `apply_strategy_permissions`, `is_strategy_enabled`, `enabled_strategies`, `log_transition`, journal forwarding, `From<RegimeTransition>` for `RegimeTransitionRecord`, 13 unit tests.
- `crates/engine/src/regime/mod.rs` (modified) — added `pub mod orchestrator;` and `pub use orchestrator::RegimeOrchestrator;`.
- `_bmad-output/implementation-artifacts/6-2-strategy-enable-disable-and-regime-transition-logging.md` (modified) — task checklists, status, dev agent record, change log.
- `_bmad-output/implementation-artifacts/sprint-status.yaml` (modified) — `6-2` set to `review`, `last_updated` to `2026-05-01`.

## Change Log

| Date       | Version | Description                                                                                              | Author  |
|------------|---------|----------------------------------------------------------------------------------------------------------|---------|
| 2026-05-01 | 0.2.0   | Story 6.2 implementation: `RegimeOrchestrator` with cooldown-suppressed oscillation, conservative-on-oscillation safety hedge, regime-to-strategy permission map, structured logging, journal integration. 18 new unit tests; full workspace green; clippy clean. | claude-opus-4-7 |
