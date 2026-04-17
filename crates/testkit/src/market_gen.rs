use crate::book_builder::OrderBookBuilder;
use futures_bmad_core::OrderBook;

/// Normal trading conditions: tight spread, deep book around ES ~4500.
pub fn normal_trading_book() -> OrderBook {
    OrderBookBuilder::new()
        .bid(4500.00, 150)
        .bid(4499.75, 120)
        .bid(4499.50, 80)
        .bid(4499.25, 60)
        .bid(4499.00, 40)
        .ask(4500.25, 140)
        .ask(4500.50, 110)
        .ask(4500.75, 70)
        .ask(4501.00, 50)
        .ask(4501.25, 30)
        .build()
}

/// FOMC volatility spike: spread widens, levels thin out, price moves rapidly.
pub fn fomc_volatility_spike() -> Vec<OrderBook> {
    vec![
        // Pre-announcement: normal
        normal_trading_book(),
        // Announcement: spread widens, thin levels
        OrderBookBuilder::new()
            .bid(4498.00, 20)
            .bid(4497.50, 15)
            .bid(4497.00, 10)
            .ask(4502.00, 25)
            .ask(4502.50, 15)
            .ask(4503.00, 10)
            .timestamp(1)
            .build(),
        // Post-spike: price moved up, recovering
        OrderBookBuilder::new()
            .bid(4510.00, 80)
            .bid(4509.75, 60)
            .bid(4509.50, 40)
            .ask(4510.50, 70)
            .ask(4510.75, 50)
            .ask(4511.00, 30)
            .timestamp(2)
            .build(),
    ]
}

/// Flash crash: bid side collapses, massive spread, then recovery.
pub fn flash_crash_sequence() -> Vec<OrderBook> {
    vec![
        // Normal book
        normal_trading_book(),
        // Bid collapse
        OrderBookBuilder::new()
            .bid(4480.00, 5)
            .bid(4479.00, 3)
            .bid(4478.00, 2)
            .ask(4500.25, 200)
            .ask(4500.50, 150)
            .ask(4500.75, 100)
            .timestamp(1)
            .build(),
        // Recovery
        OrderBookBuilder::new()
            .bid(4495.00, 100)
            .bid(4494.75, 80)
            .bid(4494.50, 60)
            .ask(4496.00, 90)
            .ask(4496.25, 70)
            .ask(4496.50, 50)
            .timestamp(2)
            .build(),
    ]
}

/// Empty book: no levels, not tradeable.
pub fn empty_book() -> OrderBook {
    OrderBook::empty()
}

/// Reconnection gap: normal book, then empty (disconnected), then normal again.
pub fn reconnection_gap() -> Vec<OrderBook> {
    vec![
        normal_trading_book(),
        OrderBook::empty(), // Gap during disconnection
        OrderBookBuilder::new()
            .bid(4501.00, 130)
            .bid(4500.75, 100)
            .bid(4500.50, 70)
            .ask(4501.25, 120)
            .ask(4501.50, 90)
            .ask(4501.75, 60)
            .timestamp(2)
            .build(),
    ]
}
