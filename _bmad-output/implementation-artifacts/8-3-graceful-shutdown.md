# Story 8.3: Graceful Shutdown

Status: ready-for-dev

## Story

As a trader-operator,
I want a clean shutdown that leaves no orphaned positions or orders,
So that I can safely restart the system without risk exposure.

## Acceptance Criteria (BDD)

- Given `engine/src/lifecycle/shutdown.rs` When shutdown signal received (SIGTERM/SIGINT/programmatic) Then executes: 1. Disable strategies 2. Cancel open entry/limit orders (preserve stops) 3. Wait for pending confirmations (timeout) 4. Verify flat 5. Cancel remaining stops 6. Disconnect broker 7. Flush journal (checkpoint WAL) 8. Flush Parquet 9. Stop heartbeat 10. Exit
- Given positions exist at shutdown When cannot flatten within timeout Then logs warning, exchange-resting stops remain, operator alerted
- Given forced shutdown (SIGKILL/crash) When terminates Then exchange-resting stops protect positions, WAL replay + reconciliation recover state on next startup

## Tasks / Subtasks

### Task 1: Implement signal handler registration (AC: SIGTERM/SIGINT/programmatic)
- 1.1: In `crates/engine/src/lifecycle/shutdown.rs`, use `tokio::signal` to register handlers for SIGTERM and SIGINT
- 1.2: Create a `ShutdownSignal` that can be triggered programmatically (e.g., from circuit breaker or fatal error) using a `tokio::sync::watch` channel
- 1.3: Combine signal and programmatic triggers: whichever fires first initiates shutdown
- 1.4: Log signal type at `warn` level: "Shutdown initiated by {SIGTERM|SIGINT|programmatic: reason}"
- 1.5: Ignore duplicate signals during shutdown (second SIGINT does not force-kill)

### Task 2: Implement shutdown orchestrator (AC: ordered shutdown sequence)
- 2.1: Implement `pub async fn run_shutdown(system: RunningSystem, reason: ShutdownReason) -> ExitCode`
- 2.2: Define `ShutdownReason` enum: `Signal(SignalType)`, `CircuitBreak(String)`, `FatalError(String)`, `Operator`
- 2.3: Execute shutdown steps sequentially, each step logged at `info` level with timing
- 2.4: Define configurable timeout for overall shutdown (default 30s); if exceeded, log error and proceed to forced cleanup

### Task 3: Implement Step 1 — Disable strategies (AC: disable strategies)
- 3.1: Set strategy manager state to `Disabled` — no new signals processed, no new orders generated
- 3.2: Log number of active strategies disabled

### Task 4: Implement Step 2 — Cancel open entry/limit orders, preserve stops (AC: cancel entries, preserve stops)
- 4.1: Query active orders from order manager, partition into: entry/limit orders and stop orders
- 4.2: Submit cancel requests for all entry/limit orders via broker adapter
- 4.3: CRITICAL: do NOT cancel stop orders at this stage — stops protect open positions
- 4.4: Log count of cancelled entry/limit orders and preserved stop orders

### Task 5: Implement Step 3 — Wait for pending confirmations (AC: timeout on confirmations)
- 5.1: Wait for cancel confirmations from broker with per-order timeout (5s)
- 5.2: Wait for any pending fill confirmations for recently submitted orders
- 5.3: On timeout per order: log warning with order details, mark as uncertain
- 5.4: Update local position state with any fills received during wait

### Task 6: Implement Step 4-5 — Verify flat, then cancel stops (AC: verify flat, cancel stops)
- 6.1: Query current position state from order manager
- 6.2: If flat (no open positions): cancel all remaining stop orders, log "Position flat, stops cancelled"
- 6.3: If NOT flat: log critical warning "Shutdown with open position: {instrument} {size} {side}", keep exchange-resting stops in place
- 6.4: If not flat, emit operator alert (log at `error` level with structured fields for alerting)

### Task 7: Implement Step 6 — Disconnect broker (AC: broker disconnect)
- 7.1: Call `BrokerAdapter::disconnect()` to cleanly close WebSocket connections
- 7.2: Wait for disconnect confirmation with timeout (5s)
- 7.3: Log disconnect status

### Task 8: Implement Steps 7-10 — Flush journal, Parquet, stop heartbeat, exit (AC: flush, exit)
- 8.1: Flush SQLite journal: call `PRAGMA wal_checkpoint(TRUNCATE)` to ensure all WAL data written to main database
- 8.2: Flush Parquet writer: finalize any open row groups, close file handles
- 8.3: Stop heartbeat task (cancel the tokio task)
- 8.4: Log "Shutdown complete" at `info` level with summary: reason, duration, position state at exit
- 8.5: Return appropriate `ExitCode` (0 for clean, 1 for shutdown-with-position)

### Task 9: Handle forced shutdown / crash recovery documentation (AC: crash recovery)
- 9.1: Document in code comments that SIGKILL cannot be caught — exchange-resting stops are the safety net
- 9.2: Ensure WAL replay in startup (Story 8.2, Task 6) handles crash recovery
- 9.3: Add `last_shutdown_clean: bool` flag to journal metadata, set to true only on clean exit
- 9.4: On next startup, if `last_shutdown_clean == false`, log warning "Previous shutdown was not clean, running full reconciliation"

### Task 10: Unit tests (AC: all)
- 10.1: Test clean shutdown with no positions: all steps execute, stops cancelled, exit code 0
- 10.2: Test shutdown with open position: stops preserved, exit code 1, operator alert logged
- 10.3: Test entry/limit orders cancelled but stops preserved during step 2
- 10.4: Test pending confirmation timeout: uncertain orders logged, shutdown continues
- 10.5: Test signal handler: SIGTERM triggers shutdown sequence
- 10.6: Test programmatic shutdown: circuit breaker trigger initiates shutdown
- 10.7: Test journal WAL checkpoint called on clean shutdown
- 10.8: Test duplicate signal ignored during active shutdown

## Dev Notes

### Architecture Patterns & Constraints
- CRITICAL SAFETY RULE: Stop orders are NEVER cancelled while a position is open. Stops are the last line of defense. Only cancel stops after verifying the position is flat
- Exchange-resting stops survive our process death — this is the fundamental safety guarantee. Even if SIGKILL terminates us, the exchange still holds the stop order
- The shutdown sequence is the mirror of startup: startup builds up state, shutdown tears it down
- WAL checkpoint on shutdown ensures the SQLite database is in a clean state for next startup. Without checkpoint, WAL replay on next startup takes longer
- Parquet flush ensures no market data is lost. Open row groups are finalized before exit
- The 30s overall shutdown timeout prevents hanging during network issues. After timeout, we accept potential data loss and exit

### Project Structure Notes
```
crates/engine/src/lifecycle/
├── mod.rs              (module declarations, ShutdownReason)
└── shutdown.rs         (run_shutdown, signal handling)
```

### References
- Story 8.2 (Startup Sequence) — crash recovery via WAL replay
- Story 5.1 (Circuit Breaker Framework) — programmatic shutdown trigger
- Story 4.1 (SQLite Event Journal) — WAL checkpoint
- Story 2.4 (Parquet Recording) — Parquet flush

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
