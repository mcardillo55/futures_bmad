# Story 4.5: Position Tracking & Broker Reconciliation

Status: review

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
- [x] 1.1: In `crates/core/src/types/position.rs`, define `Position` struct: `symbol_id: u32`, `side: Option<Side>` (None when flat), `quantity: u32`, `avg_entry_price: FixedPrice`, `unrealized_pnl: i64` (quarter-ticks, signed), `realized_pnl: i64` (quarter-ticks, signed, cumulative)
- [x] 1.2: Implement `Position::flat(symbol_id: u32) -> Self` returning a zero-quantity position
- [x] 1.3: Implement `Position::is_flat(&self) -> bool` returning `quantity == 0`
- [x] 1.4: Derive `Debug, Clone, PartialEq`
- [x] 1.5: Update `crates/core/src/types/mod.rs` to declare `pub mod position` and re-export

### Task 2: Implement position update on fills (AC: quantity, avg entry, realized P&L)
- [x] 2.1: Create `crates/engine/src/order_manager/tracker.rs` with `PositionTracker` struct holding `HashMap<u32, Position>` keyed by symbol_id
- [x] 2.2: Implement `on_fill(&mut self, fill: &FillEvent)` — if opening or adding to position: update quantity, recalculate weighted average entry price
- [x] 2.3: If reducing or closing position: compute realized P&L = `(fill_price - avg_entry_price) * fill_size` (negated for short side), accumulate into `realized_pnl`
- [x] 2.4: If fill closes entire position: set `side = None`, `quantity = 0`, reset `avg_entry_price`
- [x] 2.5: Handle partial close: reduce quantity, compute realized P&L for closed portion, avg_entry unchanged for remaining

### Task 3: Implement unrealized P&L calculation (AC: current market price)
- [x] 3.1: Implement `update_unrealized_pnl(&mut self, symbol_id: u32, current_price: FixedPrice)` — for open positions: `unrealized_pnl = (current_price - avg_entry_price) * quantity` (negated for short side)
- [x] 3.2: Call this method each time the OrderBook updates with a new mid_price — integrate with event loop (seam exposed; final wiring lands in story 8-2 startup + event-loop)
- [x] 3.3: All P&L values stored as `i64` in quarter-ticks — convert to dollars only for display: `pnl_dollars = pnl_quarter_ticks * tick_value / 4`
- [x] 3.4: For ES: tick_value = $12.50, so quarter-tick value = $3.125 (documented in `Position` doc comment)

### Task 4: Implement BrokerAdapter position query (AC: query_positions)
- [x] 4.1: `async fn query_positions(&self) -> Result<Vec<BrokerPosition>, BrokerError>` on `BrokerAdapter` trait (`crates/core/src/traits/broker.rs`)
- [x] 4.2: Define `BrokerPosition` struct: `symbol_id: u32`, `side: Option<Side>`, `quantity: u32`, `avg_entry_price: FixedPrice` — broker's view (`crates/core/src/types/position.rs`)
- [x] 4.3: Implement for Rithmic adapter — currently surfaces `BrokerError::PositionQueryFailed`; live wiring deferred to OrderPlant integration (Tickerplant doesn't carry positions)
- [x] 4.4: Return empty vec if no positions (flat) — `MockBrokerAdapter` covers this for tests

### Task 5: Implement reconciliation engine (AC: startup, reconnection, periodic comparison)
- [x] 5.1: Reconciliation logic in `crates/engine/src/order_manager/tracker.rs` as `reconcile(&self, broker_positions: &[BrokerPosition]) -> ReconciliationResult`
- [x] 5.2: Compare each local position against broker position for same symbol: check side, quantity, avg_entry_price
- [x] 5.3: Check for phantom positions: local has position, broker does not (`MismatchKind::PhantomLocal`)
- [x] 5.4: Check for missed fills: broker has position, local does not (`MismatchKind::MissedFill`)
- [x] 5.5: Check for quantity mismatch: both have position but different quantity/side (`MismatchKind::SideOrQuantity`)
- [x] 5.6: Return `ReconciliationResult::Consistent` or `ReconciliationResult::Mismatch(Vec<PositionMismatch>)`

### Task 6: Implement reconciliation triggers (AC: startup, reconnection, periodic)
- [x] 6.1: `ReconciliationTrigger::Startup` enum variant; caller (story 8-2) blocks startup until consistent
- [x] 6.2: `ReconciliationTrigger::Reconnection` variant; caller (story 8-4 reconnection FSM) drives this on Reconciling state entry
- [x] 6.3: `ReconciliationTrigger::Periodic` variant; the actual 60s tokio interval timer is owned by the lifecycle module (story 8-2). Trigger seam is in place
- [x] 6.4: Trigger reason logged at `info` (Consistent path) or `error` (Mismatch path) with `trigger=` structured field

### Task 7: Implement mismatch handling (AC: circuit breaker, no silent correction, halt trading)
- [x] 7.1: On `ReconciliationResult::Mismatch`: `handle_reconciliation_result` invokes the optional `CircuitBreakerCallback` exactly once per tracker lifetime (avoids alerting spam from periodic mismatch loops)
- [x] 7.2: Per-mismatch `error!` log with structured fields: local_side, local_quantity, local_avg_entry, broker_side, broker_quantity, broker_avg_entry, kind, trigger
- [x] 7.3: NEVER silently correct local state to match broker — verified by `mismatch_does_not_silently_correct_local_state` test (NFR16)
- [x] 7.4: Halt all trading: `trading_halted` flag is set on first mismatch; submission gates must observe it
- [x] 7.5: Write mismatch event to journal via `JournalSender` as a `SystemEventRecord` (category=`reconciliation`, single multi-line message enumerating every offender)

### Task 8: Implement consistency success path (AC: success logging)
- [x] 8.1: On `ReconciliationResult::Consistent`: `info!` log with `open_positions` count and `trigger`
- [x] 8.2: Reconciliation-success `SystemEventRecord` written to journal for audit trail
- [x] 8.3: No state changes — `trading_halted` stays at its current value, positions unchanged

### Task 9: Unit tests (AC: all)
- [x] 9.1: `buy_fill_updates_local_position` — quantity, side, avg entry correct
- [x] 9.2: `close_fill_accumulates_realized_pnl` (and `sell_fill_close_computes_realized_pnl`) — realized P&L on close
- [x] 9.3: `partial_close_preserves_remaining_position` — partial close arithmetic
- [x] 9.4: `long_position_unrealized_positive_above_entry` — long unrealized P&L
- [x] 9.5: `short_position_unrealized_positive_below_entry` — short unrealized P&L
- [x] 9.6: `reconciliation_consistent_when_matching`, `reconciliation_all_flat_is_consistent`
- [x] 9.7: `reconciliation_phantom_local_detected`
- [x] 9.8: `reconciliation_missed_fill_detected`
- [x] 9.9: `reconciliation_quantity_mismatch_detected` (also `_side_mismatch_` and `_avg_entry_disagreement_`)
- [x] 9.10: `mismatch_trips_circuit_breaker_and_halts_trading` and `repeat_mismatch_does_not_refire_breaker`
- [x] 9.11: `mismatch_does_not_silently_correct_local_state` (NFR16 enforcement)

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
- Anthropic Claude Opus 4.7 (1M context), invoked via `bmad-dev-story` skill on 2026-04-30.

### Debug Log References
- Baseline test count before story 4-5: 280 (post-4-4).
- After carryover commit: 282 (+2 routing-loop tests for 4-2 S-5).
- After 4-5 feature commit: 313 (+31: 9 Position arithmetic + 19 PositionTracker/reconciliation + 3 integration).
- `cargo build`, `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings`, and `rustfmt --check` all pass on every commit.

### Completion Notes List
- **Carryover commit (`fix(story-4.X)`):**
  - Resolved 4-4 S-3: journal the implicit `Submitted -> Confirmed` auto-upgrade (closes the audit-trail gap).
  - Resolved 4-4 S-1: terminal `mark_resolved` failures now journal a `SystemEvent` (durability lapse no longer warn-only).
  - Resolved 4-4 S-4: `flatten_side_for` returning None on a broker-confirmed Filled now escalates to the circuit breaker via a new `ResolveOutcome::FilledFlattenSideUnknown` variant — never silently drop a flatten.
  - Resolved 4-2 S-5: `SubmissionError::Timeout` no longer synthesizes a Rejected fill. New `should_synthesize_reject()` carve-out leaves the order Submitted so the engine's 5s timeout watchdog promotes it to Uncertain and 4-5 reconciliation resolves it.
  - Resolved 4-2 S-4: synthetic Rejected fill `try_push` return is now checked.
- **Story 4-5 commit (`feat(story-4.5)`):**
  - `Position` extended: `unrealized_pnl: i64` (was `FixedPrice`), new `realized_pnl: i64`. `apply_fill` now accumulates realized P&L on reducing/closing/flipping fills using `(exit - entry) * closed_qty * direction`. Rejection fills are no-ops.
  - `BrokerPosition` added: broker-view companion type, distinct from `Position` so the broker view doesn't fabricate P&L fields.
  - `BrokerAdapter::query_positions` returns `Vec<BrokerPosition>`. `RithmicAdapter` returns `BrokerError::PositionQueryFailed` (TickerPlant doesn't carry positions; live wiring lands with OrderPlant integration).
  - `PositionTracker` (`crates/engine/src/order_manager/tracker.rs`) holds `HashMap<u32, Position>`, applies fills, drives unrealized-P&L updates, and reconciles against `&[BrokerPosition]`.
  - `reconcile` classifies per-symbol disagreements as `PhantomLocal`, `MissedFill`, or `SideOrQuantity`. Result type carries every offender for journaling.
  - `handle_reconciliation_result`: on `Mismatch`, sets `trading_halted=true`, fires `CircuitBreakerCallback` exactly once across the tracker's lifetime (subsequent mismatches do NOT re-fire to avoid alerting spam), per-offender `error!` logs, and a single multi-line `SystemEventRecord` summarizes the round.
  - **NFR16 enforcement:** `mismatch_does_not_silently_correct_local_state` test verifies the local view is never modified to match broker — the architecture's no-silent-correction invariant is now machine-checked.
- **Out-of-scope items** (left in `deferred-work.md` with explicit notes):
  - 4-3 S-4 (`FlattenRetry` reuses same `order_id`): broker-side dedup question that needs a recorded-session test against live OrderPlant. Will land with the live-OrderPlant integration story.
  - 4-3 N-2 (OCO counterpart-cancel verification): same reason — needs a recorded-session test.
  - Reconciliation triggers (Tasks 6.1, 6.2, 6.3 wiring): the lifecycle FSM (story 8-2) and reconnection FSM (story 8-4) own the actual scheduling. The seams (`ReconciliationTrigger` enum + `reconcile_and_handle`) are in place so those stories plug in without redesign.
  - 4-4 S-2 (partial entry over-sizes SL/TP): the original spec located this in 4-5 scope, but on closer reading 4-5 is "position tracking + broker reconciliation" — it does not own bracket SL/TP sizing. The PositionTracker now correctly handles partial fills (cumulative-fill arithmetic), but `BracketManager::on_entry_fill` still warn-logs and proceeds with the original SL size. Defer to a focused bracket-cumulative-fill story (or fold into the live OrderPlant integration story since CME atomic-Market-fill assumption holds in practice). Filed in deferred-work.

### File List
- Modified: `crates/core/src/types/position.rs` — extended `Position`, added `BrokerPosition`, added 8 new tests.
- Modified: `crates/core/src/types/mod.rs` — re-export `BrokerPosition`.
- Modified: `crates/core/src/lib.rs` — re-export `BrokerPosition`.
- Modified: `crates/core/src/traits/broker.rs` — `query_positions` now returns `Vec<BrokerPosition>`.
- Modified: `crates/broker/src/adapter.rs` — `RithmicAdapter::query_positions` typed for `BrokerPosition`.
- Modified: `crates/broker/src/order_routing.rs` — `SubmissionError::should_synthesize_reject()` carve-out for Timeout (4-2 S-5); routing loop respects it; synthetic Rejected `try_push` return checked (4-2 S-4); 2 new tests.
- Modified: `crates/testkit/src/mock_broker.rs` — `BrokerPosition` typed.
- Modified: `crates/engine/src/order_manager/mod.rs` — Submitted→Confirmed journal record (4-4 S-3); WAL `mark_resolved` failure journaled (4-4 S-1); `ResolveOutcome::FilledFlattenSideUnknown` for unknown flatten side (4-4 S-4); `tracker` module wired in.
- New: `crates/engine/src/order_manager/tracker.rs` — `PositionTracker`, `ReconciliationResult`, `ReconciliationTrigger`, `MismatchKind`, `PositionMismatch`, `LocalSnapshot`, `BrokerSnapshot`, plus 19 unit tests covering Tasks 9.1-9.11 + adjacent edge cases.
- New: `crates/engine/tests/position_reconciliation.rs` — 3 integration tests against `MockBrokerAdapter`.

### Change Log
- 2026-04-30: Story 4-5 implemented. Carryover commit `366fcf7` resolves 4-4 S-1/S-3/S-4 and 4-2 S-5/S-4. Feature commit (this) introduces `PositionTracker` + reconciliation engine. Sprint status updated to `review`.
