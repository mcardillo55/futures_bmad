//! `ParquetReplaySource` — Story 7.1 own-format Parquet reader for replay input.
//!
//! Wraps the lower-level [`crate::data::ParquetDataSource`] (which handles single
//! files) and adds:
//!
//! * **Path-or-directory input**: a single `.parquet` file is read directly; a
//!   directory is enumerated for `*.parquet` children, sorted lexicographically
//!   (date-partitioned files like `2026-04-15.parquet` therefore replay in
//!   chronological order), and iterated in order.
//! * **Schema validation on open**: opening a file with a missing or
//!   wrong-typed expected column fails fast with a clear error rather than
//!   surfacing the mismatch as silently dropped events at iteration time.
//! * **Iterator + DataSource impl**: the source is both a Rust [`Iterator`]
//!   (per Story 7.1 Task 2.3) and an [`crate::data::DataSource`] (so the
//!   existing [`crate::replay::ReplayDriver`] can drain it without
//!   modification).
//!
//! The lower-level [`crate::data::ParquetDataSource`] retains its own tests
//! (Story 2.5); this layer adds the directory enumeration + schema gate that
//! the replay orchestrator needs.

use std::fs;
use std::path::{Path, PathBuf};

use arrow::datatypes::DataType;
use futures_bmad_core::MarketEvent;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::fs::File;
use tracing::{info, warn};

use crate::data::data_source::DataSource;
use crate::data::parquet_source::ParquetDataSource;

/// Required Parquet columns and their types. Story 7.1 Task 2.5 — open-time
/// schema validation.
const EXPECTED_COLUMNS: &[(&str, DataType)] = &[
    ("timestamp", DataType::Int64),
    ("price", DataType::Int64),
    ("size", DataType::UInt32),
    ("side", DataType::Int8),
    ("event_type", DataType::Int8),
    ("symbol_id", DataType::UInt32),
];

#[derive(Debug, thiserror::Error)]
pub enum ReplaySourceError {
    #[error("path does not exist: {0}")]
    PathMissing(PathBuf),
    #[error("io error opening {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("Parquet error opening {0}: {1}")]
    Parquet(PathBuf, String),
    #[error("schema mismatch in {0}: missing column `{1}`")]
    MissingColumn(PathBuf, String),
    #[error("schema mismatch in {0}: column `{1}` has type {2:?}, expected {3:?}")]
    WrongColumnType(PathBuf, String, DataType, DataType),
    #[error("no Parquet files found in directory {0}")]
    EmptyDirectory(PathBuf),
}

/// Parquet-backed replay input.
///
/// Construct with [`ParquetReplaySource::open`] passing either a single
/// `.parquet` file or a directory containing date-partitioned files.
pub struct ParquetReplaySource {
    /// Files to iterate, in the order they were resolved (directory: sorted
    /// lexicographically; single file: just one entry).
    files: Vec<PathBuf>,
    /// Currently-open file source (lazy: opened on first event from each file).
    current: Option<ParquetDataSource>,
    /// Index into `files` of the next file to open.
    next_file_idx: usize,
    /// Total events emitted so far (across all files).
    total_events: usize,
}

impl std::fmt::Debug for ParquetReplaySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParquetReplaySource")
            .field("files", &self.files)
            .field("next_file_idx", &self.next_file_idx)
            .field("total_events", &self.total_events)
            .field("has_current", &self.current.is_some())
            .finish()
    }
}

impl ParquetReplaySource {
    /// Open a Parquet replay source.
    ///
    /// `path` may be a single `.parquet` file OR a directory containing one or
    /// more `.parquet` files (typical: date-partitioned daily files such as
    /// `data/market/MES/2026-04-15.parquet`). Directory contents are sorted
    /// by filename so date-partitioned files replay chronologically.
    ///
    /// All files referenced have their schema validated up front so the caller
    /// gets a single, clear error rather than partial replay output before a
    /// later file fails mid-stream.
    pub fn open(path: &Path) -> Result<Self, ReplaySourceError> {
        if !path.exists() {
            return Err(ReplaySourceError::PathMissing(path.to_path_buf()));
        }

        let files = if path.is_dir() {
            let mut entries: Vec<PathBuf> = fs::read_dir(path)
                .map_err(|e| ReplaySourceError::Io(path.to_path_buf(), e))?
                .filter_map(|res| res.ok())
                .map(|d| d.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("parquet"))
                .collect();
            if entries.is_empty() {
                return Err(ReplaySourceError::EmptyDirectory(path.to_path_buf()));
            }
            entries.sort();
            entries
        } else {
            vec![path.to_path_buf()]
        };

        // Validate schema on every file at open time so partial replay never
        // happens — operators want a single clean error before any events are
        // emitted.
        for f in &files {
            validate_schema(f)?;
        }

        info!(
            target: "replay::data_source",
            files = files.len(),
            "ParquetReplaySource opened"
        );

        Ok(Self {
            files,
            current: None,
            next_file_idx: 0,
            total_events: 0,
        })
    }

    /// Total events produced so far across all files.
    pub fn events_emitted(&self) -> usize {
        self.total_events
    }

    /// Number of resolved input files (1 for single-file mode, N for directory).
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Resolve the next file in the queue and load it. Returns `Ok(false)`
    /// when no more files remain.
    fn advance_file(&mut self) -> Result<bool, ReplaySourceError> {
        while self.next_file_idx < self.files.len() {
            let path = self.files[self.next_file_idx].clone();
            self.next_file_idx += 1;
            match ParquetDataSource::new(path.clone()) {
                Ok(src) => {
                    info!(
                        target: "replay::data_source",
                        path = %path.display(),
                        "loaded replay file"
                    );
                    self.current = Some(src);
                    return Ok(true);
                }
                Err(e) => {
                    // Schema was validated up front; a load failure here is
                    // unexpected (file deleted between open and read, IO
                    // glitch, etc). Skip with a warn rather than aborting the
                    // entire replay.
                    warn!(
                        target: "replay::data_source",
                        path = %path.display(),
                        error = %e,
                        "skipping unreadable replay file"
                    );
                }
            }
        }
        Ok(false)
    }

    fn next_event_internal(&mut self) -> Option<MarketEvent> {
        loop {
            if let Some(src) = self.current.as_mut()
                && let Some(ev) = src.next_event()
            {
                self.total_events += 1;
                return Some(ev);
            }
            // Current source exhausted (or none yet) — advance.
            match self.advance_file() {
                Ok(true) => continue,
                Ok(false) => return None,
                Err(e) => {
                    warn!(
                        target: "replay::data_source",
                        error = %e,
                        "advance_file failed during replay"
                    );
                    return None;
                }
            }
        }
    }
}

/// Validate that the file at `path` carries the columns the engine's
/// own-format reader expects, with the right Arrow types. Returns the parsed
/// schema on success (informational).
fn validate_schema(path: &Path) -> Result<(), ReplaySourceError> {
    let file = File::open(path).map_err(|e| ReplaySourceError::Io(path.to_path_buf(), e))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| ReplaySourceError::Parquet(path.to_path_buf(), e.to_string()))?;
    let schema = builder.schema();

    for (name, expected_type) in EXPECTED_COLUMNS {
        let field = schema
            .field_with_name(name)
            .map_err(|_| ReplaySourceError::MissingColumn(path.to_path_buf(), (*name).into()))?;
        if field.data_type() != expected_type {
            return Err(ReplaySourceError::WrongColumnType(
                path.to_path_buf(),
                (*name).into(),
                field.data_type().clone(),
                expected_type.clone(),
            ));
        }
    }

    Ok(())
}

impl Iterator for ParquetReplaySource {
    type Item = MarketEvent;

    fn next(&mut self) -> Option<MarketEvent> {
        self.next_event_internal()
    }
}

impl DataSource for ParquetReplaySource {
    fn next_event(&mut self) -> Option<MarketEvent> {
        self.next_event_internal()
    }

    fn reset(&mut self) {
        self.current = None;
        self.next_file_idx = 0;
        self.total_events = 0;
    }

    fn event_count(&self) -> usize {
        self.total_events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::parquet_writer::MarketDataWriter;
    use chrono::NaiveDate;
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

    /// 2.3 — single-file replay yields events in order.
    #[test]
    fn single_file_iterates_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = MarketDataWriter::new(dir.path().to_path_buf(), "MES".into());
        let ts0 = 1_776_470_400_000_000_000u64;
        for i in 0..5u64 {
            writer
                .write_event(&make_event(ts0 + i * 1_000_000, 18_000 + i as i64))
                .unwrap();
        }
        writer.flush().unwrap();

        let path = crate::persistence::parquet_writer::file_path_for(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        );
        let mut src = ParquetReplaySource::open(&path).unwrap();
        for i in 0..5 {
            let ev = src.next().unwrap();
            assert_eq!(ev.price.raw(), 18_000 + i as i64);
        }
        assert!(src.next().is_none());
        assert_eq!(src.events_emitted(), 5);
        assert_eq!(src.file_count(), 1);
    }

    /// 2.4 — directory input enumerates child `.parquet` files in date order.
    #[test]
    fn directory_iterates_files_chronologically() {
        let dir = tempfile::tempdir().unwrap();
        // Day 1 (2026-04-18) and Day 2 (2026-04-19) — file_path_for puts both
        // under `<dir>/market/MES/`.
        let ts1 = 1_776_470_400_000_000_000u64;
        {
            let mut w1 = MarketDataWriter::new(dir.path().to_path_buf(), "MES".into());
            w1.write_event(&make_event(ts1, 18_000)).unwrap();
            w1.write_event(&make_event(ts1 + 1_000_000, 18_001))
                .unwrap();
            w1.flush().unwrap();
        }
        let ts2 = ts1 + 86_400_000_000_000;
        {
            let mut w2 = MarketDataWriter::new(dir.path().to_path_buf(), "MES".into());
            w2.write_event(&make_event(ts2, 18_100)).unwrap();
            w2.flush().unwrap();
        }

        let symbol_dir = dir.path().join("market").join("MES");
        let mut src = ParquetReplaySource::open(&symbol_dir).unwrap();
        assert_eq!(src.file_count(), 2);

        // Lexicographic sort on YYYY-MM-DD.parquet => chronological.
        let e1 = src.next().unwrap();
        assert_eq!(e1.price.raw(), 18_000);
        let e2 = src.next().unwrap();
        assert_eq!(e2.price.raw(), 18_001);
        let e3 = src.next().unwrap();
        assert_eq!(e3.price.raw(), 18_100);
        assert!(src.next().is_none());
    }

    /// 2.5 — schema validation on a foreign Parquet file fails fast with a
    /// clear error.
    #[test]
    fn open_rejects_file_with_wrong_schema() {
        use arrow::array::Int64Array;
        use arrow::datatypes::{Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foreign.parquet");

        // Build a Parquet file with only a `timestamp` column (missing all
        // others).
        let schema = Arc::new(Schema::new(vec![Field::new(
            "timestamp",
            DataType::Int64,
            false,
        )]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Int64Array::from(vec![1_i64, 2, 3]))],
        )
        .unwrap();
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        let err = ParquetReplaySource::open(&path).unwrap_err();
        assert!(
            matches!(err, ReplaySourceError::MissingColumn(_, ref c) if c == "price"),
            "unexpected error: {err:?}"
        );
    }

    /// 2.5b — schema validation on a column with wrong type fails fast.
    #[test]
    fn open_rejects_file_with_wrong_column_type() {
        use arrow::array::{Float64Array, Int8Array, Int64Array, UInt32Array};
        use arrow::datatypes::{Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrong_type.parquet");

        // `price` is Float64 instead of Int64 — should be rejected.
        let schema = Arc::new(Schema::new(vec![
            Field::new("timestamp", DataType::Int64, false),
            Field::new("price", DataType::Float64, false),
            Field::new("size", DataType::UInt32, false),
            Field::new("side", DataType::Int8, false),
            Field::new("event_type", DataType::Int8, false),
            Field::new("symbol_id", DataType::UInt32, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(vec![1_i64])),
                Arc::new(Float64Array::from(vec![100.0_f64])),
                Arc::new(UInt32Array::from(vec![1_u32])),
                Arc::new(Int8Array::from(vec![0_i8])),
                Arc::new(Int8Array::from(vec![0_i8])),
                Arc::new(UInt32Array::from(vec![0_u32])),
            ],
        )
        .unwrap();
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        let err = ParquetReplaySource::open(&path).unwrap_err();
        assert!(
            matches!(err, ReplaySourceError::WrongColumnType(_, ref c, _, _) if c == "price"),
            "unexpected error: {err:?}"
        );
    }

    /// `open` on a missing path surfaces a clear error.
    #[test]
    fn open_rejects_missing_path() {
        let err = ParquetReplaySource::open(Path::new("/no/such/path.parquet")).unwrap_err();
        assert!(matches!(err, ReplaySourceError::PathMissing(_)));
    }

    /// `open` on an empty directory surfaces a clear error rather than
    /// silently producing zero events.
    #[test]
    fn open_rejects_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = ParquetReplaySource::open(dir.path()).unwrap_err();
        assert!(matches!(err, ReplaySourceError::EmptyDirectory(_)));
    }

    /// `DataSource::reset` rewinds both file index and event counter.
    #[test]
    fn reset_rewinds_iteration() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = MarketDataWriter::new(dir.path().to_path_buf(), "MES".into());
        let ts0 = 1_776_470_400_000_000_000u64;
        writer.write_event(&make_event(ts0, 18_000)).unwrap();
        writer.flush().unwrap();
        let path = crate::persistence::parquet_writer::file_path_for(
            dir.path(),
            "MES",
            NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        );

        let mut src = ParquetReplaySource::open(&path).unwrap();
        assert!(src.next().is_some());
        assert!(src.next().is_none());

        src.reset();
        assert_eq!(src.events_emitted(), 0);
        assert!(src.next().is_some());
    }
}
