use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{Int8Array, Int64Array, UInt32Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use chrono::NaiveDate;
use futures_bmad_core::{MarketEvent, MarketEventType, Side};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use tracing::info;

const ROW_GROUP_SIZE: usize = 8192;

/// Encodes MarketEventType as i8 for Parquet storage.
fn event_type_to_i8(et: MarketEventType) -> i8 {
    match et {
        MarketEventType::Trade => 0,
        MarketEventType::BidUpdate => 1,
        MarketEventType::AskUpdate => 2,
        MarketEventType::BookSnapshot => 3,
    }
}

/// Encodes Side as i8 for Parquet storage.
fn side_to_i8(side: Option<Side>) -> i8 {
    match side {
        Some(Side::Buy) => 0,
        Some(Side::Sell) => 1,
        None => -1,
    }
}

/// Build the Parquet schema for market data.
fn market_data_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("timestamp", DataType::Int64, false),
        Field::new("price", DataType::Int64, false),
        Field::new("size", DataType::UInt32, false),
        Field::new("side", DataType::Int8, false),
        Field::new("event_type", DataType::Int8, false),
        Field::new("symbol_id", DataType::UInt32, false),
    ]))
}

/// Generate the file path for a given symbol and date.
pub fn file_path_for(base_dir: &Path, symbol: &str, date: NaiveDate) -> PathBuf {
    base_dir
        .join("market")
        .join(symbol)
        .join(format!("{}.parquet", date.format("%Y-%m-%d")))
}

/// Tracks the current date for file rollover detection.
pub struct DateTracker {
    current_date: Option<NaiveDate>,
}

impl Default for DateTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DateTracker {
    pub fn new() -> Self {
        Self { current_date: None }
    }

    /// Check if the date has changed. Returns the new date if a rollover occurred.
    pub fn check_rollover(&mut self, event_date: NaiveDate) -> Option<NaiveDate> {
        match self.current_date {
            Some(current) if current == event_date => None,
            _ => {
                self.current_date = Some(event_date);
                Some(event_date)
            }
        }
    }

    pub fn current_date(&self) -> Option<NaiveDate> {
        self.current_date
    }
}

/// Writes MarketEvents to Parquet files, buffering into row groups.
/// Handles date boundary rollover and file creation.
pub struct MarketDataWriter {
    base_dir: PathBuf,
    symbol: String,
    schema: Arc<Schema>,
    writer: Option<ArrowWriter<File>>,
    buffer: Vec<MarketEvent>,
    date_tracker: DateTracker,
    events_written: u64,
}

impl MarketDataWriter {
    pub fn new(base_dir: PathBuf, symbol: String) -> Self {
        Self {
            base_dir,
            symbol,
            schema: market_data_schema(),
            writer: None,
            buffer: Vec::with_capacity(ROW_GROUP_SIZE),
            date_tracker: DateTracker::new(),
            events_written: 0,
        }
    }

    /// Write a MarketEvent. Buffers internally and flushes at ROW_GROUP_SIZE threshold.
    pub fn write_event(&mut self, event: &MarketEvent) -> Result<(), ParquetWriteError> {
        let event_date = nanos_to_date(event.timestamp.as_nanos());

        // Check for date rollover — flush old date's data before switching
        if let Some(current) = self.date_tracker.current_date()
            && current != event_date
        {
            // Flush buffer to old date's file, then close it
            self.flush_buffer()?;
            self.close_writer()?;
            info!(
                old_date = %current,
                new_date = %event_date,
                "Parquet file date rollover"
            );
        }
        // Update tracker to new date (or set initial date)
        self.date_tracker.check_rollover(event_date);

        self.buffer.push(*event);

        if self.buffer.len() >= ROW_GROUP_SIZE {
            self.flush_buffer()?;
        }

        Ok(())
    }

    /// Flush any buffered events and close the current file.
    pub fn flush(&mut self) -> Result<(), ParquetWriteError> {
        self.flush_buffer()?;
        self.close_writer()?;
        Ok(())
    }

    /// Flush the internal buffer to the current Parquet file.
    fn flush_buffer(&mut self) -> Result<(), ParquetWriteError> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        self.ensure_writer()?;
        let batch = self.buffer_to_record_batch()?;
        self.writer
            .as_mut()
            .unwrap()
            .write(&batch)
            .map_err(|e| ParquetWriteError::Write(e.to_string()))?;

        self.events_written += self.buffer.len() as u64;
        self.buffer.clear();
        Ok(())
    }

    /// Convert the buffer to an Arrow RecordBatch.
    fn buffer_to_record_batch(&self) -> Result<RecordBatch, ParquetWriteError> {
        let timestamps: Vec<i64> = self
            .buffer
            .iter()
            .map(|e| e.timestamp.as_nanos() as i64)
            .collect();
        let prices: Vec<i64> = self.buffer.iter().map(|e| e.price.raw()).collect();
        let sizes: Vec<u32> = self.buffer.iter().map(|e| e.size).collect();
        let sides: Vec<i8> = self.buffer.iter().map(|e| side_to_i8(e.side)).collect();
        let event_types: Vec<i8> = self
            .buffer
            .iter()
            .map(|e| event_type_to_i8(e.event_type))
            .collect();
        let symbol_ids: Vec<u32> = self.buffer.iter().map(|e| e.symbol_id).collect();

        RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(Int64Array::from(timestamps)),
                Arc::new(Int64Array::from(prices)),
                Arc::new(UInt32Array::from(sizes)),
                Arc::new(Int8Array::from(sides)),
                Arc::new(Int8Array::from(event_types)),
                Arc::new(UInt32Array::from(symbol_ids)),
            ],
        )
        .map_err(|e| ParquetWriteError::Schema(e.to_string()))
    }

    /// Ensure a writer exists for the current date.
    fn ensure_writer(&mut self) -> Result<&mut ArrowWriter<File>, ParquetWriteError> {
        if self.writer.is_none() {
            let date = self
                .date_tracker
                .current_date()
                .ok_or(ParquetWriteError::NoDate)?;
            self.open_writer(date)?;
        }
        Ok(self.writer.as_mut().unwrap())
    }

    /// Open a new Parquet file writer for the given date.
    fn open_writer(&mut self, date: NaiveDate) -> Result<(), ParquetWriteError> {
        let path = file_path_for(&self.base_dir, &self.symbol, date);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ParquetWriteError::Io(format!("create_dir_all: {e}")))?;
        }

        let file = File::create(&path)
            .map_err(|e| ParquetWriteError::Io(format!("create {}: {e}", path.display())))?;

        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();

        let writer = ArrowWriter::try_new(file, self.schema.clone(), Some(props))
            .map_err(|e| ParquetWriteError::Write(e.to_string()))?;

        info!(path = %path.display(), "opened Parquet file for recording");
        self.writer = Some(writer);
        Ok(())
    }

    /// Close the current writer, finalizing the Parquet file.
    fn close_writer(&mut self) -> Result<(), ParquetWriteError> {
        if let Some(writer) = self.writer.take() {
            writer
                .close()
                .map_err(|e| ParquetWriteError::Write(e.to_string()))?;
        }
        Ok(())
    }

    pub fn events_written(&self) -> u64 {
        self.events_written
    }
}

/// Convert nanosecond timestamp to NaiveDate (UTC).
fn nanos_to_date(nanos: u64) -> NaiveDate {
    let secs = (nanos / 1_000_000_000) as i64;
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.date_naive())
        .unwrap_or(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap())
}

#[derive(Debug, thiserror::Error)]
pub enum ParquetWriteError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("Parquet write error: {0}")]
    Write(String),
    #[error("Schema error: {0}")]
    Schema(String),
    #[error("no date set for writer")]
    NoDate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_bmad_core::{FixedPrice, UnixNanos};
    use parquet::file::reader::SerializedFileReader;
    use parquet::record::reader::RowIter;

    fn make_event(ts_nanos: u64, price_raw: i64, size: u32) -> MarketEvent {
        MarketEvent {
            timestamp: UnixNanos::new(ts_nanos),
            symbol_id: 0,
            sequence: 0,
            event_type: MarketEventType::Trade,
            price: FixedPrice::new(price_raw),
            size,
            side: Some(Side::Buy),
        }
    }

    #[test]
    fn file_path_generation() {
        let base = Path::new("/tmp/data");
        let date = NaiveDate::from_ymd_opt(2026, 4, 17).unwrap();
        let path = file_path_for(base, "MES", date);
        assert_eq!(
            path,
            PathBuf::from("/tmp/data/market/MES/2026-04-17.parquet")
        );
    }

    #[test]
    fn date_tracker_detects_rollover() {
        let mut tracker = DateTracker::new();
        let d1 = NaiveDate::from_ymd_opt(2026, 4, 17).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 4, 18).unwrap();

        // First date is always a "rollover"
        assert_eq!(tracker.check_rollover(d1), Some(d1));
        // Same date — no rollover
        assert_eq!(tracker.check_rollover(d1), None);
        // New date — rollover
        assert_eq!(tracker.check_rollover(d2), Some(d2));
    }

    #[test]
    fn write_and_read_parquet() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = MarketDataWriter::new(dir.path().to_path_buf(), "MES".to_string());

        // 2026-04-18 00:00:00 UTC in nanos
        let ts = 1_776_470_400_000_000_000u64;
        for i in 0..10 {
            writer
                .write_event(&make_event(
                    ts + i * 1_000_000,
                    18000 + i as i64,
                    10 + i as u32,
                ))
                .unwrap();
        }
        writer.flush().unwrap();

        // Read it back
        let path = file_path_for(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        );
        assert!(path.exists());

        let file = File::open(&path).unwrap();
        let reader = SerializedFileReader::new(file).unwrap();
        let mut row_iter = RowIter::from_file_into(Box::new(reader));

        let mut count = 0;
        while let Some(Ok(_row)) = row_iter.next() {
            count += 1;
        }
        assert_eq!(count, 10);
        assert_eq!(writer.events_written(), 10);
    }

    #[test]
    fn buffer_flushes_at_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = MarketDataWriter::new(dir.path().to_path_buf(), "MES".to_string());

        let ts = 1_776_470_400_000_000_000u64;
        // Write exactly ROW_GROUP_SIZE events
        for i in 0..ROW_GROUP_SIZE {
            writer
                .write_event(&make_event(ts + i as u64 * 1_000_000, 18000, 10))
                .unwrap();
        }
        // Buffer should have auto-flushed
        assert_eq!(writer.events_written(), ROW_GROUP_SIZE as u64);

        writer.flush().unwrap();
    }

    #[test]
    fn date_rollover_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = MarketDataWriter::new(dir.path().to_path_buf(), "MES".to_string());

        // Day 1: 2026-04-18
        let ts1 = 1_776_470_400_000_000_000u64;
        writer.write_event(&make_event(ts1, 18000, 10)).unwrap();

        // Day 2: 2026-04-19 (+ 86400 seconds)
        let ts2 = ts1 + 86_400_000_000_000;
        writer.write_event(&make_event(ts2, 18100, 20)).unwrap();

        writer.flush().unwrap();

        let path1 = file_path_for(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        );
        let path2 = file_path_for(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
        );

        assert!(path1.exists(), "day 1 file should exist");
        assert!(path2.exists(), "day 2 file should exist");
    }

    #[test]
    fn graceful_flush_writes_remaining() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = MarketDataWriter::new(dir.path().to_path_buf(), "MES".to_string());

        let ts = 1_776_470_400_000_000_000u64;
        // Write fewer than ROW_GROUP_SIZE events
        for i in 0..5 {
            writer
                .write_event(&make_event(ts + i * 1_000_000, 18000, 10))
                .unwrap();
        }
        // Events are buffered, not yet written
        assert_eq!(writer.events_written(), 0);

        // Graceful flush
        writer.flush().unwrap();
        assert_eq!(writer.events_written(), 5);
    }
}
