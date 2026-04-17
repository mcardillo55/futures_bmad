use crate::types::FixedPrice;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct TradingConfig {
    pub symbol: String,
    pub max_position_size: u32,
    pub max_daily_loss: FixedPrice,
    pub max_consecutive_losses: u32,
    pub edge_multiple_threshold: f64,
    pub session_start: String,
    pub session_end: String,
    pub max_spread_threshold: FixedPrice,
}
