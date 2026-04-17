use futures_core::{FixedPrice, Level, OrderBook, UnixNanos};

/// Fluent builder for OrderBook in tests. Uses f64 prices for ergonomics.
pub struct OrderBookBuilder {
    bids: Vec<Level>,
    asks: Vec<Level>,
    timestamp: UnixNanos,
}

impl OrderBookBuilder {
    pub fn new() -> Self {
        Self { bids: Vec::new(), asks: Vec::new(), timestamp: UnixNanos::default() }
    }

    pub fn bid(mut self, price: f64, size: u32) -> Self {
        self.bids.push(Level {
            price: FixedPrice::from_f64(price),
            size,
            order_count: 1,
        });
        self
    }

    pub fn bid_with_count(mut self, price: f64, size: u32, count: u16) -> Self {
        self.bids.push(Level {
            price: FixedPrice::from_f64(price),
            size,
            order_count: count,
        });
        self
    }

    pub fn ask(mut self, price: f64, size: u32) -> Self {
        self.asks.push(Level {
            price: FixedPrice::from_f64(price),
            size,
            order_count: 1,
        });
        self
    }

    pub fn ask_with_count(mut self, price: f64, size: u32, count: u16) -> Self {
        self.asks.push(Level {
            price: FixedPrice::from_f64(price),
            size,
            order_count: count,
        });
        self
    }

    pub fn timestamp(mut self, nanos: u64) -> Self {
        self.timestamp = UnixNanos::new(nanos);
        self
    }

    pub fn build(self) -> OrderBook {
        assert!(self.bids.len() <= 10, "max 10 bid levels");
        assert!(self.asks.len() <= 10, "max 10 ask levels");

        // Validate bids descending
        for i in 1..self.bids.len() {
            assert!(
                self.bids[i].price.raw() < self.bids[i - 1].price.raw(),
                "bids must be in descending price order: {} >= {}",
                self.bids[i].price,
                self.bids[i - 1].price
            );
        }

        // Validate asks ascending
        for i in 1..self.asks.len() {
            assert!(
                self.asks[i].price.raw() > self.asks[i - 1].price.raw(),
                "asks must be in ascending price order: {} <= {}",
                self.asks[i].price,
                self.asks[i - 1].price
            );
        }

        let mut book = OrderBook::empty();
        book.timestamp = self.timestamp;

        for (i, level) in self.bids.iter().enumerate() {
            book.update_bid(i, *level);
        }
        for (i, level) in self.asks.iter().enumerate() {
            book.update_ask(i, *level);
        }

        book
    }
}

impl Default for OrderBookBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_build() {
        let book = OrderBookBuilder::new()
            .bid(4500.00, 50)
            .bid(4499.75, 30)
            .ask(4500.25, 40)
            .ask(4500.50, 20)
            .build();

        assert_eq!(book.bid_count, 2);
        assert_eq!(book.ask_count, 2);
        assert_eq!(book.best_bid().unwrap().price, FixedPrice::from_f64(4500.00));
        assert_eq!(book.best_ask().unwrap().price, FixedPrice::from_f64(4500.25));
    }

    #[test]
    #[should_panic(expected = "bids must be in descending")]
    fn non_monotonic_bids_panic() {
        OrderBookBuilder::new().bid(4500.00, 50).bid(4500.25, 30).build();
    }

    #[test]
    #[should_panic(expected = "asks must be in ascending")]
    fn non_monotonic_asks_panic() {
        OrderBookBuilder::new().ask(4500.50, 40).ask(4500.25, 20).build();
    }

    #[test]
    #[should_panic(expected = "max 10 bid levels")]
    fn max_levels_enforced() {
        let mut builder = OrderBookBuilder::new();
        for i in 0..11 {
            builder = builder.bid(4500.0 - i as f64 * 0.25, 10);
        }
        builder.build();
    }
}
