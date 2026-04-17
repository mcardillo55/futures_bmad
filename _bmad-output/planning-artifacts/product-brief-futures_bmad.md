---
title: "Product Brief: Futures Trading System"
status: "complete"
created: "2026-04-12"
updated: "2026-04-12"
inputs:
  - "_bmad-output/brainstorming/brainstorming-session-2026-04-12-001.md"
  - "_bmad-output/planning-artifacts/research/domain-futures-microstructure-strategy-research-2026-04-12.md"
  - "_bmad-output/planning-artifacts/research/technical-execution-and-architecture-research-2026-04-12.md"
---

# Product Brief: Automated Futures Trading System

## Executive Summary

An automated trading system for CME equity index futures (ES/NQ micro and standard contracts) that reads market microstructure in real time — order flow, depth-of-book dynamics, and regime state — to identify and execute high-probability trades with institutional-grade safety. The system occupies a viable but underpopulated niche between retail traders (100ms+ latency, indicator-based, 74-89% failure rate) and institutional HFT ($30K+/month, sub-microsecond). At $250-350/month infrastructure cost and 0.5-2ms latency, it delivers 100-300x the speed of retail platforms while costing 100x less than institutional setups.

The core thesis: most retail algo traders fail because they rely on lagging indicators, ignore transaction costs, and can't detect when market conditions have changed. This system solves all three by enforcing a zero-lag signal philosophy, fee-aware trade gating, and adaptive regime detection — built as a personal trading platform designed to compound returns beyond passive index fund performance.

## The Problem

Retail futures traders face a brutal landscape. Studies across 8 million traders show 74-89% lose money. 80% quit within a year. The tools available to them — NinjaTrader, Sierra Chart, off-the-shelf bots — suffer from fundamental limitations:

- **Lagging signals:** Most platforms encourage strategies built on moving averages, RSI, MACD — indicators that smooth away the very information that matters. By the time a signal fires, the edge is gone.
- **Fee blindness:** Transaction costs on futures (exchange fees, commissions, slippage) compound ruthlessly. Research shows execution costs consumed 78% of gross returns in one documented scalping system. Most retail tools don't model fees as a first-class concern.
- **No regime awareness:** Markets spend ~60% of time in rotational/choppy conditions where most strategies bleed. Commercial platforms offer no built-in mechanism to detect and sit out unfavorable conditions.
- **Platform tax:** Commercial platforms impose their own latency overhead, limit customization, and charge $50-250/month for the privilege of running strategies in constrained environments.

The result: retail algo traders build systems that look profitable in backtests but bleed out in live markets through fees, slippage, and regime mismatch.

## The Solution

A custom-built automated trading system with three architectural pillars:

**Microstructure-first signals.** The primary data model is a continuous stream of order book states and trade events — not OHLC bars. Signals derive from current market state: order book imbalance (OBI), volume-weighted price of informed trading (VPIN), microprice, absorption detection, and cumulative delta divergence. Lagging indicators are architecturally banned from entry decisions.

**Fee-aware everything.** Every trade is evaluated net of all costs — exchange fees, commissions, API fees, and modeled slippage — before submission. Trades below a minimum edge threshold (>2x total fees) are rejected. This is not a feature; it's a design constraint enforced at the signal gating layer.

**Adaptive regime detection.** The system classifies market regime in real time — trending, rotational, or volatile — using quantifiable microstructure signals (volatility clustering, book depth changes, spread dynamics, volume profile shifts). Strategies are enabled or disabled based on regime state. The system knows when to sit out.

Execution safety is non-negotiable: every trade enters as an atomic bracket order (entry + take-profit + stop-loss), with stops resting at the exchange level. A multi-layer circuit breaker system enforces daily loss limits, consecutive loss limits, and position size constraints. An independent watchdog on a separate cloud provider flattens all positions if the primary system goes dark.

## What Makes This Different

**Strategy-first architecture.** The infrastructure exists to serve the strategy, not the other way around. The data model, signal pipeline, risk management, and execution engine are designed as a coherent system around microstructure data — purpose-built to express a specific trading thesis without the compromises of general-purpose platforms.

**Zero-lag as a design constraint.** Lagging indicators are not merely discouraged — they are structurally excluded from entry decisions. Every signal is classified as current-state, leading, or lagging at the architecture level.

**Broker-agnostic low-latency connectivity.** The system connects to exchanges through brokerage APIs (Rithmic, CQG, or alternatives) rather than routing through retail platform middleware. A clean abstraction layer prevents vendor lock-in while preserving low-latency execution.

**The builder is the user.** Built by someone with direct experience designing FPGA firmware for low-latency trading applications, prior algo trading systems, and professional software engineering. This background informs every architectural decision — from why Rust (predictable latency, no GC pauses) to why exchange-resting stops (understanding matching engine guarantee models). Zero signal loss between market insight and implementation. No vendor generalization diluting specificity.

**Cost structure.** ~$250-350/month total operating cost vs. $30K+ for institutional infrastructure, while achieving latency competitive with the underserved mid-frequency tier that no commercial product currently targets.

## Who This Serves

This is a personal trading system built for a single operator: an experienced trader and software engineer with low-latency trading infrastructure background, trading $1,000-5,000 in initial capital on CME micro equity index futures (MES, MNQ), with the goal of compounding returns that outperform passive index fund investments. Initial capital is sufficient for meaningful MES position sizing with brokers offering reduced day-trading margins (~$50/contract), with additional capital allocation available once the system demonstrates a proven edge.

## Success Criteria

- **Paper trading profitability** before any live capital deployment — validated edge on simulated fills with realistic slippage modeling over a statistically meaningful sample size
- **Live returns exceeding broad index fund performance** (S&P 500 / total market) on a risk-adjusted basis, targeting Sharpe ratio > 1.5 over rolling 12-month periods (vs. ~0.5 for passive index buy-and-hold)
- **Positive net returns after all costs** — infrastructure, exchange fees, commissions, and slippage
- **Maximum drawdown within defined limits** — circuit breakers enforced, no catastrophic loss scenarios
- **System uptime and reliability** — autonomous operation during market hours with graceful degradation under failure conditions
- **Kill criteria:** If paper trading shows negative expectancy after 500+ trades, or live drawdown exceeds 30% of starting capital, the system halts and the thesis is re-examined
- **Paper-to-live validation:** Paper trading results must be discounted for optimistic fill assumptions. Transition to live requires slippage model validation (comparing expected vs. actual fill quality), a minimum trade count for statistical significance, and graduated capital deployment — starting with a single MES contract before scaling

## Scope

**V1 (Minimum Viable System):**
- Exchange/broker connectivity (broker-agnostic design, initial implementation against one provider)
- Microstructure data ingestion and order book reconstruction
- One validated signal strategy with fee-aware gating
- Atomic bracket order execution with exchange-resting stops
- Multi-layer circuit breaker system
- Market quality score and basic regime detection
- Paper trading validation on MES/MNQ
- Independent watchdog kill switch
- Historical data replay for development and research (same code path as live)

**Explicitly out of scope for V1:**
- Multiple simultaneous strategies
- Instruments beyond ES/NQ (CL, GC, ZB, etc.)
- External intelligence feeds (news, sentiment, prediction markets)
- Adaptive position sizing / MES-to-ES graduation
- Mobile monitoring or web UI
- Any form of commercial licensing or multi-user support

## Vision

If the edge is proven and compounding, the system evolves along two axes:

**Depth:** More sophisticated signal composition, additional microstructure features (iceberg detection, anomalous flow detection, external event awareness), and adaptive calibration that extends alpha lifespan as markets evolve. Cross-instrument data feeds (treasury futures, VIX, crude) as leading signal inputs for regime detection — reading correlated markets without trading them. The historical replay engine becomes a first-class quantitative research lab — every day of live trading generates new data, feeding signal discovery, which feeds new strategies to validate. This research flywheel accelerates over time.

**Scale:** Graduated position sizing from MES micros to ES standard contracts based on statistical proof of edge — not intuition. Capital growth through compounding, not deposits. The system remains a personal trading platform — lean, focused, and optimized for one operator.

The long-term aspiration is a self-sustaining trading system that compounds capital autonomously while the operator focuses on research, signal discovery, and system evolution — a personal "trading operating system" rather than a single bot running a single strategy.
