# Story 4.5: Position Tracking & Broker Reconciliation

Status: ready-for-dev

## Story

As a trader-operator,
I want local position state always consistent with the broker,
So that I never have phantom positions or missed fills.

## Acceptance Criteria (BDD)

- Given `engine/src/order_manager/tracker.rs` When fills received Then local Position updated (quantity, side, avg entry), unrealized P&L from current market price, realized P&L on close (quarter-ticks * tick value)
- Given reconciliation triggered (startup, reconnection, periodic) When broker queried via BrokerAdapter::query_positions() Then local compared against broker state
- Given position mismatch When detected Then circuit breaker triggered immediately (NFR16), no silent correction, mismatch details logged, trading halted
- Given positions consistent When reconciliation completes Then success logged with both states, normal operation continues

## Tasks / Subtasks

### Task 1: Define Position struct (AC: quantity, side, avg entry, P&L)
- 1.1: In `crates/core/src/types/position.rs`, define `Position` struct: `symbol_id: u32`, `side: Option<Side>` (None when flat), `quantity: u32`, `avg_entry_price: FixedPrice`, `unrealized_pnl: i64` (quarter-ticks, signed), `realized_pnl: i64` (quarter-ticks, signed, cumulative)
- 1.2: Implement `Position::flat(symbol_id: u32) -> Self` returning a zero-quantity position
- 1.3: Implement `Position::is_flat(&self) -> bool` returning `quantity == 0`
- 1.4: Derive `Debug, Clone, PartialEq`
- 1.5: Update `crates/core/src/types/mod.rs` to declare `pub mod position` and re-export

### Task 2: Implement position update on fills (AC: quantity, avg entry, realized P&L)
- 2.1: Create `crates/engine/src/order_manager/tracker.rs` with `PositionTracker` struct holding `HashMap<u32, Position>` keyed by symbol_id
- 2.2: Implement `on_fill(&mut self, fill: &FillEvent)` — if opening or adding to position: update quantity, recalculate weighted average entry price
- 2.3: If reducing or closing position: compute realized P&L = `(fill_price - avg_entry_price) * fill_size` (negated for short side), accumulate into `realized_pnl`
- 2.4: If fill closes entire position: set `side = None`, `quantity = 0`, reset `avg_entry_price`
- 2.5: Handle partial close: reduce quantity, compute realized P&L for closed portion, avg_entry unchanged for remaining

### Task 3: Implement unrealized P&L calculation (AC: current market price)
- 3.1: Implement `update_unrealized_pnl(&mut self, symbol_id: u32, current_price: FixedPrice)` — for open positions: `unrealized_pnl = (current_price - avg_entry_price) * quantity` (negated for short side)
- 3.2: Call this method each time the OrderBook updates with a new mid_price — integrate with event loop
- 3.3: All P&L values stored as `i64` in quarter-ticks — convert to dollars only for display: `pnl_dollars = pnl_quarter_ticks * tick_value / 4`
- 3.4: For ES: tick_value = $12.50, so quarter-tick value = $3.125

### Task 4: Implement BrokerAdapter position query (AC: query_positions)
- 4.1: Add `async fn query_positions(&self) -> Result<Vec<BrokerPosition>>` to `BrokerAdapter` trait in `crates/broker/src/adapter.rs`
- 4.2: Define `BrokerPosition` struct: `symbol_id: u32`, `side: Option<Side>`, `quantity: u32`, `avg_entry_price: FixedPrice` — this is the broker's view
- 4.3: Implement for Rithmic adapter: query current positions via rithmic-rs API
- 4.4: Return empty vec if no positions (flat) — this is a valid state

### Task 5: Implement reconciliation engine (AC: startup, reconnection, periodic comparison)
- 5.1: Create reconciliation logic in `crates/engine/src/order_manager/tracker.rs` as `reconcile(&self, broker_positions: &[BrokerPosition]) -> ReconciliationResult`
- 5.2: Compare each local position against broker position for same symbol: check side, quantity, avg_entry_price
- 5.3: Check for phantom positions: local has position, broker does not
- 5.4: Check for missed fills: broker has position, local does not
- 5.5: Check for quantity mismatch: both have position but different quantity/side
- 5.6: Return `ReconciliationResult::Consistent` or `ReconciliationResult::Mismatch(Vec<PositionMismatch>)`

### Task 6: Implement reconciliation triggers (AC: startup, reconnection, periodic)
- 6.1: On startup: call `query_positions()` and `reconcile()` before accepting any trading signals — block startup until reconciliation passes
- 6.2: On reconnection (broker FSM enters Reconciling state): trigger reconciliation before resuming trading
- 6.3: Periodic: schedule reconciliation every 60 seconds via Tokio interval timer — run on async runtime, results communicated to engine via channel
- 6.4: Log reconciliation trigger reason at `info` level

### Task 7: Implement mismatch handling (AC: circuit breaker, no silent correction, halt trading)
- 7.1: On `ReconciliationResult::Mismatch`: trigger circuit breaker immediately — call circuit breaker activation via callback/channel
- 7.2: Log mismatch at `error` level with structured fields: local_side, local_quantity, local_avg_entry, broker_side, broker_quantity, broker_avg_entry, mismatch_type
- 7.3: NEVER silently correct local state to match broker — this masks bugs and is explicitly forbidden (NFR16)
- 7.4: Halt all trading: set trading_halted flag, reject all new orders until manual review
- 7.5: Write mismatch event to journal via `JournalSender`

### Task 8: Implement consistency success path (AC: success logging)
- 8.1: On `ReconciliationResult::Consistent`: log at `info` level with both local and broker state snapshots
- 8.2: Write reconciliation success event to journal for audit trail
- 8.3: Continue normal operation — no state changes needed

### Task 9: Unit tests (AC: all)
- 9.1: Test position update on buy fill: quantity increases, avg entry calculated correctly
- 9.2: Test position update on sell fill (close): realized P&L computed correctly, position goes flat
- 9.3: Test partial close: quantity reduces, realized P&L for closed portion, remaining position intact
- 9.4: Test unrealized P&L: long position with price above entry -> positive, below -> negative
- 9.5: Test unrealized P&L: short position with price below entry -> positive, above -> negative
- 9.6: Test reconciliation consistent: local matches broker -> Consistent result
- 9.7: Test reconciliation mismatch (phantom): local has position, broker flat -> Mismatch
- 9.8: Test reconciliation mismatch (missed fill): broker has position, local flat -> Mismatch
- 9.9: Test reconciliation mismatch (quantity): both have position, different quantities -> Mismatch
- 9.10: Test mismatch triggers circuit breaker and halts trading
- 9.11: Test no silent correction: after mismatch, local state unchanged

## Dev Notes

### Architecture Patterns & Constraints
- Position mismatch handling is the most critical safety feature in this story. The system MUST halt on mismatch — never silently correct. Silent correction masks bugs that could lead to catastrophic losses (NFR16).
- `PositionTracker` lives on the engine hot path and is updated synchronously on each fill. Reconciliation queries run on the async Tokio runtime and communicate results back to the engine thread.
- P&L is always stored as `i64` quarter-ticks (signed for losses). Dollar conversion: `pnl_dollars = pnl_quarter_ticks as f64 * tick_value / 4.0` — only at display/reporting layer, never in core computation.
- Average entry price uses weighted average: `new_avg = (old_avg * old_qty + fill_price * fill_qty) / (old_qty + fill_qty)` — integer arithmetic with FixedPrice, round toward zero.
- Reconciliation at startup is blocking — the system will not trade until positions are verified. This is a hard requirement.
- Periodic reconciliation (60s) is a safety net — most mismatches should be caught immediately via fill processing. The periodic check catches edge cases like missed network messages.

### Project Structure Notes
```
crates/core/src/types/
├── mod.rs
└── position.rs         (Position struct)

crates/broker/src/
└── adapter.rs          (BrokerAdapter::query_positions, BrokerPosition)

crates/engine/src/
└── order_manager/
    └── tracker.rs      (PositionTracker, reconciliation logic)
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Position Management, Reconciliation
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 4, Story 4.5
- Story 4.1: Event journal for reconciliation event persistence
- Story 4.2: FillEvent type for position updates
- Story 4.3: BracketOrder for understanding position lifecycle
- Story 4.4: Order state machine, Uncertain/PendingRecon reconciliation
- Dependencies: rusqlite 0.38.0, tracing 0.1.44, rithmic-rs 0.7.2
- NFR16: No silent correction, circuit breaker on mismatch

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
