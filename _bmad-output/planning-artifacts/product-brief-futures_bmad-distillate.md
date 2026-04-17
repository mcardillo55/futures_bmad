---
title: "Product Brief Distillate: futures_bmad"
type: llm-distillate
source: "product-brief-futures_bmad.md"
created: "2026-04-12"
purpose: "Token-efficient context for downstream PRD creation"
---

# Product Brief Distillate: Automated Futures Trading System

## Builder Profile

- Experienced trader with prior algo trading systems
- Former software engineer at firm designing FPGA firmware for low-latency trading applications
- Understands exchange matching engine behavior, wire protocol optimization, hardware-software co-design
- Motivated by: commercial platforms are expensive, questionably implemented, and not customizable enough for strategy expression
- Solo operator — no team, no commercial intent

## Capital & Financial Context

- Starting capital: $1,000-5,000
- Brokers like Tradovate offer ~$50 MES day-trading margin (not the standard ~$1,320)
- At $50 margin, $5K supports meaningful position sizing (up to ~100 contracts theoretically, though risk limits would constrain far below this)
- User willing to allocate more capital once system proves profitable
- Success target: compounding returns exceeding 401k in broad index funds (S&P 500 Sharpe ~0.5)
- Infrastructure costs ~$250-350/month treated as R&D investment during proving phase
- 60/40 tax treatment on Section 1256 contracts (~26.8% blended vs 37% ordinary income)

## Technology Decisions (Confirmed)

- **Rust over C++**: rithmic-rs v1.0.0 (March 2026) provides native R|Protocol implementation; Rust achieves 98.7% of C++ latency; memory safety critical for solo maintainer; Databento client is Rust-native; hftbacktest is Rust-core
- **Broker-agnostic design**: Rithmic is leading candidate but NOT a requirement — any brokerage/exchange connection works; clean abstraction layer prevents vendor lock-in; Tradovate also mentioned as option
- **R|Protocol over R|API+**: WebSocket + protobuf, language-agnostic, low-ms latency (irrelevant delta when VPS latency is 0.5-2ms), eliminates C++ dependency
- **Research stack**: Databento historical → Parquet → Polars + DuckDB; hftbacktest for microstructure backtesting with queue position simulation
- **Deployment**: Bare metal + systemd on Aurora-area VPS (NOT Docker — adds 1-2ms network overhead); CPU core isolation for hot path
- **Python analysis layer**: Connected via pyo3 + pyo3-arrow for zero-copy data exchange

## Infrastructure Cost Breakdown

| Item | Monthly Cost |
|------|-------------|
| VPS (QuantVPS Pro+ Chicago or similar) | $50-130 |
| Rithmic API (if chosen, via EdgeClear) | $20-100 |
| FCM commissions (MES) | ~$0.20-0.69/side |
| CME exchange fees | ~$0.62/side |
| Databento historical | Pay-per-byte (~$2/query, $125 free credit) |
| Databento live (Plus tier, if needed) | $1,399 (may use broker data feed instead) |
| AWS watchdog | $5-10 |
| **Realistic total (no Databento live)** | **$250-350** |

- Databento live pricing changed April 2025 — old $32.65/month option eliminated; Plus tier $1,399/mo is minimum for live CME data through Databento
- May use Rithmic's own data feed for live, Databento for historical research only

## MES Round-Trip Fee Analysis

- CME exchange: $0.62/side
- FCM commission: $0.20-0.69/side
- Rithmic API (if applicable): $0.10/side
- **Total: $1.84-2.82 per round-trip**
- Fee-aware gating requires edge > 2x fees = > $3.68-5.64 per round-trip
- Slippage modeled asymmetrically: winning trades fill at limit; losing trades (stops) fill with 0.5-1 tick adverse slippage

## Competitive Landscape

### Commercial Platforms (What This Replaces)

| Platform | Language | Monthly Cost | Key Limitation |
|----------|----------|-------------|----------------|
| NinjaTrader | C# | ~$60 | Managed code overhead, locked ecosystem |
| Sierra Chart | C++ (ACSIL) | $36 | Steep learning curve, limited automation |
| Bookmap | Java | $49-99 | Visualization-focused, not execution engine |
| Jigsaw Daytradr | N/A | $50 | Visualization tool, not automated |
| MotiveWave | Java | $49-249 | Java performance ceiling |
| Quantower | C# | Free-$70 | Newer, smaller community |

- Key gap: retail tools show you the DOM; they don't automate interpretation. Automated microstructure-based trading is rare at retail level.
- No commercial product serves the 1-10ms latency tier — retail can't get below ~100ms (interpreted languages, middleware, GUI event loops), institutional starts at $30K+/month

### Competitive Tiers in the Market

| Tier | Latency | Cost | This System's Relationship |
|------|---------|------|---------------------------|
| Ultra-HFT (Jump, Citadel, Virtu) | <1μs | $30K+/month | Not competing — 1000x slower but irrelevant |
| Mid-Tier Quant (Optiver, DRW) | μs-ms | $10K+/month | Adjacent — similar strategies, higher infrastructure |
| Systematic Funds (RenTech, Two Sigma) | ms | Massive | Different strategies entirely |
| **This system** | **0.5-2ms** | **$250-350/month** | **Uncontested retail lane** |
| Retail (NinjaTrader, Pine Script) | 50-200ms+ | $50-250/month | 100-300x slower |

## Strategy & Signal Evidence (Academic)

- **Momentum dominates mean reversion** for equity index futures at short horizons
- Overnight-intraday reversal: Sharpe 2-5x higher than conventional daily reversal
- Intraday momentum (first 30m → last 30m): Sharpe 0.87-1.73
- OBI (Order Book Imbalance): Linear relationship with short-horizon price changes (Cont et al. 2014)
- HMM regime detection on ES: Sharpe >2.0 in academic study (1-minute frequency)
- **Critical constraint**: Single-feature alpha already arbitraged by sub-100μs participants — composite signals required (OBI + VPIN + microprice + regime state) at structural levels
- Carver's scalping bot: Failed after 2 days — transaction costs consumed 78% of gross returns. Fee awareness is existential

## Alpha Decay & Adaptation

- Maven Securities: Mean-reversion alpha loses 5.6% annualized (US) to execution delays
- Decay rate accelerating: 36 bps/year increase in US
- At this latency tier: strategies decay over months-to-quarters, not years
- **Mitigation**: Continuous adaptive calibration (rolling EWMA, Bayesian parameter updating, regime-conditional switching), research pipeline for new signals
- Historical replay engine doubles as research lab — every live trading day generates new training data

## Regulatory Context

- **No CFTC registration required** for personal automated trading (Reg AT withdrawn June 2020)
- Anti-spoofing rules apply: CEA Section 4c(a)(5), CME Rule 575
- CME Messaging Efficiency Program: Surcharge $1,000/day if exceeding message benchmarks
- Every order must be bona fide intent to trade — no phantom orders
- Cancel-to-fill ratios >90% consistently triggers surveillance
- 2025 Flatiron Futures case: $200K penalty + 12-month ban for spoofing ES/NQ (small firm)

## Market Context (2025-2026)

- CME 2025 ADV: 28.1M contracts (6% YoY), accelerating in Q1 2026 to 35.5M (16% YoY)
- MES ADV: 1.2M (+35% YoY); MNQ: 1.6M (record)
- ~60-67% of US futures volume is algorithmic
- 74-89% of retail traders lose money; 80% quit within a year; only 5-8% consistently profitable over 3 years
- Algorithmic trading market: ~$20B in 2026, retail segment 38.5% share, growing 8.32% CAGR

## CME Cloud Migration (2028 Planning)

- CME Globex migrating from Aurora to private Google Cloud regions
- Production target: 2028, DR: 2029
- Aurora colocation may become unavailable or more expensive
- **Design for portability** — don't over-invest in Aurora-specific infrastructure
- 18-month notice guarantee provides buffer

## Rejected Ideas & Decisions

- **IBKR rejected**: 10-50ms+ structural latency floor makes it unsuitable for latency-sensitive strategies
- **C++ rejected for Rust**: rithmic-rs eliminates C++ requirement; Rust offers memory safety, safe concurrency, ecosystem alignment (Databento, hftbacktest native)
- **Docker rejected for deployment**: 1-2ms network overhead from bridged networking; bare metal + systemd preferred
- **Rithmic NOT locked in**: Broker-agnostic design required; Rithmic is a leading candidate, not a requirement
- **No commercial/licensing intent**: Purely personal system, no plans to sell, license, or manage external capital
- **No mobile/web UI in V1**: Backend trading engine only
- **Mean reversion de-prioritized**: Academic evidence favors momentum/composite signals for equity index futures

## Open Questions for PRD

- Which specific signal strategy for V1? OBI-based? VPIN? Microprice? Composite? Selection criteria needed
- What constitutes "validated" for paper trading? Minimum trade count, statistical significance level, out-of-sample requirements
- How many months/years of historical data for replay validation? Walk-forward methodology?
- How to detect overfitting? Monte Carlo permutation tests, in-sample vs out-of-sample Sharpe degradation?
- Fee-gating threshold: is 2x fees the right multiple, or should it be derived from strategy's actual distribution?
- Paper-to-live transition: what slippage discount to apply? How to validate fill model?
- Regime detection method: HMM, threshold-based, or ML? Number of states? Lookback window?
- rithmic-rs maturity risk: v1.0.0, single maintainer, 161 commits — mitigation plan if it stalls?
- MES liquidity during volatile conditions: do microstructure signals degrade on thinner books?

## Scope Signals from User

- **In for V1**: Broker connectivity, order book reconstruction, one signal strategy, bracket orders, circuit breakers, regime detection, paper trading, watchdog, historical replay
- **Out for V1**: Multiple strategies, instruments beyond ES/NQ, external intelligence feeds, adaptive position sizing, mobile/web UI, commercial features
- **Future interest**: Cross-instrument data (treasuries, VIX, crude) as read-only signal inputs for regime detection — user liked this idea but wants it post-V1
- **No hard limits on future scope**: User open to expanding beyond ES/NQ if better edge found elsewhere
- **Paper first**: User explicitly wants paper trading profitability proven before any live capital deployment

## Reviewer Findings Worth Preserving

### Skeptic (Key Items)
- $1-5K capital with $250-350/mo infrastructure costs requires 5-35% monthly returns just to break even — early phase should frame costs as R&D
- Paper trading fills are systematically optimistic (no market impact, no queue position effects) — 5-15% performance gap expected
- "100-300x faster than retail" claim needs benchmarking against specific platforms — IBKR TWS executes in 10-30ms, not 100ms+
- Regime detection is inherently lagging at transition points (when it matters most)
- Strategy viability at this exact latency/capacity/fee tier is the fundamental unknown

### Opportunity (Key Items)
- Historical replay engine deserves architectural prominence as a first-class research lab, not just a development aid
- Signal library compounds over time — multiple uncorrelated signals on same instrument improve Sharpe faster than adding instruments
- Research flywheel: live trading → new data → signal discovery → new strategies → validation → live. Name this cycle explicitly
- Quantified execution quality analytics (fill quality vs microprice, slippage decomposition, alpha decay curves) create feedback loop detecting edge degradation before P&L does

### Capital Viability (Contextual Review)
- With $50 margin brokers (Tradovate), capital adequacy is less constrained than standard margin suggests
- User willing to add capital once edge is proven — early phase is validation, not profit extraction
- Infrastructure costs are fixed; trading returns scale with capital — the economics improve dramatically above ~$20K
