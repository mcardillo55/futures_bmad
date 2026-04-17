use futures_core::{FixedPrice, Level, OrderBook};
use proptest::prelude::*;

fn arb_level(price_raw: i64, size: u32) -> Level {
    Level { price: FixedPrice::new(price_raw), size, order_count: 1 }
}

proptest! {
    #[test]
    fn not_tradeable_with_few_bids(
        ask_prices in prop::array::uniform3(1i64..1000),
    ) {
        let mut book = OrderBook::empty();
        // Only 2 bids
        book.update_bid(0, arb_level(100, 10));
        book.update_bid(1, arb_level(99, 5));
        // 3 asks ascending
        let mut sorted = ask_prices;
        sorted.sort();
        for (i, &p) in sorted.iter().enumerate() {
            book.update_ask(i, arb_level(200 + p, 10));
        }
        prop_assert!(!book.is_tradeable(FixedPrice::new(i64::MAX)));
    }

    #[test]
    fn not_tradeable_with_few_asks(
        bid_prices in prop::array::uniform3(1i64..1000),
    ) {
        let mut book = OrderBook::empty();
        // 3 bids descending
        let mut sorted = bid_prices;
        sorted.sort();
        sorted.reverse();
        for (i, &p) in sorted.iter().enumerate() {
            book.update_bid(i, arb_level(p, 10));
        }
        // Only 2 asks
        book.update_ask(0, arb_level(1001, 10));
        book.update_ask(1, arb_level(1002, 5));
        prop_assert!(!book.is_tradeable(FixedPrice::new(i64::MAX)));
    }

    #[test]
    fn not_tradeable_non_monotonic_bids(
        base in 100i64..10000,
    ) {
        let mut book = OrderBook::empty();
        // Non-descending bids: second >= first
        book.update_bid(0, arb_level(base, 10));
        book.update_bid(1, arb_level(base + 1, 5)); // wrong: ascending
        book.update_bid(2, arb_level(base - 1, 3));
        book.update_ask(0, arb_level(base + 10, 10));
        book.update_ask(1, arb_level(base + 11, 5));
        book.update_ask(2, arb_level(base + 12, 3));
        prop_assert!(!book.is_tradeable(FixedPrice::new(i64::MAX)));
    }

    #[test]
    fn best_bid_is_highest(
        prices in prop::array::uniform5(1i64..100000),
    ) {
        let mut book = OrderBook::empty();
        let mut sorted = prices;
        sorted.sort();
        sorted.reverse();
        for (i, &p) in sorted.iter().enumerate() {
            book.update_bid(i, arb_level(p, 10));
        }
        let best = book.best_bid().unwrap();
        prop_assert_eq!(best.price.raw(), sorted[0]);
    }

    #[test]
    fn spread_non_negative_for_valid_books(
        bid_base in 100i64..10000,
        ask_offset in 1i64..1000,
    ) {
        let mut book = OrderBook::empty();
        book.update_bid(0, arb_level(bid_base, 10));
        book.update_ask(0, arb_level(bid_base + ask_offset, 10));
        let spread = book.spread().unwrap();
        prop_assert!(spread.raw() >= 0);
    }
}
