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

/// Outcome of a single poll on a [`MarketDataFeed`].
///
/// Three states distinguish "an event is ready", "the stream is alive but
/// momentarily quiet", and "the stream has ended forever". The distinction
/// matters for live streaming sources where transient quiet windows between
/// frames are normal — collapsing idle and end-of-stream into one signal
/// would terminate the orchestrator on the first quiet period.
#[derive(Debug)]
pub enum NextEvent {
    /// A market event is ready for the orchestrator to process.
    Event(MarketEvent),
    /// No event is available *right now*, but the stream is still live.
    /// Callers must keep polling — this is NOT an end-of-stream signal.
    /// Streaming adapters (e.g. the Epic 8 `RithmicMarketDataFeed`) return
    /// `Idle` between live frames.
    Idle,
    /// The stream has ended permanently and will not produce further
    /// events. Sticky: once a feed returns `Terminated`, every subsequent
    /// call must also return `Terminated` (callers may rely on this when
    /// driving their own outer loops).
    Terminated,
}

/// Source of market events feeding the paper-trading event loop.
///
/// Implementors return events in arrival order via three signals:
///
/// * [`NextEvent::Event`] — a market event is ready for processing.
/// * [`NextEvent::Idle`] — the stream is alive but currently has nothing to
///   yield. The orchestrator drains its SPSCs and polls again.
/// * [`NextEvent::Terminated`] — the stream has ended; the orchestrator
///   exits. Sticky — once a feed returns `Terminated`, every subsequent
///   call must also return `Terminated`.
///
/// Production wiring: Epic 8 will provide a `RithmicMarketDataFeed` adapter
/// that drains `futures_bmad_broker::MarketDataStream::next_event` (async)
/// and republishes the events on a sync handle the orchestrator can poll.
/// That adapter maps "no async message available" to [`NextEvent::Idle`]
/// and an explicit shutdown signal to [`NextEvent::Terminated`].
pub trait MarketDataFeed: Send {
    /// Pull the next feed state. See [`NextEvent`] for the three-state
    /// contract — and in particular the `Terminated` stickiness invariant.
    fn next_event(&mut self) -> NextEvent;
}

/// In-memory feed backed by a `Vec` — the canonical test double.
///
/// Yields events in the order they were inserted (each as
/// [`NextEvent::Event`]), then returns [`NextEvent::Terminated`] forever
/// once the underlying `Vec` is drained. Cheap to construct and trivially
/// predictable, so end-to-end paper-trading tests can assert on engine
/// behaviour without standing up a broker connection.
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
    fn next_event(&mut self) -> NextEvent {
        match self.events.next() {
            Some(ev) => {
                self.yielded += 1;
                NextEvent::Event(ev)
            }
            // `std::vec::IntoIter::next()` returns `None` forever after
            // exhaustion, so the `Terminated` invariant is sticky for free.
            None => NextEvent::Terminated,
        }
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
    fn vec_feed_yields_in_order_then_terminated() {
        let mut feed = VecMarketDataFeed::new(vec![ev(1, 100), ev(2, 101), ev(3, 102)]);
        match feed.next_event() {
            NextEvent::Event(e) => assert_eq!(e.timestamp.as_nanos(), 1),
            other => panic!("expected Event(ts=1), got {other:?}"),
        }
        match feed.next_event() {
            NextEvent::Event(e) => assert_eq!(e.timestamp.as_nanos(), 2),
            other => panic!("expected Event(ts=2), got {other:?}"),
        }
        match feed.next_event() {
            NextEvent::Event(e) => assert_eq!(e.timestamp.as_nanos(), 3),
            other => panic!("expected Event(ts=3), got {other:?}"),
        }
        assert!(matches!(feed.next_event(), NextEvent::Terminated));
        assert_eq!(feed.yielded(), 3);
    }

    #[test]
    fn vec_feed_empty_yields_terminated_immediately() {
        let mut feed = VecMarketDataFeed::new(Vec::new());
        assert!(matches!(feed.next_event(), NextEvent::Terminated));
        assert_eq!(feed.yielded(), 0);
    }

    #[test]
    fn vec_feed_terminated_state_is_sticky() {
        let mut feed = VecMarketDataFeed::new(vec![ev(1, 100)]);
        assert!(matches!(feed.next_event(), NextEvent::Event(_)));
        for _ in 0..10 {
            assert!(
                matches!(feed.next_event(), NextEvent::Terminated),
                "Terminated must stay sticky across repeated polls"
            );
        }
        assert_eq!(feed.yielded(), 1);
    }
}
