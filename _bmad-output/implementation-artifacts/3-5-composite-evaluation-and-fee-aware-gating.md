# Story 3.5: Composite Evaluation & Fee-Aware Gating

Status: done

## Story

As a trader-operator,
I want all signals combined into a single trade/no-trade decision with fee-aware filtering,
So that only high-probability, profitable-after-costs trades are taken.

## Acceptance Criteria (BDD)

- Given `engine/src/signals/mod.rs` with `SignalPipeline` When constructed Then contains named fields: `obi: ObiSignal`, `vpin: VpinSignal`, `microprice: MicropriceSignal` — concrete types, zero vtable overhead
- Given `engine/src/signals/composite.rs` When composite evaluation runs Then ALL signals must be valid (`is_valid() == true`), weighted composite score computed from signal values (weights from config), expected edge converted to FixedPrice via banker's rounding, unique `decision_id: u64` generated for every evaluation
- Given `engine/src/risk/fee_gate.rs` When `FeeGate` evaluates Then total round-trip = `2 * (exchange + commission + api) + slippage` in quarter-ticks, trade permitted only if `edge > minimum_edge_multiple * cost`, if `fee_schedule_date` >30 days old a warning is logged, if >60 days old ALL trade evaluations blocked, staleness gate never blocks flattening or safety orders
- Given all conditions met When decision is "trade" Then output includes: direction (Side), expected edge (FixedPrice), composite score, decision_id, all individual signal values — logged with decision_id
- Given any condition fails When decision is "no trade" Then reason logged with decision_id

## Tasks / Subtasks

### Task 1: Define SignalPipeline in signals/mod.rs (AC: concrete types, zero vtable)
- [x] 1.1: In `crates/engine/src/signals/mod.rs`, define:
  ```rust
  pub struct SignalPipeline {
      pub obi: ObiSignal,
      pub vpin: VpinSignal,
      pub microprice: MicropriceSignal,
  }
  ```
- [x] 1.2: Implement `SignalPipeline::new(obi_config, vpin_config) -> Self` constructor
- [x] 1.3: Implement `SignalPipeline::update_all(&mut self, book: &OrderBook, trade: Option<&MarketEvent>, clock: &dyn Clock)`:
  - Calls `update()` on each signal in sequence
  - Returns tuple or struct of all signal results
- [x] 1.4: Implement `SignalPipeline::all_valid(&self) -> bool`:
  - Returns `self.obi.is_valid() && self.vpin.is_valid() && self.microprice.is_valid()`
- [x] 1.5: Implement `SignalPipeline::reset(&mut self)`:
  - Calls `reset()` on all signals
- [x] 1.6: Implement `SignalPipeline::snapshot(&self) -> PipelineSnapshot`:
  - Captures snapshot from all signals

### Task 2: Implement composite evaluation (AC: weighted scoring, edge conversion, decision_id)
- [x] 2.1: Create `crates/engine/src/signals/composite.rs`
- [x] 2.2: Define `CompositeConfig` struct: `{ obi_weight: f64, vpin_weight: f64, microprice_weight: f64, historical_edge_per_unit: f64 }`
- [x] 2.3: Define `TradeDecision` struct:
  ```rust
  pub struct TradeDecision {
      pub decision_id: u64,
      pub direction: Side,
      pub expected_edge: FixedPrice,
      pub composite_score: f64,
      pub obi_value: f64,
      pub vpin_value: f64,
      pub microprice_value: f64,
      pub reason: DecisionReason,
  }
  ```
- [x] 2.4: Define `DecisionReason` enum: `Trade`, `NoTradeSignalInvalid(String)`, `NoTradeEdgeBelowThreshold`, `NoTradeFeeGateBlocked`, `NoTradeNotAtLevel`
- [x] 2.5: Implement `CompositeEvaluator` struct with fields:
  - `config: CompositeConfig`
  - `decision_counter: u64` — monotonic counter for decision_id generation
- [x] 2.6: Implement `CompositeEvaluator::evaluate(&mut self, pipeline: &SignalPipeline, fee_gate: &FeeGate, clock: &dyn Clock) -> TradeDecision`:
  - Generate `decision_id` by incrementing counter (unique u64, NFR17 causality tracing)
  - Check `pipeline.all_valid()` — if false, return NoTradeSignalInvalid with which signal(s) invalid
  - Compute weighted composite score: `score = obi * w_obi + vpin * w_vpin + microprice_deviation * w_micro`
  - Determine direction from score sign: positive = Buy, negative = Sell
  - Compute expected edge: `edge_f64 = score.abs() * historical_edge_per_unit`
  - Convert to FixedPrice via banker's rounding: `FixedPrice::from_f64(edge_f64)` (uses round-half-to-even)
  - Call `fee_gate.permits_trade(expected_edge, clock)` — if false, return NoTradeEdgeBelowThreshold or NoTradeFeeGateBlocked
  - If all pass, return Trade decision with all values populated
  - Log decision with decision_id: `tracing::info!(decision_id, direction, edge, score, obi, vpin, microprice, reason)`

### Task 3: Implement FeeGate (AC: round-trip cost, edge threshold, staleness)
- [x] 3.1: Create `crates/engine/src/risk/fee_gate.rs`
- [x] 3.2: Create `crates/engine/src/risk/mod.rs` with `pub mod fee_gate;`
- [x] 3.3: Update `crates/engine/src/lib.rs` to declare `pub mod risk`
- [x] 3.4: Define `FeeGate` struct matching architecture:
  ```rust
  pub struct FeeGate {
      pub exchange_fee_per_side: FixedPrice,
      pub commission_per_side: FixedPrice,
      pub api_fee_per_side: FixedPrice,
      pub slippage_model: FixedPrice,
      pub minimum_edge_multiple: f64,
      pub fee_schedule_date: chrono::NaiveDate,
  }
  ```
- [x] 3.5: Implement `FeeGate::total_round_trip_cost(&self) -> FixedPrice`:
  - `per_side = exchange + commission + api` (all FixedPrice saturating_add)
  - `total = per_side.saturating_mul(2).saturating_add(slippage)`
  - All computation in quarter-ticks (integer arithmetic)
- [x] 3.6: Implement `FeeGate::permits_trade(&self, expected_edge: FixedPrice, clock: &dyn Clock) -> Result<bool, FeeGateReason>`:
  - Check staleness first:
    - Compute days since `fee_schedule_date` using `clock.wall_clock()`
    - If >60 days: `tracing::error!("Fee schedule >60 days stale, blocking trades")`, return `Err(FeeGateReason::StaleSchedule)`
    - If >30 days: `tracing::warn!("Fee schedule >30 days stale")`
  - Compute threshold: `min_edge = (minimum_edge_multiple * total_round_trip_cost.to_f64()).round_ties_even()` converted to FixedPrice
  - Return `Ok(expected_edge > min_edge_fixedprice)`
- [x] 3.7: Implement `FeeGate::from_config(config: &FeeConfig) -> Self`:
  - Load from `core/src/config/fees.rs` FeeConfig struct
- [x] 3.8: Define `FeeGateReason` enum: `Permitted`, `EdgeBelowThreshold { edge: FixedPrice, threshold: FixedPrice }`, `StaleSchedule { days: u64 }`

### Task 4: Staleness safety exception (AC: never blocks flattening/safety)
- [x] 4.1: Implement `FeeGate::permits_flatten(&self) -> bool`: always returns `true` — staleness gate never applies to safety/flatten orders
- [x] 4.2: Document in code comments: "Staleness gate applies to trade evaluation only. Flattening and safety orders are never gated."
- [x] 4.3: The caller (event loop / order manager) must use `permits_flatten()` for safety orders, `permits_trade()` for new trade evaluation

### Task 5: Decision logging with causality tracing (AC: decision_id in all logs)
- [x] 5.1: All log lines in `evaluate()` include `decision_id` field for structured tracing
- [x] 5.2: Trade decision log: `tracing::info!(decision_id = %id, direction = ?dir, edge = %edge, score = %score, obi = %obi, vpin = %vpin, microprice = %mp, "trade decision")`
- [x] 5.3: No-trade decision log: `tracing::info!(decision_id = %id, reason = ?reason, "no-trade decision")`
- [x] 5.4: Fee gate block log: includes cost breakdown and edge vs threshold

### Task 6: Write unit tests (AC: all evaluation paths, fee computation, staleness)
- [x] 6.1: Test: SignalPipeline construction with concrete types, all_valid() when all signals valid
- [x] 6.2: Test: all_valid() false when any one signal is invalid
- [x] 6.3: Test: composite score computation with known weights and signal values
- [x] 6.4: Test: direction derived correctly from positive/negative composite score
- [x] 6.5: Test: expected edge conversion to FixedPrice via banker's rounding
- [x] 6.6: Test: decision_id is unique and incrementing across evaluations
- [x] 6.7: Test: FeeGate total_round_trip_cost computation:
  - exchange=2qt, commission=1qt, api=1qt, slippage=2qt per side
  - total = 2*(2+1+1) + 2 = 10 quarter-ticks
- [x] 6.8: Test: FeeGate permits_trade true when edge > multiple * cost
- [x] 6.9: Test: FeeGate permits_trade false when edge <= multiple * cost
- [x] 6.10: Test: FeeGate staleness warning at 31 days (permits, but warns)
- [x] 6.11: Test: FeeGate staleness block at 61 days (blocks trade evaluation)
- [x] 6.12: Test: FeeGate permits_flatten always true even when stale
- [x] 6.13: Test: full evaluate() path — all signals valid, edge sufficient = Trade decision
- [x] 6.14: Test: full evaluate() path — one signal invalid = NoTradeSignalInvalid
- [x] 6.15: Test: full evaluate() path — edge below threshold = NoTradeEdgeBelowThreshold
- [x] 6.16: All tests use `testkit::SimClock`

## Dev Notes

### Architecture Patterns & Constraints
- SignalPipeline: CONCRETE types, zero vtable overhead — no `Box<dyn Signal>`, no dynamic dispatch
- Phase 2 trigger for dynamic dispatch: when >3 signal types or runtime config needed (not this story)
- decision_id: monotonic u64 counter, unique per evaluation — enables causality tracing (NFR17)
- Every trade links: signal values -> composite score -> decision_id -> order_id
- FeeGate: ALL fees in quarter-ticks (FixedPrice) — integer arithmetic on hot path
- Round-trip formula: `2 * (exchange + commission + api) + slippage`
- minimum_edge_multiple default: 2.0 (edge must exceed 2x fees)
- Staleness: 30d warn, 60d block — computed from `fee_schedule_date` vs `clock.wall_clock()`
- CRITICAL: staleness never blocks safety/flatten — separate code path (`permits_flatten`)
- Expected edge conversion: `FixedPrice::from_f64()` which uses banker's rounding (round half to even)
- Composite weights from config: `[signals.weights]` section in TOML

### Project Structure Notes
```
crates/engine/src/
├── signals/
│   ├── mod.rs          # SignalPipeline struct + pub mod {obi, vpin, microprice, levels, composite}
│   ├── obi.rs          # Story 3.1
│   ├── vpin.rs         # Story 3.2
│   ├── microprice.rs   # Story 3.3
│   ├── levels.rs       # Story 3.4
│   └── composite.rs    # CompositeEvaluator, TradeDecision, DecisionReason
├── risk/
│   ├── mod.rs          # pub mod fee_gate;
│   └── fee_gate.rs     # FeeGate, FeeGateReason

crates/core/src/
├── config/
│   └── fees.rs         # FeeConfig struct (source of truth for fee values)
├── traits/
│   └── signal.rs       # Signal trait
```

### References
- Architecture: SignalPipeline, Composite Evaluation, FeeGate struct, Fee-Aware Gating section, causality logging
- Epics: Epic 3, Story 3.5
- Dependencies: `core` (Signal trait, FixedPrice, Side, OrderBook, Clock, FeeConfig, MarketEvent), `testkit` (SimClock — dev-dependency), `chrono` (NaiveDate for fee staleness)
- Config: `core/src/config/fees.rs` — FeeConfig per instrument (`[fees.mes]` section in TOML)
- Cross-references: Story 3.1 (ObiSignal), Story 3.2 (VpinSignal), Story 3.3 (MicropriceSignal), Story 3.4 (LevelEngine)
- Constraint: `risk/` must NOT import from `signals/` — FeeGate receives edge as parameter, does not read signals directly

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6 (1M context)

### Debug Log References
One test fix: balanced book gives OBI=0 so used VPIN weight for edge test.

### Completion Notes List
- SignalPipeline with concrete OBI/VPIN/Microprice types, zero vtable overhead
- CompositeEvaluator with weighted scoring, monotonic decision_id, structured tracing
- FeeGate with integer round-trip cost, edge multiple threshold, staleness checking (30d warn, 60d block)
- permits_flatten() always true — staleness never blocks safety orders
- TradeDecision with full signal values and causality tracing via decision_id
- 16 tests covering all evaluation paths, fee computation, staleness, pipeline lifecycle
- All 208 workspace tests pass, zero clippy warnings

### Change Log
- 2026-04-17: Implemented Story 3.5 — Composite Evaluation & Fee-Aware Gating (all tasks complete)

### File List
- crates/engine/src/signals/composite.rs (new)
- crates/engine/src/signals/mod.rs (modified — added SignalPipeline, PipelineSnapshot, composite module)
- crates/engine/src/risk/mod.rs (new)
- crates/engine/src/risk/fee_gate.rs (new)
- crates/engine/src/lib.rs (modified — added pub mod risk)
- crates/engine/tests/composite_tests.rs (new)
