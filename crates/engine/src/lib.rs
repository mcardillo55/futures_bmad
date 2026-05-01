// futures_bmad_engine: Trading engine, signal processing, and strategy orchestration.

pub mod buffer_monitor;
pub mod connection;
pub mod data;
pub mod data_quality;
pub mod event_loop;
pub mod ingest;
pub mod order_book;
pub mod order_manager;
pub mod persistence;
pub mod replay;
pub mod risk;
pub mod signals;
pub mod spsc;

pub use buffer_monitor::{BufferMonitor, BufferState};
pub use event_loop::{EventLoop, EventLoopHandle};
pub use spsc::{
    MARKET_EVENT_QUEUE_CAPACITY, MarketEventConsumer, MarketEventProducer, market_event_queue,
};
