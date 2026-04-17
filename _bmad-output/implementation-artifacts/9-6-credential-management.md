# Story 9.6: Credential Management

Status: ready-for-dev

## Story

As a trader-operator,
I want broker credentials handled securely,
So that API keys are never exposed in config files, logs, or source control.

## Acceptance Criteria (BDD)

- Given broker API credentials When loaded Then from env vars only (NFR11), wrapped in secrecy::SecretString, .env supported for local dev (gitignored), .env.example documents vars without values
- Given credentials in memory When any logging occurs Then SecretString Debug/Display emit [REDACTED], never in structured logs, errors, or metrics
- Given connection to Rithmic When credentials used Then passed to rithmic-rs connection, TLS (NFR14), zeroized from memory when no longer needed (secrecy default)

## Tasks / Subtasks

### Task 1: Add secrecy and dotenvy dependencies (AC: SecretString wrapping, .env support)
- 1.1: Ensure workspace `Cargo.toml` has `secrecy = "0.10.3"` with `serde` feature enabled in `[workspace.dependencies]`
- 1.2: Add `dotenvy = "0.15"` to workspace and engine Cargo.toml for .env file loading
- 1.3: Add `secrecy` to engine Cargo.toml dependencies

### Task 2: Define credential types with SecretString (AC: wrapped in SecretString, Debug/Display emit [REDACTED])
- 2.1: In `engine/src/config/` (or `broker/src/`), define `BrokerCredentials` struct with fields: `username` (String), `password` (SecretString), `api_key` (SecretString), `api_secret` (SecretString)
- 2.2: Implement `Debug` for `BrokerCredentials` manually: display username, emit `[REDACTED]` for all SecretString fields
- 2.3: Implement `Display` for `BrokerCredentials`: show only username and connection endpoint, never secrets
- 2.4: Do NOT derive `Serialize` for `BrokerCredentials` — credentials must never be serializable to prevent accidental logging/export
- 2.5: SecretString already implements `Zeroize` on drop (secrecy 0.10.3 default) — document this in code comments

### Task 3: Implement credential loading from environment variables (AC: from env vars only, NFR11)
- 3.1: Define required env var names as constants: `RITHMIC_USERNAME`, `RITHMIC_PASSWORD`, `RITHMIC_API_KEY`, `RITHMIC_API_SECRET`
- 3.2: Implement `BrokerCredentials::from_env() -> Result<Self, CredentialError>` that reads from env vars
- 3.3: Wrap password and API key/secret values in `SecretString::from(value)` immediately after reading from env
- 3.4: Return clear error messages if required env vars are missing: `CredentialError::MissingVar(var_name)` — do NOT include the value in the error
- 3.5: Call `dotenvy::dotenv().ok()` in `main.rs` before credential loading to support .env files in development

### Task 4: Create .env.example and ensure .env is gitignored (AC: .env.example documents vars, .env gitignored)
- 4.1: Create `.env.example` at project root with all required vars documented but no values:
  ```
  # Rithmic broker credentials
  RITHMIC_USERNAME=
  RITHMIC_PASSWORD=
  RITHMIC_API_KEY=
  RITHMIC_API_SECRET=
  ```
- 4.2: Verify `.gitignore` includes `.env` (but NOT `.env.example`)
- 4.3: Add comment in `.env.example` explaining: "Copy to .env and fill in values. Never commit .env"

### Task 5: Wire credentials into broker connection (AC: passed to rithmic-rs, TLS, zeroized)
- 5.1: In `broker/src/adapter.rs` (or `connection.rs`), accept `BrokerCredentials` in `RithmicAdapter::new()` or `connect()`
- 5.2: Extract secret values using `secret.expose_secret()` only at the point of passing to rithmic-rs connection
- 5.3: Ensure rithmic-rs connection is configured with TLS enabled (NFR14) — reject non-TLS configurations
- 5.4: After connection is established, `BrokerCredentials` can be dropped — SecretString zeroizes memory on drop
- 5.5: If reconnection is needed, credentials must be retained in the adapter (store `BrokerCredentials` in the adapter struct, zeroized only on adapter drop)

### Task 6: Verify credentials never leak into logs, errors, or metrics (AC: never in structured logs, errors, metrics)
- 6.1: Audit all `tracing::info!`, `tracing::error!`, `tracing::debug!` calls in broker crate — ensure none log credential fields
- 6.2: In `BrokerError` variants that include connection details, ensure only endpoint/host info is included, never credentials
- 6.3: Verify that even if `BrokerCredentials` is accidentally passed to a `tracing::info!("{:?}", creds)` call, it emits `[REDACTED]` (guaranteed by custom Debug impl)
- 6.4: In metrics (Story 9.4), verify no credential-related data appears in any metric label or value
- 6.5: Add `#[deny(clippy::print_stderr, clippy::print_stdout)]` to broker crate to prevent accidental println of credentials

### Task 7: Define CredentialError type (AC: clear error messages without exposing values)
- 7.1: Define `CredentialError` enum: `MissingVar(String)` (var name, not value), `InvalidFormat(String)` (var name), `TlsRequired` (connection attempted without TLS)
- 7.2: Implement `Display` for `CredentialError` with helpful messages: "Missing required environment variable: RITHMIC_PASSWORD"
- 7.3: Implement `std::error::Error` for `CredentialError` (or use `thiserror`)
- 7.4: Ensure error messages guide the operator: reference `.env.example` in missing var errors

### Task 8: Unit and integration tests (AC: all)
- 8.1: Test `BrokerCredentials::from_env()` with all vars set — returns Ok with correct values
- 8.2: Test `BrokerCredentials::from_env()` with missing var — returns `CredentialError::MissingVar` with var name
- 8.3: Test `Debug` impl for `BrokerCredentials` — output contains `[REDACTED]`, never contains actual secret values
- 8.4: Test `Display` impl for `BrokerCredentials` — output contains only username, no secrets
- 8.5: Test `SecretString` zeroization: after dropping credentials, verify (best-effort) memory is zeroed
- 8.6: Test that serialization is not possible (no Serialize derive) — compile-time guarantee
- 8.7: Test `.env.example` file exists and contains all required var names

## Dev Notes

### Architecture Patterns & Constraints
- NFR11: No plaintext credentials in config files, logs, or source control. Environment variables are the ONLY source
- NFR14: All broker connections must use TLS. The credential loading code should enforce this
- secrecy 0.10.3 provides: `SecretString` (wraps String), `ExposeSecret` trait (explicit opt-in to read value), `Zeroize` on drop (memory zeroing)
- `expose_secret()` should be called at exactly one point: when passing credentials to rithmic-rs. Nowhere else
- `.env` file is for LOCAL DEVELOPMENT ONLY — production uses systemd environment directives or similar
- The `BrokerCredentials` struct lives in the engine crate (not core) because core should not have secrecy as a dependency
- Reconnection requires keeping credentials alive in the adapter — they are NOT zeroized after first connection

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── main.rs                 # dotenvy::dotenv().ok(), BrokerCredentials::from_env()
│   └── config/
│       └── loader.rs           # Credential loading, env var constants
crates/broker/
├── src/
│   ├── adapter.rs              # Accept BrokerCredentials, expose_secret() for connection
│   └── connection.rs           # TLS enforcement, credential usage point
.env.example                    # Committed — documents required env vars
.env                            # Gitignored — local dev credentials
.gitignore                      # Must include .env
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Sections: Security (NFR11-14), credential management gap
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 9, Story 9.6
- Dependencies: secrecy 0.10.3, dotenvy 0.15, thiserror 2.0.18
- NFR11: No plaintext credentials
- NFR14: Encrypted channels only (TLS)

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
