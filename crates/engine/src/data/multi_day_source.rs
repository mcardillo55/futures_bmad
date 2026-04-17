use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use futures_bmad_core::MarketEvent;
use tracing::{info, warn};

use super::data_source::DataSource;
use super::parquet_source::ParquetDataSource;
use crate::persistence::parquet_writer::file_path_for;

/// Loads multiple daily Parquet files chronologically for multi-day replay.
/// Files are discovered by date range and symbol, sorted by date.
pub struct MultiDayDataSource {
    files: Vec<PathBuf>,
    current_source: Option<ParquetDataSource>,
    file_index: usize,
    total_events: usize,
    files_processed: usize,
    files_skipped: usize,
    last_ts: u64,
}

impl MultiDayDataSource {
    /// Create a multi-day source for the given date range and symbol.
    /// Discovers files matching data/market/{symbol}/{YYYY-MM-DD}.parquet.
    pub fn new(base_dir: &Path, symbol: &str, start_date: NaiveDate, end_date: NaiveDate) -> Self {
        let mut files = Vec::new();
        let mut date = start_date;
        while date <= end_date {
            let path = file_path_for(base_dir, symbol, date);
            if path.exists() {
                files.push(path);
            }
            date = date.succ_opt().unwrap_or(date);
        }

        info!(
            symbol,
            start = %start_date,
            end = %end_date,
            files_found = files.len(),
            "multi-day data source initialized"
        );

        Self {
            files,
            current_source: None,
            file_index: 0,
            total_events: 0,
            files_processed: 0,
            files_skipped: 0,
            last_ts: 0,
        }
    }

    /// Try to advance to the next file.
    fn advance_file(&mut self) -> bool {
        while self.file_index < self.files.len() {
            let path = &self.files[self.file_index];
            self.file_index += 1;

            match ParquetDataSource::new(path.clone()) {
                Ok(source) => {
                    info!(path = %path.display(), "loaded Parquet file for replay");
                    self.current_source = Some(source);
                    self.files_processed += 1;
                    return true;
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "skipping unreadable Parquet file");
                    self.files_skipped += 1;
                }
            }
        }
        false
    }

    pub fn files_processed(&self) -> usize {
        self.files_processed
    }

    pub fn files_skipped(&self) -> usize {
        self.files_skipped
    }
}

impl DataSource for MultiDayDataSource {
    fn next_event(&mut self) -> Option<MarketEvent> {
        loop {
            // Try current source
            if let Some(ref mut source) = self.current_source
                && let Some(event) = source.next_event()
            {
                let ts = event.timestamp.as_nanos();
                if ts < self.last_ts {
                    warn!(
                        ts,
                        last_ts = self.last_ts,
                        "non-monotonic timestamp across file boundary"
                    );
                }
                self.last_ts = ts;
                self.total_events += 1;
                return Some(event);
            }

            // Current source exhausted — try next file
            if !self.advance_file() {
                // Log summary when all files processed
                if self.files_processed > 0 || self.files_skipped > 0 {
                    info!(
                        total_events = self.total_events,
                        files_processed = self.files_processed,
                        files_skipped = self.files_skipped,
                        "multi-day replay complete"
                    );
                }
                return None;
            }
        }
    }

    fn reset(&mut self) {
        self.current_source = None;
        self.file_index = 0;
        self.total_events = 0;
        self.files_processed = 0;
        self.files_skipped = 0;
        self.last_ts = 0;
    }

    fn event_count(&self) -> usize {
        self.total_events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::parquet_writer::MarketDataWriter;
    use futures_bmad_core::{FixedPrice, MarketEventType, Side, UnixNanos};

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
    fn multi_day_iterates_chronologically() {
        let dir = tempfile::tempdir().unwrap();

        // Write day 1 (2026-04-18)
        let ts1 = 1_776_470_400_000_000_000u64;
        let mut w1 = MarketDataWriter::new(dir.path().to_path_buf(), "MES".to_string());
        w1.write_event(&make_event(ts1, 18000)).unwrap();
        w1.write_event(&make_event(ts1 + 1_000_000, 18001)).unwrap();
        w1.flush().unwrap();

        // Write day 2 (2026-04-19)
        let ts2 = ts1 + 86_400_000_000_000;
        let mut w2 = MarketDataWriter::new(dir.path().to_path_buf(), "MES".to_string());
        w2.write_event(&make_event(ts2, 18100)).unwrap();
        w2.flush().unwrap();

        let start = NaiveDate::from_ymd_opt(2026, 4, 18).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let mut source = MultiDayDataSource::new(dir.path(), "MES", start, end);

        // Should get events in order: day1_e1, day1_e2, day2_e1
        let e1 = source.next_event().unwrap();
        assert_eq!(e1.price.raw(), 18000);

        let e2 = source.next_event().unwrap();
        assert_eq!(e2.price.raw(), 18001);

        let e3 = source.next_event().unwrap();
        assert_eq!(e3.price.raw(), 18100);

        assert!(source.next_event().is_none());
        assert_eq!(source.event_count(), 3);
        assert_eq!(source.files_processed(), 2);
    }

    #[test]
    fn skips_missing_dates() {
        let dir = tempfile::tempdir().unwrap();

        // Only write day 2, skip day 1
        let ts2 = 1_776_470_400_000_000_000u64 + 86_400_000_000_000;
        let mut w = MarketDataWriter::new(dir.path().to_path_buf(), "MES".to_string());
        w.write_event(&make_event(ts2, 18100)).unwrap();
        w.flush().unwrap();

        let start = NaiveDate::from_ymd_opt(2026, 4, 18).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let mut source = MultiDayDataSource::new(dir.path(), "MES", start, end);

        let e = source.next_event().unwrap();
        assert_eq!(e.price.raw(), 18100);
        assert!(source.next_event().is_none());
        assert_eq!(source.files_processed(), 1);
    }
}
