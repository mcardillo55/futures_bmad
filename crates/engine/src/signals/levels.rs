use std::collections::HashMap;
use std::path::Path;

use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType};
use tracing::{info, warn};

use crate::data::data_source::DataSource;
use crate::data::parquet_source::ParquetDataSource;

/// Type of structural price level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelType {
    PriorDayHigh,
    PriorDayLow,
    Vpoc,
    ManualLevel,
}

/// Source of a structural level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelSource {
    Historical,
    Configured,
    Computed,
}

/// A structural price level with metadata.
#[derive(Debug, Clone, Copy)]
pub struct StructuralLevel {
    pub price: FixedPrice,
    pub level_type: LevelType,
    pub source: LevelSource,
}

/// Proximity result for a single level check.
#[derive(Debug, Clone, Copy)]
pub struct LevelProximity {
    pub level: StructuralLevel,
    pub distance: FixedPrice,
    pub at_level: bool,
}

/// Session summary data for computing structural levels.
#[derive(Debug, Clone)]
pub struct SessionData {
    pub high: FixedPrice,
    pub low: FixedPrice,
    /// Volume by price (raw FixedPrice -> total volume).
    pub volume_by_price: HashMap<i64, u64>,
}

impl SessionData {
    /// Build session data from an iterator of trade events.
    pub fn from_trades(trades: impl Iterator<Item = MarketEvent>) -> Option<Self> {
        let mut high = FixedPrice::new(i64::MIN);
        let mut low = FixedPrice::new(i64::MAX);
        let mut volume_by_price: HashMap<i64, u64> = HashMap::new();
        let mut count = 0u64;

        for event in trades {
            if event.event_type != MarketEventType::Trade {
                continue;
            }
            if event.size == 0 {
                continue;
            }
            count += 1;
            if event.price > high {
                high = event.price;
            }
            if event.price < low {
                low = event.price;
            }
            *volume_by_price.entry(event.price.raw()).or_insert(0) += event.size as u64;
        }

        if count == 0 {
            return None;
        }

        Some(Self {
            high,
            low,
            volume_by_price,
        })
    }

    /// Compute VPOC (price with highest volume). On tie, lower price wins.
    pub fn vpoc(&self) -> Option<FixedPrice> {
        if self.volume_by_price.is_empty() {
            return None;
        }

        let mut best_price_raw: i64 = 0;
        let mut best_volume: u64 = 0;
        let mut found = false;

        for (&price_raw, &volume) in &self.volume_by_price {
            if !found
                || volume > best_volume
                || (volume == best_volume && price_raw < best_price_raw)
            {
                best_price_raw = price_raw;
                best_volume = volume;
                found = true;
            }
        }

        if found {
            Some(FixedPrice::new(best_price_raw))
        } else {
            None
        }
    }
}

/// Configuration for the level engine.
#[derive(Debug, Clone)]
pub struct LevelConfig {
    /// Distance in quarter-ticks for "at level" detection.
    pub proximity_threshold: FixedPrice,
    /// Manually configured price levels (as FixedPrice).
    pub manual_levels: Vec<FixedPrice>,
}

impl LevelConfig {
    pub fn new(proximity_threshold_qticks: i64, manual_levels_f64: &[f64]) -> Self {
        assert!(
            proximity_threshold_qticks >= 0,
            "proximity_threshold must be non-negative"
        );
        let manual_levels = manual_levels_f64
            .iter()
            .filter_map(|&p| FixedPrice::from_f64(p).ok())
            .collect();
        Self {
            proximity_threshold: FixedPrice::new(proximity_threshold_qticks),
            manual_levels,
        }
    }
}

/// Engine that manages structural price levels and proximity detection.
///
/// NOT a Signal — this gates when signals are evaluated. Signals fire at
/// structural levels rather than continuously, reducing noise.
pub struct LevelEngine {
    levels: Vec<StructuralLevel>,
    proximity_threshold: FixedPrice,
    prior_day_high: Option<FixedPrice>,
    prior_day_low: Option<FixedPrice>,
    vpoc: Option<FixedPrice>,
    manual_levels: Vec<FixedPrice>,
}

impl LevelEngine {
    pub fn new(config: LevelConfig) -> Self {
        let manual_levels = config.manual_levels.clone();
        let proximity_threshold = config.proximity_threshold;

        let mut engine = Self {
            levels: Vec::new(),
            proximity_threshold,
            prior_day_high: None,
            prior_day_low: None,
            vpoc: None,
            manual_levels,
        };

        engine.rebuild_levels();
        engine
    }

    /// Load historical levels from session data.
    pub fn load_historical(&mut self, session: &SessionData) {
        self.prior_day_high = Some(session.high);
        self.prior_day_low = Some(session.low);
        self.vpoc = session.vpoc();

        self.rebuild_levels();

        info!(
            pdh = %self.prior_day_high.unwrap(),
            pdl = %self.prior_day_low.unwrap(),
            vpoc = ?self.vpoc,
            "Structural levels loaded"
        );
    }

    /// Load historical data from a Parquet file.
    pub fn load_from_parquet(&mut self, path: &Path) -> Result<(), String> {
        if !path.exists() {
            warn!(path = %path.display(), "Parquet file not found, using configured levels only");
            return Ok(());
        }

        let mut source = ParquetDataSource::new(path.to_path_buf()).map_err(|e| e.to_string())?;
        let iter = std::iter::from_fn(|| source.next_event());

        match SessionData::from_trades(iter) {
            Some(session) => {
                self.load_historical(&session);
                Ok(())
            }
            None => {
                warn!(path = %path.display(), "No trade data in Parquet file");
                Ok(())
            }
        }
    }

    /// Refresh levels from a new session's data.
    pub fn refresh_session(&mut self, new_session: &SessionData) {
        self.prior_day_high = Some(new_session.high);
        self.prior_day_low = Some(new_session.low);
        self.vpoc = new_session.vpoc();

        self.rebuild_levels();

        info!(
            pdh = %self.prior_day_high.unwrap(),
            pdl = %self.prior_day_low.unwrap(),
            vpoc = ?self.vpoc,
            "Session refresh complete"
        );
    }

    /// Check proximity of current price to all structural levels.
    pub fn check_proximity(&self, current_price: FixedPrice) -> Vec<LevelProximity> {
        let threshold = self.proximity_threshold.raw() as u64;
        self.levels
            .iter()
            .map(|level| {
                let distance_raw = current_price.raw().abs_diff(level.price.raw());
                let distance = FixedPrice::new(distance_raw as i64);
                let at_level = distance_raw <= threshold;
                LevelProximity {
                    level: *level,
                    distance,
                    at_level,
                }
            })
            .collect()
    }

    /// Returns true if price is within proximity threshold of any level.
    pub fn is_at_any_level(&self, current_price: FixedPrice) -> bool {
        let threshold = self.proximity_threshold.raw() as u64;
        self.levels.iter().any(|level| {
            let distance = current_price.raw().abs_diff(level.price.raw());
            distance <= threshold
        })
    }

    /// Access all active levels.
    pub fn levels(&self) -> &[StructuralLevel] {
        &self.levels
    }

    /// Rebuild the levels vec from current state.
    fn rebuild_levels(&mut self) {
        self.levels.clear();

        if let Some(pdh) = self.prior_day_high {
            self.levels.push(StructuralLevel {
                price: pdh,
                level_type: LevelType::PriorDayHigh,
                source: LevelSource::Historical,
            });
        }

        if let Some(pdl) = self.prior_day_low {
            self.levels.push(StructuralLevel {
                price: pdl,
                level_type: LevelType::PriorDayLow,
                source: LevelSource::Historical,
            });
        }

        if let Some(vpoc) = self.vpoc {
            self.levels.push(StructuralLevel {
                price: vpoc,
                level_type: LevelType::Vpoc,
                source: LevelSource::Computed,
            });
        }

        for &price in &self.manual_levels {
            self.levels.push(StructuralLevel {
                price,
                level_type: LevelType::ManualLevel,
                source: LevelSource::Configured,
            });
        }
    }
}
