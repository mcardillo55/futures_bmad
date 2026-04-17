---
stepsCompleted: ['step-01-init', 'step-02-discovery', 'step-02b-vision', 'step-02c-executive-summary', 'step-03-success', 'step-04-journeys', 'step-05-domain', 'step-06-innovation-skipped', 'step-07-project-type', 'step-08-scoping', 'step-09-functional', 'step-10-nonfunctional', 'step-11-polish']
inputDocuments:
  - product-brief-futures_bmad.md
  - product-brief-futures_bmad-distillate.md
  - research/domain-futures-microstructure-strategy-research-2026-04-12.md
  - research/technical-execution-and-architecture-research-2026-04-12.md
  - brainstorming/brainstorming-session-2026-04-12-001.md
documentCounts:
  briefs: 2
  research: 2
  brainstorming: 1
  projectDocs: 0
workflowType: 'prd'
classification:
  projectType: "Real-time trading engine (event-driven, latency-sensitive, bare-metal binary on VPS)"
  domain: "Fintech (algorithmic trading on CME equity index futures)"
  complexity: "High"
  riskProfile: "High (financial, operational, regulatory)"
  projectContext: "Greenfield"
  userRole: "Solo developer-operator (builder = user = risk-bearer)"
  tenancy: "Single-user, single-tenant"
  systemModes: "Dual-mode — live trading engine + historical replay research engine (+ paper/live distinction)"
  safetyArchitecture: "Embedded risk controls as first-class subsystem (watchdog, circuit breakers)"
  regulatoryEnvironment: "CME futures (CFTC, exchange rules) — constant regardless of broker"
  complianceBurden: "Variable — high with DMA (Rithmic), lower with managed broker APIs (Tradovate)"
  v1SuccessFrame: "Proof-of-concept validation engine, not P&L maximization"
---

# Product Requirements Document - futures_bmad

**Author:** Michael
**Date:** 2026-04-12

## Executive Summary

An automated trading system for CME equity index futures (ES/NQ micro and standard contracts) that applies institutional-grade trading principles — microstructure signal analysis, adaptive regime detection, and fee-aware execution gating — at personal operator scale. The system occupies an uncontested latency tier (0.5-2ms) between retail platforms (100ms+, $50-250/month) and institutional HFT infrastructure (sub-microsecond, $30K+/month), running on a $250-350/month infrastructure budget.

The core thesis: most retail algo traders fail (74-89% lose money) because they automate the wrong methodology — lagging indicators, fee-blind execution, and no regime awareness. This system is architecturally constrained to only express strategies grounded in validated microstructure research: order book imbalance, VPIN, microprice, and composite signal analysis at structural price levels. Lagging indicators are structurally excluded from entry decisions — not discouraged, but banned by design.

Built in Rust for predictable latency and memory safety, with broker-agnostic exchange connectivity (Rithmic R|Protocol as leading candidate, Tradovate as alternative). Safety architecture is a first-class subsystem: atomic bracket orders with exchange-resting stops, multi-layer circuit breakers, and an independent watchdog kill switch on a separate cloud provider. V1 is framed as a proof-of-concept validation engine — paper trading profitability must be demonstrated before any live capital deployment, starting with single MES contracts at $1,000-5,000 initial capital.

The system operates in dual mode: a live trading engine for real-time execution and a historical replay research engine that shares the same code path, enabling quantitative signal discovery and strategy validation. Every day of live trading generates new data, feeding a research flywheel that compounds the system's edge over time.

### What Makes This Special

This is not a faster way to automate retail trading — it's a fundamentally different methodology brought to personal scale:

- **Institutional principles, personal operation.** The same signal types prop firms rely on (order flow, microstructure, regime state) applied at 1/100th the infrastructure cost, by a builder with direct experience in low-latency trading infrastructure (FPGA firmware design) and prior algo trading systems (bitcoin algo trader).
- **Architectural constraints as quality filters.** Zero-lag signals, fee-aware gating (>2x total fees minimum edge), and regime detection aren't features — they're design constraints that prevent the system from doing the wrong things. The system sits out unfavorable conditions rather than trading through them.
- **Builder is the user.** Zero signal loss between market insight and implementation. No vendor generalization diluting specificity. The system expresses exactly one operator's trading thesis with no compromises for generality.

### Project Classification

| Dimension | Classification |
|---|---|
| **Project Type** | Real-time trading engine (event-driven, latency-sensitive, bare-metal binary) |
| **Domain** | Fintech — algorithmic trading on CME equity index futures |
| **Complexity** | High |
| **Risk Profile** | High (financial, operational, regulatory) |
| **Project Context** | Greenfield |
| **User Role** | Solo developer-operator (builder = user = risk-bearer) |
| **Tenancy** | Single-user, single-tenant |
| **System Modes** | Dual-mode — live trading + historical replay research (+ paper/live) |
| **Safety Architecture** | Embedded risk controls as first-class subsystem |
| **Regulatory Environment** | CME futures (CFTC, exchange rules) — constant regardless of broker |
| **Compliance Burden** | Variable — high with DMA (Rithmic), lower with managed APIs (Tradovate) |
| **V1 Success Frame** | Proof-of-concept validation engine |

## Success Criteria

### User Success

- **Primary metric:** System returns consistently exceed buy-and-hold index fund performance (S&P 500 / total market) across all meaningful timescales — weekly, monthly, quarterly, annually
- **Edge validation:** Positive expectancy over 500+ trades in paper trading before any live capital deployment
- **Regime intelligence:** System correctly identifies and sits out unfavorable market conditions (rotational/choppy regimes), avoiding the bleed that destroys most retail strategies
- **Fee survival:** Net positive returns after all costs — exchange fees, commissions, slippage, and infrastructure — not just gross profitability
- **Kill criteria:** If paper trading shows negative expectancy after 500+ trades, or live drawdown exceeds 30% of starting capital, the system halts and the thesis is re-examined

### Business Success

- **Edge proof:** Percentage returns consistently exceed passive index fund buy-and-hold, confirming a real, persistent edge
- **Capital scaling readiness:** Once edge is proven on minimal capital (single MES contracts, $1-5K), confident scaling — returns are percentage-based, so proven edge scales linearly
- **Infrastructure ROI:** Trading returns cover infrastructure costs ($250-350/month), transitioning from R&D investment to self-sustaining operation
- **Research flywheel producing:** New signals discovered through historical replay that backtest and paper trade more profitably than current signals, compounding edge over time

### Technical Success

- **System uptime:** Autonomous operation during market hours with graceful degradation under failure conditions
- **Execution quality:** Fills within modeled slippage expectations; paper-to-live performance gap within acceptable range (5-15% degradation expected)
- **Safety integrity:** Circuit breakers, watchdog, and bracket orders functioning correctly under all conditions including edge cases (network failure, exchange outage, extreme volatility)
- **Latency budget:** Tick-to-decision within strategy-required latency budget, verified through instrumentation
- **Dual-mode reliability:** Historical replay produces deterministic, reproducible results on the same data; live and replay code paths share the same logic

### Measurable Outcomes

| Metric | Target | Timeframe |
|---|---|---|
| Paper trading expectancy | Positive, after all modeled costs | 500+ trades |
| Returns vs buy-and-hold | Outperform on weekly+ timescales | Rolling basis |
| Maximum drawdown | <30% of capital | Per deployment |
| System uptime during market hours | >99.5% | Monthly |
| Paper-to-live performance gap | <15% degradation | First 3 months live |
| New signal discovery | ≥1 validated signal per quarter | Ongoing |

## User Journeys

### Journey 1: Trader-Operator — Live Trading Session (Success Path)

**Michael, Trader-Operator** — It's 8:45 AM CT, fifteen minutes before the CME equity index futures open. Michael opens a terminal on his local machine and SSH's into the Aurora-area VPS. He checks the system status — all connections healthy, watchdog heartbeat confirmed, circuit breakers armed. Yesterday's P&L summary is clean: 4 trades taken, 3 winners, net positive after fees.

He reviews the pre-market regime assessment. The system has already ingested overnight data and classified the current regime as trending based on overnight-to-open price action, VIX levels, and order book depth. The momentum strategy is enabled. He confirms the daily risk parameters — max loss $200, max consecutive losses 3, position size 2 MES contracts — and arms the system for autonomous operation.

At 9:32 AM, the system detects strong order book imbalance at a key structural level. The composite signal fires — OBI, VPIN, and microprice all aligned. Fee-aware gating confirms the expected edge exceeds 2x total round-trip costs. The system submits an atomic bracket order: 2 MES long, take-profit 8 ticks, stop-loss 4 ticks, both resting at the exchange.

The trade hits take-profit at 9:34 AM. Michael sees the fill confirmation in the log stream. He doesn't intervene — the system is doing exactly what it's designed to do.

At 11:15 AM, the regime detector identifies a shift to rotational/choppy conditions. The system disables the momentum strategy and logs the regime transition. No trades are taken for the next 90 minutes. Michael checks in over lunch, sees the system correctly sitting out, and goes back to other work.

By 3:00 PM, the session ends. 3 trades taken, 2 winners, 1 stop-loss hit. Net positive after fees. The system logs all trade data, execution quality metrics, and regime state transitions to the event journal for later research analysis.

**Requirements revealed:** System status via log tailing, pre-market regime assessment, configurable daily risk parameters, autonomous execution with real-time logging, regime-driven strategy enable/disable, end-of-day summary and data archival.

### Journey 2: Trader-Operator — Edge Case: Circuit Breaker Event

**Michael, Trader-Operator** — It's a volatile Wednesday. FOMC announcement at 2:00 PM CT. The system has been trading normally in the morning session — 2 trades, both small winners. At 1:55 PM, volatility spikes and the regime detector flags a transition to high-volatility state. The system disables all strategies and holds flat.

At 2:01 PM, the market gaps 15 points on the announcement. The system remains flat — correct behavior. But at 2:08 PM, a delayed fill notification arrives from a bracket order that was supposed to have been cancelled. The system now has an unexpected position. The circuit breaker detects the anomaly — position exists outside of any active strategy context — and immediately submits a market order to flatten.

The flatten order fills with 2 ticks of adverse slippage. The circuit breaker logs the incident with full context: original order ID, cancellation timestamp, delayed fill timestamp, flatten execution details. Michael receives an alert flagging the anomaly.

He reviews the incident log that evening. The root cause: a race condition between the cancel request and the exchange matching engine during extreme volatility. He patches the code and replays the problematic market data sequence through the historical replay engine to confirm the fix handles the edge case correctly.

**Requirements revealed:** Event-aware trading windows (scheduled events like FOMC), anomalous position detection, automated flattening on circuit breaker trigger, incident logging with full audit trail, operator alerting on circuit breaker events.

### Journey 3: Researcher — Signal Discovery and Validation

> **Note:** This journey represents the Phase 2 research workflow. MVP research uses the replay engine and external analysis tools; the Python research layer (PyO3 + Arrow) described here is a Phase 2 capability.

**Michael, Researcher** — It's Saturday morning. Michael has been thinking about whether absorption detection — large resting orders that absorb aggressive flow without moving price — could improve the composite signal. He fires up the research environment on his local machine.

He loads 3 months of historical MES tick data from the Parquet data store. Using the Python research layer (connected via PyO3), he writes a feature extractor for absorption events: identify price levels where aggressive volume exceeds 3x the rolling average but price fails to move more than 1 tick.

He runs the feature through the historical replay engine — the same code path that runs live, but fed with historical data for deterministic replay. He generates a signal that combines absorption detection with the existing OBI and microprice signals at structural levels.

Initial backtest results over 3 months: the composite signal with absorption has a higher win rate (62% vs 57%) and better risk-adjusted returns than the current signal set. He runs walk-forward validation, splitting the data into in-sample (first 2 months) and out-of-sample (last month). The out-of-sample results hold: 59% win rate, positive expectancy after fees.

He then runs the signal through paper trading for 2 weeks to validate against live market conditions. After 120 paper trades, the signal confirms — better than the existing composite. He promotes the new signal configuration to the live system.

**Requirements revealed:** Historical data ingestion and Parquet storage, Python research layer (Phase 2), historical replay engine sharing live code path, walk-forward validation, paper trading validation mode, signal configuration promotion workflow (research → paper → live).

### Journey 4: Risk Manager — Watchdog Activation and System Recovery

**Michael, Risk Manager** — It's 10:30 AM CT on a Thursday. Michael is away from his desk. The primary trading system on the Aurora VPS experiences an unrecoverable error — a segfault in the market data processing path causes the main process to crash. Systemd attempts a restart, but the process crashes again on the same corrupted state.

The independent watchdog, running on a separate AWS instance, detects that it has missed 3 consecutive heartbeats from the primary system (30-second threshold). The watchdog activates its kill protocol: it connects directly to the broker API and submits market orders to flatten all open positions. Two MES positions are closed at market with 1 tick of slippage each.

The watchdog sends an alert to Michael's phone: "WATCHDOG ACTIVATED — Primary system unresponsive. 2 positions flattened. Broker connection healthy. System halted."

Michael SSH's into the VPS an hour later. He reviews the crash logs — the segfault was caused by an unexpected message format from the broker's ticker plant during a market data burst. He patches the code and replays the problematic market data sequence through the historical replay engine to confirm the fix.

**Requirements revealed:** Independent watchdog on separate cloud provider, heartbeat monitoring with configurable threshold, autonomous position flattening via broker API, operator alerting, crash logging with full diagnostic context, replay of specific market data sequences for debugging.

### Journey Requirements Summary

| Capability Area | Journeys |
|---|---|
| **Broker connectivity & order execution** | All |
| **Regime detection & strategy management** | J1, J2 |
| **Circuit breaker system** | J2, J4 |
| **Independent watchdog** | J4 |
| **Operator alerting** | J2, J4 |
| **Historical replay engine** | J3, J4 |
| **Python research layer** | J3 (Phase 2) |
| **Data ingestion & storage** | J1, J3 |
| **Fee-aware trade gating** | J1 |
| **Logging & event journal** | All |
| **Signal configuration management** | J3 |
| **System health monitoring** | J1, J4 |

## Domain-Specific Requirements

### Compliance & Regulatory

- **CFTC/NFA:** No registration required for personal automated trading (Reg AT withdrawn June 2020). System operates under personal trading exemption.
- **Anti-spoofing (CME Rule 575, CEA Section 4c(a)(5)):** Every order must represent bona fide intent to trade. No phantom orders, layered bids/offers, or patterns interpretable as spoofing. The 2025 Flatiron Futures precedent ($200K penalty + 12-month ban for small-firm ES/NQ spoofing) makes this a hard constraint.
- **CME Messaging Efficiency Program:** Monitor and stay within message rate benchmarks. Cancel-to-fill ratios consistently >90% trigger surveillance — relevant if limit orders are ever introduced.
- **Tax reporting:** Section 1256 contracts — 60/40 tax treatment (~26.8% blended rate vs 37% ordinary income). System logs trade data suitable for tax reporting (realized P&L per contract, per day).
- **Rithmic conformance test:** If Rithmic is selected, the application must pass conformance testing (2-4 weeks) covering connection management, reconnection recovery, order state transitions, position resync, and risk management integration.

### Technical Constraints

- **Order execution strategy:** Prefer market orders over limit orders to avoid order state management complexity and slippage risk from partial fills, stale orders, and cancel/replace race conditions. Limit orders introduce significant state management burden (learned from prior algo trading experience).
- **Data retention:** Trade logs and order history retained indefinitely (low storage, valuable for performance analysis and taxes). Market data (tick/L2) retained for years as free backtesting data (Parquet compression, tiered storage).
- **Determinism:** Historical replay must produce identical results on identical data. Live and replay code paths share the same logic.

### Integration Requirements

- **Broker connectivity:** Broker-agnostic abstraction layer. Initial implementation against one provider (Rithmic R|Protocol via rithmic-rs or Tradovate REST/WebSocket). Clean interface boundary so broker can be swapped without touching strategy logic.
- **Broker API terms of service:** **[OPEN RESEARCH ITEM]** — Usage limits, rate limits, data redistribution restrictions, and ongoing compliance obligations for each candidate broker API need to be researched before implementation.
- **Market data feeds:** Live data via broker connection. Historical data via Databento (pay-per-byte, Parquet format). Both sources through a unified data model.
- **Watchdog integration:** Independent process on separate cloud provider (AWS) with its own broker API credentials for autonomous position flattening.

### Risk Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Anti-spoofing violation | $200K+ penalty, trading ban | Bona fide intent on every order; monitor cancel-to-fill ratios; prefer market orders; log all order intent |
| CME messaging surcharge | $1,000/day surcharge | Rate limiting on order submission; monitor message counts vs benchmarks |
| Broker API breaking changes | System downtime, missed trades | Abstraction layer isolates strategy from broker; conformance test catches issues early |
| Market data gaps | Missed signals, stale state | Detect and log data gaps; halt trading if data quality degrades below threshold |
| Order state corruption | Orphaned positions, unexpected exposure | Robust order state machine; position reconciliation on reconnect; circuit breaker on state mismatch |
| Multi-year data storage costs | Growing infrastructure burden | Parquet compression (~10:1 on tick data); tiered storage (hot/warm/cold); periodic cost review |

## Trading Engine Specific Requirements

### Project-Type Overview

Single-binary, event-driven trading engine deployed directly on a Linux VPS (no containerization) with systemd process management. No GUI — operator interaction is via SSH, log tailing, and configuration files. Architecture prioritizes latency predictability, operational safety, and deterministic behavior.

### Technical Architecture

- **Deployment:** Binary runs directly on Aurora-area Linux VPS via systemd. No Docker — avoids container networking overhead.
- **Upgrades:** No deployments during market hours unless critical and all positions are flat. Deploy via git pull, compile on VPS, restart service.
- **Observability (V1):** Structured log tailing via SSH. Future: TUI or web-based dashboard (Phase 2).
- **Configuration:** TOML or YAML config files. Changes require service restart — no hot-reloading. Keeps configuration deterministic and auditable.
- **Data persistence:** SQLite event journal for transactional data (trades, orders, P&L, circuit breaker events). Parquet for market data (ticks, L2 snapshots) — columnar format for compression and research compatibility.
- **Build pipeline:** Local development and testing. Deploy via git. Compile on VPS. No CI/CD in V1.

### Architecture: Hot Path

- **Single-threaded hot path:** Market data → signal computation → risk check → order decision on a single thread, busy-polling, pinned to an isolated CPU core. No async runtime on the hot path.
- **I/O boundary separation:** Broker connections and data persistence on Tokio async runtime on separate cores, communicating with the hot path via SPSC lock-free ring buffers (rtrb).
- **Zero-allocation hot path:** Pre-allocate all data structures at startup. No heap allocations in the critical trading loop. Arena allocators where dynamic allocation is unavoidable.

### System Lifecycle

- **Startup:** Load config → connect to broker → verify connectivity → sync position state → arm circuit breakers → start watchdog heartbeat → begin market data ingestion → enable strategy (if regime permits)
- **Shutdown:** Disable strategy → cancel open orders → verify flat → stop market data → disconnect broker → flush logs → stop heartbeat

### Testing Strategy

- **Unit tests:** Comprehensive coverage across all components — signal logic, fee-aware gating, order state machine, circuit breakers, regime detection, position tracking, data ingestion
- **Historical replay:** Same code path as live, fed with historical data. Primary integration test harness — correct results on known data validates core logic
- **Paper trading:** Live market conditions with simulated execution. Primary confidence gate before live deployment
- **Validation progression:** Unit tests → historical replay → paper trading → live (single MES) → scaled live

## Project Scoping & Phased Development

### MVP Strategy & Philosophy

**MVP Approach:** Validation-first — a working engine that can replay historical data and paper trade live markets, proving the architecture and signal pipeline work before risking real capital. Optimizing for confidence in the system, not P&L.

**Resource Requirements:** Solo developer-operator, $250-350/month infrastructure (VPS + broker API + watchdog VPS + market data)

### MVP Feature Set (Phase 1)

**Core User Journeys Supported:**
- Journey 1 (Trader-Operator): Full — deploy, monitor, intervene, shut down
- Journey 4 (Replay/Validation): Full — historical replay sharing live code path
- Journey 3 (Risk Manager): Partial — circuit breakers, position limits, watchdog; advanced regime-based adaptation is Phase 2

**Must-Have Capabilities:**
- Real-time market data ingestion (L1/L2 via Rithmic or Tradovate)
- Microstructure signal engine (OBI, VPIN, microprice at structural levels)
- Market order execution with atomic bracket orders (exchange-resting stops)
- Fee-aware trade gating (>2x total fees minimum edge)
- Multi-layer circuit breakers (position limits, daily loss, consecutive loss)
- Independent watchdog on separate provider
- Historical replay engine (same code path as live)
- Paper trading mode
- SQLite event journal + Parquet market data storage
- Config-file-driven parameters (restart-required)
- Configurable event-aware trading windows (scheduled events like FOMC — sit out or reduce exposure)
- Continuous market data recording even when not actively trading
- Systemd deployment, tail-logs observability
- Unit test coverage on all components

### Post-MVP Features

**Phase 2 (Growth):**
- Python research layer (PyO3 + Arrow integration)
- Continuous adaptive calibration (Bayesian updating of signal thresholds by session/regime context)
- Anomaly detection as event proxy (depth evaporation, spread blowout, volume spikes as regime break signals)
- TUI or web-based monitoring dashboard
- Multi-contract support (beyond MES/MNQ)
- Signal performance analytics and attribution
- Adaptive risk scaling based on regime

**Phase 3 (Expansion):**
- Broker-agnostic abstraction (run on Rithmic and Tradovate interchangeably)
- Portfolio-level risk management across instruments
- Automated signal parameter optimization
- Advanced order types (limit orders with state management)
- Remote management capabilities
- Multi-strategy orchestration
- External intelligence feeds (prediction markets, sentiment, news)

### Risk Mitigation Strategy

**Technical Risks:** Latency targets on VPS — mitigated by busy-polling architecture, zero-allocation hot path, and pre-allocation at startup. Replay engine validates performance before live deployment.

**Market Risks:** Signals may not be profitable — mitigated by paper trading validation gate before live capital, fee-aware gating preventing marginal trades, and small initial capital ($1-5K on micro contracts).

**Resource Risks:** Solo operator — mitigated by keeping MVP scope tight (no Python layer, no dashboard, no multi-broker), automated watchdog reducing monitoring burden, and circuit breakers as autonomous safety net.

## Functional Requirements

### Market Data Ingestion

- **FR1:** System can connect to a broker's market data feed and receive real-time tick and Level 2 order book data for configured CME equity index futures contracts
- **FR2:** System can reconstruct and maintain a current order book state from streaming market data updates
- **FR3:** System can ingest historical market data from Parquet files for replay processing
- **FR4:** System can detect and flag market data gaps or quality degradation during live operation
- **FR5:** System can record market data continuously during market hours, regardless of whether trading strategies are active

### Signal Analysis & Trade Decision

- **FR6:** System can compute order book imbalance (OBI) from current order book state
- **FR7:** System can compute volume-synchronized probability of informed trading (VPIN) from trade flow data
- **FR8:** System can compute microprice from order book state
- **FR9:** System can identify structural price levels from market data (prior day high/low, volume point of control, key reaction levels)
- **FR10:** System can evaluate composite signals at structural price levels and generate trade/no-trade decisions
- **FR11:** System can gate trade decisions based on fee-aware minimum edge threshold (expected edge must exceed 2x total round-trip costs)

### Order Execution

- **FR12:** System can submit market orders to the exchange via broker API
- **FR13:** System can submit atomic bracket orders (entry + take-profit + stop-loss) with protective stops resting at the exchange
- **FR14:** System can cancel open orders
- **FR15:** System can track order state transitions (submitted, filled, partially filled, cancelled, rejected) from broker confirmations
- **FR16:** System can reconcile local position state with broker-reported position state

### Risk Management & Safety

- **FR17:** System can enforce configurable position size limits (max contracts per position)
- **FR18:** System can enforce configurable daily loss limits and halt trading when exceeded
- **FR19:** System can track consecutive losses and halt trading when a configured threshold is exceeded
- **FR20:** System can detect anomalous positions (positions existing outside active strategy context) and automatically flatten them
- **FR21:** System can send heartbeats to an independent watchdog process at a configurable interval
- **FR22:** Watchdog can detect missed heartbeats and autonomously flatten all positions via its own broker connection
- **FR23:** Watchdog can send operator alerts (email/push notification) when activated
- **FR24:** System can alert the operator on circuit breaker events
- **FR25:** System can respect configurable event-aware trading windows (e.g., FOMC, CPI, NFP) by disabling strategies or reducing exposure during scheduled high-impact events

### Regime Detection & Strategy Management

- **FR26:** System can classify current market regime (trending, rotational, volatile) from market data
- **FR27:** System can enable or disable trading strategies based on current regime classification
- **FR28:** System can detect and log regime transitions during a trading session

### Historical Replay & Validation

- **FR29:** System can replay historical market data through the same code path used for live trading
- **FR30:** System can produce deterministic, reproducible results when replaying the same historical data
- **FR31:** System can operate in paper trading mode — processing live market data and generating signals/decisions without submitting real orders
- **FR32:** System can record paper trade results with the same fidelity as live trade results

### Data Persistence & Logging

- **FR33:** System can log all trade events, order state transitions, and P&L records to a structured event journal (SQLite)
- **FR34:** System can store market data (ticks, L2 snapshots) in columnar format (Parquet) for long-term retention
- **FR35:** System can log circuit breaker events, regime transitions, and system state changes with full diagnostic context
- **FR36:** System can produce structured logs suitable for real-time monitoring via log tailing
- **FR37:** System can log trade data in a format suitable for Section 1256 tax reporting (realized P&L per contract, per day)

### System Lifecycle & Configuration

- **FR38:** Operator can configure strategy parameters, risk limits, regime thresholds, and broker connectivity via configuration files
- **FR39:** System can execute a startup sequence (connect → verify → sync positions → arm safety systems → begin operation)
- **FR40:** System can execute a graceful shutdown sequence (disable strategies → cancel orders → verify flat → disconnect → flush logs)
- **FR41:** System can recover from broker disconnections and resynchronize state on reconnect
- **FR42:** Operator can deploy updated system binaries and restart the service without data loss

## Non-Functional Requirements

### Performance

- **NFR1:** Tick-to-decision latency within the budget required by the active trading strategy — architecture supports sub-millisecond processing to preserve optionality for latency-sensitive strategies
- **NFR2:** Zero heap allocations on the hot path during steady-state operation — all data structures pre-allocated at startup
- **NFR3:** No garbage collection pauses, lock contention, or context switches on the hot path thread
- **NFR4:** Market data ingestion keeps pace with broker feed rates without dropping messages under normal market conditions
- **NFR5:** Historical replay processes every tick in sequence through the same code path as live trading, with no sampling or skipping — elapsed time is compressed (no waiting for real-time gaps between ticks), targeting at minimum 100x real-time throughput to keep multi-month backtests practical

### Reliability

- **NFR6:** System uptime >99.5% during CME regular trading hours (8:30 AM - 3:00 PM CT), measured as "running, connected to broker, and able to trade" — excluding planned maintenance and non-market hours
- **NFR7:** Broker disconnections detected within 5 seconds and reconnection attempted automatically with position state resynchronization
- **NFR8:** Watchdog detects primary system failure within 30 seconds (configurable heartbeat threshold) and flattens positions autonomously
- **NFR9:** No data loss on unclean shutdown — event journal durable (SQLite WAL mode or equivalent), in-flight state recoverable on restart
- **NFR10:** Circuit breakers activate within one tick of detecting a violation condition — no delayed enforcement

### Security

- **NFR11:** Broker API credentials not stored in plaintext in configuration files or source control — environment variables or encrypted config at minimum
- **NFR12:** VPS access restricted to SSH key authentication only — no password authentication
- **NFR13:** Watchdog-to-primary communication authenticated to prevent unauthorized kill signals
- **NFR14:** No broker credentials or trading data transmitted over unencrypted channels

### Data Integrity

- **NFR15:** Historical replay produces bit-identical results on identical input data — full determinism required for signal validation
- **NFR16:** Position state always reconcilable with broker-reported state — any mismatch triggers circuit breaker, not silent correction
- **NFR17:** Event journal maintains referential integrity — every trade event traceable from signal through execution to P&L
