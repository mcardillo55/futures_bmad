use chrono::NaiveDate;
use futures_bmad_core::{Clock, FixedPrice, OrderBook, Side, Signal};
use futures_bmad_engine::risk::FeeGate;
use futures_bmad_engine::signals::{
    CompositeConfig, CompositeEvaluator, DecisionReason, SignalPipeline,
};
use futures_bmad_testkit::{OrderBookBuilder, SimClock};

const MAX_SPREAD: i64 = 100;

fn sim_clock() -> SimClock {
    // 2026-04-17 in nanos (approximate)
    SimClock::new(1_776_556_800_000_000_000)
}

fn tradeable_book() -> OrderBook {
    OrderBookBuilder::new()
        .bid(4482.00, 100)
        .bid(4481.75, 50)
        .bid(4481.50, 50)
        .ask(4482.25, 100)
        .ask(4482.50, 50)
        .ask(4482.75, 50)
        .build()
}

fn default_fee_gate() -> FeeGate {
    FeeGate {
        exchange_fee_per_side: FixedPrice::new(2),
        commission_per_side: FixedPrice::new(1),
        api_fee_per_side: FixedPrice::new(1),
        slippage_model: FixedPrice::new(2),
        minimum_edge_multiple: 2.0,
        fee_schedule_date: NaiveDate::from_ymd_opt(2026, 4, 10).unwrap(),
    }
}

fn default_composite_config() -> CompositeConfig {
    CompositeConfig {
        obi_weight: 1.0,
        vpin_weight: 0.5,
        microprice_weight: 0.0,
        historical_edge_per_unit: 10.0,
    }
}

fn warm_pipeline(pipeline: &mut SignalPipeline, clock: &SimClock) {
    use futures_bmad_core::{MarketEvent, MarketEventType, UnixNanos};

    let book = tradeable_book();

    // Warm up OBI (warmup=1)
    pipeline.obi.update(&book, None, clock);

    // Warm up VPIN: need to fill 2 buckets of size 10 = 20 volume
    for _ in 0..20 {
        let trade = MarketEvent {
            timestamp: UnixNanos::default(),
            symbol_id: 1,
            sequence: 0,
            event_type: MarketEventType::Trade,
            price: FixedPrice::from_f64(4482.00).unwrap(),
            size: 1,
            side: Some(Side::Buy),
        };
        clock.advance_by(1_000_000);
        pipeline.vpin.update(&book, Some(&trade), clock);
    }

    // Warm up microprice
    pipeline.microprice.update(&book, None, clock);
}

// --- SignalPipeline tests ---

#[test]
fn pipeline_construction_concrete_types() {
    let pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    assert!(!pipeline.all_valid());
}

#[test]
fn pipeline_all_valid_when_all_warmed() {
    let clock = sim_clock();
    let mut pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    warm_pipeline(&mut pipeline, &clock);
    assert!(pipeline.all_valid());
}

#[test]
fn pipeline_all_valid_false_when_one_invalid() {
    let clock = sim_clock();
    let mut pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    let book = tradeable_book();

    // Only warm OBI and microprice, not VPIN
    pipeline.obi.update(&book, None, &clock);
    pipeline.microprice.update(&book, None, &clock);

    assert!(!pipeline.all_valid());
}

#[test]
fn pipeline_reset_invalidates_all() {
    let clock = sim_clock();
    let mut pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    warm_pipeline(&mut pipeline, &clock);
    assert!(pipeline.all_valid());

    pipeline.reset();
    assert!(!pipeline.all_valid());
}

// --- FeeGate tests ---

#[test]
fn fee_gate_round_trip_cost() {
    let gate = default_fee_gate();
    // per_side = 2 + 1 + 1 = 4, total = 2*4 + 2 = 10
    assert_eq!(gate.total_round_trip_cost().raw(), 10);
}

#[test]
fn fee_gate_permits_when_edge_exceeds_threshold() {
    let gate = default_fee_gate();
    // threshold = 2.0 * 10 = 20 qt -> edge must be > 20
    let edge = FixedPrice::new(21);
    assert_eq!(gate.permits_trade(edge), Ok(true));
}

#[test]
fn fee_gate_blocks_when_edge_below_threshold() {
    let gate = default_fee_gate();
    // threshold = 20, edge = 19
    let edge = FixedPrice::new(19);
    assert_eq!(gate.permits_trade(edge), Ok(false));
}

/// Pre-Epic-6 cleanup D-3: `FeeGate::permits_trade` no longer consults
/// fee-schedule staleness. The 31-day warn moved to
/// `CircuitBreakers::check_fee_staleness`; the 61-day gate is owned by
/// the same. `permits_trade` is now pure economics (edge vs fee).
#[test]
fn fee_gate_staleness_no_longer_blocks_at_61_days() {
    let clock = sim_clock();
    let mut gate = default_fee_gate();
    let today = clock.wall_clock().date_naive();
    gate.fee_schedule_date = today - chrono::Duration::days(61);

    // Edge well above the threshold — staleness has no effect; trade is
    // permitted on pure economics.
    let edge = FixedPrice::new(100);
    assert_eq!(gate.permits_trade(edge), Ok(true));
}

#[test]
fn fee_gate_permits_flatten_always() {
    let gate = default_fee_gate();
    assert!(gate.permits_flatten());

    // Even with stale schedule
    let mut stale_gate = default_fee_gate();
    stale_gate.fee_schedule_date = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();
    assert!(stale_gate.permits_flatten());
}

// --- CompositeEvaluator tests ---

#[test]
fn decision_id_increments() {
    let clock = sim_clock();
    let pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    let fee_gate = default_fee_gate();
    let mut evaluator = CompositeEvaluator::new(default_composite_config());

    let d1 = evaluator.evaluate(&pipeline, &fee_gate, &clock);
    let d2 = evaluator.evaluate(&pipeline, &fee_gate, &clock);

    assert_eq!(d1.decision_id, 1);
    assert_eq!(d2.decision_id, 2);
}

#[test]
fn evaluate_invalid_signal_returns_no_trade() {
    let clock = sim_clock();
    let pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    let fee_gate = default_fee_gate();
    let mut evaluator = CompositeEvaluator::new(default_composite_config());

    let decision = evaluator.evaluate(&pipeline, &fee_gate, &clock);
    match &decision.reason {
        DecisionReason::NoTradeSignalInvalid(msg) => {
            assert!(msg.contains("obi"));
            assert!(msg.contains("vpin"));
            assert!(msg.contains("microprice"));
        }
        other => panic!("expected NoTradeSignalInvalid, got {other:?}"),
    }
}

#[test]
fn evaluate_all_valid_edge_sufficient_returns_trade() {
    let clock = sim_clock();
    let mut pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    warm_pipeline(&mut pipeline, &clock);

    // VPIN is 1.0 (all buy volume), so use vpin_weight for a strong score
    let config = CompositeConfig {
        obi_weight: 0.0,
        vpin_weight: 100.0,
        microprice_weight: 0.0,
        historical_edge_per_unit: 100.0,
    };

    let fee_gate = default_fee_gate();
    let mut evaluator = CompositeEvaluator::new(config);

    let decision = evaluator.evaluate(&pipeline, &fee_gate, &clock);
    assert_eq!(decision.reason, DecisionReason::Trade);
    assert!(decision.expected_edge.raw() > 0);
    assert!(decision.decision_id > 0);
}

#[test]
fn evaluate_edge_below_threshold_returns_no_trade() {
    let clock = sim_clock();
    let mut pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    warm_pipeline(&mut pipeline, &clock);

    // Use tiny edge so it fails the fee gate
    let config = CompositeConfig {
        obi_weight: 0.001,
        vpin_weight: 0.0,
        microprice_weight: 0.0,
        historical_edge_per_unit: 0.001,
    };

    let fee_gate = default_fee_gate();
    let mut evaluator = CompositeEvaluator::new(config);

    let decision = evaluator.evaluate(&pipeline, &fee_gate, &clock);
    assert_eq!(decision.reason, DecisionReason::NoTradeEdgeBelowThreshold);
}

#[test]
fn evaluate_direction_from_positive_score() {
    let clock = sim_clock();
    let mut pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    warm_pipeline(&mut pipeline, &clock);

    // OBI is 0 for balanced book, VPIN is 1.0 (all buy), so composite is positive
    let config = CompositeConfig {
        obi_weight: 0.0,
        vpin_weight: 100.0,
        microprice_weight: 0.0,
        historical_edge_per_unit: 100.0,
    };

    let fee_gate = default_fee_gate();
    let mut evaluator = CompositeEvaluator::new(config);

    let decision = evaluator.evaluate(&pipeline, &fee_gate, &clock);
    assert_eq!(decision.direction, Side::Buy);
}

#[test]
/// Pre-Epic-6 cleanup D-3: a stale fee schedule no longer blocks trades
/// at the `FeeGate::permits_trade` layer — the responsibility moved to
/// `CircuitBreakers::check_fee_staleness` / `permits_trade_evaluation`.
/// `CompositeEvaluator` now permits trades through the fee gate on edge
/// alone; staleness is enforced upstream.
///
/// This test confirms the new behaviour: with a 61-day-stale schedule
/// AND sufficient edge, the evaluator returns `Trade`.
fn evaluate_stale_fee_gate_does_not_block_trade_after_d3() {
    let clock = sim_clock();
    let mut pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    warm_pipeline(&mut pipeline, &clock);

    let config = CompositeConfig {
        obi_weight: 0.0,
        vpin_weight: 100.0,
        microprice_weight: 0.0,
        historical_edge_per_unit: 100.0,
    };

    let mut fee_gate = default_fee_gate();
    let today = clock.wall_clock().date_naive();
    fee_gate.fee_schedule_date = today - chrono::Duration::days(61);

    let mut evaluator = CompositeEvaluator::new(config);
    let decision = evaluator.evaluate(&pipeline, &fee_gate, &clock);
    assert_eq!(
        decision.reason,
        DecisionReason::Trade,
        "fee staleness must NOT block trades at the FeeGate layer post-D-3"
    );
}

#[test]
fn pipeline_snapshot_captures_all() {
    let clock = sim_clock();
    let mut pipeline = SignalPipeline::new(1, FixedPrice::new(MAX_SPREAD), 10, 2);
    warm_pipeline(&mut pipeline, &clock);

    let snap = pipeline.snapshot();
    assert_eq!(snap.obi.name, "obi");
    assert_eq!(snap.vpin.name, "vpin");
    assert_eq!(snap.microprice.name, "microprice");
}
