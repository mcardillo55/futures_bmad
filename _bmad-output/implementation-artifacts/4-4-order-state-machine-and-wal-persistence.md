# Story 4.4: Order State Machine & WAL Persistence

Status: ready-for-dev

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
- 1.1: Create `crates/engine/src/order_manager/state_machine.rs` with `OrderState` enum: `Idle`, `Submitted`, `Confirmed`, `PartialFill`, `Filled`, `Rejected`, `Uncertain`, `PendingRecon`, `Resolved` — derive `Debug, Clone, Copy, PartialEq`
- 1.2: Define `OrderTransition` enum for all valid transition triggers: `Submit`, `Confirm`, `PartiallyFill`, `Fill`, `Reject`, `Timeout`, `BeginRecon`, `Resolve` — derive `Debug, Clone, Copy`
- 1.3: Implement `OrderState::try_transition(&self, trigger: OrderTransition) -> Result<OrderState, InvalidTransition>` using exhaustive `match` on `(self, trigger)` — every combination explicitly handled, no `_` catch-all
- 1.4: Valid transitions: Idle->Submitted, Submitted->Confirmed, Submitted->Rejected, Submitted->Uncertain (timeout), Confirmed->PartialFill, Confirmed->Filled, PartialFill->PartialFill, PartialFill->Filled, Uncertain->PendingRecon, PendingRecon->Resolved
- 1.5: All invalid combinations return `Err(InvalidTransition { from, trigger })` — no silent drops

### Task 2: Implement OrderStateMachine with circuit breaker integration (AC: invalid = circuit breaker)
- 2.1: Create `OrderStateMachine` struct holding current `OrderState`, `order_id: u64`, `decision_id: u64`, `submitted_at: Option<UnixNanos>`
- 2.2: Implement `transition(&mut self, trigger: OrderTransition) -> Result<OrderState>` that calls `try_transition`, on success updates internal state and returns new state
- 2.3: On `Err(InvalidTransition)`: log error at `error` level with order_id, from_state, trigger, then trigger circuit breaker via callback/channel
- 2.4: On successful transition: log at `debug` level with order_id, from_state, to_state
- 2.5: Record `submitted_at` timestamp when transitioning to `Submitted`

### Task 3: Implement WAL persistence for order submission (AC: write before submission, crash recovery)
- 3.1: Create `crates/engine/src/order_manager/wal.rs` with `OrderWal` struct wrapping a `rusqlite::Connection` to the journal database
- 3.2: Create `pending_orders` table: `order_id INTEGER PRIMARY KEY`, `decision_id INTEGER`, `symbol_id INTEGER`, `side TEXT`, `quantity INTEGER`, `state TEXT`, `submitted_at INTEGER`, `bracket_id INTEGER NULL`
- 3.3: Implement `write_before_submit(&self, order: &OrderEvent) -> Result<()>` that inserts order into `pending_orders` — this MUST complete before the order is sent to the broker
- 3.4: Implement `mark_resolved(&self, order_id: u64, final_state: OrderState) -> Result<()>` that updates the pending order with its final state (Filled, Rejected, Resolved)
- 3.5: Implement `recover_pending(&self) -> Result<Vec<PendingOrder>>` that queries all orders in `pending_orders` with non-terminal states — called at startup for crash recovery
- 3.6: On startup, for each recovered pending order: query broker for actual status, reconcile state

### Task 4: Implement Uncertain timeout detection (AC: 5s timeout, PendingRecon)
- 4.1: In `crates/engine/src/order_manager/mod.rs`, implement `check_timeouts(&mut self, now: UnixNanos)` that scans active orders in `Submitted` state
- 4.2: If `now - submitted_at > 5_000_000_000` (5 seconds in nanos): transition to `Uncertain`
- 4.3: On Uncertain: immediately pause new order submissions by setting a `submissions_paused: bool` flag
- 4.4: Initiate reconciliation: query broker for order status via `BrokerAdapter::query_order_status(order_id)`
- 4.5: Transition from `Uncertain` to `PendingRecon` once query is dispatched
- 4.6: Log at `warn` level: order_id, elapsed time, transitioning to Uncertain

### Task 5: Implement reconciliation resolution (AC: bracket-protected vs unprotected)
- 5.1: Implement `resolve_uncertain(&mut self, order_id: u64, broker_status: BrokerOrderStatus)` in order manager
- 5.2: If broker reports filled AND bracket already submitted (stop resting): transition to Resolved, bracket protects the position, log at `info` level — no additional action needed
- 5.3: If broker reports filled AND no bracket (unprotected position): transition to Resolved, trigger immediate flatten via `FlattenRetry` (Story 4.3), log at `error` level
- 5.4: If broker reports rejected: transition to Resolved (effectively Rejected), resume submissions, log at `info` level
- 5.5: If broker reports still pending: keep in PendingRecon, retry query after 1s — escalate to circuit breaker after 30s total
- 5.6: On resolution: call `OrderWal::mark_resolved()` to update WAL, resume submissions if no other Uncertain orders

### Task 6: Create OrderManager coordinator (AC: all, ties components together)
- 6.1: Create `crates/engine/src/order_manager/mod.rs` with `OrderManager` struct holding `HashMap<u64, OrderStateMachine>`, `OrderWal`, submissions_paused flag
- 6.2: Implement `submit_order(&mut self, order: OrderEvent) -> Result<()>`: check submissions_paused, write to WAL, transition to Submitted, push to order queue
- 6.3: Implement `on_fill(&mut self, fill: FillEvent)`: look up order, transition state, write to journal
- 6.4: Implement `tick(&mut self, now: UnixNanos)`: call check_timeouts for all active orders
- 6.5: Update `crates/engine/src/lib.rs` to declare `pub mod order_manager`

### Task 7: Unit tests (AC: all)
- 7.1: Test all valid state transitions succeed and return correct new state
- 7.2: Test all invalid transitions return error (exhaustive: test every invalid combination)
- 7.3: Test invalid transition triggers circuit breaker callback
- 7.4: Test WAL write-before-submit: order written, then simulated crash, recover_pending returns it
- 7.5: Test WAL mark_resolved: resolved orders not returned by recover_pending
- 7.6: Test 5s timeout: order submitted, 5s elapses, transitions to Uncertain
- 7.7: Test submissions paused during Uncertain state — new submit_order returns error
- 7.8: Test resolution: filled with bracket -> no flatten, filled without bracket -> flatten triggered
- 7.9: Test resolution: broker reports rejected -> state corrected, submissions resumed

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
### Debug Log References
### Completion Notes List
### File List
