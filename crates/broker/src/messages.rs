use futures_bmad_core::{FixedPrice, Side, UnixNanos};

/// Internal representation of a raw Rithmic market data message.
/// All Rithmic/R|Protocol details are isolated here — core and engine never see these types.
#[derive(Debug, Clone)]
pub(crate) enum RithmicMarketMessage {
    Trade {
        symbol: String,
        timestamp: UnixNanos,
        price: FixedPrice,
        size: u32,
        aggressor: Option<Side>,
    },
    BidUpdate {
        symbol: String,
        timestamp: UnixNanos,
        price: FixedPrice,
        size: u32,
    },
    AskUpdate {
        symbol: String,
        timestamp: UnixNanos,
        price: FixedPrice,
        size: u32,
    },
}

impl RithmicMarketMessage {
    pub fn symbol(&self) -> &str {
        match self {
            Self::Trade { symbol, .. }
            | Self::BidUpdate { symbol, .. }
            | Self::AskUpdate { symbol, .. } => symbol,
        }
    }

    #[allow(dead_code)]
    pub fn timestamp(&self) -> UnixNanos {
        match self {
            Self::Trade { timestamp, .. }
            | Self::BidUpdate { timestamp, .. }
            | Self::AskUpdate { timestamp, .. } => *timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_symbol_accessor() {
        let msg = RithmicMarketMessage::Trade {
            symbol: "MES".into(),
            timestamp: UnixNanos::new(1000),
            price: FixedPrice::from_f64(4500.25).unwrap(),
            size: 10,
            aggressor: Some(Side::Buy),
        };
        assert_eq!(msg.symbol(), "MES");
    }

    #[test]
    fn message_timestamp_accessor() {
        let ts = UnixNanos::new(123_456_789);
        let msg = RithmicMarketMessage::BidUpdate {
            symbol: "MNQ".into(),
            timestamp: ts,
            price: FixedPrice::from_f64(15000.0).unwrap(),
            size: 5,
        };
        assert_eq!(msg.timestamp(), ts);
    }
}
