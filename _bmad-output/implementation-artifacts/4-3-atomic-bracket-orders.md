# Story 4.3: Atomic Bracket Orders

Status: ready-for-dev

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

### Task 1: Define BracketOrder and BracketState types (AC: entry/TP/SL structure, state tracking)
- 1.1: In `crates/core/src/types/order.rs`, define `BracketOrder` struct: `entry: OrderEvent` (market), `take_profit: OrderEvent` (limit), `stop_loss: OrderEvent` (stop), `bracket_id: u64`, `decision_id: u64`, `state: BracketState`
- 1.2: Define `BracketState` enum: `NoBracket`, `EntryOnly`, `EntryAndStop`, `Full`, `Flattening` — derive `Debug, Clone, Copy, PartialEq`
- 1.3: Implement `BracketOrder::new(decision_id, symbol_id, side, quantity, tp_price, sl_price) -> Self` that constructs entry as Market, TP as Limit, SL as Stop, initial state = NoBracket
- 1.4: Implement state transition method `BracketOrder::transition(&mut self, new_state: BracketState) -> Result<()>` with validated transitions: NoBracket->EntryOnly, EntryOnly->EntryAndStop, EntryAndStop->Full, any->Flattening

### Task 2: Implement bracket submission flow (AC: entry fill triggers bracket leg submission)
- 2.1: In `crates/engine/src/order_manager/bracket.rs`, create `BracketManager` struct tracking active brackets by bracket_id
- 2.2: Implement `on_entry_fill(&mut self, fill: &FillEvent, bracket: &mut BracketOrder)` — on entry fill, transition state to EntryOnly, immediately submit stop_loss order via order queue
- 2.3: On stop_loss confirmation from exchange, transition to EntryAndStop, then submit take_profit order
- 2.4: On take_profit confirmation, transition to Full — bracket is now fully protected
- 2.5: Submit TP and SL as OCO pair at the exchange level (via Rithmic OCO order type if supported, or manage OCO semantics locally)
- 2.6: Log every bracket state transition at `info` level with bracket_id, decision_id, new state

### Task 3: Implement flatten retry on bracket failure (AC: 3 attempts, 1s interval)
- 3.1: Create `crates/broker/src/position_flatten.rs` with `FlattenRetry` struct
- 3.2: Implement `flatten_with_retry(&self, symbol_id: u32, quantity: u32, side: Side) -> Result<FlattenOutcome>` that submits a market order to close the position
- 3.3: On rejection, wait 1 second (`tokio::time::sleep`), retry — up to 3 total attempts
- 3.4: Return `FlattenOutcome::Success(FillEvent)` or `FlattenOutcome::Failed { attempts: u8 }` after exhausting retries
- 3.5: Transition bracket state to `Flattening` when flatten retry begins
- 3.6: Log each retry attempt at `warn` level with attempt number, symbol, rejection reason

### Task 4: Implement panic mode (AC: all trading disabled, stops preserved)
- 4.1: Create `crates/engine/src/risk/panic_mode.rs` with `PanicMode` struct and `PanicState` enum (Normal, PanicActive)
- 4.2: Implement `activate(&mut self, reason: &str)` — sets PanicActive, logs at `error` level
- 4.3: On panic activation: cancel all pending entry orders and limit orders (via order queue), but NEVER cancel resting stop-loss orders — stops must be preserved
- 4.4: Set global trading flag to disabled — all signal evaluation short-circuits, no new orders accepted
- 4.5: Send operator alert (log at `error` level with structured fields for alerting integration)
- 4.6: System requires manual restart — `PanicState::PanicActive` persists until process restart
- 4.7: Write panic event to journal via `JournalSender`

### Task 5: Implement bracket exit processing (AC: OCO fill, P&L, position flat)
- 5.1: In `BracketManager`, implement `on_bracket_fill(&mut self, fill: &FillEvent)` — determine if fill is TP or SL
- 5.2: On TP fill: OCO counterpart (SL) auto-cancelled by exchange — verify cancellation received, log if not
- 5.3: On SL fill: OCO counterpart (TP) auto-cancelled by exchange — verify cancellation received, log if not
- 5.4: Compute realized P&L: `(exit_price - entry_price) * quantity` in quarter-ticks, multiply by tick value for dollar P&L
- 5.5: Update position to flat (quantity = 0)
- 5.6: Write bracket completion event to journal with decision_id, entry_price, exit_price, realized_pnl, exit_type (TP or SL)
- 5.7: Remove completed bracket from active tracking

### Task 6: Unit tests (AC: all)
- 6.1: Test BracketOrder construction: entry is Market, TP is Limit at correct price, SL is Stop at correct price
- 6.2: Test BracketState transitions: valid transitions succeed, invalid transitions return error
- 6.3: Test bracket submission flow: entry fill -> stop submitted -> stop confirmed -> TP submitted -> TP confirmed -> Full
- 6.4: Test flatten retry: 1st attempt rejected, 2nd succeeds — verify 1s delay, state = Flattening
- 6.5: Test flatten retry exhaustion: 3 rejections -> panic mode activated
- 6.6: Test panic mode: trading disabled, entries cancelled, stops preserved, panic persists
- 6.7: Test TP fill: position flat, P&L computed correctly, SL cancellation expected
- 6.8: Test SL fill: position flat, P&L computed correctly (negative), TP cancellation expected

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
### Debug Log References
### Completion Notes List
### File List
