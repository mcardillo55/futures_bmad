use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use arrow::array::{Array, Int8Array, Int64Array, UInt32Array};
use futures_bmad_core::{FixedPrice, MarketEvent, MarketEventType, Side, UnixNanos};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tracing::warn;

use super::data_source::DataSource;

/// Reads own-format Parquet files and produces MarketEvents.
/// Loads lazily via record-batch iteration to avoid loading entire file into memory.
pub struct ParquetDataSource {
    path: PathBuf,
    events: Vec<MarketEvent>,
    cursor: usize,
    total_read: usize,
    skipped: usize,
}

impl ParquetDataSource {
    pub fn new(path: PathBuf) -> Result<Self, String> {
        let mut source = Self {
            path: path.clone(),
            events: Vec::new(),
            cursor: 0,
            total_read: 0,
            skipped: 0,
        };
        source.load_file(&path)?;
        Ok(source)
    }

    fn load_file(&mut self, path: &PathBuf) -> Result<(), String> {
        let file =
            File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;

        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| format!("failed to read Parquet metadata: {e}"))?;

        let reader = builder
            .build()
            .map_err(|e| format!("failed to build Parquet reader: {e}"))?;

        let mut last_ts: u64 = 0;

        for batch_result in reader {
            let batch = match batch_result {
                Ok(b) => b,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "skipping corrupt record batch");
                    self.skipped += 1;
                    continue;
                }
            };

            let timestamps = batch
                .column_by_name("timestamp")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            let prices = batch
                .column_by_name("price")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            let sizes = batch
                .column_by_name("size")
                .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
            let sides = batch
                .column_by_name("side")
                .and_then(|c| c.as_any().downcast_ref::<Int8Array>());
            let event_types = batch
                .column_by_name("event_type")
                .and_then(|c| c.as_any().downcast_ref::<Int8Array>());
            let symbol_ids = batch
                .column_by_name("symbol_id")
                .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());

            let (timestamps, prices, sizes, sides, event_types, symbol_ids) =
                match (timestamps, prices, sizes, sides, event_types, symbol_ids) {
                    (Some(t), Some(p), Some(s), Some(si), Some(et), Some(sym)) => {
                        (t, p, s, si, et, sym)
                    }
                    _ => {
                        warn!(path = %path.display(), "skipping batch with missing columns");
                        self.skipped += 1;
                        continue;
                    }
                };

            for i in 0..batch.num_rows() {
                let ts = timestamps.value(i) as u64;

                if ts < last_ts {
                    warn!(
                        row = self.total_read + i,
                        ts, last_ts, "non-monotonic timestamp detected"
                    );
                }
                last_ts = ts;

                let event = MarketEvent {
                    timestamp: UnixNanos::new(ts),
                    symbol_id: symbol_ids.value(i),
                    sequence: 0,
                    event_type: i8_to_event_type(event_types.value(i)),
                    price: FixedPrice::new(prices.value(i)),
                    size: sizes.value(i),
                    side: i8_to_side(sides.value(i)),
                };
                self.events.push(event);
            }
            self.total_read += batch.num_rows();
        }

        if self.skipped > 0 {
            warn!(
                path = %path.display(),
                total = self.total_read,
                skipped = self.skipped,
                "Parquet ingestion summary"
            );
        }

        Ok(())
    }

    pub fn skipped(&self) -> usize {
        self.skipped
    }
}

fn i8_to_event_type(v: i8) -> MarketEventType {
    match v {
        0 => MarketEventType::Trade,
        1 => MarketEventType::BidUpdate,
        2 => MarketEventType::AskUpdate,
        3 => MarketEventType::BookSnapshot,
        _ => MarketEventType::Trade,
    }
}

fn i8_to_side(v: i8) -> Option<Side> {
    match v {
        0 => Some(Side::Buy),
        1 => Some(Side::Sell),
        _ => None,
    }
}

impl DataSource for ParquetDataSource {
    fn next_event(&mut self) -> Option<MarketEvent> {
        if self.cursor < self.events.len() {
            let event = self.events[self.cursor];
            self.cursor += 1;
            Some(event)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::parquet_writer::MarketDataWriter;

    fn make_event(ts: u64, price: i64, size: u32) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(ts),
            symbol_id: 0,
            sequence: 0,
            event_type: MarketEventType::Trade,
            price: FixedPrice::new(price),
            size,
            side: Some(Side::Buy),
        }
    }

    #[test]
    fn read_own_format_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = MarketDataWriter::new(dir.path().to_path_buf(), "MES".to_string());

        let ts_base = 1_776_470_400_000_000_000u64;
        let events: Vec<MarketEvent> = (0..10)
            .map(|i| make_event(ts_base + i * 1_000_000, 18000 + i as i64, 10 + i as u32))
            .collect();

        for event in &events {
            writer.write_event(event).unwrap();
        }
        writer.flush().unwrap();

        let path = crate::persistence::parquet_writer::file_path_for(
            dir.path(),
            "MES",
            chrono::NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        );

        let mut source = ParquetDataSource::new(path).unwrap();
        for (i, expected) in events.iter().enumerate() {
            let actual = source.next_event().expect(&format!("event {i}"));
            assert_eq!(actual.timestamp, expected.timestamp);
            assert_eq!(actual.price.raw(), expected.price.raw());
            assert_eq!(actual.size, expected.size);
        }
        assert!(source.next_event().is_none());
        assert_eq!(source.event_count(), 10);
    }

    #[test]
    fn reset_replays_from_start() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = MarketDataWriter::new(dir.path().to_path_buf(), "MES".to_string());

        let ts_base = 1_776_470_400_000_000_000u64;
        writer.write_event(&make_event(ts_base, 18000, 10)).unwrap();
        writer.flush().unwrap();

        let path = crate::persistence::parquet_writer::file_path_for(
            dir.path(),
            "MES",
            chrono::NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        );

        let mut source = ParquetDataSource::new(path).unwrap();
        assert!(source.next_event().is_some());
        assert!(source.next_event().is_none());

        source.reset();
        assert!(source.next_event().is_some());
    }
}
