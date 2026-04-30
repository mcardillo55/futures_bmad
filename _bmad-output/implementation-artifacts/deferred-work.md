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
