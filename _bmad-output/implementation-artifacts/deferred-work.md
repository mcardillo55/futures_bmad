# Deferred Work

## Deferred from: code review of story-1.6 (2026-04-16)

- [x] `ConfigValidationError` missing `std::error::Error` impl [crates/core/src/config/validation.rs:3] — fixed in spec-deferred-review-fixes
- [x] `FixedPrice::from_f64` accepts non-finite values [crates/core/src/types/fixed_price.rs:53] — fixed in spec-deferred-review-fixes

## Deferred from: code review of story-2.1 (2026-04-16)

- Env var test thread safety — `unsafe set_var/remove_var` in `connection.rs` tests may race in parallel test execution. Pre-existing Rust limitation with env var mutation in tests.

## Deferred from: code review of story-4.2 (2026-04-30)

- N-1: `OrderSubmitter` (broker) and `BrokerAdapter::submit_order` (core) are two parallel "submit" trait surfaces — recommend consolidation in story 4.4 when the WAL-write-before-submit invariant lands. Blast-radius into testkit + `RithmicAdapter` makes the rename out of scope for 4.2.
- N-2: `route_pending_orders` drains in a tight `while let Some(...)` loop with no `tokio::task::yield_now()` — risks starving Tokio peers under burst (e.g., bracket bursts in 4.3).
- N-3: `DecisionIdMap` entries are removed only on terminal reports; non-terminal partials that never reach a terminal (broker disconnect mid-stream) leak — owned by 4.5 reconciliation.
- [x] N-4: Auto-upgrade `Submitted -> Confirmed` on first non-rejection fill skips the journal record for that arc. **Resolved in 4-5 carryover commit (`366fcf7`).** Same fix as 4-4 S-3.
- N-5: Orphan fills journal `decision_id = Some(0)` (rather than `None`) — ambiguous with a legitimate `decision_id = 0`.
- N-6: `publish_execution_report` does not validate `side` against the originating order — defensive check belongs in the live `rithmic-rs` listener story.
- [x] S-4 (try_push return ignored at order_routing.rs:347 for synthetic reject fill): **Resolved in 4-5 carryover commit (`366fcf7`)** — `try_push` return is now checked and an error is logged when the FillQueue is full so the engine knows the synthetic reject was not observed.
- [x] S-5 (Submission `Timeout` synthesizes `RejectReason::Unknown` via the rejection path): **Resolved in 4-5 carryover commit (`366fcf7`)** — new `SubmissionError::should_synthesize_reject()` carves out Timeout. Order stays Submitted; the engine's 5s timeout watchdog promotes it to Uncertain and 4-5 reconciliation queries the broker for the true status.

## Deferred from: code review of story-4.3 (2026-04-30)

- S-2: `BracketManager::on_entry_fill` ignores `FillType::Partial` fill size — SL/TP submitted at full bracket quantity, oversizing if entry partials. CME Market entries are typically atomic, but the AC doesn't restrict to atomic fills. Owned by 4-5 (cumulative-fill tracking + position reconciliation). [`crates/engine/src/order_manager/bracket.rs:253-335`]
- S-4: `FlattenRetry` reuses the same `order_id` across all 3 retries — relies on broker-side `RequestNewOrder` dedup by `user_tag` that is not verified in the diff. Either verify the live OrderPlant dedupes (recorded-session test) or allocate a fresh `order_id` per attempt. Owned by the live OrderPlant integration story. [`crates/broker/src/position_flatten.rs:127-194`]
- N-1: `BracketOrder::new` doc comment is unclear about when to prefer `new` vs `from_decision` — minor doc-only clean-up.
- N-2: OCO native-vs-emulated assumption is unverified in the diff — story 4-5 reconciliation should carry an explicit recorded-session test showing the OCO counterpart-cancel arriving on TP/SL fill. [`crates/engine/src/order_manager/bracket.rs:30-33`]
- N-3: `OrderCancellation::cancel_entries_and_limits` returns `usize` instead of `Result<usize, _>` — partial-failure visibility lost. Revisit when the engine-side iterator lands in 4-4/4-5/epic-5. [`crates/engine/src/risk/panic_mode.rs:40-46`]
- N-4: `FlattenRetry::flatten_with_retry` does not enforce a precondition that a position exists — could create one in the wrong direction if engine engages flatten on a stale bracket. Add a debug-assert or doc warning. [`crates/broker/src/position_flatten.rs:121-130`]
- N-5: `BracketManager::on_stop_confirmed` does not distinguish "duplicate ack" from "out-of-order confirmation after Flattening" — both branches warn-log identically. [`crates/engine/src/order_manager/bracket.rs:344-361`]

Note: S-1 (Rejected entry handling in `on_entry_fill`) and S-3 (engage_flatten error swallowing) from the 4-3 review are NOT deferred — they should land in 4-4 before the engine event loop wires the bracket manager onto a live fill stream. They are tracked in the 4-3 review report under Should-Fix.

## Deferred from: code review of story-4.4 (2026-04-30)

- [x] S-1: Terminal `mark_resolved` failure leaves WAL row stale (warn-only); in-memory state advances but WAL row stays at previous non-terminal state. **Resolved in 4-5 carryover commit (`366fcf7`):** failure now journaled as a `SystemEvent` so operators see the durability lapse without grepping logs. Recovery still re-finds the row and reconciles via broker query.
- [x] S-3: Auto-upgrade `Submitted -> Confirmed` on first non-rejection fill skips the journal record for that arc. **Resolved in 4-5 carryover commit (`366fcf7`):** the implicit transition is now journaled with `decision_id` so the audit trail is complete from order birth.
- [x] S-4: `flatten_side_for` silently returns None when WAL is absent or row missing, collapsing `FilledFlattenRequired` into `NotApplicable` and dropping the flatten despite broker confirming filled. **Resolved in 4-5 carryover commit (`366fcf7`):** new `ResolveOutcome::FilledFlattenSideUnknown` variant escalates to the circuit breaker via the registered callback, journals a SystemEvent, and surfaces the unknown-side condition explicitly to the caller. NFR16 compliance restored.
- N-1: `wal.rs` has an awkward `let _ = order.order_type;` workaround and a `_silence_unused_order_type` fn to suppress unused-field warnings — both can be removed cleanly. [`crates/engine/src/order_manager/wal.rs:168, 348-351`]
- N-2: `WalError::BadRow` does not include the offending `order_id` — add it for forensic diagnosis. [`crates/engine/src/order_manager/wal.rs:251, 284`]
- N-3: A real `FillEvent` arriving for an order in `Uncertain`/`PendingRecon` trips the circuit-breaker callback via `InvalidTransition` (race with reconciliation). Position safety is preserved (`resolve_uncertain` is authoritative) but the breaker shouldn't fire for a benign data race. Absorb the fill silently, log at info, and let recon resolve. [`crates/engine/src/order_manager/mod.rs:550-566`]
- N-4: `OrderManager::tick(now)` requires monotonic `now` from caller — document the contract in the module-level docs.
- N-5: `wal::state_to_str`/`parse_state` are open-coded match arms over `core::OrderState`; risk drifting if a new variant is added. Centralize on `OrderState` (e.g., `OrderState::as_str() -> &'static str` + `from_str`). [`crates/engine/src/order_manager/wal.rs:315-346`]
- N-6: `OrderManager::with_wal` uses `..Self::new(journal)` spread; future field additions risk silent drop. Consider a builder pattern as the field set grows. [`crates/engine/src/order_manager/mod.rs:245-250`]
- AUDIT-1 (carryover): Over-fill / zero-size / partial-arithmetic-inconsistent fills are rejected with `FillOutcome::InvalidFillSize` and a `warn!` log, but NOT journaled as a `SystemEvent`. Forensic replay sees no record of the malformed fill. Promote the warn-log to a journal record.

Note: S-2 from this review (partial entry over-sizes SL/TP via warn-only) is the same item already tracked as 4-3 S-2 above and remains owned by story 4-5 (position reconciliation). Live trading must NOT proceed until 4.5 lands or the engine adds a panic-mode trigger for any partial entry.

## Deferred from: story-4.5 implementation (2026-04-30)

- 4-3 S-2 / 4-4 S-2 (carryforward, partial entry over-sizes SL/TP): the original 4-5 spec earmarked this for "position reconciliation" scope, but on closer reading the fix lives entirely inside `BracketManager::on_entry_fill` — it is NOT a position-tracking concern. The PositionTracker now correctly handles partial fills (cumulative-fill arithmetic via `Position::apply_fill`), but `BracketManager` still warn-logs and proceeds with the original SL/TP quantity. Recommend a focused follow-up story to (a) accumulate partial entry fills against `BracketEntry`, (b) defer SL/TP submission until the entry is fully filled OR re-size the SL/TP to the cumulative quantity, and (c) panic-mode escalate if the entry partial-fills and then stalls past a timeout. Until that lands, CME atomic-Market-fill assumption holds in practice for liquid futures (the spec's target). [`crates/engine/src/order_manager/bracket.rs:325-334`]
- 4-3 S-4 (`FlattenRetry` reuses same `order_id` across retries): out-of-scope of 4-5 (4-5 is engine-side position tracking + reconciliation; this is a broker-side dedup question that needs a recorded-session test against the live OrderPlant). Will land with the live-OrderPlant integration story. [`crates/broker/src/position_flatten.rs:127-194`]
- 4-3 N-2 (OCO counterpart-cancel verification): out-of-scope of 4-5 — needs a recorded Rithmic OrderPlant session. Will land with the live-OrderPlant integration story. [`crates/engine/src/order_manager/bracket.rs:30-33`]
- Reconciliation triggers (Tasks 6.1 startup-blocking, 6.2 reconnection-FSM-driven, 6.3 periodic 60s tokio interval): the seams (`ReconciliationTrigger` enum + `PositionTracker::reconcile_and_handle`) are in place. Actual scheduling lives in story 8-2 (lifecycle/startup) and story 8-4 (reconnection FSM); folding it into 4-5 would require building those modules out of order.
- `BracketManager::on_bracket_fill` does not yet drive `PositionTracker::apply_fill` — the engine event-loop wiring (story 8-2 startup or a subsequent event-loop integration story) will own the wire-up. The seam is in place: `FlattenOutcome::TakeProfit { realized_pnl_quarter_ticks, .. }` already carries the closed-leg P&L; the event loop can forward fills to both `BracketManager` and `PositionTracker`.
- Live `BrokerAdapter::query_positions` for `RithmicAdapter` returns `BrokerError::PositionQueryFailed` — the TickerPlant doesn't carry positions; live wiring lands with the OrderPlant/PnlPlant integration story.
