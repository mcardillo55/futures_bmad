# Story 1.4: Order & Position Types

Status: ready-for-dev

## Story

As a developer,
I want complete order and position domain types,
So that order lifecycle tracking and position management have well-defined type-safe representations.

## Acceptance Criteria (BDD)

- Given the `core` crate When `OrderState` enum is implemented Then it includes variants: `Idle`, `Submitted`, `Confirmed`, `PartialFill`, `Filled`, `Rejected`, `Cancelled`, `PendingCancel`, `Uncertain`, `PendingRecon`, `Resolved`
- Given the `core` crate When `OrderParams` struct is implemented Then it contains fields for symbol, side, quantity, order type (Market/Limit/Stop), and price (optional)
- Given the `core` crate When `BracketOrder` struct is implemented Then it contains `entry: OrderParams`, `take_profit: OrderParams`, `stop_loss: OrderParams`
- Given the `core` crate When `BracketState` enum is implemented Then it includes variants: `NoBracket`, `EntryOnly`, `EntryAndStop`, `Full`, `Flattening`
- Given the `core` crate When `FillEvent` struct is implemented Then it contains `order_id: u64`, `fill_price: FixedPrice`, `fill_size: u32`, `timestamp: UnixNanos`, `side: Side`
- Given the `core` crate When `Position` struct is implemented Then it tracks `symbol_id: u32`, `side: Option<Side>`, `quantity: u32`, `avg_entry_price: FixedPrice`, `unrealized_pnl: FixedPrice` and provides methods to apply fills and compute P&L in quarter-ticks
- Given property tests exist for order state transitions When arbitrary state transitions are attempted Then only valid transitions succeed and invalid transitions are rejected

## Tasks / Subtasks

### Task 1: Create module structure (AC: types exist in core)
- 1.1: Create `crates/core/src/types/order.rs`
- 1.2: Create `crates/core/src/types/position.rs`
- 1.3: Update `crates/core/src/types/mod.rs` to declare `pub mod order` and `pub mod position`

### Task 2: Implement OrderState enum and state machine (AC: OrderState variants, valid transitions)
- 2.1: Define `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum OrderState { Idle, Submitted, Confirmed, PartialFill, Filled, Rejected, Cancelled, PendingCancel, Uncertain, PendingRecon, Resolved }`
- 2.2: Implement `OrderState::can_transition_to(&self, next: OrderState) -> bool` encoding the valid state machine:
  - Idle -> Submitted
  - Submitted -> Confirmed, Rejected, Uncertain
  - Confirmed -> PartialFill, Filled, PendingCancel, Uncertain
  - PartialFill -> PartialFill, Filled, PendingCancel, Uncertain
  - PendingCancel -> Cancelled, Filled, Uncertain
  - Uncertain -> PendingRecon
  - PendingRecon -> Resolved
- 2.3: Implement `OrderState::try_transition(&self, next: OrderState) -> Result<OrderState, OrderStateError>` that validates before transitioning

### Task 3: Implement OrderType and OrderParams (AC: OrderParams fields)
- 3.1: Define `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum OrderType { Market, Limit, Stop }`
- 3.2: Define `OrderParams` struct with: `symbol_id: u32`, `side: Side`, `quantity: u32`, `order_type: OrderType`, `price: Option<FixedPrice>`
- 3.3: Implement validation: Limit and Stop orders must have price, Market must not

### Task 4: Implement BracketOrder and BracketState (AC: BracketOrder, BracketState)
- 4.1: Define `BracketOrder { entry: OrderParams, take_profit: OrderParams, stop_loss: OrderParams }`
- 4.2: Define `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum BracketState { NoBracket, EntryOnly, EntryAndStop, Full, Flattening }`
- 4.3: Implement `BracketOrder::new(entry, take_profit, stop_loss) -> Result<Self, BracketOrderError>` with validation: entry is Market, take_profit is Limit, stop_loss is Stop; sides are consistent (entry Buy -> TP Sell, SL Sell)

### Task 5: Implement FillEvent (AC: FillEvent fields)
- 5.1: Define `FillEvent` struct in `crates/core/src/events/fill.rs` or in `order.rs`: `order_id: u64`, `fill_price: FixedPrice`, `fill_size: u32`, `timestamp: UnixNanos`, `side: Side`
- 5.2: Derive `Debug, Clone, Copy`
- 5.3: Update events module to include fill event

### Task 6: Implement Position (AC: Position tracking, fill application, P&L)
- 6.1: Define `Position { symbol_id: u32, side: Option<Side>, quantity: u32, avg_entry_price: FixedPrice, unrealized_pnl: FixedPrice }`
- 6.2: Implement `Position::flat(symbol_id: u32) -> Position` constructor for no position
- 6.3: Implement `apply_fill(&mut self, fill: &FillEvent)` — updates quantity, side, avg_entry_price using weighted average for same-side fills, reduces for opposite-side fills
- 6.4: Implement `update_unrealized_pnl(&mut self, current_price: FixedPrice)` — computes P&L in quarter-ticks: `(current_price - avg_entry_price) * quantity * direction_multiplier`
- 6.5: Implement `is_flat(&self) -> bool` — returns true when quantity == 0
- 6.6: Implement `is_long(&self) -> bool` and `is_short(&self) -> bool`

### Task 7: Write property tests for state transitions (AC: valid/invalid transitions)
- 7.1: Create `crates/core/tests/order_state_properties.rs`
- 7.2: Property test: arbitrary OrderState pairs — `can_transition_to` matches the defined state machine
- 7.3: Property test: `try_transition` succeeds for all valid transitions and fails for all invalid ones
- 7.4: Property test: terminal states (Filled, Rejected, Cancelled, Resolved) cannot transition to any state

### Task 8: Write unit tests for Position
- 8.1: Test applying a buy fill to a flat position creates a long
- 8.2: Test applying a sell fill to a long position reduces quantity
- 8.3: Test applying an opposite fill that exceeds quantity flips the position
- 8.4: Test unrealized P&L calculation for long and short positions
- 8.5: Test P&L uses quarter-tick arithmetic (integer, no floats)

## Dev Notes

### Architecture Patterns & Constraints
- Order state machine: Idle -> Submitted -> Confirmed -> [PartialFill ->] Filled. Also Confirmed -> PendingCancel -> Cancelled. Any confirmed-like state -> Uncertain -> PendingRecon -> Resolved
- BracketOrder: entry is always Market, take_profit is always Limit, stop_loss is always Stop
- Position P&L must be computed entirely in FixedPrice (quarter-ticks) — no float math
- For average entry price on partial fills: use weighted average in integer arithmetic. Be careful about integer division rounding.
- All order-related types should derive common traits (Debug, Clone, Copy where possible)
- FillEvent lives in the events module alongside MarketEvent

### Project Structure Notes
```
crates/core/src/
├── types/
│   ├── mod.rs
│   ├── order.rs        (OrderState, OrderType, OrderParams, BracketOrder, BracketState)
│   └── position.rs     (Position)
└── events/
    ├── mod.rs
    └── market.rs        (MarketEvent — already exists from Story 1.3)

crates/core/tests/
└── order_state_properties.rs
```

### References
- Architecture document: `docs/architecture.md` — Section: Order State Machine, Position Management
- Epics document: `docs/epics.md` — Epic 1, Story 1.4

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
