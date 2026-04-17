use futures_bmad_broker::MarketDataStream;
use futures_bmad_core::BrokerError;
use tracing::info;

use crate::spsc::MarketEventProducer;

/// Async task that bridges the broker's MarketDataStream to the engine's SPSC producer.
/// Runs on Tokio runtime (I/O thread), pushes events to the SPSC ring buffer.
/// No shared mutexes — only the lock-free SPSC queue crosses the thread boundary.
pub async fn ingest_market_data(
    stream: &mut MarketDataStream,
    producer: &mut MarketEventProducer,
) -> Result<(), BrokerError> {
    info!("market data ingest started");

    loop {
        let event = stream.next_event().await?;
        producer.try_push(event);
    }
}
