use futures_bmad_core::{Level, MarketEvent, MarketEventType, OrderBook};

/// Apply a MarketEvent to an OrderBook in-place (no allocation).
/// L1 updates (BidUpdate/AskUpdate) update the best bid/ask level.
/// Trade events update the last-trade information but don't change the book.
pub fn apply_market_event(book: &mut OrderBook, event: &MarketEvent) {
    book.timestamp = event.timestamp;

    match event.event_type {
        MarketEventType::BidUpdate => {
            let level = Level {
                price: event.price,
                size: event.size,
                order_count: 0,
            };
            book.update_bid(0, level);
        }
        MarketEventType::AskUpdate => {
            let level = Level {
                price: event.price,
                size: event.size,
                order_count: 0,
            };
            book.update_ask(0, level);
        }
        MarketEventType::BookSnapshot => {
            // Full book snapshots would replace the entire book.
            // For L1-only feeds, this is handled as a combined bid+ask update.
            let level = Level {
                price: event.price,
                size: event.size,
                order_count: 0,
            };
            match event.side {
                Some(futures_bmad_core::Side::Buy) => book.update_bid(0, level),
                Some(futures_bmad_core::Side::Sell) => book.update_ask(0, level),
                None => {}
            }
        }
        MarketEventType::Trade => {
            // Trades don't update the order book directly.
            // They may be consumed by signals (VPIN, etc.) in future stories.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_bmad_core::{FixedPrice, Side, UnixNanos};

    fn make_event(
        event_type: MarketEventType,
        price_raw: i64,
        size: u32,
        side: Option<Side>,
    ) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(1_000_000_000),
            symbol_id: 0,
            sequence: 0,
            event_type,
            price: FixedPrice::new(price_raw),
            size,
            side,
        }
    }

    #[test]
    fn bid_update_sets_best_bid() {
        let mut book = OrderBook::empty();
        let event = make_event(MarketEventType::BidUpdate, 18000, 50, Some(Side::Buy));
        apply_market_event(&mut book, &event);

        assert_eq!(book.bid_count, 1);
        assert_eq!(book.best_bid().unwrap().price.raw(), 18000);
        assert_eq!(book.best_bid().unwrap().size, 50);
    }

    #[test]
    fn ask_update_sets_best_ask() {
        let mut book = OrderBook::empty();
        let event = make_event(MarketEventType::AskUpdate, 18004, 30, Some(Side::Sell));
        apply_market_event(&mut book, &event);

        assert_eq!(book.ask_count, 1);
        assert_eq!(book.best_ask().unwrap().price.raw(), 18004);
        assert_eq!(book.best_ask().unwrap().size, 30);
    }

    #[test]
    fn trade_does_not_change_book() {
        let mut book = OrderBook::empty();
        let event = make_event(MarketEventType::Trade, 18002, 10, Some(Side::Buy));
        apply_market_event(&mut book, &event);

        assert_eq!(book.bid_count, 0);
        assert_eq!(book.ask_count, 0);
    }

    #[test]
    fn sequential_updates_overwrite_in_place() {
        let mut book = OrderBook::empty();

        // First bid
        apply_market_event(
            &mut book,
            &make_event(MarketEventType::BidUpdate, 18000, 50, Some(Side::Buy)),
        );
        assert_eq!(book.best_bid().unwrap().price.raw(), 18000);

        // Updated bid at same level
        apply_market_event(
            &mut book,
            &make_event(MarketEventType::BidUpdate, 18004, 75, Some(Side::Buy)),
        );
        assert_eq!(book.best_bid().unwrap().price.raw(), 18004);
        assert_eq!(book.best_bid().unwrap().size, 75);
        assert_eq!(book.bid_count, 1); // still 1 level
    }

    #[test]
    fn timestamp_updates_on_every_event() {
        let mut book = OrderBook::empty();
        let mut event = make_event(MarketEventType::Trade, 18000, 10, None);
        event.timestamp = UnixNanos::new(999);
        apply_market_event(&mut book, &event);
        assert_eq!(book.timestamp, UnixNanos::new(999));
    }
}
