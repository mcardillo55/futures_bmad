use std::collections::VecDeque;

use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType, Side, UnixNanos};
use rithmic_rs::rti::messages::RithmicMessage;
use tracing::warn;

use crate::messages::RithmicMarketMessage;

const MALFORMED_WINDOW_SECS: u64 = 60;
const MALFORMED_CIRCUIT_BREAK_THRESHOLD: usize = 10;

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("malformed message: {0}")]
    Malformed(String),
    #[error("circuit break: {count} malformed messages in 60s window")]
    CircuitBreak { count: usize },
    #[error("irrelevant message type")]
    Irrelevant,
}

/// Symbol-to-ID mapping for converting string symbols to u32 symbol_id.
pub struct SymbolMap {
    symbols: Vec<String>,
}

impl SymbolMap {
    pub fn new() -> Self {
        Self {
            symbols: Vec::new(),
        }
    }

    pub fn get_or_insert(&mut self, symbol: &str) -> u32 {
        if let Some(idx) = self.symbols.iter().position(|s| s == symbol) {
            return idx as u32;
        }
        let idx = self.symbols.len() as u32;
        self.symbols.push(symbol.to_string());
        idx
    }
}

/// Validates incoming Rithmic messages, converting well-formed ones to MarketEvent.
/// Tracks malformed messages in a 60-second sliding window and triggers circuit break
/// when the threshold is exceeded.
pub struct MessageValidator {
    malformed_timestamps: VecDeque<u64>,
    symbol_map: SymbolMap,
    pending_event: Option<MarketEvent>,
}

impl MessageValidator {
    pub fn new() -> Self {
        Self {
            malformed_timestamps: VecDeque::new(),
            symbol_map: SymbolMap::new(),
            pending_event: None,
        }
    }

    /// Drain any pending event queued from a previous BBO with both bid+ask.
    pub fn take_pending(&mut self) -> Option<MarketEvent> {
        self.pending_event.take()
    }

    /// Convert a RithmicMessage to an internal RithmicMarketMessage.
    /// Returns Err(Irrelevant) for non-market-data messages.
    /// Returns Err(Malformed) for market data missing required fields.
    /// For BBO with both bid+ask, returns bid and queues ask as pending_event.
    pub(crate) fn parse_rithmic_message(
        &mut self,
        message: &RithmicMessage,
    ) -> Result<RithmicMarketMessage, ValidationError> {
        match message {
            RithmicMessage::LastTrade(trade) => {
                let symbol = trade
                    .symbol
                    .as_deref()
                    .ok_or_else(|| ValidationError::Malformed("LastTrade missing symbol".into()))?;
                let price_f64 = trade.trade_price.ok_or_else(|| {
                    ValidationError::Malformed("LastTrade missing trade_price".into())
                })?;
                let size = trade.trade_size.ok_or_else(|| {
                    ValidationError::Malformed("LastTrade missing trade_size".into())
                })?;
                let ssboe = trade
                    .ssboe
                    .ok_or_else(|| ValidationError::Malformed("LastTrade missing ssboe".into()))?;
                let usecs = trade.usecs.unwrap_or(0);

                let price = FixedPrice::from_f64(price_f64).map_err(|_| {
                    ValidationError::Malformed(format!("LastTrade non-finite price: {price_f64}"))
                })?;
                let timestamp = UnixNanos::new(rithmic_rs::rithmic_to_unix_nanos(ssboe, usecs));
                let aggressor = trade.aggressor.and_then(|a| match a {
                    1 => Some(Side::Buy),
                    2 => Some(Side::Sell),
                    _ => None,
                });

                if size < 0 {
                    return Err(ValidationError::Malformed(format!(
                        "LastTrade negative trade_size: {size}"
                    )));
                }

                Ok(RithmicMarketMessage::Trade {
                    symbol: symbol.to_string(),
                    timestamp,
                    price,
                    size: size as u32,
                    aggressor,
                })
            }
            RithmicMessage::BestBidOffer(bbo) => {
                let symbol = bbo
                    .symbol
                    .as_deref()
                    .ok_or_else(|| ValidationError::Malformed("BBO missing symbol".into()))?;
                let ssboe = bbo
                    .ssboe
                    .ok_or_else(|| ValidationError::Malformed("BBO missing ssboe".into()))?;
                let usecs = bbo.usecs.unwrap_or(0);
                let timestamp = UnixNanos::new(rithmic_rs::rithmic_to_unix_nanos(ssboe, usecs));

                let bid = bbo.bid_price.map(|bid_price_f64| {
                    let price = FixedPrice::from_f64(bid_price_f64).map_err(|_| {
                        ValidationError::Malformed(format!(
                            "BBO non-finite bid_price: {bid_price_f64}"
                        ))
                    })?;
                    let size = bbo.bid_size.unwrap_or(0).max(0) as u32;
                    Ok(RithmicMarketMessage::BidUpdate {
                        symbol: symbol.to_string(),
                        timestamp,
                        price,
                        size,
                    })
                });

                let ask = bbo.ask_price.map(|ask_price_f64| {
                    let price = FixedPrice::from_f64(ask_price_f64).map_err(|_| {
                        ValidationError::Malformed(format!(
                            "BBO non-finite ask_price: {ask_price_f64}"
                        ))
                    })?;
                    let size = bbo.ask_size.unwrap_or(0).max(0) as u32;
                    Ok(RithmicMarketMessage::AskUpdate {
                        symbol: symbol.to_string(),
                        timestamp,
                        price,
                        size,
                    })
                });

                match (bid, ask) {
                    (Some(bid_result), Some(ask_result)) => {
                        // Return bid now; queue ask as pending for next validate call
                        let ask_msg = ask_result?;
                        let ask_symbol_id = self.symbol_map.get_or_insert(ask_msg.symbol());
                        if let RithmicMarketMessage::AskUpdate {
                            timestamp,
                            price,
                            size,
                            ..
                        } = &ask_msg
                        {
                            self.pending_event = Some(MarketEvent {
                                timestamp: *timestamp,
                                symbol_id: ask_symbol_id,
                                sequence: 0,
                                event_type: MarketEventType::AskUpdate,
                                price: *price,
                                size: *size,
                                side: Some(Side::Sell),
                            });
                        }
                        bid_result
                    }
                    (Some(bid_result), None) => bid_result,
                    (None, Some(ask_result)) => ask_result,
                    (None, None) => Err(ValidationError::Malformed(
                        "BBO has neither bid_price nor ask_price".into(),
                    )),
                }
            }
            _ => Err(ValidationError::Irrelevant),
        }
    }

    /// Validate a RithmicMessage, converting it to a MarketEvent.
    /// Tracks malformed messages and triggers circuit break when threshold exceeded.
    /// Call take_pending() after each validate() to drain any queued ask event from dual-sided BBO.
    pub(crate) fn validate(
        &mut self,
        message: &RithmicMessage,
        now_nanos: u64,
    ) -> Result<MarketEvent, ValidationError> {
        let parsed = match self.parse_rithmic_message(message) {
            Ok(msg) => msg,
            Err(ValidationError::Irrelevant) => return Err(ValidationError::Irrelevant),
            Err(ValidationError::Malformed(reason)) => {
                warn!(reason = %reason, "malformed Rithmic message");
                self.record_malformed(now_nanos)?;
                return Err(ValidationError::Malformed(reason));
            }
            Err(e) => return Err(e),
        };

        let symbol_id = self.symbol_map.get_or_insert(parsed.symbol());

        let event = match parsed {
            RithmicMarketMessage::Trade {
                timestamp,
                price,
                size,
                aggressor,
                ..
            } => MarketEvent {
                timestamp,
                symbol_id,
                sequence: 0,
                event_type: MarketEventType::Trade,
                price,
                size,
                side: aggressor,
            },
            RithmicMarketMessage::BidUpdate {
                timestamp,
                price,
                size,
                ..
            } => MarketEvent {
                timestamp,
                symbol_id,
                sequence: 0,
                event_type: MarketEventType::BidUpdate,
                price,
                size,
                side: Some(Side::Buy),
            },
            RithmicMarketMessage::AskUpdate {
                timestamp,
                price,
                size,
                ..
            } => MarketEvent {
                timestamp,
                symbol_id,
                sequence: 0,
                event_type: MarketEventType::AskUpdate,
                price,
                size,
                side: Some(Side::Sell),
            },
        };

        Ok(event)
    }

    /// Record a malformed message timestamp and check for circuit break.
    fn record_malformed(&mut self, now_nanos: u64) -> Result<(), ValidationError> {
        self.malformed_timestamps.push_back(now_nanos);
        self.prune_window(now_nanos);

        let count = self.malformed_timestamps.len();
        if count > MALFORMED_CIRCUIT_BREAK_THRESHOLD {
            warn!(
                count,
                "circuit break triggered: too many malformed messages in 60s window"
            );
            return Err(ValidationError::CircuitBreak { count });
        }
        Ok(())
    }

    /// Remove timestamps older than the 60-second window.
    fn prune_window(&mut self, now_nanos: u64) {
        let window_nanos = MALFORMED_WINDOW_SECS * 1_000_000_000;
        let cutoff = now_nanos.saturating_sub(window_nanos);
        while let Some(&front) = self.malformed_timestamps.front() {
            if front < cutoff {
                self.malformed_timestamps.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn malformed_count_in_window(&self) -> usize {
        self.malformed_timestamps.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rithmic_rs::rti::LastTrade;

    const NANOS_PER_SEC: u64 = 1_000_000_000;

    fn make_last_trade(
        symbol: Option<&str>,
        price: Option<f64>,
        size: Option<i32>,
        ssboe: Option<i32>,
    ) -> RithmicMessage {
        RithmicMessage::LastTrade(LastTrade {
            template_id: 150,
            symbol: symbol.map(|s| s.to_string()),
            exchange: Some("CME".to_string()),
            presence_bits: None,
            clear_bits: None,
            is_snapshot: None,
            trade_price: price,
            trade_size: size,
            aggressor: Some(1), // Buy
            exchange_order_id: None,
            aggressor_exchange_order_id: None,
            net_change: None,
            percent_change: None,
            volume: None,
            vwap: None,
            trade_time: None,
            ssboe,
            usecs: Some(500_000),
            source_ssboe: None,
            source_usecs: None,
            source_nsecs: None,
            jop_ssboe: None,
            jop_nsecs: None,
        })
    }

    #[test]
    fn validate_well_formed_trade() {
        let mut validator = MessageValidator::new();
        let msg = make_last_trade(Some("MES"), Some(4500.25), Some(10), Some(1_704_067_200));
        let now = 1_704_067_200 * NANOS_PER_SEC;

        let event = validator.validate(&msg, now).unwrap();
        assert_eq!(event.event_type, MarketEventType::Trade);
        assert_eq!(event.size, 10);
        assert_eq!(event.side, Some(Side::Buy));
    }

    #[test]
    fn validate_malformed_missing_symbol() {
        let mut validator = MessageValidator::new();
        let msg = make_last_trade(None, Some(4500.25), Some(10), Some(1_704_067_200));
        let now = 1_704_067_200 * NANOS_PER_SEC;

        let result = validator.validate(&msg, now);
        assert!(matches!(result, Err(ValidationError::Malformed(_))));
    }

    #[test]
    fn validate_malformed_missing_price() {
        let mut validator = MessageValidator::new();
        let msg = make_last_trade(Some("MES"), None, Some(10), Some(1_704_067_200));
        let now = 1_704_067_200 * NANOS_PER_SEC;

        let result = validator.validate(&msg, now);
        assert!(matches!(result, Err(ValidationError::Malformed(_))));
    }

    #[test]
    fn circuit_break_after_threshold() {
        let mut validator = MessageValidator::new();
        let base_nanos = 1_704_067_200 * NANOS_PER_SEC;

        // Send 11 malformed messages within 60s — 11th triggers circuit break
        for i in 0..=10 {
            let msg = make_last_trade(None, Some(4500.25), Some(10), Some(1_704_067_200));
            let now = base_nanos + i * NANOS_PER_SEC; // 1 second apart
            let result = validator.validate(&msg, now);

            if i < 10 {
                // First 10 are just Malformed (threshold is >10, so up to 10 inclusive)
                assert!(
                    matches!(result, Err(ValidationError::Malformed(_))),
                    "iteration {i}: expected Malformed"
                );
            } else {
                // 11th triggers circuit break
                assert!(
                    matches!(result, Err(ValidationError::CircuitBreak { .. })),
                    "iteration {i}: expected CircuitBreak"
                );
            }
        }
    }

    #[test]
    fn sliding_window_no_circuit_break_when_spread_beyond_60s() {
        let mut validator = MessageValidator::new();
        let base_nanos = 1_704_067_200 * NANOS_PER_SEC;

        // Send 20 malformed messages, each 7 seconds apart (total 140s)
        // Window of 60s should never contain more than ~9 messages
        for i in 0..20 {
            let msg = make_last_trade(None, Some(4500.25), Some(10), Some(1_704_067_200));
            let now = base_nanos + i * 7 * NANOS_PER_SEC;
            let result = validator.validate(&msg, now);

            // Should never trigger circuit break
            assert!(
                !matches!(result, Err(ValidationError::CircuitBreak { .. })),
                "iteration {i}: unexpected CircuitBreak, window count: {}",
                validator.malformed_count_in_window()
            );
        }
    }

    #[test]
    fn irrelevant_message_type_returns_irrelevant() {
        let mut validator = MessageValidator::new();
        let msg = RithmicMessage::ConnectionError;
        let result = validator.validate(&msg, 0);
        assert!(matches!(result, Err(ValidationError::Irrelevant)));
    }

    #[test]
    fn validate_bbo_bid_update() {
        let mut validator = MessageValidator::new();
        let msg = RithmicMessage::BestBidOffer(rithmic_rs::rti::BestBidOffer {
            template_id: 151,
            symbol: Some("MNQ".into()),
            exchange: Some("CME".into()),
            presence_bits: None,
            clear_bits: None,
            is_snapshot: None,
            bid_price: Some(15000.50),
            bid_size: Some(25),
            bid_orders: None,
            bid_implicit_size: None,
            bid_time: None,
            ask_price: None,
            ask_size: None,
            ask_orders: None,
            ask_implicit_size: None,
            ask_time: None,
            lean_price: None,
            ssboe: Some(1_704_067_200),
            usecs: Some(0),
        });
        let now = 1_704_067_200 * 1_000_000_000u64;

        let event = validator.validate(&msg, now).unwrap();
        assert_eq!(event.event_type, MarketEventType::BidUpdate);
        assert_eq!(event.side, Some(Side::Buy));
        assert_eq!(event.size, 25);
    }

    #[test]
    fn validate_bbo_ask_update() {
        let mut validator = MessageValidator::new();
        let msg = RithmicMessage::BestBidOffer(rithmic_rs::rti::BestBidOffer {
            template_id: 151,
            symbol: Some("MNQ".into()),
            exchange: Some("CME".into()),
            presence_bits: None,
            clear_bits: None,
            is_snapshot: None,
            bid_price: None,
            bid_size: None,
            bid_orders: None,
            bid_implicit_size: None,
            bid_time: None,
            ask_price: Some(15001.00),
            ask_size: Some(30),
            ask_orders: None,
            ask_implicit_size: None,
            ask_time: None,
            lean_price: None,
            ssboe: Some(1_704_067_200),
            usecs: Some(0),
        });
        let now = 1_704_067_200 * 1_000_000_000u64;

        let event = validator.validate(&msg, now).unwrap();
        assert_eq!(event.event_type, MarketEventType::AskUpdate);
        assert_eq!(event.side, Some(Side::Sell));
        assert_eq!(event.size, 30);
    }

    #[test]
    fn validate_bbo_both_sides_emits_bid_then_pending_ask() {
        let mut validator = MessageValidator::new();
        let msg = RithmicMessage::BestBidOffer(rithmic_rs::rti::BestBidOffer {
            template_id: 151,
            symbol: Some("MES".into()),
            exchange: Some("CME".into()),
            presence_bits: None,
            clear_bits: None,
            is_snapshot: None,
            bid_price: Some(4500.00),
            bid_size: Some(50),
            bid_orders: None,
            bid_implicit_size: None,
            bid_time: None,
            ask_price: Some(4500.25),
            ask_size: Some(40),
            ask_orders: None,
            ask_implicit_size: None,
            ask_time: None,
            lean_price: None,
            ssboe: Some(1_704_067_200),
            usecs: Some(0),
        });
        let now = 1_704_067_200 * 1_000_000_000u64;

        // First call returns bid
        let bid_event = validator.validate(&msg, now).unwrap();
        assert_eq!(bid_event.event_type, MarketEventType::BidUpdate);
        assert_eq!(bid_event.side, Some(Side::Buy));
        assert_eq!(bid_event.size, 50);

        // Pending event should be the ask
        let ask_event = validator
            .take_pending()
            .expect("should have pending ask event");
        assert_eq!(ask_event.event_type, MarketEventType::AskUpdate);
        assert_eq!(ask_event.side, Some(Side::Sell));
        assert_eq!(ask_event.size, 40);

        // No more pending
        assert!(validator.take_pending().is_none());
    }

    #[test]
    fn validate_negative_trade_size_is_malformed() {
        let mut validator = MessageValidator::new();
        let msg = RithmicMessage::LastTrade(LastTrade {
            template_id: 150,
            symbol: Some("MES".to_string()),
            exchange: Some("CME".to_string()),
            presence_bits: None,
            clear_bits: None,
            is_snapshot: None,
            trade_price: Some(4500.25),
            trade_size: Some(-5),
            aggressor: Some(1),
            exchange_order_id: None,
            aggressor_exchange_order_id: None,
            net_change: None,
            percent_change: None,
            volume: None,
            vwap: None,
            trade_time: None,
            ssboe: Some(1_704_067_200),
            usecs: Some(0),
            source_ssboe: None,
            source_usecs: None,
            source_nsecs: None,
            jop_ssboe: None,
            jop_nsecs: None,
        });
        let now = 1_704_067_200 * 1_000_000_000u64;

        let result = validator.validate(&msg, now);
        assert!(matches!(result, Err(ValidationError::Malformed(ref m)) if m.contains("negative")));
    }
}
