# Story 2.1: Rithmic Connection & Market Data Subscription

Status: ready-for-dev

## Story

As a trader-operator,
I want the system to connect to Rithmic and receive live market data,
So that I have real-time L1/L2 feeds for configured CME futures contracts.

## Acceptance Criteria (BDD)

- Given valid Rithmic credentials in env vars and broker config in TOML When RithmicAdapter initializes Then it establishes WebSocket connection to Rithmic TickerPlant, credentials via `secrecy` crate, TLS encrypted
- Given a live connection When adapter subscribes to configured symbols Then it receives real-time L1/L2 updates, protobuf deserialized via `prost`
- Given incoming messages When `message_validator` processes each Then well-formed messages produce MarketEvent, malformed messages are logged/skipped/counted in 60s sliding window, >10 malformed in 60s triggers circuit-break signal
- Given connection failure When adapter cannot connect Then returns `BrokerError::ConnectionLost` with diagnostic context, no partial state

## Tasks / Subtasks

### Task 1: Define BrokerAdapter trait and error types in core (AC: connection, error handling)
- 1.1: Define `BrokerAdapter` trait in `core/src/broker.rs` with async `connect()`, `subscribe()`, `disconnect()` methods
- 1.2: Define `BrokerError` enum in `core/src/error.rs` with variants: `ConnectionLost`, `AuthenticationFailed`, `SubscriptionFailed`, `ProtocolError`, each carrying diagnostic context string
- 1.3: Define `MarketEvent` struct in `core/src/market.rs` with fields for timestamp (i64 nanos), symbol_id, price (i64 quarter-ticks), size (u32), side, event_type

### Task 2: Implement Rithmic connection management (AC: WebSocket, TLS, credentials)
- 2.1: Create `broker/src/connection.rs` with `RithmicConnection` struct holding `tokio_tungstenite::WebSocketStream`
- 2.2: Implement `connect()` that reads credentials from env vars into `secrecy::SecretString`, establishes TLS WebSocket to Rithmic TickerPlant endpoint
- 2.3: Implement connection handshake using rithmic-rs protocol (login request/response via protobuf)
- 2.4: Ensure credentials are never logged — use `secrecy::ExposeSecret` only at the TLS handshake boundary
- 2.5: On failure, return `BrokerError::ConnectionLost` with endpoint and error detail, tear down any partial state

### Task 3: Implement RithmicAdapter (AC: BrokerAdapter trait, subscription)
- 3.1: Create `broker/src/adapter.rs` with `RithmicAdapter` struct implementing `BrokerAdapter` trait
- 3.2: Implement `subscribe()` to send Rithmic subscription requests for configured symbols from TOML config
- 3.3: Implement receive loop that reads WebSocket frames, deserializes protobuf via `prost`, yields raw Rithmic messages
- 3.4: Create `broker/src/messages.rs` with internal Rithmic message types, keeping all R|Protocol details isolated in broker crate

### Task 4: Implement message validation (AC: well-formed/malformed, sliding window, circuit break)
- 4.1: Create `broker/src/message_validator.rs` with `MessageValidator` struct
- 4.2: Implement `validate()` that converts well-formed Rithmic messages to `MarketEvent`, returns `Result<MarketEvent, ValidationError>`
- 4.3: Implement 60-second sliding window counter for malformed messages using a `VecDeque<Instant>` of malformed timestamps
- 4.4: When malformed count exceeds 10 in 60s window, emit circuit-break signal (return specific error variant or set flag)
- 4.5: Log every malformed message at `warn` level with message details (excluding any sensitive data)

### Task 5: Implement market data stream assembly (AC: L1/L2 updates)
- 5.1: Create `broker/src/market_data.rs` with `MarketDataStream` that wraps RithmicAdapter and MessageValidator
- 5.2: Implement `async fn next_event(&mut self) -> Result<MarketEvent, BrokerError>` that reads, validates, and returns events
- 5.3: Wire L1 (best bid/ask) and L2 (depth) update parsing from protobuf into MarketEvent fields

### Task 6: Unit tests (AC: all)
- 6.1: Test connection error returns `BrokerError::ConnectionLost` with context
- 6.2: Test message validator accepts well-formed protobuf, rejects malformed
- 6.3: Test sliding window counter triggers circuit break after 11th malformed in 60s
- 6.4: Test sliding window counter does NOT trigger when malformed messages are spread beyond 60s
- 6.5: Test credentials are `SecretString` and Debug impl does not leak values

## Dev Notes

### Architecture Patterns & Constraints
- All Rithmic/R|Protocol details are isolated in the broker crate — core and engine never see Rithmic types
- Credentials flow: env vars -> `secrecy::SecretString` -> exposed only at TLS handshake point -> never in logs
- The adapter runs on a Tokio async runtime (I/O thread), producing events that will be sent to the engine hot path via SPSC (Story 2.2)
- Connection management should be stateless on failure — if connect fails, no cleanup is needed
- protobuf deserialization uses `prost` (not protobuf-rs) to match workspace dependency

### Project Structure Notes
```
crates/broker/
├── src/
│   ├── lib.rs
│   ├── adapter.rs          (RithmicAdapter: BrokerAdapter impl)
│   ├── connection.rs       (WebSocket/TLS connection management)
│   ├── market_data.rs      (MarketDataStream assembly)
│   ├── message_validator.rs (validation + sliding window)
│   └── messages.rs         (internal Rithmic message types)
crates/core/
├── src/
│   ├── broker.rs           (BrokerAdapter trait)
│   ├── market.rs           (MarketEvent struct)
│   └── error.rs            (BrokerError enum)
```

### References
- Architecture document: `docs/architecture.md` — Section: Broker Adapter, Market Data Pipeline
- Epics document: `docs/epics.md` — Epic 2, Story 2.1
- Dependencies: rithmic-rs =0.7.2, tokio 1.51.1, tokio-tungstenite 0.29.0, prost 0.14.3, secrecy 0.10.3, tracing 0.1.44, thiserror 2.0.18

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
