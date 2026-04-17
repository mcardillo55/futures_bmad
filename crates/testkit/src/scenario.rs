use futures_core::{MarketEvent, OrderBook};

pub struct Scenario {
    pub name: &'static str,
    pub books: Vec<OrderBook>,
    pub events: Vec<MarketEvent>,
}

impl Scenario {
    pub fn normal_trading() -> Self {
        Self {
            name: "normal_trading",
            books: vec![crate::market_gen::normal_trading_book()],
            events: Vec::new(),
        }
    }

    pub fn fomc_spike() -> Self {
        Self {
            name: "fomc_volatility_spike",
            books: crate::market_gen::fomc_volatility_spike(),
            events: Vec::new(),
        }
    }

    pub fn flash_crash() -> Self {
        Self {
            name: "flash_crash",
            books: crate::market_gen::flash_crash_sequence(),
            events: Vec::new(),
        }
    }

    pub fn reconnection() -> Self {
        Self {
            name: "reconnection_gap",
            books: crate::market_gen::reconnection_gap(),
            events: Vec::new(),
        }
    }
}
