# Story 5.6: Operator Alerting on Circuit Breaker Events

Status: review

## Story

As a trader-operator,
I want to be notified when circuit breakers activate,
So that I can investigate and intervene when the system halts trading.

## Acceptance Criteria (BDD)

- Given any circuit breaker (not gate) activates When activation occurs Then written to dedicated alert log, includes: breaker type, trigger reason, timestamp, position state, P&L
- Given panic mode activates When escalation occurs Then alert with severity "critical" including all flatten attempt details
- Given notification mechanism When alert written Then external script/hook invoked (configurable path), receives JSON on stdin, script failure never blocks system

## Tasks / Subtasks

### Task 1: Define alert types (AC: breaker type, trigger reason, timestamp, position state, P&L)
- [x] 1.1: Create `engine/src/risk/alerting.rs` with `Alert` struct containing: `severity` (AlertSeverity enum), `breaker_type` (BreakerType), `trigger_reason` (String), `timestamp` (UnixNanos), `position_state` (PositionSnapshot), `current_pnl` (i64 quarter-ticks)
- [x] 1.2: Define `AlertSeverity` enum: `Warning`, `Error`, `Critical`
- [x] 1.3: Define `PositionSnapshot` struct: `symbol` (String), `size` (i32), `side` (Option<Side>), `unrealized_pnl` (i64)
- [x] 1.4: Implement `serde::Serialize` for Alert and all nested types (for JSON output to script)
- [x] 1.5: Map breaker activations to severity: most breakers = `Error`, panic mode = `Critical`

### Task 2: Implement dedicated alert log (AC: separate from general structured logs)
- [x] 2.1: Create a separate `tracing` subscriber/layer for alert events writing to a dedicated file (e.g., `data/alerts.log`) — implemented as a dedicated file writer in `AlertManager` (no shared tracing pipeline so the alert log is provably isolated from the general structured log)
- [x] 2.2: Alert log format: JSON lines, one alert per line, with all fields from Alert struct
- [x] 2.3: Alert log is append-only, never rotated by the engine (external rotation via logrotate or similar) — file is reopened in append mode per write
- [x] 2.4: Ensure alert log writes are flushed immediately (fsync) — alerts must not be lost in a crash (`File::sync_all` per write)
- [x] 2.5: Alert log path is configurable in TOML config (`AlertingConfig::alert_log_path`)

### Task 3: Implement panic mode alert (AC: severity critical, flatten attempt details)
- [x] 3.1: Define `PanicAlert` struct extending Alert with: `flatten_attempts` (Vec<FlattenAttemptDetail>) — implemented as `Alert` with optional `flatten_attempts` populated only on panic-mode activations (avoids two-struct fork; serialised JSON omits the field on non-panic alerts)
- [x] 3.2: `FlattenAttemptDetail` contains: attempt_number (u8), order_details (String), rejection_reason (Option<String>), timestamp
- [x] 3.3: Panic mode alert severity is always `Critical`
- [x] 3.4: Include complete PanicContext from Story 5.3 in the alert payload (`PanicContext` struct in `panic_mode.rs`)

### Task 4: Implement external notification hook (AC: configurable path, JSON on stdin, failure never blocks)
- [x] 4.1: Add `alert_script_path: Option<String>` to config (TOML) (`AlertingConfig`)
- [x] 4.2: Implement `invoke_notification_script(script_path: &str, alert_json: &str)` that spawns the script as a child process
- [x] 4.3: Pass alert payload as JSON on stdin to the script process
- [x] 4.4: Set a timeout on the script (e.g., 10 seconds) — kill if it exceeds (`with_script_timeout`, default 10s)
- [x] 4.5: If script fails (non-zero exit, timeout, not found): log the failure at `warn` level, never block system
- [x] 4.6: If `alert_script_path` is None/not configured: skip script invocation, alert is still written to log
- [x] 4.7: Script invocation is async (spawned on Tokio runtime) — never blocks the event loop or hot path — implemented with a fire-and-forget OS thread instead of Tokio so we don't add an async-runtime dependency to a synchronous code path; semantics are identical (script invocation never blocks the AlertManager loop or the producers)

### Task 5: Implement AlertManager to coordinate logging and notification (AC: all)
- [x] 5.1: Create `AlertManager` struct with fields: alert_log_path, script_path (Option), crossbeam receiver
- [x] 5.2: Implement `fn process_alert(&self, alert: Alert)` that writes to alert log AND invokes script
- [x] 5.3: AlertManager receives alerts via `crossbeam_channel::Receiver<Alert>` — decoupled from hot path
- [x] 5.4: Run AlertManager on a separate thread (or Tokio task) to never block the event loop (`AlertManager::spawn`)
- [x] 5.5: Filter: only circuit breaker activations (not gate activations) trigger alerts — `Alert::for_breaker` only accepts `CircuitBreakerType`; gates have no alerting wiring

### Task 6: Wire alerting into CircuitBreakers and PanicMode (AC: integration)
- [x] 6.1: In CircuitBreakers, when `trip_breaker()` is called for breaker-category types, send Alert to AlertManager channel — `CircuitBreakers::with_alerts(...)` opt-in + `trip_breaker_with_context(...)`; emits an `Error` alert for `BreakerCategory::Breaker` trips and stays silent for gates
- [x] 6.2: In PanicMode, when `activate()` is called, send PanicAlert with Critical severity (`PanicMode::new_with_alerts` + `activate_with_context`)
- [x] 6.3: Gate activations/clearances are NOT sent to AlertManager (they are logged in general structured logs only) — explicit `BreakerCategory::Breaker` filter inside `CircuitBreakers::trip_breaker_with_context`; `FeeGate` is unchanged and emits no alerts; verified by `gate_trip_does_not_emit_alert`
- [x] 6.4: Ensure alert channel is bounded — if AlertManager falls behind, use `try_send` and log overflow (`AlertSender::send` uses `try_send`; full / disconnected paths log at warn)

### Task 7: Unit tests (AC: all)
- [x] 7.1: Test breaker activation produces Alert with correct fields (`breaker_alert_carries_all_fields`)
- [x] 7.2: Test gate activation does NOT produce an alert (`gate_trip_does_not_emit_alert`, `gate_activations_have_no_alert_constructor`)
- [x] 7.3: Test panic mode produces Alert with severity Critical and flatten attempt details (`panic_alert_is_critical_with_flatten_attempts`, `activate_emits_critical_alert_with_flatten_context`)
- [x] 7.4: Test Alert serializes to valid JSON with all required fields (`alert_serializes_to_json_with_required_fields`, `panic_alert_serializes_with_flatten_attempts`)
- [x] 7.5: Test notification script receives correct JSON on stdin (`notification_script_receives_json_on_stdin`, unix-only)
- [x] 7.6: Test notification script failure is logged but does not block system (`notification_script_failure_does_not_block`)
- [x] 7.7: Test notification script timeout is enforced (`notification_script_timeout_is_enforced`)
- [x] 7.8: Test missing/unconfigured script path: alert still written to log, no script invoked (`missing_script_path_still_logs_alert`)
- [x] 7.9: Test alert log writes are separate from general structured log output (`alert_log_is_dedicated_file`)
- [x] 7.10: Test AlertManager processes multiple alerts in order (`manager_processes_multiple_alerts_in_order`)
- [x] 7.11: Bounded channel overflow drops alert without blocking (`send_drops_when_channel_full_without_blocking`)

## Dev Notes

### Architecture Patterns & Constraints
- Alert log is separate from general structured logs (`tracing` JSON output) — dedicated file for operator-critical events
- External notification script is the V1 notification mechanism — simple, flexible, no external service dependencies
- Script receives JSON on stdin, can be any executable (bash script for email, curl to Slack webhook, etc.)
- Script failure NEVER blocks the system — alerting is best-effort for notification, guaranteed for logging
- AlertManager runs on a separate thread/task, receives alerts via crossbeam channel — fully decoupled from hot path
- Only breaker activations trigger alerts, not gate activations (gates are routine operational events)
- Panic mode alerts are severity Critical and include full flatten context
- Alert log is flushed immediately (fsync) to survive crashes
- `unsafe` is forbidden in `engine/src/risk/`

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── risk/
│   │   ├── alerting.rs         # Alert, AlertSeverity, PositionSnapshot, PanicAlert, AlertManager
│   │   ├── circuit_breakers.rs # Sends alerts on breaker activation
│   │   └── panic_mode.rs       # Sends critical alert on panic mode activation
│   └── config/
│       └── loader.rs           # alert_log_path, alert_script_path config loading
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Section: Monitoring (V1), circuit breaker and connection failure events write to alert log
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 5, Story 5.6
- Dependencies: crossbeam-channel 0.5.15, serde (JSON serialization), tracing 0.1.44, tokio 1.51.1 (async script invocation)

## Dev Agent Record

### Agent Model Used
- Claude Opus 4.7 (1M context) running BMad dev-story workflow.

### Debug Log References
- `cargo build --workspace` — clean.
- `cargo test --workspace` — all crates pass; 19 tests in `engine::risk::*` (alerting + panic_mode integration).
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.

### Completion Notes List
- Added `engine/src/risk/alerting.rs` containing `Alert`, `AlertSeverity`, `BreakerKind`, `PositionSnapshot`, `FlattenAttemptDetail`, `AlertSender`, `AlertReceiver`, `AlertManager`. Module-level `#![deny(unsafe_code)]` enforced at `engine/src/risk/mod.rs`.
- `AlertManager` writes one JSON line per alert and `fsync`s the file (`File::sync_all`) before continuing. The log file is reopened in append mode for every write so external rotation (logrotate) never tears a write or loses data.
- External script invocation is fire-and-forget on a short-lived OS thread (per the story's "never blocks the hot path" rule). A watchdog inside that thread enforces the configurable timeout (default 10s) and kills the child on expiry. Non-zero exit, timeout, and spawn failure are logged at `warn`; none of them propagate.
- Bounded crossbeam channel (capacity 1024) decouples producers from disk/process I/O. `AlertSender::send` uses `try_send`; full and disconnected channels are logged at `warn` and never block.
- `PanicMode::new_with_alerts` + `activate_with_context` is the integration surface for Story 5.6 Task 6.2. The pre-existing `activate(...)` signature is preserved as a thin shim so existing call sites continue to compile.
- `CircuitBreakers::with_alerts(...)` + `trip_breaker_with_context(...)` is the Story 5.6 wiring for breaker trips (Task 6.1). The pre-existing `trip_breaker(...)` is preserved and forwards to the context-aware variant with empty context. Gate trips deliberately bypass the alert send via the `BreakerCategory::Breaker` filter so that Task 6.3 holds without depending on caller discipline.
- Added `core::AlertingConfig` (`alert_log_path`, `alert_script_path`, `alert_script_timeout_ms`) with stable defaults; `AlertManager::from_config` constructs a manager from the parsed config. Sample stanza added to `config/default.toml`.
- Gate activations are deliberately not exposed to alerting: `Alert::for_breaker` accepts only `CircuitBreakerType`, and `FeeGate` was not changed. Documented and tested by `gate_activations_have_no_alert_constructor` and the dedicated-log test.

### File List
- `crates/engine/src/risk/alerting.rs` (new)
- `crates/engine/src/risk/mod.rs` (extended Story 5.1 module docs, exported alerting types alongside existing `CircuitBreakers`/`DenialReason`/`TradingDenied`)
- `crates/engine/src/risk/circuit_breakers.rs` (added `with_alerts`, `trip_breaker_with_context`, four new alerting integration tests)
- `crates/engine/src/risk/panic_mode.rs` (added `new_with_alerts`, `activate_with_context`, `PanicContext`, two new tests)
- `crates/engine/Cargo.toml` (added `serde`, `serde_json` deps)
- `crates/core/src/config/alerting.rs` (new)
- `crates/core/src/config/mod.rs` (export `AlertingConfig`)
- `crates/core/src/lib.rs` (re-export `AlertingConfig`)
- `Cargo.toml` (added `serde_json` workspace dep)
- `config/default.toml` (added commented `[alerting]` stanza)

### Change Log
- 2026-04-30 — Initial implementation of Story 5.6 (operator alerting on circuit-breaker events): alert types, durable JSON-lines alert log with fsync per write, fire-and-forget external script invocation with timeout enforcement, bounded crossbeam channel, panic-mode integration via `activate_with_context`, dedicated `AlertingConfig`.
