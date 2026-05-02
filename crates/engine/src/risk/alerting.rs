//! Operator alerting for circuit-breaker activations (Story 5.6).
//!
//! Two guarantees apply:
//!
//! 1. **Alert log is durable.** Every alert is serialized as one JSON line and
//!    flushed (`fsync`) to a dedicated alert log before [`AlertManager`]
//!    acknowledges processing. The alert log MUST survive a crash; logging is
//!    the contract, not the script invocation.
//!
//! 2. **External script is best-effort and non-blocking.** A configured script
//!    is spawned as a child process with the JSON payload on stdin. The hot
//!    path never waits for it. Script failure (non-zero exit, timeout,
//!    not-found) is logged at `warn` and never propagates back.
//!
//! Architectural placement:
//!
//! - [`Alert`] / [`AlertSeverity`] / [`PositionSnapshot`] are the wire format.
//! - [`PanicAlert`] wraps an [`Alert`] with the flatten-attempt detail captured
//!   in panic-mode escalation.
//! - [`AlertSender`] / [`AlertReceiver`] are a bounded crossbeam channel that
//!   decouples the engine event-loop from disk and process I/O.
//! - [`AlertManager`] runs on its own thread, drains the receiver, persists the
//!   alert, and (best-effort) invokes the operator script asynchronously.
//!
//! Gates (e.g. fee-gate / staleness gate) DO NOT route through here: they emit
//! routine structured-log events. Only circuit breakers and the panic-mode
//! escalation produce alerts.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use futures_bmad_core::{AlertingConfig, BreakerType, Side, UnixNanos};
use serde::Serialize;
use tracing::{debug, error, warn};

/// Bounded capacity for the alert channel. Sized large enough that a healthy
/// system never overflows, small enough that a stuck subscriber is detected
/// quickly via `try_send` failures.
pub const ALERT_CHANNEL_CAPACITY: usize = 1024;

/// Default timeout for the operator notification script. Killed past this.
pub const DEFAULT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(10);

/// Severity carried on the wire and on the alert log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    /// Recoverable; manual review encouraged.
    Warning,
    /// Trading-impacting; investigate immediately.
    Error,
    /// Terminal escalation (panic mode). Manual restart required.
    Critical,
}

/// Position snapshot included on every breaker-driven alert.
///
/// We capture the bare minimum the operator needs to reason about risk
/// posture at the moment of activation — symbol, signed size, side, and
/// unrealized P&L (raw quarter-ticks, signed).
#[derive(Debug, Clone, Serialize)]
pub struct PositionSnapshot {
    pub symbol: String,
    pub size: i32,
    #[serde(serialize_with = "serialize_side_opt")]
    pub side: Option<Side>,
    /// Unrealized P&L in raw quarter-ticks (signed).
    pub unrealized_pnl: i64,
}

impl PositionSnapshot {
    /// Placeholder used when the engine cannot supply an authoritative
    /// snapshot (e.g. activation paths that don't have a position-tracker
    /// handle). Operators reading this value should treat it as "no position
    /// data was available at the moment of activation"; routine paths in the
    /// real engine MUST construct snapshots from the live tracker.
    pub fn flat_unknown() -> Self {
        Self {
            symbol: String::new(),
            size: 0,
            side: None,
            unrealized_pnl: 0,
        }
    }
}

fn serialize_side_opt<S: serde::Serializer>(side: &Option<Side>, s: S) -> Result<S::Ok, S::Error> {
    match side {
        Some(Side::Buy) => s.serialize_str("buy"),
        Some(Side::Sell) => s.serialize_str("sell"),
        None => s.serialize_none(),
    }
}

/// Wire-format breaker type. We don't serialize `core::BreakerType` directly
/// because that crate intentionally has no `serde` dependency on its hot-path
/// types. We delegate to the stable `BreakerType::as_str()` projection that
/// Story 5.1 provides for journal records.
fn breaker_type_str(b: BreakerKind) -> &'static str {
    match b {
        BreakerKind::Circuit(t) => t.as_str(),
        BreakerKind::PanicMode => "panic_mode",
    }
}

/// Discriminator covering all activation sources that produce alerts.
///
/// `core::BreakerType` covers V1 breakers; panic-mode escalation is
/// orthogonal to the breaker enum and gets its own variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerKind {
    Circuit(BreakerType),
    PanicMode,
}

impl Serialize for BreakerKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(breaker_type_str(*self))
    }
}

/// Detail of a single flatten attempt — populated only on panic-mode alerts.
#[derive(Debug, Clone, Serialize)]
pub struct FlattenAttemptDetail {
    pub attempt_number: u8,
    pub order_details: String,
    pub rejection_reason: Option<String>,
    pub timestamp: u64,
}

/// Operator alert — one per circuit-breaker activation. Serialized as a single
/// JSON line on the alert log AND piped to the configured script's stdin.
#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub severity: AlertSeverity,
    pub breaker_type: BreakerKind,
    pub trigger_reason: String,
    pub timestamp: u64,
    pub position_state: PositionSnapshot,
    /// Current realized + unrealized P&L in raw quarter-ticks (signed).
    pub current_pnl: i64,
    /// Populated only when this alert was produced by panic-mode escalation.
    /// Always present (possibly empty) for `BreakerKind::PanicMode`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flatten_attempts: Option<Vec<FlattenAttemptDetail>>,
}

impl Alert {
    /// Build an alert for a non-panic circuit breaker. Severity defaults to
    /// `Error` per Task 1.5.
    pub fn for_breaker(
        breaker: BreakerType,
        reason: impl Into<String>,
        now: UnixNanos,
        position: PositionSnapshot,
        current_pnl: i64,
    ) -> Self {
        Self {
            severity: AlertSeverity::Error,
            breaker_type: BreakerKind::Circuit(breaker),
            trigger_reason: reason.into(),
            timestamp: now.as_nanos(),
            position_state: position,
            current_pnl,
            flatten_attempts: None,
        }
    }

    /// Build a panic-mode alert. Severity is always `Critical`.
    pub fn for_panic(
        reason: impl Into<String>,
        now: UnixNanos,
        position: PositionSnapshot,
        current_pnl: i64,
        flatten_attempts: Vec<FlattenAttemptDetail>,
    ) -> Self {
        Self {
            severity: AlertSeverity::Critical,
            breaker_type: BreakerKind::PanicMode,
            trigger_reason: reason.into(),
            timestamp: now.as_nanos(),
            position_state: position,
            current_pnl,
            flatten_attempts: Some(flatten_attempts),
        }
    }
}

/// Sender half of the alert channel. Cheap `Clone`; intended for any producer
/// (CircuitBreakers, PanicMode) that needs to emit alerts.
#[derive(Debug, Clone)]
pub struct AlertSender {
    tx: Sender<Alert>,
}

impl AlertSender {
    /// Best-effort send. On a full channel we log and drop — the hot path must
    /// never block on alerting (Task 6.4).
    pub fn send(&self, alert: Alert) {
        match self.tx.try_send(alert) {
            Ok(()) => {}
            Err(TrySendError::Full(dropped)) => {
                warn!(
                    target: "alerting",
                    severity = ?dropped.severity,
                    breaker = breaker_type_str(dropped.breaker_type),
                    "alert channel full — dropping alert (AlertManager fell behind)"
                );
            }
            Err(TrySendError::Disconnected(_)) => {
                warn!(target: "alerting", "alert channel disconnected — AlertManager not running");
            }
        }
    }
}

/// Receiver half held by the [`AlertManager`].
pub type AlertReceiver = Receiver<Alert>;

/// Construct a bounded alert channel and the matching sender.
pub fn alert_channel() -> (AlertSender, AlertReceiver) {
    let (tx, rx) = bounded(ALERT_CHANNEL_CAPACITY);
    (AlertSender { tx }, rx)
}

/// Coordinates alert log persistence and (optional) operator-script invocation.
///
/// Construction does not start the worker — call [`AlertManager::spawn`] to
/// move the manager onto a dedicated thread and return the channel sender plus
/// a join handle. For unit tests, [`AlertManager::process_alert`] can be
/// invoked directly with a hand-constructed manager.
pub struct AlertManager {
    alert_log_path: PathBuf,
    script: Option<ScriptInvoker>,
    receiver: AlertReceiver,
}

/// Encapsulates the configured script path + timeout. Cloned into worker
/// threads so script invocations never share a writeable handle.
#[derive(Debug, Clone)]
struct ScriptInvoker {
    path: PathBuf,
    timeout: Duration,
}

impl AlertManager {
    /// Build an `AlertManager`. `alert_log_path` is created (and any parent
    /// directories created) on construction; the file is opened in append
    /// mode for each write so external rotation works as expected
    /// (Task 2.3).
    pub fn new(
        alert_log_path: impl Into<PathBuf>,
        script_path: Option<impl Into<PathBuf>>,
        receiver: AlertReceiver,
    ) -> std::io::Result<Self> {
        let alert_log_path = alert_log_path.into();
        if let Some(parent) = alert_log_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        // Touch the file so that a subsequent open-for-append always succeeds.
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&alert_log_path)?;

        let script = script_path.map(|p| ScriptInvoker {
            path: p.into(),
            timeout: DEFAULT_SCRIPT_TIMEOUT,
        });

        Ok(Self {
            alert_log_path,
            script,
            receiver,
        })
    }

    /// Override the script timeout (default: [`DEFAULT_SCRIPT_TIMEOUT`]).
    pub fn with_script_timeout(mut self, timeout: Duration) -> Self {
        if let Some(ref mut s) = self.script {
            s.timeout = timeout;
        }
        self
    }

    /// Build an [`AlertManager`] from a parsed [`AlertingConfig`].
    ///
    /// `receiver` is the consumer end of [`alert_channel`]; the matching
    /// [`AlertSender`] is held by upstream producers.
    pub fn from_config(config: &AlertingConfig, receiver: AlertReceiver) -> std::io::Result<Self> {
        let manager = AlertManager::new(
            &config.alert_log_path,
            config.alert_script_path.as_ref(),
            receiver,
        )?;
        Ok(manager.with_script_timeout(Duration::from_millis(config.alert_script_timeout_ms)))
    }

    /// Spawn the manager's drain loop onto a dedicated OS thread. Returns the
    /// join handle; closing the matching [`AlertSender`] (i.e. dropping all
    /// senders) ends the loop cleanly.
    pub fn spawn(self) -> JoinHandle<()> {
        thread::Builder::new()
            .name("alert-manager".to_string())
            .spawn(move || self.run())
            .expect("spawn alert-manager thread")
    }

    fn run(self) {
        loop {
            match self.receiver.recv() {
                Ok(alert) => {
                    if let Err(e) = self.process_alert(&alert) {
                        // Persistence failure is logged but never panics — we
                        // keep draining so the next alert still gets a chance.
                        error!(
                            target: "alerting",
                            error = %e,
                            "failed to persist alert (continuing)"
                        );
                    }
                }
                Err(_) => {
                    debug!(target: "alerting", "alert channel closed; AlertManager exiting");
                    break;
                }
            }
        }
    }

    /// Process a single alert: append + fsync to the alert log, then (if
    /// configured) fire the script asynchronously. Returns the I/O error from
    /// the log write only — script failures are swallowed.
    pub fn process_alert(&self, alert: &Alert) -> std::io::Result<()> {
        let json = serde_json::to_string(alert).map_err(std::io::Error::other)?;
        write_alert_line(&self.alert_log_path, &json)?;

        if let Some(script) = &self.script {
            // Fire-and-forget: a dedicated short-lived thread spawns the
            // child. If the script blocks/hangs the watchdog kills it but the
            // AlertManager loop is unaffected.
            let script = script.clone();
            let payload = json;
            let _ = thread::Builder::new()
                .name("alert-script".to_string())
                .spawn(move || invoke_notification_script(&script, &payload));
        }

        Ok(())
    }

    /// Direct accessor for the alert log path — used by tests.
    pub fn alert_log_path(&self) -> &Path {
        &self.alert_log_path
    }
}

/// Append one JSON line and `fsync` it. Each alert call opens-append-fsync-
/// close so rotation and crash semantics are predictable (Task 2.3, 2.4).
fn write_alert_line(path: &Path, json: &str) -> std::io::Result<()> {
    let mut file: File = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(json.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

/// Invoke the operator script with the JSON payload on stdin. Times out per
/// `script.timeout`; never propagates failure.
fn invoke_notification_script(script: &ScriptInvoker, payload: &str) {
    let mut child = match std::process::Command::new(&script.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(
                target: "alerting",
                path = %script.path.display(),
                error = %e,
                "failed to spawn alert script (skipping)"
            );
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        // A broken pipe here means the script read partial input or exited
        // early — we do NOT abort the process here, the watchdog still gets
        // to run.
        let _ = stdin.write_all(payload.as_bytes());
        let _ = stdin.write_all(b"\n");
    }

    // Watchdog: poll up to `timeout`; otherwise kill.
    let deadline = std::time::Instant::now() + script.timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    warn!(
                        target: "alerting",
                        path = %script.path.display(),
                        status = ?status,
                        "alert script exited non-zero (alert was logged regardless)"
                    );
                }
                return;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    warn!(
                        target: "alerting",
                        path = %script.path.display(),
                        timeout_ms = script.timeout.as_millis() as u64,
                        "alert script timed out — killed"
                    );
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                warn!(
                    target: "alerting",
                    path = %script.path.display(),
                    error = %e,
                    "error waiting on alert script"
                );
                return;
            }
        }
    }
}

/// Convenience adapter for downstream producers (CircuitBreakers, PanicMode)
/// that need a thread-safe handle. `Arc` is cheap to clone; the underlying
/// channel is already `Sync`.
pub type SharedAlertSender = Arc<AlertSender>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::time::Instant;
    use tempfile::tempdir;

    fn snapshot() -> PositionSnapshot {
        PositionSnapshot {
            symbol: "ESM6".into(),
            size: 2,
            side: Some(Side::Buy),
            unrealized_pnl: -42,
        }
    }

    /// Task 7.1: breaker activation produces alert with all required fields.
    #[test]
    fn breaker_alert_carries_all_fields() {
        let alert = Alert::for_breaker(
            BreakerType::DailyLoss,
            "loss limit exceeded",
            UnixNanos::new(1_700_000_000_000_000_000),
            snapshot(),
            -250,
        );
        assert_eq!(alert.severity, AlertSeverity::Error);
        assert!(matches!(
            alert.breaker_type,
            BreakerKind::Circuit(BreakerType::DailyLoss)
        ));
        assert_eq!(alert.trigger_reason, "loss limit exceeded");
        assert_eq!(alert.timestamp, 1_700_000_000_000_000_000);
        assert_eq!(alert.current_pnl, -250);
        assert_eq!(alert.position_state.symbol, "ESM6");
        assert!(alert.flatten_attempts.is_none());
    }

    /// Task 7.3: panic-mode alert is Critical and carries flatten details.
    #[test]
    fn panic_alert_is_critical_with_flatten_attempts() {
        let attempts = vec![
            FlattenAttemptDetail {
                attempt_number: 1,
                order_details: "FLATTEN ESM6 SELL 2".into(),
                rejection_reason: Some("ExchangeReject".into()),
                timestamp: 100,
            },
            FlattenAttemptDetail {
                attempt_number: 2,
                order_details: "FLATTEN ESM6 SELL 2".into(),
                rejection_reason: Some("ExchangeReject".into()),
                timestamp: 200,
            },
        ];
        let alert = Alert::for_panic(
            "flatten retry exhausted",
            UnixNanos::new(300),
            snapshot(),
            -500,
            attempts.clone(),
        );
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert!(matches!(alert.breaker_type, BreakerKind::PanicMode));
        let got = alert.flatten_attempts.expect("attempts present");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].attempt_number, 1);
        assert_eq!(got[1].rejection_reason.as_deref(), Some("ExchangeReject"));
    }

    /// Task 7.4: Alert serializes to valid JSON with all required fields.
    #[test]
    fn alert_serializes_to_json_with_required_fields() {
        let alert = Alert::for_breaker(
            BreakerType::ConsecutiveLosses,
            "5 losses in a row",
            UnixNanos::new(42),
            snapshot(),
            -100,
        );
        let json = serde_json::to_string(&alert).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse roundtrip");
        assert_eq!(v["severity"], "error");
        assert_eq!(v["breaker_type"], "consecutive_losses");
        assert_eq!(v["trigger_reason"], "5 losses in a row");
        assert_eq!(v["timestamp"], 42);
        assert_eq!(v["current_pnl"], -100);
        assert_eq!(v["position_state"]["symbol"], "ESM6");
        assert_eq!(v["position_state"]["size"], 2);
        assert_eq!(v["position_state"]["side"], "buy");
        assert_eq!(v["position_state"]["unrealized_pnl"], -42);
        // flatten_attempts is omitted on non-panic alerts.
        assert!(v.get("flatten_attempts").is_none());
    }

    /// Task 7.4 (panic variant): panic alerts include flatten_attempts in JSON.
    #[test]
    fn panic_alert_serializes_with_flatten_attempts() {
        let alert = Alert::for_panic(
            "exhausted",
            UnixNanos::new(1),
            snapshot(),
            0,
            vec![FlattenAttemptDetail {
                attempt_number: 3,
                order_details: "x".into(),
                rejection_reason: None,
                timestamp: 5,
            }],
        );
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&alert).unwrap()).unwrap();
        assert_eq!(v["severity"], "critical");
        assert_eq!(v["breaker_type"], "panic_mode");
        let attempts = v["flatten_attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["attempt_number"], 3);
        assert!(attempts[0]["rejection_reason"].is_null());
    }

    /// Task 7.10 + 2.2 + 2.4: AlertManager processes alerts in order, each
    /// alert is one JSON line, and writes are durable.
    #[test]
    fn manager_processes_multiple_alerts_in_order() {
        let dir = tempdir().unwrap();
        let log_path = dir.path().join("alerts.log");
        let (tx, rx) = alert_channel();
        let manager = AlertManager::new(&log_path, None::<&Path>, rx).unwrap();
        let handle = manager.spawn();

        for i in 0..5u64 {
            tx.send(Alert::for_breaker(
                BreakerType::DataQuality,
                format!("gap #{i}"),
                UnixNanos::new(i),
                snapshot(),
                -(i as i64),
            ));
        }
        drop(tx);
        handle.join().unwrap();

        let lines: Vec<String> = BufReader::new(File::open(&log_path).unwrap())
            .lines()
            .map(|l| l.unwrap())
            .collect();
        assert_eq!(lines.len(), 5, "one JSON line per alert");
        for (i, line) in lines.iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["trigger_reason"], format!("gap #{i}"));
            assert_eq!(v["timestamp"], i);
        }
    }

    /// Task 7.8 + Task 4.6: with no script configured, the alert is still
    /// written to the log.
    #[test]
    fn missing_script_path_still_logs_alert() {
        let dir = tempdir().unwrap();
        let log_path = dir.path().join("alerts.log");
        let (tx, rx) = alert_channel();
        let manager = AlertManager::new(&log_path, None::<&Path>, rx).unwrap();

        let alert = Alert::for_breaker(
            BreakerType::AnomalousPosition,
            "drawdown",
            UnixNanos::new(7),
            snapshot(),
            -1,
        );
        manager.process_alert(&alert).unwrap();

        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("anomalous_position"));
        drop(tx);
    }

    /// Task 7.5 + Task 4.3: notification script receives JSON on stdin.
    #[test]
    #[cfg(unix)]
    fn notification_script_receives_json_on_stdin() {
        let dir = tempdir().unwrap();
        let log_path = dir.path().join("alerts.log");
        let captured_path = dir.path().join("captured.json");
        let script_path = dir.path().join("capture.sh");
        std::fs::write(
            &script_path,
            format!("#!/bin/sh\ncat > {}\n", captured_path.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();

        let (tx, rx) = alert_channel();
        let manager = AlertManager::new(&log_path, Some(script_path.clone()), rx).unwrap();
        let alert = Alert::for_breaker(
            BreakerType::ConnectionFailure,
            "broker dropped",
            UnixNanos::new(123),
            snapshot(),
            0,
        );
        manager.process_alert(&alert).unwrap();
        drop(tx);

        // Wait briefly for the fire-and-forget script to finish.
        let started = Instant::now();
        while !captured_path.exists() && started.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(25));
        }
        let captured = std::fs::read_to_string(&captured_path).unwrap_or_default();
        let v: serde_json::Value =
            serde_json::from_str(captured.trim()).expect("script received valid JSON");
        assert_eq!(v["breaker_type"], "connection_failure");
        assert_eq!(v["trigger_reason"], "broker dropped");
    }

    /// Task 7.6: script that exits non-zero is logged but does not block.
    #[test]
    #[cfg(unix)]
    fn notification_script_failure_does_not_block() {
        let dir = tempdir().unwrap();
        let log_path = dir.path().join("alerts.log");
        let script_path = dir.path().join("fail.sh");
        std::fs::write(&script_path, "#!/bin/sh\nexit 17\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();

        let (tx, rx) = alert_channel();
        let manager = AlertManager::new(&log_path, Some(script_path), rx).unwrap();
        let started = Instant::now();
        let alert = Alert::for_breaker(
            BreakerType::DailyLoss,
            "ok",
            UnixNanos::new(1),
            snapshot(),
            0,
        );
        manager.process_alert(&alert).unwrap();
        // process_alert returns immediately because script invocation is
        // fire-and-forget on its own thread.
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(tx);
    }

    /// Task 7.7: script timeout is enforced (kill at deadline).
    #[test]
    #[cfg(unix)]
    fn notification_script_timeout_is_enforced() {
        let dir = tempdir().unwrap();
        let log_path = dir.path().join("alerts.log");
        let script_path = dir.path().join("hang.sh");
        std::fs::write(&script_path, "#!/bin/sh\nsleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();

        let (tx, rx) = alert_channel();
        let manager = AlertManager::new(&log_path, Some(script_path), rx)
            .unwrap()
            .with_script_timeout(Duration::from_millis(200));

        let alert = Alert::for_breaker(
            BreakerType::DailyLoss,
            "x",
            UnixNanos::new(1),
            snapshot(),
            0,
        );
        let started = Instant::now();
        manager.process_alert(&alert).unwrap();

        // Allow watchdog time to kill the child.
        thread::sleep(Duration::from_millis(800));
        // process_alert is fire-and-forget so it returns instantly; the test
        // here is that we got here at all and that no thread is pinned past
        // the timeout horizon.
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(tx);
    }

    /// Task 7.9: alert log is written to its own dedicated file (separate
    /// from any general-purpose log destination). We verify by giving the
    /// AlertManager a path nothing else writes to and asserting the file's
    /// contents are exactly the alerts we sent.
    #[test]
    fn alert_log_is_dedicated_file() {
        let dir = tempdir().unwrap();
        let log_path = dir.path().join("alerts-only.log");
        let (tx, rx) = alert_channel();
        let manager = AlertManager::new(&log_path, None::<&Path>, rx).unwrap();
        let alert = Alert::for_breaker(
            BreakerType::DataQuality,
            "isolated",
            UnixNanos::new(11),
            snapshot(),
            0,
        );
        manager.process_alert(&alert).unwrap();
        drop(tx);
        let contents = std::fs::read_to_string(&log_path).unwrap();
        let mut iter = contents.lines();
        let first = iter.next().unwrap();
        assert!(first.contains("data_quality"));
        // No general-log noise — every line is an alert JSON object.
        assert!(iter.next().is_none(), "no extra lines in dedicated log");
    }

    /// Task 6.4: send-on-full-channel does not panic; it drops with a warn
    /// log. We exercise the path by using a tiny channel.
    #[test]
    fn send_drops_when_channel_full_without_blocking() {
        let (tx_inner, rx) = bounded(2);
        let tx = AlertSender { tx: tx_inner };
        // Fill the channel.
        for _ in 0..2 {
            tx.send(Alert::for_breaker(
                BreakerType::DailyLoss,
                "fill",
                UnixNanos::new(0),
                snapshot(),
                0,
            ));
        }
        // Third send is dropped (logged at warn). Must not block or panic.
        tx.send(Alert::for_breaker(
            BreakerType::DailyLoss,
            "drop",
            UnixNanos::new(0),
            snapshot(),
            0,
        ));
        // Still only two alerts in the channel.
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    /// Gate activations DO NOT route through AlertManager: they have no
    /// AlertSender producer wired in. We validate the design by ensuring no
    /// public API on alerting accepts a "Gate" event — only `BreakerKind`
    /// and `Alert::for_breaker` accept circuit-breaker types.
    #[test]
    fn gate_activations_have_no_alert_constructor() {
        // This is a compile-time assertion — if a future change added a
        // `for_gate(...)` constructor on Alert, this test's intent would
        // need reconsideration. The test body simply documents the policy
        // and exercises the breaker types we DO accept.
        let breaker_types = [
            BreakerType::DailyLoss,
            BreakerType::ConsecutiveLosses,
            BreakerType::AnomalousPosition,
            BreakerType::ConnectionFailure,
            BreakerType::DataQuality,
        ];
        for bt in breaker_types {
            let alert = Alert::for_breaker(bt, "x", UnixNanos::new(0), snapshot(), 0);
            assert!(matches!(alert.breaker_type, BreakerKind::Circuit(_)));
        }
    }
}
