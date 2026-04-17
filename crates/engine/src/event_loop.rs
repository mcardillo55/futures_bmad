use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_bmad_core::OrderBook;
use tracing::info;

use crate::buffer_monitor::{BufferMonitor, BufferState};
use crate::order_book::apply_market_event;
use crate::spsc::MarketEventConsumer;

/// Hot-path event loop that consumes MarketEvents from SPSC and updates OrderBook.
/// Runs on a dedicated pinned core. Never allocates after initialization.
pub struct EventLoop {
    consumer: MarketEventConsumer,
    book: OrderBook,
    monitor: BufferMonitor,
    events_processed: u64,
    running: Arc<AtomicBool>,
}

/// Handle for stopping the event loop from another thread.
#[derive(Clone)]
pub struct EventLoopHandle {
    running: Arc<AtomicBool>,
}

impl EventLoopHandle {
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }
}

impl EventLoop {
    pub fn new(consumer: MarketEventConsumer) -> Self {
        Self {
            consumer,
            book: OrderBook::empty(),
            monitor: BufferMonitor::new(),
            events_processed: 0,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get a handle that can stop the event loop from another thread.
    pub fn handle(&self) -> EventLoopHandle {
        EventLoopHandle {
            running: self.running.clone(),
        }
    }

    /// Pin the current thread to a specific CPU core.
    pub fn pin_to_core(core_id: usize) -> bool {
        let core = core_affinity::CoreId { id: core_id };
        core_affinity::set_for_current(core)
    }

    /// Run the event loop. Spins, polling the SPSC consumer.
    /// Returns when `stop()` is called via handle, or producer is dropped.
    pub fn run(&mut self) {
        self.running.store(true, Ordering::Release);
        info!("event loop started");

        while self.running.load(Ordering::Acquire) {
            self.tick();

            // Yield if buffer is empty to avoid burning CPU
            if self.consumer.is_empty() {
                std::hint::spin_loop();
            }
        }

        info!(
            events_processed = self.events_processed,
            "event loop stopped"
        );
    }

    /// Process one iteration: pop event, check thresholds, update book.
    /// Returns the buffer state after this tick.
    pub fn tick(&mut self) -> BufferState {
        // Check buffer state
        let fill = self.consumer.fill_fraction();
        let state = self.monitor.update(fill);

        // Pop and process event
        if let Some(event) = self.consumer.try_pop() {
            apply_market_event(&mut self.book, &event);
            self.events_processed += 1;
        }

        state
    }

    pub fn order_book(&self) -> &OrderBook {
        &self.book
    }

    pub fn buffer_state(&self) -> BufferState {
        self.monitor.state()
    }

    pub fn events_processed(&self) -> u64 {
        self.events_processed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spsc::market_event_queue;
    use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType, Side, UnixNanos};

    fn make_bid(price_raw: i64, size: u32) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(1_000_000_000),
            symbol_id: 0,
            event_type: MarketEventType::BidUpdate,
            price: FixedPrice::new(price_raw),
            size,
            side: Some(Side::Buy),
        }
    }

    fn make_ask(price_raw: i64, size: u32) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(1_000_000_000),
            symbol_id: 0,
            event_type: MarketEventType::AskUpdate,
            price: FixedPrice::new(price_raw),
            size,
            side: Some(Side::Sell),
        }
    }

    #[test]
    fn event_loop_processes_events() {
        let (mut producer, consumer) = market_event_queue(16);
        let mut event_loop = EventLoop::new(consumer);

        producer.try_push(make_bid(18000, 50));
        producer.try_push(make_ask(18004, 30));

        event_loop.tick();
        event_loop.tick();

        assert_eq!(event_loop.events_processed(), 2);
        assert_eq!(
            event_loop.order_book().best_bid().unwrap().price.raw(),
            18000
        );
        assert_eq!(
            event_loop.order_book().best_ask().unwrap().price.raw(),
            18004
        );
    }

    #[test]
    fn tick_returns_normal_when_buffer_low() {
        let (_producer, consumer) = market_event_queue(16);
        let mut event_loop = EventLoop::new(consumer);

        let state = event_loop.tick();
        assert_eq!(state, BufferState::Normal);
    }

    #[test]
    fn buffer_state_escalates_with_fill() {
        let (mut producer, consumer) = market_event_queue(16);
        let mut event_loop = EventLoop::new(consumer);

        for i in 0..13 {
            producer.try_push(make_bid(18000 + i, 10));
        }

        let state = event_loop.tick();
        assert_eq!(state, BufferState::TradingDisabled);
    }

    #[test]
    fn handle_can_stop_loop() {
        let (_producer, consumer) = market_event_queue(16);
        let mut event_loop = EventLoop::new(consumer);
        let handle = event_loop.handle();

        // Simulate: start running, then stop via handle
        event_loop.running.store(true, Ordering::Release);
        assert!(event_loop.running.load(Ordering::Acquire));
        handle.stop();
        assert!(!event_loop.running.load(Ordering::Acquire));
    }
}
