use futures_bmad_core::{
    Clock, FixedPrice, MarketEvent, OrderBook, Signal, SignalSnapshot, UnixNanos,
};

/// Volume-weighted mid-price signal.
///
/// Computes `(bid_price * ask_size + ask_price * bid_size) / (bid_size + ask_size)`
/// using top-of-book only. When ask size dominates, microprice moves toward bid
/// (large ask = selling pressure). O(1), zero allocation.
pub struct MicropriceSignal {
    value: Option<f64>,
    valid: bool,
    max_spread: FixedPrice,
    last_update_ts: UnixNanos,
}

impl MicropriceSignal {
    pub fn new(max_spread: FixedPrice) -> Self {
        Self {
            value: None,
            valid: false,
            max_spread,
            last_update_ts: UnixNanos::default(),
        }
    }
}

impl Signal for MicropriceSignal {
    fn update(
        &mut self,
        book: &OrderBook,
        _trade: Option<&MarketEvent>,
        clock: &dyn Clock,
    ) -> Option<f64> {
        if !book.is_tradeable(self.max_spread) {
            self.value = None;
            self.valid = false;
            return None;
        }

        if book.bid_count == 0 || book.ask_count == 0 {
            self.value = None;
            self.valid = false;
            return None;
        }

        let bid_price = book.bids[0].price.to_f64();
        let bid_size = book.bids[0].size as f64;
        let ask_price = book.asks[0].price.to_f64();
        let ask_size = book.asks[0].size as f64;

        if bid_size == 0.0 || ask_size == 0.0 {
            self.value = None;
            self.valid = false;
            return None;
        }

        let denom = bid_size + ask_size;
        let microprice = (bid_price * ask_size + ask_price * bid_size) / denom;

        if !microprice.is_finite() {
            self.value = None;
            self.valid = false;
            return None;
        }

        self.value = Some(microprice);
        self.valid = true;
        self.last_update_ts = clock.now();
        Some(microprice)
    }

    fn name(&self) -> &'static str {
        "microprice"
    }

    fn is_valid(&self) -> bool {
        self.valid && self.value.is_some()
    }

    fn reset(&mut self) {
        self.value = None;
        self.valid = false;
        self.last_update_ts = UnixNanos::default();
    }

    fn snapshot(&self) -> SignalSnapshot {
        SignalSnapshot {
            name: "microprice",
            value: self.value,
            valid: self.is_valid(),
            timestamp: self.last_update_ts,
        }
    }
}
