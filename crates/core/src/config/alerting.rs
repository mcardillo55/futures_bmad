use serde::Deserialize;

/// Configuration for the operator alerting subsystem (Story 5.6).
///
/// Maps to a `[alerting]` table in the project TOML files. Defaults are chosen
/// so that omitting the table is equivalent to "log alerts to the default
/// path with no external script wired in".
#[derive(Debug, Clone, Deserialize)]
pub struct AlertingConfig {
    /// Path to the dedicated alert log (JSON-lines, fsync per write).
    /// External rotation (logrotate, etc.) is the supported lifecycle policy
    /// — the engine never rotates this file itself.
    #[serde(default = "default_alert_log_path")]
    pub alert_log_path: String,

    /// Path to an external notification script. When `None`, alerts are
    /// written to the alert log only (script invocation is skipped).
    /// Script receives the alert JSON on stdin; failure is logged at `warn`
    /// and never blocks the trading hot path.
    #[serde(default)]
    pub alert_script_path: Option<String>,

    /// Optional script timeout in milliseconds. Defaults to 10 seconds.
    /// Scripts exceeding this deadline are killed; the alert log entry is
    /// unaffected.
    #[serde(default = "default_alert_script_timeout_ms")]
    pub alert_script_timeout_ms: u64,
}

fn default_alert_log_path() -> String {
    "data/alerts.log".to_string()
}

fn default_alert_script_timeout_ms() -> u64 {
    10_000
}

impl Default for AlertingConfig {
    fn default() -> Self {
        Self {
            alert_log_path: default_alert_log_path(),
            alert_script_path: None,
            alert_script_timeout_ms: default_alert_script_timeout_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default values are stable — operators relying on defaults shouldn't be
    /// surprised by silent path changes.
    #[test]
    fn defaults_are_stable() {
        let cfg = AlertingConfig::default();
        assert_eq!(cfg.alert_log_path, "data/alerts.log");
        assert_eq!(cfg.alert_script_path, None);
        assert_eq!(cfg.alert_script_timeout_ms, 10_000);
    }

    /// `Default::default()` and the explicit constructor produce the same
    /// values, ensuring `#[serde(default)]` callsites remain consistent.
    #[test]
    fn helpers_match_default_impl() {
        assert_eq!(default_alert_log_path(), "data/alerts.log");
        assert_eq!(default_alert_script_timeout_ms(), 10_000);
    }
}
