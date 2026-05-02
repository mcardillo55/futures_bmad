---
title: 'Close pre-Epic-8 MarketDataFeed semantic gap (B-1)'
type: 'refactor'
created: '2026-05-02'
status: 'done'
baseline_commit: 'eda7b57'
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-7-retro-2026-05-02.md'
  - '{project-root}/_bmad-output/implementation-artifacts/deferred-work.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** `MarketDataFeed::next_event() -> Option<MarketEvent>` (`crates/engine/src/paper/data_feed.rs:32`) conflates "no event right now" with "stream ended." For replay's bounded `Vec` source the conflation happens to work, but for Story 8.2's live `RithmicMarketDataFeed` adapter the FIRST momentary quiet period between Rithmic frames would terminate `pump_until_idle` (`paper/orchestrator.rs:393-429`). Story 7.3 dev notes claim Epic 8 only needs "a `RithmicMarketDataFeed` impl and one swap of the `let feed = ` line" — that is structurally false while the trait conflates idle and EOF. Pre-Epic-8 because Story 8.2's live-data wiring path depends on the trait having the right shape.

**Approach:** Replace the trait method's `Option<MarketEvent>` return with `enum NextEvent { Event(MarketEvent), Idle, Terminated }`. Rewrite `pump_until_idle` and `tick` (paper orchestrator) so the loop terminates ONLY on `Terminated`; on `Idle` it skips the SPSC push and continues to drain SPSCs; on `Event(_)` it proceeds as today. `VecMarketDataFeed` returns `Terminated` after the underlying `Vec` is drained (matches today's `None` semantics for tests). Update test files that match on `next_event() == None`.

## Boundaries & Constraints

**Always:**
- `NextEvent::Terminated` is the ONLY value that ends `pump_until_idle`'s loop.
- `NextEvent::Idle` keeps the loop running — orchestrator drains SPSCs and polls again. No `break`, no early return.
- `NextEvent::Event(ev)` proceeds as today: monotonicity check, push to SPSC, drain consumer, drain orders, drain fills.
- `VecMarketDataFeed` returns `Terminated` once the `Vec` is drained — and continues to return `Terminated` on subsequent calls (not `Idle`). The "exhausted feed stays terminated" invariant is asserted in tests.
- `tick` returns `NextEvent` (changed from `()`) so callers driving their own loop can decide when to exit. Existing test calls to `tick()` ignore the return via `let _ = orch.tick();`.

**Ask First:**
- If `NextEvent` needs to derive any trait beyond `Debug` (e.g., `Clone`, `PartialEq` for tests), surface before adding.
- If any caller outside `crates/engine/src/paper/` implements `MarketDataFeed`, halt and report (investigation found only `VecMarketDataFeed` in `paper/data_feed.rs:61-68`).
- If updating `tick`'s return type breaks an unrelated `cfg(test)` accessor, surface.

**Never:**
- Do NOT change replay's `ParquetReplaySource::next_event_internal` (own method, not part of the trait — out of scope).
- Do NOT touch `OrderManager`, `BracketManager`, `EventLoop`, `CircuitBreakers`, or any subsystem wiring (Story 8.2 territory).
- Do NOT add async support to `MarketDataFeed` — the trait stays sync; Epic 8's Rithmic adapter bridges async-to-sync at its own boundary.
- Do NOT introduce a `Result<MarketEvent, FeedStatus>` variant — `Idle`/`Terminated` are normal control flow, not errors.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Feed has events queued | `feed.next_event()` | `NextEvent::Event(ev)` | N/A |
| Feed currently empty (streaming case, not yet terminated) | `feed.next_event()` between live frames | `NextEvent::Idle` — `pump_until_idle` continues, drains SPSCs | N/A |
| Feed exhausted (test `Vec`) | `feed.next_event()` after drain | `NextEvent::Terminated` — `pump_until_idle` exits | N/A |
| Exhausted feed polled again | Subsequent `next_event()` calls after first `Terminated` | `NextEvent::Terminated` (sticky) | N/A |
| `tick` while feed has events | Call `tick()` with queued events | Returns `NextEvent::Event(_)`; orchestrator pushes + drains | N/A |
| `tick` while feed is idle | Call `tick()` between live frames | Returns `NextEvent::Idle`; orchestrator drains SPSCs but doesn't push | N/A |
| `tick` after termination | Call `tick()` after feed exhausted | Returns `NextEvent::Terminated`; caller decides to exit outer loop | N/A |

</frozen-after-approval>

## Code Map

- `crates/engine/src/paper/data_feed.rs` — define `pub enum NextEvent { Event(MarketEvent), Idle, Terminated }` (with `Debug` derive); change trait method signature at line 32; update `VecMarketDataFeed` impl at lines 61-68; rewrite trait + struct rustdoc to describe the three-state contract; update unit tests at lines 88-104.
- `crates/engine/src/paper/orchestrator.rs` — rewrite `pump_until_idle` (line 393-429) to match on `NextEvent`; rewrite `tick` (line 435-449) to return `NextEvent` and apply the same three-arm match.
- `crates/engine/tests/paper_trading.rs`, `crates/engine/tests/paper_trade_recording.rs` — update any test that asserts `next_event() == None` to use `matches!(_, NextEvent::Terminated)`; update any `tick()` call that ignored the unit return to ignore the `NextEvent` return.
- `_bmad-output/implementation-artifacts/deferred-work.md` — mark 7-3 S-3 (line 182) `[x]` with one-line resolution note (commit hash). Leave B-2/B-3/B-4/B-5 entries (already tracked in their original 7-x sections) untouched.

## Tasks & Acceptance

**Execution:**
- [x] `crates/engine/src/paper/data_feed.rs` -- introduce `pub enum NextEvent { Event(MarketEvent), Idle, Terminated }` with `#[derive(Debug)]`; change trait `next_event` return type from `Option<MarketEvent>` to `NextEvent`; rewrite trait rustdoc to enumerate the three states explicitly (Event = data ready, Idle = transient empty, Terminated = stream ended forever) -- B-1 trait shape.
- [x] `crates/engine/src/paper/data_feed.rs` -- update `VecMarketDataFeed::next_event` (line 61-68) so `events.next()` -> `Some(ev)` returns `NextEvent::Event(ev)`, and `None` returns `NextEvent::Terminated` (sticky — no reachable path from `Terminated` back to `Event`/`Idle`); rewrite struct rustdoc -- B-1 test-double impl.
- [x] `crates/engine/src/paper/data_feed.rs` (tests) -- update `vec_feed_yields_in_order_then_none` and `vec_feed_empty_yields_none_immediately` to assert on `NextEvent::Event`/`NextEvent::Terminated` variants; rename to `..._then_terminated` and `..._terminated_immediately`; add new `vec_feed_terminated_state_is_sticky` asserting that subsequent calls after termination still return `Terminated` -- B-1 test coverage.
- [x] `crates/engine/src/paper/orchestrator.rs` -- rewrite `pump_until_idle` (line 393-429): replace `while let Some(event) = self.feed.next_event()` with `loop { match self.feed.next_event() { NextEvent::Event(event) => { ... existing push + drain logic ... }, NextEvent::Idle => { self.drain_market_consumer(); self.drain_orders_and_simulate_fills(); self.drain_fills(); }, NextEvent::Terminated => break, } }`. Preserve the final-drain block after the loop -- B-1 wiring (blocking pump).
- [x] `crates/engine/src/paper/orchestrator.rs` -- rewrite `tick` (line 435-449) to return `NextEvent`: `pub fn tick(&mut self) -> NextEvent`. Match on `self.feed.next_event()`: `Event(ev)` runs the SPSC push path then drains then returns `Event(ev)`; `Idle` skips push, drains, returns `Idle`; `Terminated` skips push, drains residual SPSCs, returns `Terminated` -- B-1 wiring (single-iteration variant).
- [x] `crates/engine/tests/paper_trading.rs`, `crates/engine/tests/paper_trade_recording.rs` -- update any callsite that assumed `tick() -> ()` to discard the new `NextEvent` return (e.g., `let _ = orch.tick();`); update any direct test on `feed.next_event()` to match on `NextEvent` variants -- B-1 integration test churn.
- [x] `_bmad-output/implementation-artifacts/deferred-work.md` -- mark 7-3 S-3 (line 182) `[x]` with commit-hash resolution note.

**Acceptance Criteria:**
- Given `VecMarketDataFeed::new(vec![ev1, ev2])`, when `next_event()` is called four times, then the first two return `NextEvent::Event(_)`, the third returns `NextEvent::Terminated`, and the fourth also returns `NextEvent::Terminated` (sticky).
- Given a `MarketDataFeed` impl that returns `NextEvent::Idle` indefinitely, when `pump_until_idle` is called with a brief test timeout (drop the orchestrator after N polls), then the loop did NOT exit on its own — only the explicit drop ended it. (Test via a custom mock feed in `data_feed.rs::tests` that returns `Idle` for N polls then `Terminated`; assert that `events_pushed == 0` AND `pump_until_idle` returned only after the `Terminated`.)
- Given a `VecMarketDataFeed` with two events, when `pump_until_idle` runs, then `events_pushed == 2`, the orchestrator's final state matches today's behavior, and `PaperTradingSummary` is identical to the pre-refactor result.
- Given `tick` is called on a feed returning `Idle`, when the call returns, then `events_pushed` is unchanged AND the SPSC drains were called (verified by counters).
- Given the workspace, when `rg "Option<MarketEvent>" crates/engine/src/paper/`, then zero matches.
- Given the workspace, when `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings` run, then all succeed with no new warnings.

## Spec Change Log

(empty)

## Design Notes

**Enum vs `Result`.** `Idle` and `Terminated` are normal control flow, not errors — an `enum` keeps both off the error path and lets the `match` cover the three cases exhaustively.

**Sticky `Terminated`.** Once a feed returns `Terminated`, subsequent calls must also return `Terminated` (never revert to `Idle` or `Event`). Matters because Story 8.2's host loop may call `tick()` after noticing termination. `VecMarketDataFeed` inherits this from `std::vec::IntoIter::next()`'s sticky-`None` semantics; production adapters need an explicit `terminated: bool` flag. Documented in trait rustdoc.

**Out of scope.** B-2/B-3/B-4/B-5 (originally bundled in the draft) are mechanically independent of B-1, tracked in `deferred-work.md`, and will land in a follow-up cleanup or fold into Story 8.2's first commit.

## Verification

**Commands:**
- `cargo build --workspace` -- expected: clean build.
- `cargo test --workspace --no-fail-fast` -- expected: existing + new tests pass; new `vec_feed_terminated_state_is_sticky` test green; `vec_feed_yields_..._then_terminated` rename + variant assertions pass.
- `cargo clippy --workspace --all-targets -- -D warnings` -- expected: no new lints.

**Manual checks:**
- `rg "Option<MarketEvent>" crates/engine/src/paper/` -- expected: zero matches.
- `rg "next_event\(\)" crates/engine/src/paper/ crates/engine/tests/` -- expected: every match is followed by a `match` block with three arms (`Event` / `Idle` / `Terminated`), or by `let _ = ...` if the return value is ignored. No `if let Some(...) = ...next_event()` patterns.

## Suggested Review Order

**Trait shape (the API change)**

- The new three-state contract — explicit `Event` / `Idle` / `Terminated` decoupling.
  [`data_feed.rs:26`](../../crates/engine/src/paper/data_feed.rs#L26)

- Trait method signature now returns `NextEvent`; doc comment enumerates the three states + sticky invariant.
  [`data_feed.rs:57`](../../crates/engine/src/paper/data_feed.rs#L57)

- `VecMarketDataFeed` impl — sticky `Terminated` falls out of `IntoIter::next`'s sticky-`None` semantics.
  [`data_feed.rs:90`](../../crates/engine/src/paper/data_feed.rs#L90)

**Orchestrator wiring (the load-bearing behavior change)**

- `pump_until_idle` rewritten as `loop { match }` — exits ONLY on `Terminated`.
  [`orchestrator.rs:401`](../../crates/engine/src/paper/orchestrator.rs#L401)

- `Idle` arm: drains SPSCs and yields the scheduler — bounds CPU spend without preempting Story 8.2's real backoff design.
  [`orchestrator.rs:433`](../../crates/engine/src/paper/orchestrator.rs#L433)

- `tick` now returns `NextEvent` so hosts driving their own outer loop can decide when to exit.
  [`orchestrator.rs:465`](../../crates/engine/src/paper/orchestrator.rs#L465)

**Test surface**

- Sticky-`Terminated` invariant locked: 10 polls after exhaustion, all return `Terminated`.
  [`data_feed.rs:148`](../../crates/engine/src/paper/data_feed.rs#L148)

- New `market_producer_mut` test accessor — lets the AC test pre-load the SPSC to verify the `Idle`-arm drain actually fires.
  [`orchestrator.rs:369`](../../crates/engine/src/paper/orchestrator.rs#L369)

- B-1 AC: `pump_until_idle` does NOT exit on `Idle`, AND consumes a pre-loaded SPSC event during Idle polls.
  [`orchestrator.rs:763`](../../crates/engine/src/paper/orchestrator.rs#L763)

- `tick` return-value contract — `Idle` / `Idle` / `Terminated` sequence with sticky behavior.
  [`orchestrator.rs:785`](../../crates/engine/src/paper/orchestrator.rs#L785)

**Bookkeeping**

- 7-3 S-3 marked resolved with summary of the three-state contract; six review-time defers appended.
  [`deferred-work.md`](./deferred-work.md)
