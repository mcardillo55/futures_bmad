//! Startup-time config + broker-mode selection (Story 7.3).
//!
//! The full config loader lands in Epic 8; this module covers ONLY what
//! Story 7.3 needs: pull a [`futures_bmad_core::BrokerMode`] out of a
//! TOML file at the `[broker] mode = "..."` key. That single value drives
//! the adapter-injection decision in `main.rs` (Story 7.3 Task 2.1).
//!
//! This is the ONLY place [`BrokerMode`] is inspected — every downstream
//! component receives the trait object via dependency injection, never the
//! raw enum (Story 7.3 Task 2.5).

use std::fs;
use std::path::Path;

use futures_bmad_core::BrokerMode;
use serde::Deserialize;
use tracing::warn;

/// Subset of the engine config TOML schema relevant to Story 7.3.
///
/// Only the `[broker] mode = "..."` key is required by this story; the
/// remaining fields will be added by Epic 8 (`8-1-configuration-loading`).
/// `serde(deny_unknown_fields)` is intentionally NOT set so adding more
/// keys later does not break this loader.
#[derive(Debug, Clone, Deserialize)]
pub struct StartupConfig {
    #[serde(default)]
    pub broker: BrokerSection,
}

/// `[broker]` section of the engine config.
///
/// `mode` defaults to [`BrokerMode::Live`] (fail-closed) when omitted so a
/// brand-new config that forgot the key does NOT silently drop into
/// paper. Live mode itself is independently gated by the `--live` CLI
/// flag, so the default cannot route real orders by accident.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BrokerSection {
    #[serde(default)]
    pub mode: BrokerMode,
    /// Other Story 7.3-irrelevant fields (Rithmic-side credential pointers
    /// added by Epic 8) are tolerated via the absence of
    /// `deny_unknown_fields`. Future stories may upgrade this struct.
    #[serde(default)]
    pub server: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StartupConfigError {
    #[error("config file not found: {0}")]
    NotFound(std::path::PathBuf),
    #[error("io error reading {0}: {1}")]
    Io(std::path::PathBuf, std::io::Error),
    #[error("toml parse error in {0}: {1}")]
    Parse(std::path::PathBuf, String),
}

/// Load and parse a TOML config file.
///
/// Errors surface the file path so operators see exactly which config
/// failed to parse.
pub fn load_config(path: &Path) -> Result<StartupConfig, StartupConfigError> {
    if !path.exists() {
        return Err(StartupConfigError::NotFound(path.to_path_buf()));
    }
    let text =
        fs::read_to_string(path).map_err(|e| StartupConfigError::Io(path.to_path_buf(), e))?;
    let cfg: StartupConfig = toml::from_str(&text)
        .map_err(|e| StartupConfigError::Parse(path.to_path_buf(), e.to_string()))?;
    Ok(cfg)
}

/// Story 7.3 Task 6.2 — non-fatal credentials sanity check.
///
/// In paper mode we still need Rithmic credentials for *market data* (the
/// TickerPlant is the data source even in paper mode), so the presence of
/// any Rithmic env vars is NOT an error. We only warn when both
/// `mode = "paper"` AND `RITHMIC_ACCOUNT_ID` is set, because that
/// combination implies an operator wired up an account ID intending to
/// route orders, then accidentally flipped to paper.
///
/// The check reads env vars directly so it surfaces the operator's actual
/// runtime environment, not whatever config snapshot was loaded.
pub fn sanity_check_paper_credentials(mode: BrokerMode) {
    if mode != BrokerMode::Paper {
        return;
    }
    if std::env::var("RITHMIC_ACCOUNT_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some()
    {
        warn!(
            target: "paper",
            "RITHMIC_ACCOUNT_ID is set but mode = paper — orders will NOT route to this account. \
             If you intended to trade live, set [broker] mode = \"live\" in your config."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Story 7.3 Task 7.1 (mirror) — config loader correctly parses
    /// `mode = "paper"`.
    #[test]
    fn loads_paper_mode_from_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("paper.toml");
        std::fs::write(
            &path,
            r#"[broker]
mode = "paper"
"#,
        )
        .unwrap();
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.broker.mode, BrokerMode::Paper);
    }

    #[test]
    fn loads_live_mode_from_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.toml");
        std::fs::write(
            &path,
            r#"[broker]
mode = "live"
"#,
        )
        .unwrap();
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.broker.mode, BrokerMode::Live);
    }

    /// Default mode is live — an operator who forgot to set `mode` does
    /// NOT silently drop into paper. (Live mode itself is gated by the
    /// `--live` CLI flag, so this default still cannot route real orders
    /// by accident.)
    #[test]
    fn default_mode_is_live_when_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.toml");
        std::fs::write(&path, "[broker]\n").unwrap();
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.broker.mode, BrokerMode::Live);
    }

    /// Missing file produces a typed error with the path embedded —
    /// keeps operator-facing error messages precise.
    #[test]
    fn missing_file_returns_typed_error() {
        let path = std::path::PathBuf::from("/nonexistent/__never_exists__.toml");
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, StartupConfigError::NotFound(_)));
    }

    /// Malformed TOML surfaces as `Parse` with the parser's message.
    #[test]
    fn malformed_toml_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.toml");
        std::fs::write(
            &path,
            r#"[broker
mode "paper"
"#,
        )
        .unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, StartupConfigError::Parse(_, _)));
    }

    /// Repository-shipped `config/paper.toml` parses to `BrokerMode::Paper`
    /// — guards against an accidental edit that breaks the production
    /// config without anyone noticing until first deploy.
    #[test]
    fn shipped_paper_config_parses_to_paper_mode() {
        let workspace_root = workspace_root();
        let path = workspace_root.join("config").join("paper.toml");
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.broker.mode, BrokerMode::Paper);
    }

    #[test]
    fn shipped_live_config_parses_to_live_mode() {
        let workspace_root = workspace_root();
        let path = workspace_root.join("config").join("live.toml");
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.broker.mode, BrokerMode::Live);
    }

    fn workspace_root() -> std::path::PathBuf {
        // CARGO_MANIFEST_DIR points at crates/engine; walk up two levels.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest.parent().unwrap().parent().unwrap().to_path_buf()
    }
}
