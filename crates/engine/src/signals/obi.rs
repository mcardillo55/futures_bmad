use futures_bmad_core::{
    Clock, FixedPrice, MarketEvent, OrderBook, Signal, SignalSnapshot, UnixNanos,
};

/// Order Book Imbalance signal.
///
/// Computes `(total_bid_size - total_ask_size) / (total_bid_size + total_ask_size)`
/// across all populated book levels. Result is in [-1.0, 1.0] where positive
/// indicates bid pressure and negative indicates ask pressure.
///
/// O(1) computation — OrderBook has fixed depth (10 levels max).
/// Zero heap allocation.
pub struct ObiSignal {
    value: Option<f64>,
    update_count: u64,
    warmup_period: u64,
    max_spread: FixedPrice,
    last_update_ts: UnixNanos,
}

impl ObiSignal {
    pub fn new(warmup_period: u64, max_spread: FixedPrice) -> Self {
        Self {
            value: None,
            update_count: 0,
            warmup_period,
            max_spread,
            last_update_ts: UnixNanos::default(),
        }
    }
}

impl Signal for ObiSignal {
    fn update(
        &mut self,
        book: &OrderBook,
        _trade: Option<&MarketEvent>,
        clock: &dyn Clock,
    ) -> Option<f64> {
        if !book.is_tradeable(self.max_spread) {
            self.value = None;
            return None;
        }

        if book.bid_count == 0 || book.ask_count == 0 {
            self.value = None;
            return None;
        }

        let num_bids = book.bid_count as usize;
        let num_asks = book.ask_count as usize;

        let total_bid: u64 = book.bids[..num_bids].iter().map(|l| l.size as u64).sum();
        let total_ask: u64 = book.asks[..num_asks].iter().map(|l| l.size as u64).sum();

        let denom = total_bid + total_ask;
        if denom == 0 {
            self.value = None;
            return None;
        }

        let obi = (total_bid as f64 - total_ask as f64) / denom as f64;

        if !obi.is_finite() {
            self.value = None;
            return None;
        }

        self.value = Some(obi);
        self.update_count += 1;
        self.last_update_ts = clock.now();

        if self.update_count >= self.warmup_period {
            Some(obi)
        } else {
            None
        }
    }

    fn name(&self) -> &'static str {
        "obi"
    }

    fn is_valid(&self) -> bool {
        self.update_count >= self.warmup_period && self.value.is_some()
    }

    fn reset(&mut self) {
        self.value = None;
        self.update_count = 0;
        self.last_update_ts = UnixNanos::default();
    }

    fn snapshot(&self) -> SignalSnapshot {
        SignalSnapshot {
            name: "obi",
            value: self.value,
            valid: self.is_valid(),
            timestamp: self.last_update_ts,
        }
    }
}
