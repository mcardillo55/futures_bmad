use futures_bmad_core::MarketEvent;
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use tracing::warn;

/// Default SPSC ring buffer capacity (2^17 = 131072).
pub const MARKET_EVENT_QUEUE_CAPACITY: usize = 131_072;

/// Producer side of the MarketEvent SPSC queue.
/// Lives on the I/O thread (Tokio async runtime).
pub struct MarketEventProducer {
    producer: Producer<MarketEvent>,
    drop_count: u64,
}

impl MarketEventProducer {
    /// Non-blocking push. On full buffer, logs a warning and increments drop counter.
    pub fn try_push(&mut self, event: MarketEvent) -> bool {
        match self.producer.push(event) {
            Ok(()) => true,
            Err(PushError::Full(_)) => {
                self.drop_count += 1;
                if self.drop_count % 1000 == 1 {
                    warn!(
                        drop_count = self.drop_count,
                        "SPSC buffer full, dropping MarketEvent"
                    );
                }
                false
            }
        }
    }

    pub fn drop_count(&self) -> u64 {
        self.drop_count
    }

    /// Available slots for writing.
    pub fn available_slots(&self) -> usize {
        self.producer.slots()
    }

    /// Fill fraction (0.0 to 1.0).
    pub fn fill_fraction(&self) -> f64 {
        let capacity = self.producer.buffer().capacity();
        let used = capacity - self.producer.slots();
        used as f64 / capacity as f64
    }
}

/// Consumer side of the MarketEvent SPSC queue.
/// Lives on the hot-path engine thread (pinned core).
pub struct MarketEventConsumer {
    consumer: Consumer<MarketEvent>,
}

impl MarketEventConsumer {
    /// Non-blocking pop. Returns None if buffer is empty.
    pub fn try_pop(&mut self) -> Option<MarketEvent> {
        self.consumer.pop().ok()
    }

    /// Number of events available for reading.
    pub fn available(&self) -> usize {
        self.consumer.slots()
    }

    /// Fill fraction (0.0 to 1.0).
    pub fn fill_fraction(&self) -> f64 {
        let capacity = self.consumer.buffer().capacity();
        let used = self.consumer.slots();
        used as f64 / capacity as f64
    }

    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }
}

/// Create a new MarketEvent SPSC queue pair.
///
/// # Panics
/// Panics if `capacity` is less than 2.
pub fn market_event_queue(capacity: usize) -> (MarketEventProducer, MarketEventConsumer) {
    assert!(capacity >= 2, "SPSC queue capacity must be at least 2");
    let (producer, consumer) = RingBuffer::new(capacity);
    (
        MarketEventProducer {
            producer,
            drop_count: 0,
        },
        MarketEventConsumer { consumer },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_bmad_core::{FixedPrice, MarketEventType, UnixNanos};

    fn make_event(price_raw: i64) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(1_000_000_000),
            symbol_id: 0,
            sequence: 0,
            event_type: MarketEventType::Trade,
            price: FixedPrice::new(price_raw),
            size: 10,
            side: None,
        }
    }

    #[test]
    fn push_pop_roundtrip() {
        let (mut producer, mut consumer) = market_event_queue(16);
        assert!(producer.try_push(make_event(100)));
        let event = consumer.try_pop().unwrap();
        assert_eq!(event.price.raw(), 100);
    }

    #[test]
    fn pop_empty_returns_none() {
        let (_producer, mut consumer) = market_event_queue(16);
        assert!(consumer.try_pop().is_none());
        assert!(consumer.is_empty());
    }

    #[test]
    fn full_buffer_drops_and_counts() {
        let (mut producer, _consumer) = market_event_queue(4);
        // Fill the buffer
        for i in 0..4 {
            assert!(producer.try_push(make_event(i)));
        }
        // This should fail — buffer is full
        assert!(!producer.try_push(make_event(999)));
        assert_eq!(producer.drop_count(), 1);

        // Drop more
        assert!(!producer.try_push(make_event(999)));
        assert_eq!(producer.drop_count(), 2);
    }

    #[test]
    fn fill_fraction_reflects_usage() {
        let (mut producer, consumer) = market_event_queue(8);
        assert_eq!(consumer.fill_fraction(), 0.0);

        for i in 0..4 {
            producer.try_push(make_event(i));
        }
        // 4 out of 8 slots used = 0.5
        assert!((consumer.fill_fraction() - 0.5).abs() < 0.01);
    }
}
