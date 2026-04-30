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
- N-4: Auto-upgrade `Submitted -> Confirmed` on first non-rejection fill skips the journal record for that arc — forensic replay sees Confirmed appear with no `Submitted -> Confirmed` entry.
- N-5: Orphan fills journal `decision_id = Some(0)` (rather than `None`) — ambiguous with a legitimate `decision_id = 0`.
- N-6: `publish_execution_report` does not validate `side` against the originating order — defensive check belongs in the live `rithmic-rs` listener story.

## Deferred from: code review of story-4.3 (2026-04-30)

- S-2: `BracketManager::on_entry_fill` ignores `FillType::Partial` fill size — SL/TP submitted at full bracket quantity, oversizing if entry partials. CME Market entries are typically atomic, but the AC doesn't restrict to atomic fills. Owned by 4-5 (cumulative-fill tracking + position reconciliation). [`crates/engine/src/order_manager/bracket.rs:253-335`]
- S-4: `FlattenRetry` reuses the same `order_id` across all 3 retries — relies on broker-side `RequestNewOrder` dedup by `user_tag` that is not verified in the diff. Either verify the live OrderPlant dedupes (recorded-session test) or allocate a fresh `order_id` per attempt. Owned by the live OrderPlant integration story. [`crates/broker/src/position_flatten.rs:127-194`]
- N-1: `BracketOrder::new` doc comment is unclear about when to prefer `new` vs `from_decision` — minor doc-only clean-up.
- N-2: OCO native-vs-emulated assumption is unverified in the diff — story 4-5 reconciliation should carry an explicit recorded-session test showing the OCO counterpart-cancel arriving on TP/SL fill. [`crates/engine/src/order_manager/bracket.rs:30-33`]
- N-3: `OrderCancellation::cancel_entries_and_limits` returns `usize` instead of `Result<usize, _>` — partial-failure visibility lost. Revisit when the engine-side iterator lands in 4-4/4-5/epic-5. [`crates/engine/src/risk/panic_mode.rs:40-46`]
- N-4: `FlattenRetry::flatten_with_retry` does not enforce a precondition that a position exists — could create one in the wrong direction if engine engages flatten on a stale bracket. Add a debug-assert or doc warning. [`crates/broker/src/position_flatten.rs:121-130`]
- N-5: `BracketManager::on_stop_confirmed` does not distinguish "duplicate ack" from "out-of-order confirmation after Flattening" — both branches warn-log identically. [`crates/engine/src/order_manager/bracket.rs:344-361`]

Note: S-1 (Rejected entry handling in `on_entry_fill`) and S-3 (engage_flatten error swallowing) from the 4-3 review are NOT deferred — they should land in 4-4 before the engine event loop wires the bracket manager onto a live fill stream. They are tracked in the 4-3 review report under Should-Fix.
