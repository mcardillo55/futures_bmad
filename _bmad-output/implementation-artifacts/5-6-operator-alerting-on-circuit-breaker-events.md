# Story 5.6: Operator Alerting on Circuit Breaker Events

Status: ready-for-dev

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
- 1.1: Create `engine/src/risk/alerting.rs` with `Alert` struct containing: `severity` (AlertSeverity enum), `breaker_type` (BreakerType), `trigger_reason` (String), `timestamp` (UnixNanos), `position_state` (PositionSnapshot), `current_pnl` (i64 quarter-ticks)
- 1.2: Define `AlertSeverity` enum: `Warning`, `Error`, `Critical`
- 1.3: Define `PositionSnapshot` struct: `symbol` (String), `size` (i32), `side` (Option<Side>), `unrealized_pnl` (i64)
- 1.4: Implement `serde::Serialize` for Alert and all nested types (for JSON output to script)
- 1.5: Map breaker activations to severity: most breakers = `Error`, panic mode = `Critical`

### Task 2: Implement dedicated alert log (AC: separate from general structured logs)
- 2.1: Create a separate `tracing` subscriber/layer for alert events writing to a dedicated file (e.g., `data/alerts.log`)
- 2.2: Alert log format: JSON lines, one alert per line, with all fields from Alert struct
- 2.3: Alert log is append-only, never rotated by the engine (external rotation via logrotate or similar)
- 2.4: Ensure alert log writes are flushed immediately (fsync) — alerts must not be lost in a crash
- 2.5: Alert log path is configurable in TOML config

### Task 3: Implement panic mode alert (AC: severity critical, flatten attempt details)
- 3.1: Define `PanicAlert` struct extending Alert with: `flatten_attempts` (Vec<FlattenAttemptDetail>)
- 3.2: `FlattenAttemptDetail` contains: attempt_number (u8), order_details (String), rejection_reason (Option<String>), timestamp
- 3.3: Panic mode alert severity is always `Critical`
- 3.4: Include complete PanicContext from Story 5.3 in the alert payload

### Task 4: Implement external notification hook (AC: configurable path, JSON on stdin, failure never blocks)
- 4.1: Add `alert_script_path: Option<String>` to config (TOML)
- 4.2: Implement `invoke_notification_script(script_path: &str, alert_json: &str)` that spawns the script as a child process
- 4.3: Pass alert payload as JSON on stdin to the script process
- 4.4: Set a timeout on the script (e.g., 10 seconds) — kill if it exceeds
- 4.5: If script fails (non-zero exit, timeout, not found): log the failure at `warn` level, never block system
- 4.6: If `alert_script_path` is None/not configured: skip script invocation, alert is still written to log
- 4.7: Script invocation is async (spawned on Tokio runtime) — never blocks the event loop or hot path

### Task 5: Implement AlertManager to coordinate logging and notification (AC: all)
- 5.1: Create `AlertManager` struct with fields: alert_log_path, script_path (Option), crossbeam receiver
- 5.2: Implement `fn process_alert(&self, alert: Alert)` that writes to alert log AND invokes script
- 5.3: AlertManager receives alerts via `crossbeam_channel::Receiver<Alert>` — decoupled from hot path
- 5.4: Run AlertManager on a separate thread (or Tokio task) to never block the event loop
- 5.5: Filter: only circuit breaker activations (not gate activations) trigger alerts

### Task 6: Wire alerting into CircuitBreakers and PanicMode (AC: integration)
- 6.1: In CircuitBreakers, when `trip_breaker()` is called for breaker-category types, send Alert to AlertManager channel
- 6.2: In PanicMode, when `activate()` is called, send PanicAlert with Critical severity
- 6.3: Gate activations/clearances are NOT sent to AlertManager (they are logged in general structured logs only)
- 6.4: Ensure alert channel is bounded — if AlertManager falls behind, use `try_send` and log overflow

### Task 7: Unit tests (AC: all)
- 7.1: Test breaker activation produces Alert with correct fields (type, reason, timestamp, position, P&L)
- 7.2: Test gate activation does NOT produce an alert
- 7.3: Test panic mode produces Alert with severity Critical and flatten attempt details
- 7.4: Test Alert serializes to valid JSON with all required fields
- 7.5: Test notification script receives correct JSON on stdin
- 7.6: Test notification script failure is logged but does not block system
- 7.7: Test notification script timeout is enforced
- 7.8: Test missing/unconfigured script path: alert still written to log, no script invoked
- 7.9: Test alert log writes are separate from general structured log output
- 7.10: Test AlertManager processes multiple alerts in order

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
### Debug Log References
### Completion Notes List
### File List
