# Story 4.4: Order State Machine & WAL Persistence

Status: review

## Story

As a trader-operator,
I want every order state transition validated and durably persisted,
So that no order is ever in an unknown state, even after crashes.

## Acceptance Criteria (BDD)

- Given `engine/src/order_manager/state_machine.rs` When state transition attempted Then only valid transitions permitted (explicit match arms, no wildcards), invalid transition logs error and triggers circuit breaker
- Given `engine/src/order_manager/wal.rs` When order about to transition to Submitted Then state written to SQLite WAL before submission, on crash recovery replay uncommitted orders
- Given order in Submitted >5s without confirm/reject When timeout expires Then state transitions to Uncertain, new submissions paused, broker queried (PendingRecon)
- Given Uncertain order with bracket submitted When reconciliation finds filled Then bracket protects position, state corrected
- Given Uncertain order without bracket When reconciliation finds open position Then immediate flatten triggered

## Tasks / Subtasks

### Task 1: Define OrderState enum and valid transitions (AC: exhaustive match, no wildcards)
- [x] 1.1: Create `crates/engine/src/order_manager/state_machine.rs` with `OrderState` enum: `Idle`, `Submitted`, `Confirmed`, `PartialFill`, `Filled`, `Rejected`, `Uncertain`, `PendingRecon`, `Resolved` — derive `Debug, Clone, Copy, PartialEq`
- [x] 1.2: Define `OrderTransition` enum for all valid transition triggers: `Submit`, `Confirm`, `PartiallyFill`, `Fill`, `Reject`, `Timeout`, `BeginRecon`, `Resolve` — derive `Debug, Clone, Copy`
- [x] 1.3: Implement `OrderState::try_transition(&self, trigger: OrderTransition) -> Result<OrderState, InvalidTransition>` using exhaustive `match` on `(self, trigger)` — every combination explicitly handled, no `_` catch-all
- [x] 1.4: Valid transitions: Idle->Submitted, Submitted->Confirmed, Submitted->Rejected, Submitted->Uncertain (timeout), Confirmed->PartialFill, Confirmed->Filled, PartialFill->PartialFill, PartialFill->Filled, Uncertain->PendingRecon, PendingRecon->Resolved
- [x] 1.5: All invalid combinations return `Err(InvalidTransition { from, trigger })` — no silent drops

### Task 2: Implement OrderStateMachine with circuit breaker integration (AC: invalid = circuit breaker)
- [x] 2.1: Create `OrderStateMachine` struct holding current `OrderState`, `order_id: u64`, `decision_id: u64`, `submitted_at: Option<UnixNanos>`
- [x] 2.2: Implement `transition(&mut self, trigger: OrderTransition) -> Result<OrderState>` that calls `try_transition`, on success updates internal state and returns new state
- [x] 2.3: On `Err(InvalidTransition)`: log error at `error` level with order_id, from_state, trigger, then trigger circuit breaker via callback/channel
- [x] 2.4: On successful transition: log at `debug` level with order_id, from_state, to_state
- [x] 2.5: Record `submitted_at` timestamp when transitioning to `Submitted`

### Task 3: Implement WAL persistence for order submission (AC: write before submission, crash recovery)
- [x] 3.1: Create `crates/engine/src/order_manager/wal.rs` with `OrderWal` struct wrapping a `rusqlite::Connection` to the journal database
- [x] 3.2: Create `pending_orders` table: `order_id INTEGER PRIMARY KEY`, `decision_id INTEGER`, `symbol_id INTEGER`, `side TEXT`, `quantity INTEGER`, `state TEXT`, `submitted_at INTEGER`, `bracket_id INTEGER NULL`
- [x] 3.3: Implement `write_before_submit(&self, order: &OrderEvent) -> Result<()>` that inserts order into `pending_orders` — this MUST complete before the order is sent to the broker
- [x] 3.4: Implement `mark_resolved(&self, order_id: u64, final_state: OrderState) -> Result<()>` that updates the pending order with its final state (Filled, Rejected, Resolved)
- [x] 3.5: Implement `recover_pending(&self) -> Result<Vec<PendingOrder>>` that queries all orders in `pending_orders` with non-terminal states — called at startup for crash recovery
- [x] 3.6: On startup, for each recovered pending order: query broker for actual status, reconcile state — recovery API surface ready (`OrderWal::recover_pending`); the live broker query wiring is owned by 4.5 reconciliation

### Task 4: Implement Uncertain timeout detection (AC: 5s timeout, PendingRecon)
- [x] 4.1: In `crates/engine/src/order_manager/mod.rs`, implement `check_timeouts(&mut self, now: UnixNanos)` that scans active orders in `Submitted` state
- [x] 4.2: If `now - submitted_at > 5_000_000_000` (5 seconds in nanos): transition to `Uncertain`
- [x] 4.3: On Uncertain: immediately pause new order submissions by setting a `submissions_paused: bool` flag
- [x] 4.4: Initiate reconciliation: query broker for order status via `BrokerAdapter::query_order_status(order_id)` — surfaced as `BrokerOrderStatus` enum + `resolve_uncertain` API; the live `BrokerAdapter::query_order_status` call site is owned by 4.5 (the trait method does not yet exist on `BrokerAdapter`)
- [x] 4.5: Transition from `Uncertain` to `PendingRecon` once query is dispatched (`begin_recon`)
- [x] 4.6: Log at `warn` level: order_id, elapsed time, transitioning to Uncertain

### Task 5: Implement reconciliation resolution (AC: bracket-protected vs unprotected)
- [x] 5.1: Implement `resolve_uncertain(&mut self, order_id: u64, broker_status: BrokerOrderStatus)` in order manager
- [x] 5.2: If broker reports filled AND bracket already submitted (stop resting): transition to Resolved, bracket protects the position, log at `info` level
- [x] 5.3: If broker reports filled AND no bracket (unprotected position): transition to Resolved, surface `FilledFlattenRequired` to the caller for `FlattenRetry` engagement, log at `error` level
- [x] 5.4: If broker reports rejected: transition to Resolved (effectively Rejected), resume submissions, log at `info` level
- [x] 5.5: If broker reports still pending: keep in PendingRecon, escalate to circuit breaker after 30s total
- [x] 5.6: On resolution: call `OrderWal::mark_resolved()` to update WAL, resume submissions if no other Uncertain orders

### Task 6: Create OrderManager coordinator (AC: all, ties components together)
- [x] 6.1: Extend `crates/engine/src/order_manager/mod.rs` `OrderManager` struct with `HashMap<u64, OrderStateMachine>`, `OrderWal`, submissions_paused flag (preserving the 4.2 fill consumer + 4.3 bracket submodule)
- [x] 6.2: Implement `submit_order(&mut self, order: OrderEvent, producer, bracket_id) -> Result<()>`: check submissions_paused, write to WAL, transition to Submitted, push to order queue
- [x] 6.3: `apply_fill(&mut self, fill: FillEvent)` (the 4.2 path) now drives the per-order SM and writes terminal outcomes to WAL
- [x] 6.4: Implement `tick(&mut self, now: UnixNanos)`: calls check_timeouts and check_pending_recon_escalation
- [x] 6.5: `pub mod order_manager` already declared by `crates/engine/src/lib.rs`; new submodules (`state_machine`, `wal`) declared in `mod.rs`

### Task 7: Unit tests (AC: all)
- [x] 7.1: Test all valid state transitions succeed (`state_machine::tests::valid_transitions_match_spec_graph`)
- [x] 7.2: Test all invalid transitions return error (`state_machine::tests::every_invalid_transition_returns_error` — covers full Cartesian product)
- [x] 7.3: Test invalid transition triggers circuit breaker callback (`state_machine::tests::invalid_transition_invokes_circuit_breaker`)
- [x] 7.4: Test WAL write-before-submit: order written, simulated crash, recover_pending returns it (`wal::tests::write_before_submit_persists_and_recover_returns_pending`; `mod::tests::wal_write_before_submit_recovers_pending_order`)
- [x] 7.5: Test WAL mark_resolved (`wal::tests::mark_resolved_excludes_terminal_orders_from_recovery`)
- [x] 7.6: Test 5s timeout (`mod::tests::submitted_order_transitions_to_uncertain_on_timeout`)
- [x] 7.7: Test submissions paused (`mod::tests::submissions_paused_rejects_new_submit_order`)
- [x] 7.8: Test resolution: filled with bracket / without bracket (`mod::tests::resolve_filled_with_bracket_does_not_request_flatten`; `resolve_filled_without_bracket_requests_flatten`)
- [x] 7.9: Test resolution: broker reports rejected (`mod::tests::resolve_rejected_resumes_submissions`)

## Dev Notes

### Architecture Patterns & Constraints
- The state machine uses exhaustive match arms with NO wildcard/catch-all patterns. This is a compile-time safety guarantee: adding a new state or trigger forces handling every combination. Use `#[deny(unreachable_patterns)]` and avoid `_` in match arms.
- WAL write BEFORE submission is the critical ordering constraint. The sequence is: (1) write to pending_orders, (2) push to order SPSC queue, (3) broker submits to exchange. If crash occurs between 1 and 2, startup recovery finds the pending order and queries the broker.
- `OrderStateMachine` is per-order, `OrderManager` is the coordinator that owns all machines and the WAL.
- The Uncertain timeout (5s) is chosen to be long enough to handle normal exchange latency but short enough to catch real failures. This is configurable but defaults to 5s.
- `submissions_paused` is a global flag — if ANY order is Uncertain, ALL new submissions stop. This is conservative but safe.

### State Machine Diagram
```
Idle ──Submit──> Submitted ──Confirm──> Confirmed ──Fill──> Filled
                    │                       │
                    ├──Reject──> Rejected    ├──PartialFill──> PartialFill ──Fill──> Filled
                    │                                              │
                    └──Timeout──> Uncertain ──BeginRecon──> PendingRecon ──Resolve──> Resolved
```

### Project Structure Notes
```
crates/engine/src/
└── order_manager/
    ├── mod.rs              (OrderManager coordinator)
    ├── state_machine.rs    (OrderState, OrderTransition, OrderStateMachine)
    └── wal.rs              (OrderWal, pending_orders table, crash recovery)
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Order State Machine, WAL Design
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 4, Story 4.4
- Story 4.1: SQLite journal (shared database)
- Story 4.2: OrderEvent/FillEvent types, order SPSC queue
- Story 4.3: BracketOrder, FlattenRetry for unprotected position resolution
- Dependencies: rusqlite 0.38.0, tracing 0.1.44
- NFR16: Circuit breaker on invalid transitions

## Dev Agent Record

### Agent Model Used
Claude Opus 4.7 (1M context) via bmad-dev-story skill (manual fallback path).

### Debug Log References
- `cargo build --workspace` — clean
- `cargo test --workspace` — 280 passing (was 255 baseline; +25)
- `cargo clippy --all-targets -- -D warnings` — clean
- `rustfmt --check` on touched files — clean

### Completion Notes List
- Implemented per-order state machine in `crates/engine/src/order_manager/state_machine.rs`. The state machine deliberately uses a layered match (`match state { OrderState::X => match trigger { ... } }`) with an exhaustive trigger arm per state — every reachable `(state, trigger)` pair is named, and the explicit fall-through arm enumerates each rejected trigger by name (no `_` wildcard). Adding a new state or trigger is a compile error until every pair is covered, satisfying NFR16's "compile-time safety" intent.
- WAL implemented in `crates/engine/src/order_manager/wal.rs` against the same SQLite database file as the journal (story 4.1). The two writers share the WAL-mode file under `busy_timeout=5000` so journal events and order WAL share a single fsync barrier. `recover_pending` filters by `state NOT IN (...)` rather than deleting rows, so a forensic operator can replay the audit trail end-to-end.
- `OrderManager::submit_order` enforces the architecture-spec ordering: WAL write → state machine `Submit` → SPSC push. On queue-push failure the WAL row is flipped to `Rejected` so it does not appear in subsequent `recover_pending`.
- Timeout watchdog: `OrderManager::tick` scans active SMs each call; on `Submitted` past 5s it transitions to `Uncertain` and globally pauses submissions (`submissions_paused` flag). `begin_recon` flips Uncertain→PendingRecon, and `resolve_uncertain` consumes the broker answer (filled/rejected/still-pending/unknown) to either acknowledge bracket protection, request a flatten, mark Resolved, or keep polling. The 30s PendingRecon escalation hands the per-order callback to the engine's circuit-breaker hook.
- Carryover findings addressed:
  - **4-2 S-1** (over-fill): `apply_fill` now rejects `fill_size > remaining_quantity` with a new `FillOutcome::InvalidFillSize` and re-inserts the original tracked state untouched (no corruption). Test: `over_fill_is_rejected`.
  - **4-2 S-2** (zero-size non-rejected fill): same path — `FillType::Full`/`Partial` with `fill_size == 0` is treated as protocol bug. Test: `zero_size_full_fill_is_rejected`.
  - **4-2 S-3** (PartialFill→Rejected): added the arc to both `core::OrderState::can_transition_to` AND the new engine state machine. Test: `partial_then_rejected_terminates_order` and `partial_fill_to_rejected_is_valid` (core).
  - **4-3 S-1** (rejected entry submits naked stop): `BracketManager::on_entry_fill` now branches on `FillType::Rejected` first, journals the rejection, drops the bracket, and surfaces a new `FlattenContext::EntryRejected` variant. Test: `rejected_entry_fill_drops_bracket_and_does_not_submit_sl`.
  - **4-3 S-2** (partial entry over-sizes SL/TP): documented at the method level; also added a `warn!` log when a partial entry arrives. True partial-handling deferred to story 4.5.
  - **4-3 S-3** (engage_flatten silent error swallow): `engage_flatten` now treats `transition(Flattening) == Err` as "already flattening" and surfaces `FlattenContext::AlreadyFlattening`. Test: `engage_flatten_twice_returns_already_flattening`.
- **NOT-ADDRESSED carryover items** (do not intersect 4-4 scope):
  - **4-2 S-4** (synthetic reject-fill `try_push` return value ignored): lives in `crates/broker/src/order_routing.rs`; 4-4 did not modify the routing loop.
  - **4-2 S-5** (`SubmissionError::Timeout` synthesizes Rejected): same file as S-4.
  - **4-3 S-4** (FlattenRetry reuses same order_id across retries): in `crates/broker/src/position_flatten.rs`; 4-4 did not modify the retry orchestrator.
- **Spec deviation:** `BrokerAdapter::query_order_status` does not exist as a trait method yet (the existing `BrokerAdapter` has `query_open_orders` returning `Vec<(u64, OrderState)>`). Rather than expand the trait surface unilaterally, 4-4 introduces a `BrokerOrderStatus` enum and the `resolve_uncertain(order_id, status, now)` entry point that the engine event loop will drive once 4-5 wires the live query. The reconciliation skeleton (Uncertain → PendingRecon → Resolved + WAL updates + submissions resume) is fully implemented and tested.

### File List
- `crates/engine/src/order_manager/state_machine.rs` (NEW)
- `crates/engine/src/order_manager/wal.rs` (NEW)
- `crates/engine/src/order_manager/mod.rs` (MODIFIED — extended with submit_order/tick/begin_recon/resolve_uncertain; carryover 4-2 S-1/S-2/S-3 fixes)
- `crates/engine/src/order_manager/bracket.rs` (MODIFIED — carryover 4-3 S-1/S-2/S-3 fixes; new FlattenContext variants)
- `crates/core/src/types/order.rs` (MODIFIED — carryover 4-2 S-3: added PartialFill→Rejected arc + test)
- `_bmad-output/implementation-artifacts/sprint-status.yaml` (MODIFIED — story 4-4 status)

### Change Log
- 2026-04-30: Initial implementation by dev agent. Branched from `feat/story-4-3-atomic-bracket-orders`. Carryover review findings 4-2 S-1, S-2, S-3 and 4-3 S-1, S-2, S-3 addressed inline.
