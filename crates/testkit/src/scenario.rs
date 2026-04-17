use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType, OrderBook, Side, UnixNanos};

pub struct Scenario {
    pub name: &'static str,
    pub books: Vec<OrderBook>,
    pub events: Vec<MarketEvent>,
}

const SYMBOL_ID: u32 = 1;

/// Create a `BookSnapshot` event from an `OrderBook`.
fn snapshot_event(book: &OrderBook) -> MarketEvent {
    let price = book
        .best_bid()
        .map(|l| l.price)
        .unwrap_or(FixedPrice::new(0));

    let total_size: u32 = {
        let bid_size: u32 = book.bids[..book.bid_count as usize]
            .iter()
            .map(|l| l.size)
            .sum();
        let ask_size: u32 = book.asks[..book.ask_count as usize]
            .iter()
            .map(|l| l.size)
            .sum();
        bid_size + ask_size
    };

    MarketEvent {
        timestamp: book.timestamp,
        symbol_id: SYMBOL_ID,
        event_type: MarketEventType::BookSnapshot,
        price,
        size: total_size,
        side: None,
    }
}

/// Generate `Trade` events that transition from one book's best bid to the next.
///
/// Produces a sell trade at the old best bid (representing the price dropping away)
/// and a buy trade at the new best bid (representing the price arriving).
/// The trade timestamp is placed halfway between the two book timestamps.
fn transition_trades(prev: &OrderBook, next: &OrderBook) -> Vec<MarketEvent> {
    let prev_bid = match prev.best_bid() {
        Some(l) => l.price,
        None => return Vec::new(),
    };
    let next_bid = match next.best_bid() {
        Some(l) => l.price,
        None => return Vec::new(),
    };

    if prev_bid == next_bid {
        return Vec::new();
    }

    // Place trades at a timestamp between the two snapshots.
    let t0 = prev.timestamp.as_nanos();
    let t1 = next.timestamp.as_nanos();
    let mid_ts = UnixNanos::new(t0 + (t1.saturating_sub(t0)) / 2);

    let moving_down = next_bid < prev_bid;
    let trade_size: u32 = if moving_down { 25 } else { 20 };

    vec![
        // Trade at the departing price
        MarketEvent {
            timestamp: mid_ts,
            symbol_id: SYMBOL_ID,
            event_type: MarketEventType::Trade,
            price: prev_bid,
            size: trade_size,
            side: Some(if moving_down { Side::Sell } else { Side::Buy }),
        },
        // Trade at the arriving price
        MarketEvent {
            timestamp: mid_ts,
            symbol_id: SYMBOL_ID,
            event_type: MarketEventType::Trade,
            price: next_bid,
            size: trade_size,
            side: Some(if moving_down { Side::Sell } else { Side::Buy }),
        },
    ]
}

/// Build events from a book sequence: snapshots only, no trades.
fn events_from_books(books: &[OrderBook]) -> Vec<MarketEvent> {
    books.iter().map(snapshot_event).collect()
}

/// Build events from a book sequence with trade transitions between snapshots.
fn events_from_books_with_trades(books: &[OrderBook]) -> Vec<MarketEvent> {
    let mut events = Vec::new();
    for (i, book) in books.iter().enumerate() {
        events.push(snapshot_event(book));
        if i + 1 < books.len() {
            events.extend(transition_trades(book, &books[i + 1]));
        }
    }
    events
}

impl Scenario {
    pub fn normal_trading() -> Self {
        let books = vec![crate::market_gen::normal_trading_book()];
        let events = events_from_books(&books);
        Self {
            name: "normal_trading",
            books,
            events,
        }
    }

    pub fn fomc_spike() -> Self {
        let books = crate::market_gen::fomc_volatility_spike();
        let events = events_from_books_with_trades(&books);
        Self {
            name: "fomc_volatility_spike",
            books,
            events,
        }
    }

    pub fn flash_crash() -> Self {
        let books = crate::market_gen::flash_crash_sequence();
        let events = events_from_books_with_trades(&books);
        Self {
            name: "flash_crash",
            books,
            events,
        }
    }

    pub fn reconnection() -> Self {
        let books = crate::market_gen::reconnection_gap();
        let events = events_from_books(&books);
        Self {
            name: "reconnection_gap",
            books,
            events,
        }
    }
}
