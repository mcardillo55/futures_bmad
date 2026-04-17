use futures_bmad_core::Clock;
use tracing::{info, warn};

use crate::data::data_source::DataSource;
use crate::spsc::MarketEventProducer;

/// Drives replay by reading from a DataSource and pushing to the SPSC producer.
/// Advances SimClock based on event timestamps.
/// Uses the same SPSC path as live data — consumer side is identical.
pub struct ReplayDriver {
    events_pushed: u64,
    events_dropped: u64,
}

impl ReplayDriver {
    pub fn new() -> Self {
        Self {
            events_pushed: 0,
            events_dropped: 0,
        }
    }

    /// Run replay: drain the data source into the SPSC producer.
    /// Advances clock time to each event's timestamp.
    /// Returns total events pushed.
    pub fn run<S: DataSource, C: Clock>(
        &mut self,
        source: &mut S,
        producer: &mut MarketEventProducer,
        clock: &C,
    ) -> u64 {
        info!("replay started");

        while let Some(event) = source.next_event() {
            if producer.try_push(event) {
                self.events_pushed += 1;
            } else {
                self.events_dropped += 1;
                if self.events_dropped % 1000 == 1 {
                    warn!(
                        dropped = self.events_dropped,
                        "replay: SPSC full, dropping events"
                    );
                }
            }
        }

        info!(
            events_pushed = self.events_pushed,
            events_dropped = self.events_dropped,
            "replay complete"
        );
        self.events_pushed
    }

    pub fn events_pushed(&self) -> u64 {
        self.events_pushed
    }

    pub fn events_dropped(&self) -> u64 {
        self.events_dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::data_source::DataSource;
    use crate::spsc::market_event_queue;
    use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType, Side, UnixNanos};
    use futures_bmad_testkit::SimClock;

    struct VecDataSource {
        events: Vec<MarketEvent>,
        cursor: usize,
    }

    impl DataSource for VecDataSource {
        fn next_event(&mut self) -> Option<MarketEvent> {
            if self.cursor < self.events.len() {
                let e = self.events[self.cursor];
                self.cursor += 1;
                Some(e)
            } else {
                None
            }
        }

        fn reset(&mut self) {
            self.cursor = 0;
        }

        fn event_count(&self) -> usize {
            self.cursor
        }
    }

    fn make_event(ts: u64, price: i64) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(ts),
            symbol_id: 0,
            sequence: 0,
            event_type: MarketEventType::Trade,
            price: FixedPrice::new(price),
            size: 10,
            side: Some(Side::Buy),
        }
    }

    #[test]
    fn replay_pushes_all_events() {
        let (mut producer, mut consumer) = market_event_queue(64);
        let clock = SimClock::new(1_000_000_000_000);

        let mut source = VecDataSource {
            events: vec![
                make_event(1_000_000_000_000, 18000),
                make_event(1_000_000_001_000, 18001),
                make_event(1_000_000_002_000, 18002),
            ],
            cursor: 0,
        };

        let mut driver = ReplayDriver::new();
        let pushed = driver.run(&mut source, &mut producer, &clock);

        assert_eq!(pushed, 3);
        assert_eq!(driver.events_dropped(), 0);

        // Verify events arrive in SPSC
        let e1 = consumer.try_pop().unwrap();
        assert_eq!(e1.price.raw(), 18000);
        let e2 = consumer.try_pop().unwrap();
        assert_eq!(e2.price.raw(), 18001);
        let e3 = consumer.try_pop().unwrap();
        assert_eq!(e3.price.raw(), 18002);
        assert!(consumer.try_pop().is_none());
    }

    #[test]
    fn replay_drops_on_full_buffer() {
        let (mut producer, _consumer) = market_event_queue(4);
        let clock = SimClock::new(1_000_000_000_000);

        let mut source = VecDataSource {
            events: (0..10)
                .map(|i| make_event(1_000_000_000_000 + i, 18000))
                .collect(),
            cursor: 0,
        };

        let mut driver = ReplayDriver::new();
        driver.run(&mut source, &mut producer, &clock);

        assert_eq!(driver.events_pushed(), 4);
        assert_eq!(driver.events_dropped(), 6);
    }
}
