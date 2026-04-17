use futures_bmad_core::MarketEvent;

/// Synchronous data source producing sequential MarketEvents.
/// Used for both own-format Parquet and external format readers.
/// Synchronous because replay runs on a dedicated thread, not Tokio.
pub trait DataSource {
    /// Get the next event in timestamp order. Returns None when exhausted.
    fn next_event(&mut self) -> Option<MarketEvent>;

    /// Reset to the beginning of the data source for re-replay.
    fn reset(&mut self);

    /// Total events produced so far.
    fn event_count(&self) -> usize;
}
