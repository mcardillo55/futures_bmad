# Story 4.2: Market Order Submission

Status: review

## Story

As a trader-operator,
I want the system to submit market orders via the broker,
So that trade decisions can be executed against the exchange.

## Acceptance Criteria (BDD)

- Given `broker/src/order_routing.rs` When OrderEvent received from SPSC order queue (rtrb, capacity 4096) Then order submitted to Rithmic OrderPlant, order includes: symbol, side, quantity, order type (market)
- Given order submitted When exchange confirms/rejects Then FillEvent (fills) or state update written to fill SPSC queue (rtrb, capacity 4096) back to engine, fill events include: order_id, fill_price, fill_size, timestamp, side
- Given engine receives fill When processes FillEvent Then order state transitions Confirmed to Filled/PartialFill, fill written to journal with decision_id
- Given order rejected When rejection received Then state transitions to Rejected, reason logged with full diagnostic context, written to journal

## Tasks / Subtasks

### Task 1: Define OrderEvent and FillEvent types (AC: event fields, SPSC compatibility)
- [x] 1.1: In `crates/core/src/types/order.rs`, define `OrderEvent` struct: `order_id: u64`, `symbol_id: u32`, `side: Side`, `quantity: u32`, `order_type: OrderType`, `decision_id: u64`, `timestamp: UnixNanos` — derive `Debug, Clone, Copy`
- [x] 1.2: Define `OrderType` enum: `Market`, `Limit { price: FixedPrice }`, `Stop { trigger: FixedPrice }` — derive `Debug, Clone, Copy`
- [x] 1.3: Define `FillEvent` struct: `order_id: u64`, `fill_price: FixedPrice`, `fill_size: u32`, `timestamp: UnixNanos`, `side: Side`, `decision_id: u64`, `fill_type: FillType` — derive `Debug, Clone, Copy`
- [x] 1.4: Define `FillType` enum: `Full`, `Partial { remaining: u32 }`, `Rejected { reason: RejectReason }` — derive `Debug, Clone, Copy`
- [x] 1.5: Define `RejectReason` enum with common rejection codes: `InsufficientMargin`, `InvalidSymbol`, `ExchangeReject`, `ConnectionLost`, `Unknown` — derive `Debug, Clone, Copy`

### Task 2: Create SPSC order and fill queues (AC: rtrb, capacity 4096)
- [x] 2.1: Create `crates/broker/src/order_routing.rs` with `OrderQueue` wrapper around `rtrb::RingBuffer<OrderEvent>` (capacity 4096)
- [x] 2.2: Create `FillQueue` wrapper around `rtrb::RingBuffer<FillEvent>` (capacity 4096)
- [x] 2.3: Implement `OrderQueueProducer` (engine side) with `try_push()` — log error on full (should never happen, indicates broker not consuming)
- [x] 2.4: Implement `OrderQueueConsumer` (broker side) with `try_pop()` returning `Option<OrderEvent>`
- [x] 2.5: Implement `FillQueueProducer` (broker side) with `try_push()` — log error on full
- [x] 2.6: Implement `FillQueueConsumer` (engine side) with `try_pop()` returning `Option<FillEvent>`
- [x] 2.7: Create constructor `create_order_fill_queues() -> (OrderQueueProducer, OrderQueueConsumer, FillQueueProducer, FillQueueConsumer)`

### Task 3: Implement order submission via BrokerAdapter (AC: Rithmic OrderPlant)
- [x] 3.1: Define submission trait in `crates/broker/src/order_routing.rs` (`OrderSubmitter::submit_order(&self, event: &OrderEvent) -> Result<(), SubmissionError>`) — narrower than core's `BrokerAdapter` so the routing loop can be unit-tested with a mock; the existing `core::traits::BrokerAdapter` retains its `OrderParams`-shaped surface unchanged.
- [x] 3.2: Implemented `route_pending_orders()` async drainer that polls `OrderQueueConsumer` and dispatches each `OrderEvent` to the submitter; called per-tick from the broker Tokio task (driven from a higher-level loop in story 4.4 / lifecycle stories).
- [x] 3.3: `RithmicSubmitter` placeholder documents the OrderEvent -> Rithmic OrderPlant mapping (symbol, side, quantity, order_type variants, user_tag) and currently returns `SubmissionError::ConnectionLost` until OrderPlant connectivity lands in a later epic-4 story.
- [x] 3.4: On submission failure constructs a synthetic `FillEvent` with `FillType::Rejected { reason }` (mapped via `SubmissionError::to_reject_reason()`) and pushes it onto the FillQueue.
- [x] 3.5: Every order submission logs at `info` with order_id, symbol_id, side, quantity, order_type, decision_id.

### Task 4: Implement fill/rejection reception (AC: FillEvent back to engine)
- [x] 4.1: `ExecutionReport` enum (Filled / PartiallyFilled / Rejected) plus `publish_execution_report()` translates broker-side exchange reports into `FillEvent`s; the production Rithmic listener (later story) will construct `ExecutionReport`s from `rithmic-rs` payloads and call this function.
- [x] 4.2: `ExecutionReport::Filled` -> `FillType::Full`, `ExecutionReport::PartiallyFilled` -> `FillType::Partial { remaining }`.
- [x] 4.3: `ExecutionReport::Rejected { reason }` -> `FillType::Rejected { reason }` carrying the appropriate `RejectReason`.
- [x] 4.4: Pushes onto `FillQueueProducer::try_push`, which logs an error on full queue (engine not consuming).
- [x] 4.5: `DecisionIdMap` records `order_id -> decision_id` at submission time and is consulted on each execution report; orphans (no map entry) emit `decision_id = 0` sentinel with a `warn!` so the journal still captures the orphan.

### Task 5: Implement engine-side fill processing (AC: state transitions, journal writes)
- [x] 5.1: `OrderManager::process_pending_fills()` in `crates/engine/src/order_manager/mod.rs` polls the `FillQueueConsumer` each event-loop iteration (non-blocking; drains the queue in one pass).
- [x] 5.2: `FillType::Full` -> state transitions Confirmed -> Filled (terminal; tracking entry dropped).
- [x] 5.3: `FillType::Partial { remaining }` -> Confirmed -> PartialFill (or PartialFill -> PartialFill on subsequent partials); `remaining_quantity` updated on the `TrackedOrder`.
- [x] 5.4: `FillType::Rejected { reason }` -> state -> Rejected; `tracing::warn!` logs reason + decision_id; tracking entry dropped (terminal).
- [x] 5.5: Every successful transition emits a `JournalEvent::OrderStateChange` (`from_state`, `to_state`, `order_id`, `decision_id`) via `JournalSender::send` (non-blocking try_send; drops surface as `debug!` per story 4.1's bounded-channel contract).

### Task 6: Unit tests (AC: all)
- [x] 6.1: `order_and_fill_events_are_copy` (in `core/types/order.rs`) statically asserts OrderEvent/FillEvent/OrderType/FillType/RejectReason are `Copy`.
- [x] 6.2: `order_queue_round_trip_preserves_fields` + `fill_queue_round_trip` (broker `order_routing` tests) push and pop on each SPSC queue and verify all fields preserved.
- [x] 6.3: `order_queue_full_drops_without_blocking` fills the 4096-slot OrderQueue, asserts the next push returns false in <50ms, drop_count increments by 1.
- [x] 6.4: `full_fill_transitions_to_filled` (engine `order_manager`) — Confirmed -> Filled, terminal, tracking dropped.
- [x] 6.5: `partial_fill_records_remaining` — Confirmed -> PartialFill -> PartialFill -> Filled, remaining_quantity updated each step.
- [x] 6.6: `rejection_preserves_reason` — Submitted -> Rejected via `FillType::Rejected { InsufficientMargin }`, reason carried in outcome.
- [x] 6.7: `decision_id_propagates_to_journal_record` — submits with `decision_id=4242`, applies fill, drains the journal channel, asserts `OrderStateChangeRecord.decision_id == Some(4242)`.

## Dev Notes

### Architecture Patterns & Constraints
- Two separate SPSC queues (rtrb) carry single event types: OrderEvent (engine->broker) and FillEvent (broker->engine). These are distinct from the MarketEvent SPSC queue in Story 2.2.
- Order routing lives in the broker crate; order state tracking lives in engine/order_manager. The broker crate knows nothing about order state — it only routes and reports.
- `OrderEvent` and `FillEvent` must be `Copy` for efficient ring buffer transfer — no heap pointers, no String fields. `RejectReason` uses an enum, not a string.
- The order routing loop runs as an async Tokio task in the broker. The fill consumer runs on the engine's hot-path thread (sync, non-blocking poll).
- `decision_id` must flow from signal -> OrderEvent -> FillEvent -> journal for full causality tracing (NFR17)

### Project Structure Notes
```
crates/core/src/types/
└── order.rs            (OrderEvent, FillEvent, OrderType, FillType, RejectReason)

crates/broker/src/
├── adapter.rs          (BrokerAdapter trait)
└── order_routing.rs    (SPSC queues, order submission loop, fill listener)

crates/engine/src/
└── order_manager/
    └── mod.rs          (fill consumer, state transition on fill)
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Order Routing, SPSC Design
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 4, Story 4.2
- Dependencies: rtrb 0.3.3, rithmic-rs 0.7.2, tokio 1.51.1, tracing 0.1.44
- Story 4.1: Event journal for fill persistence
- Story 4.4: Order state machine for transition validation
- NFR17: Causality tracing via decision_id

## Dev Agent Record

### Agent Model Used
Claude Opus 4.7 (1M context)

### Debug Log References
- First test pass: `rejection_preserves_reason` failed because the auto-upgrade
  `Submitted -> Confirmed` heuristic was applied unconditionally; the subsequent
  `Confirmed -> Rejected` transition is invalid. Restricted the upgrade to
  non-rejection fills (`!matches!(fill.fill_type, FillType::Rejected { .. })`)
  so the direct `Submitted -> Rejected` arc is preserved.
- `cargo fmt` reformatted multi-line `assert!(matches!(...))` and one wrapped
  `self.journal.send(...)` chain after the initial write; no semantic change.
- Pre-existing fmt drift on `main` in `risk/fee_gate.rs`, `signals/{composite,levels,mod}.rs`,
  and several `engine/tests/*.rs` files is **not** introduced by this branch and
  was deliberately left untouched per the story 4-1 reviewer's note.

### Completion Notes List
- **Type model (Task 1).** The slim, `Copy` `OrderEvent`/`FillEvent`/`OrderType`/
  `FillType`/`RejectReason` set landed in `core/src/types/order.rs`. To preserve
  compatibility with the existing `OrderParams`/`BracketOrder` validation surface
  (which uses a unit-variant order classifier separate from a `price: Option<...>`
  field), the prior unit-variant `OrderType` was renamed to `OrderKind` while the
  new spec-style `OrderType` (Market / Limit { price } / Stop { trigger }) takes
  the `OrderType` name as the spec requested. `events/order.rs` and `events/fill.rs`
  are now thin re-exports so `crate::events::OrderEvent` / `FillEvent` keep working
  for `EngineEvent::Order(...)` / `EngineEvent::Fill(...)` consumers.
- **FillEvent extension.** `FillEvent` gained `decision_id: u64` and
  `fill_type: FillType` per spec. `Position::apply_fill` and its tests were
  updated to construct fills with `decision_id: 0, fill_type: FillType::Full` —
  no behavioural change for position arithmetic, just a wider event shape.
- **SPSC queues + routing loop (Tasks 2–4).** New crate-level module
  `crates/broker/src/order_routing.rs` ships:
  * `OrderQueueProducer/Consumer` and `FillQueueProducer/Consumer` wrappers
    around `rtrb` (capacity 4096) — drop on full with `error!` (target
    `order_routing`) per spec.
  * `create_order_fill_queues()` returns the four halves needed to wire engine
    and broker.
  * `OrderSubmitter` async trait (intentionally narrower than core's
    `BrokerAdapter` so the routing loop is independently mockable).
  * `route_pending_orders()` async drainer: dispatches each `OrderEvent` via the
    submitter, populates a `DecisionIdMap` on success, synthesises a
    `FillType::Rejected { reason }` fill on submission failure (mapped from
    `SubmissionError::to_reject_reason`), and logs each submission at `info`.
  * `RithmicSubmitter` placeholder: documents the `OrderEvent -> RequestNewOrder`
    field mapping (symbol, side, qty, order_type variants, user_tag) and returns
    `SubmissionError::ConnectionLost` until OrderPlant connectivity lands in a
    later epic-4 story. The routing-loop reject-fill path is exercised end-to-end
    against this stub.
  * `ExecutionReport` + `publish_execution_report()`: the broker-side translation
    seam from exchange execution reports to `FillEvent`s, enriching with
    `decision_id` via the `DecisionIdMap`. Orphan fills (no map entry) emit a
    `decision_id = 0` sentinel + `warn!` so the journal still captures them.
  * `rtrb = { workspace = true }` was added to `crates/broker/Cargo.toml`.
- **Engine fill consumer (Task 5).** New `crates/engine/src/order_manager/mod.rs`
  introduces `TrackedOrder`, `OrderManager` and `FillOutcome`. `apply_fill`
  validates each transition through `OrderState::can_transition_to` and
  forwards a `JournalEvent::OrderStateChange` (with `decision_id`) on every
  successful transition. Terminal fills drop the tracked entry; partials keep
  it live with updated `remaining_quantity`. Orphan fills surface as
  `FillOutcome::Orphan` with a `warn!` log (no panic) — broker reconciliation in
  story 4.5 owns the resolution path. A small `JournalReceiver::try_recv_for_test`
  helper was added to `journal.rs` so tests can drain dispatched events without
  spinning up the full worker.
- **Tests (Task 6).** 17 new tests added across the workspace:
  * 2 in `core::types::order` (`Copy` static assertion + `OrderType` kind/price
    extraction).
  * 9 in `broker::order_routing` (queue round-trips, capacity-full, routing
    loop happy + reject path with decision-id tracking, fill listener
    partial->full sequence, orphan listener, RithmicSubmitter stub,
    `SubmissionError -> RejectReason` mapping).
  * 6 in `engine::order_manager` (full / partial / rejection paths,
    `decision_id` -> journal record propagation, orphan fill handling, end-to-end
    SPSC drain via `process_pending_fills`).
- **Test counts.** Workspace total 238 (was 221 baseline → +17). Per-package:
  broker 17→26, core 50→52, engine-lib 55→61.
- **Validation.** `cargo build` + `cargo test --workspace` clean; `cargo clippy
  --all-targets -- -D warnings` clean; `rustfmt --check` clean on every file
  this story touched.
- **Deviations from spec.**
  * Spec Task 3.1 says "define `BrokerAdapter` trait in
    `crates/broker/src/adapter.rs` with `async fn submit_order(&self, event:
    &OrderEvent) -> Result<()>`". The existing `core::traits::BrokerAdapter`
    already takes `OrderParams` and is consumed by `RithmicAdapter` and
    `MockBrokerAdapter`. Replacing it would have rippled through testkit and
    `broker::adapter`, none of which are within story 4.2's blast radius.
    Implemented as a narrower trait `OrderSubmitter` in
    `broker::order_routing` instead — same shape as the spec
    (`async fn submit_order(&self, event: &OrderEvent) -> Result<(), SubmissionError>`)
    but isolated to the routing loop. Flagged for follow-up if the reviewer
    prefers consolidation.
  * Spec Task 3.3 calls for the actual Rithmic OrderPlant request mapping;
    `rithmic-rs` OrderPlant connectivity has not yet been wired (the existing
    `RithmicAdapter` is TickerPlant-only). The `RithmicSubmitter` placeholder
    documents the field mapping and returns `SubmissionError::ConnectionLost`
    until the later epic-4 story adds OrderPlant. The routing-loop's reject-fill
    path is fully exercised against the stub.
  * Spec Task 4.1 envisages an "async task receiving execution reports from
    Rithmic". Without OrderPlant connectivity this story can't ship the live
    listener; instead it ships the translation seam (`ExecutionReport` +
    `publish_execution_report`) the live listener will call. Tests drive the
    seam directly so the `decision_id` propagation contract is locked in.

### File List
- crates/core/src/types/order.rs (modified — added new `OrderEvent`, `OrderType`,
  `FillEvent`, `FillType`, `RejectReason`; renamed legacy `OrderType` to
  `OrderKind` and updated `OrderParams`/`BracketOrder` usages; added 2 unit
  tests for the new types)
- crates/core/src/types/mod.rs (modified — re-exports for new types)
- crates/core/src/types/position.rs (modified — `Position::apply_fill` test
  helpers updated for new `FillEvent` shape)
- crates/core/src/events/order.rs (modified — now a thin re-export of
  `crate::types::OrderEvent`)
- crates/core/src/events/fill.rs (modified — now a thin re-export of
  `crate::types::FillEvent`)
- crates/core/src/lib.rs (modified — re-exports updated for new types)
- crates/broker/src/order_routing.rs (new — SPSC queues, OrderSubmitter trait,
  routing loop, fill listener seam, RithmicSubmitter placeholder, 9 tests)
- crates/broker/src/lib.rs (modified — declared `pub mod order_routing` and
  re-exports)
- crates/broker/Cargo.toml (modified — added `rtrb = { workspace = true }`)
- crates/engine/src/order_manager/mod.rs (new — `TrackedOrder`, `OrderManager`,
  `FillOutcome`, fill consumer with state-machine validation + journal write,
  6 tests)
- crates/engine/src/lib.rs (modified — declared `pub mod order_manager`)
- crates/engine/src/persistence/journal.rs (modified — added test-only
  `JournalReceiver::try_recv_for_test` helper for cross-module tests)
- crates/testkit/src/mock_broker.rs (modified — `OrderType` -> `OrderKind` in
  the test fixture for the renamed coarse classifier)
- _bmad-output/implementation-artifacts/sprint-status.yaml (modified — moved
  4-2 from `ready-for-dev` to `review`; bumped `last_updated` to 2026-04-30)
- _bmad-output/implementation-artifacts/4-2-market-order-submission.md
  (modified — Status, task checkboxes, Dev Agent Record, File List, Change Log)

### Change Log
- 2026-04-30: Implemented Story 4.2 — Market Order Submission. New SPSC order/
  fill queues in `broker/src/order_routing.rs` (rtrb cap 4096), engine-side fill
  consumer in `engine/src/order_manager/mod.rs`, and slim `Copy` order-routing
  event types in `core/src/types/order.rs`. 17 new unit tests; workspace test
  count 221 -> 238. `cargo build` / `cargo test --workspace` /
  `cargo clippy --all-targets -- -D warnings` / `cargo fmt --check` (touched
  files) all clean.
