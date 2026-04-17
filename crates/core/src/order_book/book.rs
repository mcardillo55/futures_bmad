use crate::types::{FixedPrice, UnixNanos};

/// A single price level in the order book.
#[derive(Debug, Clone, Copy, Default)]
pub struct Level {
    pub price: FixedPrice,
    pub size: u32,
    pub order_count: u16,
}

impl Level {
    pub fn empty() -> Self {
        Self::default()
    }
}

const BOOK_DEPTH: usize = 10;

/// Stack-allocated order book with fixed-size arrays.
///
/// Bids are stored in descending price order (highest first).
/// Asks are stored in ascending price order (lowest first).
#[derive(Debug, Clone, Copy)]
pub struct OrderBook {
    pub bids: [Level; BOOK_DEPTH],
    pub asks: [Level; BOOK_DEPTH],
    pub bid_count: u8,
    pub ask_count: u8,
    pub timestamp: UnixNanos,
}

impl OrderBook {
    pub fn empty() -> Self {
        Self {
            bids: [Level::empty(); BOOK_DEPTH],
            asks: [Level::empty(); BOOK_DEPTH],
            bid_count: 0,
            ask_count: 0,
            timestamp: UnixNanos::default(),
        }
    }

    pub fn update_bid(&mut self, index: usize, level: Level) {
        if index >= BOOK_DEPTH {
            return;
        }
        self.bids[index] = level;
        if index as u8 >= self.bid_count {
            self.bid_count = index as u8 + 1;
        }
    }

    pub fn update_ask(&mut self, index: usize, level: Level) {
        if index >= BOOK_DEPTH {
            return;
        }
        self.asks[index] = level;
        if index as u8 >= self.ask_count {
            self.ask_count = index as u8 + 1;
        }
    }

    pub fn best_bid(&self) -> Option<&Level> {
        if self.bid_count > 0 { Some(&self.bids[0]) } else { None }
    }

    pub fn best_ask(&self) -> Option<&Level> {
        if self.ask_count > 0 { Some(&self.asks[0]) } else { None }
    }

    pub fn mid_price(&self) -> Option<FixedPrice> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        let sum = bid.price.saturating_add(ask.price);
        Some(FixedPrice::new(sum.raw() / 2))
    }

    pub fn spread(&self) -> Option<FixedPrice> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(ask.price.saturating_sub(bid.price))
    }

    pub fn is_tradeable(&self, max_spread_threshold: FixedPrice) -> bool {
        if self.bid_count < 3 || self.ask_count < 3 {
            return false;
        }

        let spread = match self.spread() {
            Some(s) => s,
            None => return false,
        };
        if spread.raw() > max_spread_threshold.raw() {
            return false;
        }

        if self.bids[0].size == 0 || self.asks[0].size == 0 {
            return false;
        }

        // Check bids descending
        for i in 1..self.bid_count as usize {
            if self.bids[i].price.raw() >= self.bids[i - 1].price.raw() {
                return false;
            }
        }

        // Check asks ascending
        for i in 1..self.ask_count as usize {
            if self.asks[i].price.raw() <= self.asks[i - 1].price.raw() {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_level(price_raw: i64, size: u32) -> Level {
        Level { price: FixedPrice::new(price_raw), size, order_count: 1 }
    }

    fn make_valid_book() -> OrderBook {
        let mut book = OrderBook::empty();
        // Bids descending: 100, 99, 98
        book.update_bid(0, make_level(100, 10));
        book.update_bid(1, make_level(99, 5));
        book.update_bid(2, make_level(98, 3));
        // Asks ascending: 101, 102, 103
        book.update_ask(0, make_level(101, 10));
        book.update_ask(1, make_level(102, 5));
        book.update_ask(2, make_level(103, 3));
        book
    }

    #[test]
    fn empty_book() {
        let book = OrderBook::empty();
        assert_eq!(book.bid_count, 0);
        assert_eq!(book.ask_count, 0);
        assert!(book.best_bid().is_none());
        assert!(book.best_ask().is_none());
    }

    #[test]
    fn update_bid_ask() {
        let mut book = OrderBook::empty();
        book.update_bid(0, make_level(100, 10));
        assert_eq!(book.bid_count, 1);
        assert_eq!(book.best_bid().unwrap().price.raw(), 100);

        book.update_ask(0, make_level(101, 5));
        assert_eq!(book.ask_count, 1);
        assert_eq!(book.best_ask().unwrap().price.raw(), 101);
    }

    #[test]
    fn mid_price_calculation() {
        let book = make_valid_book();
        let mid = book.mid_price().unwrap();
        // (100 + 101) / 2 = 100 (integer division)
        assert_eq!(mid.raw(), 100);
    }

    #[test]
    fn spread_calculation() {
        let book = make_valid_book();
        let spread = book.spread().unwrap();
        assert_eq!(spread.raw(), 1); // 101 - 100
    }

    #[test]
    fn is_tradeable_valid_book() {
        let book = make_valid_book();
        assert!(book.is_tradeable(FixedPrice::new(4))); // spread=1 <= threshold=4
    }

    #[test]
    fn is_tradeable_too_few_levels() {
        let mut book = OrderBook::empty();
        book.update_bid(0, make_level(100, 10));
        book.update_bid(1, make_level(99, 5));
        book.update_ask(0, make_level(101, 10));
        book.update_ask(1, make_level(102, 5));
        book.update_ask(2, make_level(103, 3));
        assert!(!book.is_tradeable(FixedPrice::new(4)));
    }

    #[test]
    fn is_tradeable_spread_too_wide() {
        let book = make_valid_book();
        assert!(!book.is_tradeable(FixedPrice::new(0))); // threshold=0 < spread=1
    }

    #[test]
    fn is_tradeable_non_monotonic_bids() {
        let mut book = make_valid_book();
        // Make bids non-descending
        book.bids[1] = make_level(101, 5); // 101 > 100, wrong order
        assert!(!book.is_tradeable(FixedPrice::new(4)));
    }

    #[test]
    fn update_out_of_bounds() {
        let mut book = OrderBook::empty();
        book.update_bid(10, make_level(100, 10)); // should not panic
        assert_eq!(book.bid_count, 0);
        book.update_ask(15, make_level(101, 5));
        assert_eq!(book.ask_count, 0);
    }
}
