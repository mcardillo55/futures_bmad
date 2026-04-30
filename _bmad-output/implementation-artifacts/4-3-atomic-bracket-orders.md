# Story 4.3: Atomic Bracket Orders

Status: review

## Story

As a trader-operator,
I want every trade protected by exchange-resting stops from the moment of entry,
So that I never have an unprotected position.

## Acceptance Criteria (BDD)

- Given trade decision When BracketOrder constructed Then entry=market, take_profit=limit (exchange-resting), stop_loss=stop (exchange-resting), TP/SL as OCO
- Given entry fills When bracket legs submitted Then BracketState transitions: EntryOnly to EntryAndStop to Full, each transition tracked per position
- Given entry fills but bracket submission fails When failure detected Then flatten retry: market flatten, if rejected wait 1s retry (3 attempts), after 3 fails panic mode activates (all trading disabled, entries cancelled, stops preserved, operator alerted, manual restart required)
- Given bracket TP or SL fills When fill received Then OCO counterpart auto-cancelled by exchange, position updated to flat, P&L computed in quarter-ticks, logged to journal

## Tasks / Subtasks

### Task 1: Define BracketOrder and BracketState types (AC: entry/TP/SL structure, state tracking) — [x]
- [x] 1.1: In `crates/core/src/types/order.rs`, define `BracketOrder` struct: `entry: OrderEvent` (market), `take_profit: OrderEvent` (limit), `stop_loss: OrderEvent` (stop), `bracket_id: u64`, `decision_id: u64`, `state: BracketState` — kept `entry/take_profit/stop_loss` as `OrderParams` (not `OrderEvent`) per the architecture doc and existing API; routing-event fields (`order_id`, `timestamp`) are attached by `BracketManager` at submit time. Added `bracket_id`, `decision_id`, and `state` per spec.
- [x] 1.2: Define `BracketState` enum: `NoBracket`, `EntryOnly`, `EntryAndStop`, `Full`, `Flattening` — derive `Debug, Clone, Copy, PartialEq` (already present from earlier work; left untouched).
- [x] 1.3: Implement `BracketOrder::from_decision(bracket_id, decision_id, symbol_id, side, quantity, tp_price, sl_price)` (renamed from spec's `BracketOrder::new(...)` since the struct already had a different `new` taking pre-built `OrderParams` legs). Initial state = `NoBracket`.
- [x] 1.4: Implement `BracketOrder::transition(&mut self, new_state) -> Result<(), BracketStateError>` with validated transitions: NoBracket->EntryOnly, EntryOnly->EntryAndStop, EntryAndStop->Full, any-non-Flattening->Flattening.

### Task 2: Implement bracket submission flow (AC: entry fill triggers bracket leg submission) — [x]
- [x] 2.1: In `crates/engine/src/order_manager/bracket.rs`, created `BracketManager` struct tracking active brackets by bracket_id, with reverse `leg_to_bracket` index for O(1) fill -> bracket lookup.
- [x] 2.2: `on_entry_fill(&mut self, fill, producer)` transitions `NoBracket -> EntryOnly` and pushes the stop-loss `OrderEvent` onto the engine -> broker queue. Returns `FlattenContext::EntrySubmittedStop` on success or `FlattenContext::EntryFlatten { ... }` if SL queueing fails.
- [x] 2.3: `on_stop_confirmed(bracket_id, producer, now)` transitions `EntryOnly -> EntryAndStop` and submits the take-profit leg. TP submission failure here does NOT panic (SL is already resting).
- [x] 2.4: `on_take_profit_confirmed(bracket_id, now)` transitions `EntryAndStop -> Full`.
- [x] 2.5: For story 4.3 the manager records the OCO pair (TP/SL) and relies on exchange-native OCO (Rithmic CME supports it). When TP or SL fills, `on_bracket_fill` logs the expected counterpart cancel; story 4.5 reconciliation will verify the cancel actually arrived. Local OCO emulation is a follow-up if the live OrderPlant turns out not to support native OCO.
- [x] 2.6: Every state transition logs at `info` with `bracket_id`, `decision_id`, `new_state`, plus a `SystemEvent` journal record so the audit trail is durable.

### Task 3: Implement flatten retry on bracket failure (AC: 3 attempts, 1s interval) — [x]
- [x] 3.1: Created `crates/broker/src/position_flatten.rs` with `FlattenRetry<'a, S: OrderSubmitter>` orchestrator. Re-exported from `broker::lib.rs` alongside `FlattenOutcome`, `FlattenRequest`, `FLATTEN_MAX_ATTEMPTS`, `FLATTEN_RETRY_INTERVAL`.
- [x] 3.2: `flatten_with_retry(req: FlattenRequest) -> FlattenOutcome` submits a Market order via the existing `OrderSubmitter` trait. The signature carries `FlattenRequest { order_id, symbol_id, side, quantity, decision_id, timestamp }` rather than positional args (so the engine can pre-allocate the engine-side `order_id` and propagate `decision_id` for NFR17).
- [x] 3.3: On `Err`, the loop calls `tokio::time::sleep(retry_interval)` (default 1s, override-able for tests) and retries — up to 3 total attempts.
- [x] 3.4: Returns `FlattenOutcome::Success { order_id, attempts }` (submission ack only — the actual fill flows via the FillQueue) or `FlattenOutcome::Failed { attempts: 3, last_error }`. Did not nest a full `FillEvent` inside Success because the FillEvent arrives separately on the SPSC fill queue; doing so would duplicate the source of truth.
- [x] 3.5: BracketState `Flattening` transition is owned by `BracketManager::engage_flatten` (engine side); the broker-side flatten orchestrator is intentionally agnostic of the bracket lifecycle so it stays single-responsibility.
- [x] 3.6: Each retry logs at `warn` with `order_id`, `decision_id`, `symbol_id`, `attempt`, `max_attempts`, `error`. The final exhausted-failure logs at `error` with the same fields.

### Task 4: Implement panic mode (AC: all trading disabled, stops preserved) — [x]
- [x] 4.1: Created `crates/engine/src/risk/panic_mode.rs` with `PanicMode` struct and `PanicState` enum (`Normal`, `PanicActive`). State held in `AtomicBool` so the `is_trading_enabled()` check can be sampled lock-free from any thread.
- [x] 4.2: `activate(&self, reason, now, cancellation) -> ActivationOutcome` flips the flag (idempotent via compare-exchange), logs at `error` with `alert = true` for the alerting integration to pick up.
- [x] 4.3: Cancellation is delegated to a new `OrderCancellation` trait the engine event-loop will implement. The trait method is `cancel_entries_and_limits` — it is a CONTRACT that the implementation NEVER cancels resting stops. PanicMode itself does not, and cannot, cancel any order; it asks the engine to cancel only non-stop orders.
- [x] 4.4: `is_trading_enabled()` returns `false` after activation. Engine signal evaluation will short-circuit on this flag (wired in subsequent stories that integrate into `event_loop.rs`).
- [x] 4.5: Operator alert log carries structured fields `reason`, `timestamp_nanos`, `alert = true` so the downstream alerting integration can route on the `alert` field.
- [x] 4.6: There is intentionally NO `deactivate()` method. Panic state is held in the controller for the entire process lifetime; manual restart is required.
- [x] 4.7: Activation writes both a `CircuitBreakerEventRecord` (`breaker_type = "panic_mode"`, `triggered = true`) and a `SystemEventRecord` to the journal via `JournalSender`. Both are tested in `activate_writes_journal_records`.

### Task 5: Implement bracket exit processing (AC: OCO fill, P&L, position flat) — [x]
- [x] 5.1: `BracketManager::on_bracket_fill(&mut self, fill)` looks up the bracket via the `leg_to_bracket` reverse index, then determines TP vs SL by matching the fill's `order_id` against the stored `take_profit_order_id` / `stop_loss_order_id`.
- [x] 5.2: On TP fill, the SL `order_id` is logged at `debug` as the expected auto-cancel counterpart. Story 4.5 reconciliation owns verification that the exchange actually cancelled it.
- [x] 5.3: Symmetric handling on SL fill — TP `order_id` logged as expected counterpart cancel.
- [x] 5.4: Realized P&L = `(exit_price.saturating_sub(entry_price)).saturating_mul(quantity).saturating_mul(direction)` in raw quarter-ticks, where `direction = +1` for long entries and `-1` for short entries. Stored as integer `i64`; conversion to dollars (e.g., $3.125 per quarter-tick on ES) is left for display/reporting layers.
- [x] 5.5: Position update is owned by `Position::apply_fill` (existing in `core::types::position`). The `BracketManager` surfaces the exit fill via `FlattenOutcome::TakeProfit` / `StopLoss`; the engine event loop forwards the underlying `FillEvent` through both `OrderManager::apply_fill` and the position tracker. Integration of the position update path into `event_loop.rs` is wired in story 4.5 (position tracking and reconciliation).
- [x] 5.6: Two journal records on exit: (a) a `TradeEventRecord` with `kind = "tp_fill"` or `"sl_fill"`, carrying decision_id, order_id, symbol_id, side, fill_price, fill_size; (b) a `SystemEventRecord` summarizing `bracket {id} (decision {id}) closed via {kind}, pnl_qt={value}`.
- [x] 5.7: Bracket entry is removed from `brackets` map and all three leg ids from `leg_to_bracket` on a terminal exit fill.

### Task 6: Unit tests (AC: all) — [x]
- [x] 6.1: `bracket_order_from_decision_constructs_three_legs` (`crates/core/src/types/order.rs`) — entry Market, TP Limit at tp_price, SL Stop at sl_price; sell-side symmetry verified.
- [x] 6.2: `bracket_state_transitions` (`crates/core/src/types/order.rs`) — valid forward transitions succeed; skipping ahead returns `BracketStateError`; `Flattening -> Flattening` rejected.
- [x] 6.3: `bracket_full_lifecycle_to_full_state` (`crates/engine/src/order_manager/bracket.rs`) — entry submitted -> entry fill -> SL submitted (state EntryOnly) -> SL confirmed -> TP submitted (state EntryAndStop) -> TP confirmed -> Full.
- [x] 6.4: `first_attempt_fails_second_succeeds` (`crates/broker/src/position_flatten.rs`) — uses scripted submitter that fails the first call, verifies retry path + `attempts == 2`. Retry interval is overridden to 1ms in tests; production keeps 1s.
- [x] 6.5: `flatten_exhaustion_triggers_panic_mode` (`crates/engine/src/risk/panic_mode.rs`) — composes `FlattenRetry` always-fail submitter with `PanicMode::activate` to demonstrate the policy boundary the event loop will wire.
- [x] 6.6: `activate_disables_trading_and_cancels_entries`, `activate_is_idempotent`, `panic_state_persists`, `activate_writes_journal_records` — full panic-mode contract.
- [x] 6.7: `tp_fill_computes_positive_pnl_and_clears_bracket` (`crates/engine/src/order_manager/bracket.rs`) — long entry at 100, TP at 200, qty 2 -> +200 quarter-ticks; bracket cleared.
- [x] 6.8: `sl_fill_computes_negative_pnl_and_clears_bracket` — long entry at 100, SL at 50, qty 2 -> -100 quarter-ticks; bracket cleared. Plus `short_bracket_tp_pnl_is_positive` verifying short-side direction sign.

## Dev Notes

### Architecture Patterns & Constraints
- CRITICAL safety invariant: circuit break / panic mode MUST preserve resting stop-loss orders. Only cancel entries and limit orders. An unprotected position is the worst possible state.
- BracketOrder is not `Copy` — it carries mutable state and is tracked by reference in BracketManager. It does not flow through SPSC queues; only its constituent OrderEvents do.
- OCO semantics: if the exchange supports native OCO (Rithmic does for CME), use it. Otherwise BracketManager must handle cancellation of the counterpart on fill.
- Flatten retry uses Tokio sleep — this runs on the async runtime, not the hot path. The hot path is notified of flatten outcome via the fill queue.
- P&L in quarter-ticks: for ES (E-mini S&P), 1 tick = $12.50, 1 quarter-tick = $3.125. Store P&L as integer quarter-ticks, convert to dollars only for display/reporting.

### Project Structure Notes
```
crates/core/src/types/
└── order.rs              (BracketOrder, BracketState — added to existing file)

crates/broker/src/
└── position_flatten.rs   (FlattenRetry, FlattenOutcome)

crates/engine/src/
├── order_manager/
│   └── bracket.rs        (BracketManager)
└── risk/
    ├── mod.rs
    └── panic_mode.rs     (PanicMode, PanicState)
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Bracket Orders, Risk Management
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 4, Story 4.3
- Story 4.2: OrderEvent/FillEvent types and SPSC queues
- Story 4.1: Event journal for bracket event persistence
- Story 4.4: Order state machine for transition validation
- NFR16: Circuit breaker on safety violations
- NFR17: decision_id causality tracing

## Dev Agent Record

### Agent Model Used
- Claude Opus 4.7 (1M context) via the BMad `bmad-dev-story` workflow.

### Debug Log References
- `cargo build` clean.
- `cargo test --workspace` -> 255 passed (delta +17 over 4-2 baseline of 238: +6 bracket-manager tests, +5 panic-mode tests, +4 flatten-retry tests, +2 BracketOrder/state tests in core).
- `cargo clippy --all-targets -- -D warnings` clean.
- `rustfmt --check` exit 0 on all 9 Rust files in this story's File List.

### Completion Notes List
- BracketOrder struct extended (not replaced) with `bracket_id`, `decision_id`, `state`. Existing 3-arg `BracketOrder::new(entry, tp, sl)` preserved for callers; new 5-arg `BracketOrder::new(bracket_id, decision_id, entry, tp, sl)` adds the lifecycle fields, and `BracketOrder::from_decision(...)` is the spec's intended ergonomic constructor (renamed because the original `new` taking pre-built `OrderParams` legs is the one already used in tests + future story 4.5 reconciliation paths).
- Spec said `BracketOrder.entry: OrderEvent` — kept as `OrderParams` instead. Justification: (a) the architecture doc shows `OrderParams`; (b) `OrderEvent` requires `order_id` + `timestamp` allocated at routing time, not construction time; (c) `BracketManager::build_order_event` translates legs into `OrderEvent`s when each leg is actually pushed onto the order queue, keeping a single source of truth for the bracket's logical shape. Documented in the `BracketOrder` doc comment.
- OCO semantics intentionally rely on exchange-native OCO (Rithmic CME supports it). On TP/SL fill the manager logs the expected counterpart cancellation; story 4.5 reconciliation will verify the cancel actually arrived. Local OCO emulation is a follow-up if the live OrderPlant turns out not to support native OCO.
- `FlattenRetry::flatten_with_retry` returns `FlattenOutcome::Success { order_id, attempts }` rather than `Success(FillEvent)` (as the spec wrote). Reasoning: the actual fill arrives separately on the FillQueue once the broker reports the flatten order's fill — nesting a `FillEvent` inside Success would duplicate the source of truth and create ambiguity about when the fill is "real". The submission ack and the eventual fill are distinct events.
- `PanicMode::activate` is idempotent and uses an `AtomicBool` so the engine can sample `is_trading_enabled()` lock-free from any thread. There is no `deactivate()` method — manual restart is required, per architecture spec.
- `OrderCancellation` trait is the seam between `PanicMode` and the engine event-loop's order-cancel implementation. The trait method is "cancel entries and limits" — the contract is that the implementation NEVER cancels resting stop-loss orders. The actual engine-side implementation (iterate `OrderManager`/`BracketManager`, issue cancels via the broker) is wired in subsequent stories that integrate the event loop; this story delivers the policy controller and the contract.
- Did not address the deferred items from 4-2 review (S-1 over-fill validation, S-2 zero-size fills, S-3 PartialFill->Rejected arc, S-4 reject-fill try_push return, S-5 Timeout->Uncertain) — none are required by 4-3 ACs and the deferral is recorded in the 4-2 review doc.

### File List
**Added:**
- `crates/engine/src/order_manager/bracket.rs` — `BracketManager`, `BracketSubmissionError`, `FlattenContext`, `FlattenOutcome`.
- `crates/engine/src/risk/panic_mode.rs` — `PanicMode`, `PanicState`, `OrderCancellation`, `ActivationOutcome`.
- `crates/broker/src/position_flatten.rs` — `FlattenRetry`, `FlattenRequest`, `FlattenOutcome`, `FLATTEN_MAX_ATTEMPTS`, `FLATTEN_RETRY_INTERVAL`.

**Modified:**
- `crates/core/src/types/order.rs` — extended `BracketOrder` with `bracket_id`, `decision_id`, `state`; added `BracketOrder::from_decision`, `BracketOrder::transition`, `BracketStateError`, expanded `BracketState` doc; added 3 unit tests.
- `crates/core/src/types/mod.rs` — re-export `BracketStateError`.
- `crates/core/src/lib.rs` — re-export `BracketStateError`.
- `crates/engine/src/order_manager/mod.rs` — declare `pub mod bracket;` and re-export.
- `crates/engine/src/risk/mod.rs` — declare `pub mod panic_mode;` and re-export.
- `crates/broker/src/lib.rs` — declare `pub mod position_flatten;` and re-export.
- `crates/engine/Cargo.toml` — add `async-trait` and tokio macros to `dev-dependencies` for the test harness in `panic_mode::tests::flatten_exhaustion_triggers_panic_mode`.
- `_bmad-output/implementation-artifacts/sprint-status.yaml` — moved 4-3 ready-for-dev -> in-progress -> review.
- `_bmad-output/implementation-artifacts/4-3-atomic-bracket-orders.md` — this file (status, tasks, Dev Agent Record).

## Change Log
- 2026-04-30: Story 4.3 implementation complete. Bracket lifecycle (entry → SL → TP → Full), flatten retry (3 attempts, 1s interval), panic mode (idempotent, persists, preserves stops). 17 new tests; total workspace tests 255. Branch: `feat/story-4-3-atomic-bracket-orders` off `feat/story-4-2-market-order-submission`.
- 2026-04-30: Code review completed (`_bmad-output/implementation-artifacts/4-3-code-review-2026-04-30.md`). Verdict: APPROVE-WITH-CHANGES. 0 BLOCKING / 4 SHOULD-FIX / 5 NICE-TO-HAVE.

## Senior Developer Review

**Reviewer:** Senior Developer (adversarial — Blind Hunter / Edge Case Hunter / Acceptance Auditor)
**Date:** 2026-04-30
**Verdict:** APPROVE-WITH-CHANGES
**Findings:** 0 BLOCKING / 4 SHOULD-FIX / 5 NICE-TO-HAVE
**Validation:** `cargo test --workspace` → 255 passing; `cargo clippy --all-targets -- -D warnings` clean.
**Full report:** `_bmad-output/implementation-artifacts/4-3-code-review-2026-04-30.md`.

### AC Audit
- AC1 (BracketOrder shape — entry market / TP limit / SL stop / OCO): **MET**
- AC2 (BracketState transitions: EntryOnly → EntryAndStop → Full): **MET**
- AC3 (Flatten retry 3×1s → panic mode; entries cancelled, stops preserved, manual restart): **MET**
- AC4 (TP/SL fill → OCO counterpart auto-cancelled, position flat, P&L in quarter-ticks, journaled): **PARTIAL** — TP/SL detection, P&L, journal records all present and tested. OCO counterpart cancel is logged (debug) and verification deferred to 4-5 reconciliation, per spec Task 2.5. Position-flat wiring from `on_bracket_fill` → `Position::apply_fill` is also deferred to 4-5 per Task 5.5. Both deferrals are honest and explicit in the spec.

### Verdict on Dev Deviations
1. `OrderParams` (not `OrderEvent`) on `BracketOrder` — **JUSTIFIED**. Architecture doc actually shows `OrderParams`; routing fields belong at submit time. Translation seam (`build_order_event`) is single-purpose and used four times.
2. `from_decision` constructor + legacy `new` retained — **JUSTIFIED** (minor doc-comment improvement: N-1).
3. `FlattenOutcome::Success { order_id, attempts }` (not `Success(FillEvent)`) — **JUSTIFIED, strongly**. Submission ack and fill are distinct events; embedding a synthesized FillEvent would create two sources of truth.
4. Native OCO (Rithmic CME) — **JUSTIFIED**, with N-2 carrying the unverified-on-the-wire concern forward to 4-5.
5. `OrderCancellation` trait surface ready, engine-side iterator deferred to 4-4/4-5/epic-5 — **JUSTIFIED** and honest. Trait is testable via `MockCancellation`; the "never cancel a stop" contract is enforced by docstring (the trait can't enforce it itself).

### Should-Fix
- **S-1**: `on_entry_fill` does not handle `FillType::Rejected` on the entry leg → would submit a naked stop. Live impact gated by deferred event-loop wiring; fix before 4-5. (`crates/engine/src/order_manager/bracket.rs:253-275`)
- **S-2**: `on_entry_fill` ignores `FillType::Partial` size → SL/TP oversized for partial entries. CME Market entries are typically atomic; document the assumption or implement cumulative-fill tracking. (`crates/engine/src/order_manager/bracket.rs:253-335`)
- **S-3**: `engage_flatten` swallows `transition(Flattening)` errors with `let _ = …` → could engage flatten twice. (`crates/engine/src/order_manager/bracket.rs:585`)
- **S-4**: `FlattenRetry` reuses the same `order_id` across retries — relies on broker-side dedup that is not verified in the diff. (`crates/broker/src/position_flatten.rs:127-194`)

### Nice-to-Have
- **N-1**: `BracketOrder::new` doc comment unclear about when to prefer `new` vs `from_decision`.
- **N-2**: OCO native-vs-emulated assumption unverified — story 4-5 should carry a recorded-session test.
- **N-3**: `OrderCancellation::cancel_entries_and_limits` returns `usize` instead of `Result<usize, _>`.
- **N-4**: `FlattenRetry` does not enforce a position-exists precondition.
- **N-5**: `on_stop_confirmed` does not distinguish "duplicate ack" from "out-of-order confirmation after Flattening".

### Cross-Reference
None of the 4-2 deferred items (S-1..S-5, N-1..N-6) are made worse by 4-3, except N-1 (`OrderSubmitter` dual-surface) which gains a second caller (`FlattenRetry`) — consolidation in 4-4 is now more valuable.
