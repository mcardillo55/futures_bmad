use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType, UnixNanos};
use futures_bmad_engine::signals::{
    LevelConfig, LevelEngine, LevelSource, LevelType, SessionData,
};

fn make_trade(price_raw: i64, size: u32) -> MarketEvent {
    MarketEvent {
        timestamp: UnixNanos::default(),
        symbol_id: 1,
        sequence: 0,
        event_type: MarketEventType::Trade,
        price: FixedPrice::new(price_raw),
        size,
        side: None,
    }
}

fn session_from_trades(trades: &[(i64, u32)]) -> Option<SessionData> {
    let events: Vec<MarketEvent> = trades.iter().map(|&(p, s)| make_trade(p, s)).collect();
    SessionData::from_trades(events.into_iter())
}

#[test]
fn manual_levels_loaded_from_config() {
    let config = LevelConfig::new(4, &[4482.00, 4490.00, 4475.00]);
    let engine = LevelEngine::new(config);

    let levels = engine.levels();
    assert_eq!(levels.len(), 3);
    assert!(levels.iter().all(|l| l.level_type == LevelType::ManualLevel));
    assert!(levels.iter().all(|l| l.source == LevelSource::Configured));
}

#[test]
fn prior_day_high_low_from_session() {
    let config = LevelConfig::new(4, &[]);
    let mut engine = LevelEngine::new(config);

    // Trades at various prices: low=17900, high=18000
    let session = session_from_trades(&[(17900, 10), (17950, 20), (18000, 15)]).unwrap();
    engine.load_historical(&session);

    let levels = engine.levels();
    let pdh = levels.iter().find(|l| l.level_type == LevelType::PriorDayHigh).unwrap();
    let pdl = levels.iter().find(|l| l.level_type == LevelType::PriorDayLow).unwrap();

    assert_eq!(pdh.price.raw(), 18000);
    assert_eq!(pdl.price.raw(), 17900);
    assert_eq!(pdh.source, LevelSource::Historical);
}

#[test]
fn vpoc_computed_correctly() {
    let config = LevelConfig::new(4, &[]);
    let mut engine = LevelEngine::new(config);

    // Price 17950 has highest total volume (20+30=50)
    let session = session_from_trades(&[
        (17900, 10),
        (17950, 20),
        (18000, 15),
        (17950, 30),
        (18000, 10),
    ])
    .unwrap();

    engine.load_historical(&session);

    let vpoc = engine.levels().iter().find(|l| l.level_type == LevelType::Vpoc).unwrap();
    assert_eq!(vpoc.price.raw(), 17950);
    assert_eq!(vpoc.source, LevelSource::Computed);
}

#[test]
fn vpoc_tiebreak_lower_price_wins() {
    // Two prices with same volume: 17900 and 18000 both have 25
    let session = session_from_trades(&[
        (17900, 25),
        (18000, 25),
    ])
    .unwrap();

    let vpoc = session.vpoc().unwrap();
    assert_eq!(vpoc.raw(), 17900, "on tie, lower price should win");
}

#[test]
fn proximity_at_level_exact() {
    let config = LevelConfig::new(4, &[4482.00]);
    let engine = LevelEngine::new(config);

    let price = FixedPrice::from_f64(4482.00).unwrap();
    let proximities = engine.check_proximity(price);

    assert_eq!(proximities.len(), 1);
    assert!(proximities[0].at_level);
    assert_eq!(proximities[0].distance.raw(), 0);
}

#[test]
fn proximity_within_threshold() {
    // threshold = 4 quarter-ticks = 1.00 dollar
    let config = LevelConfig::new(4, &[4482.00]);
    let engine = LevelEngine::new(config);

    // Price 3 quarter-ticks away (0.75)
    let level_raw = FixedPrice::from_f64(4482.00).unwrap().raw();
    let price = FixedPrice::new(level_raw + 3);

    let proximities = engine.check_proximity(price);
    assert!(proximities[0].at_level);
    assert_eq!(proximities[0].distance.raw(), 3);
}

#[test]
fn proximity_outside_threshold() {
    let config = LevelConfig::new(4, &[4482.00]);
    let engine = LevelEngine::new(config);

    // Price 5 quarter-ticks away (> threshold of 4)
    let level_raw = FixedPrice::from_f64(4482.00).unwrap().raw();
    let price = FixedPrice::new(level_raw + 5);

    let proximities = engine.check_proximity(price);
    assert!(!proximities[0].at_level);
    assert_eq!(proximities[0].distance.raw(), 5);
}

#[test]
fn session_refresh_updates_levels() {
    let config = LevelConfig::new(4, &[4500.00]);
    let mut engine = LevelEngine::new(config);

    // Load initial session
    let session1 = session_from_trades(&[(17900, 10), (18000, 20)]).unwrap();
    engine.load_historical(&session1);
    assert_eq!(engine.levels().len(), 4); // PDH + PDL + VPOC + 1 manual

    // Refresh with new session
    let session2 = session_from_trades(&[(17800, 30), (17850, 10)]).unwrap();
    engine.refresh_session(&session2);

    let pdh = engine.levels().iter().find(|l| l.level_type == LevelType::PriorDayHigh).unwrap();
    let pdl = engine.levels().iter().find(|l| l.level_type == LevelType::PriorDayLow).unwrap();
    assert_eq!(pdh.price.raw(), 17850);
    assert_eq!(pdl.price.raw(), 17800);

    // Manual level still present
    assert!(engine.levels().iter().any(|l| l.level_type == LevelType::ManualLevel));
}

#[test]
fn no_historical_data_manual_levels_only() {
    let config = LevelConfig::new(4, &[4482.00, 4490.00]);
    let engine = LevelEngine::new(config);

    assert_eq!(engine.levels().len(), 2);
    assert!(engine.levels().iter().all(|l| l.level_type == LevelType::ManualLevel));
}

#[test]
fn vpoc_with_empty_volume_returns_none() {
    let session = session_from_trades(&[]);
    assert!(session.is_none());
}

#[test]
fn is_at_any_level_convenience() {
    let config = LevelConfig::new(4, &[4482.00, 4490.00]);
    let engine = LevelEngine::new(config);

    let at = FixedPrice::from_f64(4482.00).unwrap();
    assert!(engine.is_at_any_level(at));

    let far = FixedPrice::from_f64(4485.00).unwrap();
    assert!(!engine.is_at_any_level(far));
}
