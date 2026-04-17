# Story 4.2: Market Order Submission

Status: ready-for-dev

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
- 1.1: In `crates/core/src/types/order.rs`, define `OrderEvent` struct: `order_id: u64`, `symbol_id: u32`, `side: Side`, `quantity: u32`, `order_type: OrderType`, `decision_id: u64`, `timestamp: UnixNanos` — derive `Debug, Clone, Copy`
- 1.2: Define `OrderType` enum: `Market`, `Limit { price: FixedPrice }`, `Stop { trigger: FixedPrice }` — derive `Debug, Clone, Copy`
- 1.3: Define `FillEvent` struct: `order_id: u64`, `fill_price: FixedPrice`, `fill_size: u32`, `timestamp: UnixNanos`, `side: Side`, `decision_id: u64`, `fill_type: FillType` — derive `Debug, Clone, Copy`
- 1.4: Define `FillType` enum: `Full`, `Partial { remaining: u32 }`, `Rejected { reason: RejectReason }` — derive `Debug, Clone, Copy`
- 1.5: Define `RejectReason` enum with common rejection codes: `InsufficientMargin`, `InvalidSymbol`, `ExchangeReject`, `ConnectionLost`, `Unknown` — derive `Debug, Clone, Copy`

### Task 2: Create SPSC order and fill queues (AC: rtrb, capacity 4096)
- 2.1: Create `crates/broker/src/order_routing.rs` with `OrderQueue` wrapper around `rtrb::RingBuffer<OrderEvent>` (capacity 4096)
- 2.2: Create `FillQueue` wrapper around `rtrb::RingBuffer<FillEvent>` (capacity 4096)
- 2.3: Implement `OrderQueueProducer` (engine side) with `try_push()` — log error on full (should never happen, indicates broker not consuming)
- 2.4: Implement `OrderQueueConsumer` (broker side) with `try_pop()` returning `Option<OrderEvent>`
- 2.5: Implement `FillQueueProducer` (broker side) with `try_push()` — log error on full
- 2.6: Implement `FillQueueConsumer` (engine side) with `try_pop()` returning `Option<FillEvent>`
- 2.7: Create constructor `create_order_fill_queues() -> (OrderQueueProducer, OrderQueueConsumer, FillQueueProducer, FillQueueConsumer)`

### Task 3: Implement order submission via BrokerAdapter (AC: Rithmic OrderPlant)
- 3.1: Define `BrokerAdapter` trait in `crates/broker/src/adapter.rs` (if not already defined) with `async fn submit_order(&self, event: &OrderEvent) -> Result<()>`
- 3.2: Implement order routing loop in `broker/src/order_routing.rs`: async task that polls `OrderQueueConsumer`, for each `OrderEvent` calls `BrokerAdapter::submit_order()`
- 3.3: Map `OrderEvent` fields to Rithmic OrderPlant request format: symbol, side, quantity, order type
- 3.4: On submission failure (network error, timeout), construct `FillEvent` with `FillType::Rejected` and push to fill queue
- 3.5: Log every order submission at `info` level with order_id, symbol, side, quantity

### Task 4: Implement fill/rejection reception (AC: FillEvent back to engine)
- 4.1: Implement broker-side fill listener: async task receiving execution reports from Rithmic
- 4.2: Map exchange fill confirmations to `FillEvent` with `FillType::Full` or `FillType::Partial`
- 4.3: Map exchange rejections to `FillEvent` with `FillType::Rejected` and appropriate `RejectReason`
- 4.4: Push each `FillEvent` to `FillQueueProducer` — on full queue log error (indicates engine not consuming)
- 4.5: Include `decision_id` from original `OrderEvent` in every `FillEvent` for causality tracing

### Task 5: Implement engine-side fill processing (AC: state transitions, journal writes)
- 5.1: In `crates/engine/src/order_manager/mod.rs`, implement fill consumer: poll `FillQueueConsumer` each event loop iteration
- 5.2: On `FillType::Full`: transition order state from Confirmed to Filled
- 5.3: On `FillType::Partial`: transition order state from Confirmed to PartialFill, track remaining quantity
- 5.4: On `FillType::Rejected`: transition order state to Rejected, log rejection reason at `warn` level
- 5.5: Write every fill/rejection to the event journal via `JournalSender` with `decision_id`

### Task 6: Unit tests (AC: all)
- 6.1: Test OrderEvent and FillEvent are `Copy` and fit efficiently in rtrb ring buffer
- 6.2: Test SPSC queue producer/consumer: push OrderEvent, pop and verify fields match
- 6.3: Test full queue behavior: push 4096 events, next push returns error, no block
- 6.4: Test fill processing: FillType::Full transitions state to Filled
- 6.5: Test fill processing: FillType::Partial transitions state to PartialFill with correct remaining
- 6.6: Test rejection processing: state transitions to Rejected, reason preserved
- 6.7: Test decision_id propagation: OrderEvent decision_id appears in FillEvent and journal write

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
### Debug Log References
### Completion Notes List
### File List
