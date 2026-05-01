---
title: 'Close pre-Epic-6 live-trading exit gates (D-1/D-2/D-3)'
type: 'refactor'
created: '2026-05-01'
status: 'done'
baseline_commit: '1e7969b'
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-5-retro-2026-05-01.md'
  - '{project-root}/_bmad-output/implementation-artifacts/deferred-work.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** Three Epic 5 retrospective action items remain open and block Epic 6: (D-1) `CircuitBreakers::permits_trading` ignores panic mode and lacks a `DenialReason::PanicModeActive` variant, leaving two parallel "can we trade?" gates; (D-2) `check_position_anomaly` and `handle_anomaly` have no production caller, so Story 5.3's anomaly-flatten AC is unsatisfied at integration; (D-3) fee-staleness is duplicated between `FeeGate::permits_trade` and `CircuitBreakers::check_fee_staleness`.

**Approach:** Make `CircuitBreakers` the single trading-gate authority. (D-1) Inject `Arc<PanicMode>` into `CircuitBreakers`; consult it from `permits_trading` / `permits_trade_evaluation`; add `DenialReason::PanicModeActive`. (D-2) Wire `EventLoop` to call `check_position_anomaly` against a `PositionTracker` snapshot on each tick; on trip, `try_send` a `FlattenRequest` on a new `tokio::sync::mpsc::Sender<FlattenRequest>` field. The matching `Receiver` and the async `handle_anomaly` consumer task remain owned by Story 8.2. (D-3) Strip the `>60 days` check and `FeeGateReason::StaleSchedule` from `FeeGate`; `CircuitBreakers::check_fee_staleness` becomes the single source.

## Boundaries & Constraints

**Always:**
- `CircuitBreakers` is the single authority for "should we trade?"; all gate state mutations route through its public API.
- `panic_mode` consultation precedes per-breaker checks in `permits_trading` / `permits_trade_evaluation`; on `is_trading_enabled() == false`, `DenialReason::PanicModeActive` is the FIRST entry in the returned reasons.
- `EventLoop` retains its synchronous, single-threaded hot-loop discipline. No `.await`, no Tokio runtime introduced inside `EventLoop`.
- The `FlattenRequest` channel is `tokio::sync::mpsc` so Story 8.2's async consumer can `.await recv()`. Producer side uses `try_send`; full-channel case logs warn + journals `SystemEvent`, never blocks or panics.

**Ask First:**
- If extending `CircuitBreakers::new` breaks more than ~10 fixtures, propose a `with_panic_mode(...)` chain method.
- If `FeeGate::permits_trade` has consumers outside `crates/engine/src/risk/` that match on `FeeGateReason::StaleSchedule`, surface them before deletion.

**Never:**
- Do NOT introduce a Tokio runtime inside `EventLoop` or spawn `handle_anomaly` from the engine crate (Story 8.2).
- Do NOT add a production caller for `permits_trading` from `OrderManager::submit_order` (Story 8.2). This spec only ensures `permits_trading` returns the right answer when called.
- Do NOT delete `FeeGate::permits_trade` itself — only the staleness branch is removed; edge-vs-fee logic stays.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Panic active, breakers Active | `PanicMode::activate()` called | `permits_trading() → Err(TradingDenied)`, `reasons[0] == PanicModeActive` | N/A |
| Panic inactive, breakers Active | Default | `permits_trading() → Ok(())` | N/A |
| Anomaly, divergence | Tracker long-3 ES; expected flat | `AnomalousPosition` trips; `try_send` `FlattenRequest { symbol_id, side: Sell, qty: 3 }` | Sender full → warn + journal `SystemEventRecord { category: "flatten_request_dropped", .. }`; never panics |
| Anomaly, no divergence | Tracker matches expected | No trip, no `FlattenRequest` | N/A |
| Fee schedule 65d old | `effective_date = today - 65d` | `check_fee_staleness` trips `FeeStaleness`; `FeeGate::permits_trade` returns based on edge alone | N/A |
| Fee schedule 30d old | `effective_date = today - 30d` | `FeeStaleness` Active; warn-only (relocated or retained) | N/A |

</frozen-after-approval>

## Code Map

- `crates/engine/src/risk/circuit_breakers.rs` — Add `panic_mode: Arc<PanicMode>` field; thread through `new()`. Consult panic mode in both `permits_trading` and `permits_trade_evaluation`.
- `crates/engine/src/risk/mod.rs` — Add `DenialReason::PanicModeActive` variant + Display arm.
- `crates/engine/src/risk/anomaly_handler.rs` — Promote/confirm `FlattenRequest` as a public struct usable as `mpsc::Sender<FlattenRequest>` payload. Update module docstring to describe pre-Epic-6 producer wiring + deferred 8.2 consumer.
- `crates/engine/src/event_loop.rs` — Add optional fields: `position_tracker: Option<Arc<PositionTracker>>`, `expected_positions: Option<Box<dyn ExpectedPositionSource>>`, `flatten_tx: Option<mpsc::Sender<FlattenRequest>>`. Add `attach_anomaly_detection(...)` and per-tick anomaly check.
- `crates/engine/src/risk/fee_gate.rs` — Remove `>60 days` branch from `permits_trade`; remove `FeeGateReason::StaleSchedule`. Keep edge-vs-fee logic.
- `crates/engine/src/order_manager/tracker.rs` — Read-only consumer; verify `PositionTracker` exposes a snapshot iterator suitable for the anomaly check.
- `_bmad-output/implementation-artifacts/deferred-work.md` — Mark D-1/D-2/D-3 resolved; flag Story 8.2's remaining ownership of the `Receiver` drain and `permits_trading` call site.

## Tasks & Acceptance

**Execution:**
- [x] `crates/engine/src/risk/mod.rs` -- add `DenialReason::PanicModeActive` variant + Display impl -- single source for the new denial reason.
- [x] `crates/engine/src/risk/circuit_breakers.rs` -- add `panic_mode: Arc<PanicMode>` field; thread through `new()` (or `with_panic_mode` per Ask First); in both `permits_trading` and `permits_trade_evaluation`, check `panic_mode.is_trading_enabled()` first and prepend `PanicModeActive` to reasons on false -- D-1 fix.
- [x] `crates/engine/src/risk/circuit_breakers.rs` (tests) -- panic-active denies; reasons-list ordering (PanicModeActive first); panic-inactive permits when breakers Active.
- [x] `crates/engine/src/risk/anomaly_handler.rs` -- expose `FlattenRequest` as a public, `Send + 'static` struct; update module docstring -- prep the producer/consumer seam.
- [x] `crates/engine/src/event_loop.rs` -- add a private `ExpectedPositionSource` trait with default empty impl; add the three optional fields; add `attach_anomaly_detection(tracker, expected, sender)`; add per-tick anomaly check that iterates tracker positions, calls `check_position_anomaly`, and `try_send`s `FlattenRequest` on trip; on `try_send` full, log warn + journal `SystemEventRecord { category: "flatten_request_dropped", .. }` -- D-2 producer wiring.
- [x] `crates/engine/src/event_loop.rs` (tests) -- divergence trips & produces FlattenRequest; no divergence produces nothing; full channel logs+journals warn without panic.
- [x] `crates/engine/src/risk/fee_gate.rs` -- delete `>60 days` branch from `permits_trade`; delete `FeeGateReason::StaleSchedule`; if the `>30 days` warn was a gate side effect, relocate it as a `tracing::warn!` from `CircuitBreakers::check_fee_staleness` -- D-3 fix.
- [x] `crates/engine/src/risk/fee_gate.rs` (tests) -- update fixtures asserting `StaleSchedule`; move staleness assertions to `circuit_breakers.rs` tests against `check_fee_staleness`.
- [x] `_bmad-output/implementation-artifacts/deferred-work.md` -- mark D-1/D-2/D-3 sources `[x]` with resolution note (commit hash); update Live-Trading Exit Gates table; explicitly note that the matching `Receiver<FlattenRequest>` drain and the `permits_trading()` invocation from `OrderManager::submit_order` remain owned by Story 8.2.

**Acceptance Criteria:**
- Given panic mode active, when any caller invokes `CircuitBreakers::permits_trading()`, then it returns `Err(TradingDenied)` with `reasons[0] == DenialReason::PanicModeActive`.
- Given a `PositionTracker` snapshot diverges from `ExpectedPositionSource`, when `EventLoop` runs its anomaly-check tick with all three seams attached, then `AnomalousPosition` trips and a `FlattenRequest` is `try_send`-ed on `flatten_tx`.
- Given the `FlattenRequest` channel is full, when the producer publishes, then it logs warn, journals a `SystemEventRecord` with `category = "flatten_request_dropped"`, and does NOT panic or block.
- Given a fee schedule >60 days stale, when `CircuitBreakers::check_fee_staleness` runs, then `FeeStaleness` trips, `permits_trade_evaluation` denies, AND `FeeGate::permits_trade` returns purely on edge-vs-fee economics with no `StaleSchedule` discriminant in scope.
- Given the workspace, when `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings` run, then all succeed with no new warnings.

## Spec Change Log

(empty)

## Design Notes

**D-1 ordering.** `PanicModeActive` first in `reasons` lets operator alerts attribute the denial to panic mode rather than secondary breaker triggers it likely caused.

**D-2 channel.** `tokio::sync::mpsc` (not `std::sync::mpsc`) because the consumer in Story 8.2 will `.await recv()`. Producer uses `try_send` to keep `EventLoop` non-blocking.

**D-2 ExpectedPositionSource.** A trait with a default empty impl today (`EmptyExpected`: any non-zero PositionTracker entry is "anomalous"). Epic 6's strategy orchestrator provides the real impl. Decouples the anomaly seam from Epic 6's strategy code.

**D-3 ownership.** Removing the duplicate from `FeeGate` collapses to one source. `FeeGate` becomes pure economics (edge vs fee). `CircuitBreakers` owns all gate state.

## Verification

**Commands:**
- `cargo build --workspace` -- expected: clean build.
- `cargo test --workspace --no-fail-fast` -- expected: existing + new tests pass.
- `cargo clippy --workspace --all-targets -- -D warnings` -- expected: no new lints.

**Manual checks:**
- `rg 'FeeGateReason::StaleSchedule' crates/` -- expected: zero matches.
- `rg 'DenialReason::PanicModeActive' crates/` -- expected: ≥2 matches (impl + tests).

## Suggested Review Order

**D-1 — Panic mode is now the single trading authority**

- New variant; panic mode has no breaker hence `breaker_type()` returns `Option`.
  [`mod.rs:76`](../../crates/engine/src/risk/mod.rs#L76)

- Single-snapshot panic-mode consultation prepends `PanicModeActive` to the reasons vec.
  [`circuit_breakers.rs:288`](../../crates/engine/src/risk/circuit_breakers.rs#L288)

- Symmetric path for the trade-evaluation gate.
  [`circuit_breakers.rs:1150`](../../crates/engine/src/risk/circuit_breakers.rs#L1150)

**D-3 — Fee staleness deduplicated; CircuitBreakers is the canonical owner**

- `FeeGate::permits_trade` is now pure edge-vs-fee economics — no staleness branch, no `_clock` param.
  [`fee_gate.rs:83`](../../crates/engine/src/risk/fee_gate.rs#L83)

- `check_fee_staleness` owns the `>60d` trip and the relocated `>30d` warn.
  [`circuit_breakers.rs:1094`](../../crates/engine/src/risk/circuit_breakers.rs#L1094)

- Caller now expects `Ok(true|false)` only — the `Err` arm is structurally unreachable.
  [`composite.rs:132`](../../crates/engine/src/signals/composite.rs#L132)

**D-2 — Anomaly producer wired into EventLoop (Story 8.2 owns the consumer)**

- The Epic-6 strategy seam: a trait whose default impl flags every held position as anomalous.
  [`event_loop.rs:39`](../../crates/engine/src/event_loop.rs#L39)

- `EmptyExpectedPositions` carries a SAFETY doc; `is_empty_default()` lets the producer warn when wired bare.
  [`event_loop.rs:69`](../../crates/engine/src/event_loop.rs#L69)

- Four-collaborator wiring point with `debug_assert!` against double-attach plus the bare-default warn.
  [`event_loop.rs:239`](../../crates/engine/src/event_loop.rs#L239)

- Per-tick check: iterate tracker, call `check_position_anomaly`, `try_send` `FlattenRequest`, journal on full/closed, disable producer on closed.
  [`event_loop.rs:429`](../../crates/engine/src/event_loop.rs#L429)

- Saturating `u32 → i32` cast — corrupt large quantities clamp to `i32::MAX` instead of flipping sign.
  [`event_loop.rs:50`](../../crates/engine/src/event_loop.rs#L50)

- Producer/consumer split documented in the module docstring; consumer is Story 8.2's.
  [`anomaly_handler.rs:1`](../../crates/engine/src/risk/anomaly_handler.rs#L1)

- `FlattenRequest` re-exported through `risk` so producer + future consumer share one import path.
  [`mod.rs:52`](../../crates/engine/src/risk/mod.rs#L52)

**Tests**

- D-2 wiring: divergence trips & publishes; no-divergence stays silent; full channel logs+journals; closed channel disables producer.
  [`event_loop.rs:953`](../../crates/engine/src/event_loop.rs#L953)

- Saturation guard for corrupt quantity (Patch 1).
  [`event_loop.rs:1138`](../../crates/engine/src/event_loop.rs#L1138)

- D-1 panic-first ordering and panic-precedes-breaker semantics.
  [`circuit_breakers.rs:2523`](../../crates/engine/src/risk/circuit_breakers.rs#L2523)

- D-3 composite test now asserts staleness no longer blocks at the FeeGate layer.
  [`composite_tests.rs`](../../crates/engine/tests/composite_tests.rs)

**Bookkeeping**

- D-1/D-2/D-3 marked resolved in their original sections; new "review of pre-epic-6 cleanup spec" section captures 7 forward-looking items for Story 8.2 / Epic 6.
  [`deferred-work.md`](./deferred-work.md)
