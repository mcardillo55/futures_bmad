//! [`MarketDataFeed`] trait — abstract source of live market events for
//! paper trading.
//!
//! Story 7.3 keeps the orchestrator decoupled from any specific
//! broker / connection implementation. In production the feed is a thin
//! wrapper around [`futures_bmad_broker::MarketDataStream`] (Rithmic
//! TickerPlant); in tests the [`VecMarketDataFeed`] implementation plays a
//! pre-recorded sequence so end-to-end paper-trading flows can be exercised
//! deterministically without a network connection.
//!
//! The trait is intentionally synchronous — the paper orchestrator owns its
//! own dedicated thread, and forcing every test to spin up a Tokio runtime
//! to publish a handful of `MarketEvent`s would be punitive. Production
//! adapters (Epic 8) bridge from async to sync at the orchestrator boundary.

use futures_bmad_core::MarketEvent;

/// Source of market events feeding the paper-trading event loop.
///
/// Implementors return events in arrival order. `next_event()` returns
/// `None` to signal "end of stream" (test feeds), or to indicate "no event
/// available right now" for streaming implementations — the orchestrator
/// treats both the same: drain whatever has been pushed so far and exit.
///
/// Production wiring: Epic 8 will provide a `RithmicMarketDataFeed` adapter
/// that drains `futures_bmad_broker::MarketDataStream::next_event` (async)
/// and republishes the events on a sync handle the orchestrator can poll.
pub trait MarketDataFeed: Send {
    /// Pull the next event, or `None` if no more events are available
    /// right now. End-of-stream and "currently empty" share this signal —
    /// callers decide their own termination policy.
    fn next_event(&mut self) -> Option<MarketEvent>;
}

/// In-memory feed backed by a `Vec` — the canonical test double.
///
/// Yields events in the order they were inserted, then returns `None`
/// forever. Cheap to construct and trivially predictable, so end-to-end
/// paper-trading tests can assert on engine behaviour without standing up a
/// broker connection.
pub struct VecMarketDataFeed {
    events: std::vec::IntoIter<MarketEvent>,
    yielded: usize,
}

impl VecMarketDataFeed {
    /// Build a feed that will replay `events` once in order.
    pub fn new(events: Vec<MarketEvent>) -> Self {
        Self {
            events: events.into_iter(),
            yielded: 0,
        }
    }

    /// Number of events successfully drained so far.
    pub fn yielded(&self) -> usize {
        self.yielded
    }
}

impl MarketDataFeed for VecMarketDataFeed {
    fn next_event(&mut self) -> Option<MarketEvent> {
        let next = self.events.next();
        if next.is_some() {
            self.yielded += 1;
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_bmad_core::{FixedPrice, MarketEventType, Side, UnixNanos};

    fn ev(ts: u64, price: i64) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(ts),
            symbol_id: 1,
            sequence: 0,
            event_type: MarketEventType::BidUpdate,
            price: FixedPrice::new(price),
            size: 10,
            side: Some(Side::Buy),
        }
    }

    #[test]
    fn vec_feed_yields_in_order_then_none() {
        let mut feed = VecMarketDataFeed::new(vec![ev(1, 100), ev(2, 101), ev(3, 102)]);
        assert_eq!(feed.next_event().unwrap().timestamp.as_nanos(), 1);
        assert_eq!(feed.next_event().unwrap().timestamp.as_nanos(), 2);
        assert_eq!(feed.next_event().unwrap().timestamp.as_nanos(), 3);
        assert!(feed.next_event().is_none());
        assert!(feed.next_event().is_none(), "exhausted feed stays None");
        assert_eq!(feed.yielded(), 3);
    }

    #[test]
    fn vec_feed_empty_yields_none_immediately() {
        let mut feed = VecMarketDataFeed::new(Vec::new());
        assert!(feed.next_event().is_none());
        assert_eq!(feed.yielded(), 0);
    }
}
