//! End to end: `time_mode` decides how a SAS TIME column reaches the file, in Parquet on both
//! write paths (the serial writer with one thread, the row-group pipeline otherwise) and in
//! CSV. The fixture holds one row, `10:10:10` (36,610 s), under seven TIME formats.
//!
//! The edge cases (missing, negative, past 24 h, fractional) are unit tests in
//! `src/time_mode.rs`; these check that every writer applies the mode to every TIME column.

use arrow_array::{
    Array, DurationNanosecondArray, Float64Array, RecordBatch, StringArray, Time64MicrosecondArray,
};
use arrow_schema::{DataType, TimeUnit};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{LogicalType, TimeUnit as ParquetTimeUnit};
use parquet::file::reader::{FileReader, SerializedFileReader};
use sas7bdat::SAS_LOGICAL_TYPE_KEY;
use sas7bdat_convert::{
    ConvertOptions, NoObserver, OutputLayout, SinkKind, TimeMode, convert_file,
};
use std::fs::File;
use std::path::{Path, PathBuf};

const SECONDS: f64 = 36_610.0;
const COLUMNS: usize = 7;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sas7bdat/tests/fixtures/corpus/parso/time_formats.sas7bdat")
}

fn convert(output: &Path, time_mode: TimeMode, sink: SinkKind, threads: Option<usize>) {
    let input = fixture();
    let options = ConvertOptions {
        layout: OutputLayout {
            sink,
            ..OutputLayout::default()
        },
        time_mode,
        parse_threads: threads,
        ..ConvertOptions::default()
    };
    let root = input.parent().expect("fixture has a parent directory");
    convert_file(root, &input, Some(output), &options, None, &NoObserver).expect("convert");
}

/// Convert to Parquet on both write paths and return the single record batch of each.
fn parquet_batches(time_mode: TimeMode) -> Vec<RecordBatch> {
    [Some(1), None]
        .into_iter()
        .map(|threads| {
            let dir = tempfile::tempdir().expect("tempdir");
            let output = dir.path().join("out.parquet");
            convert(&output, time_mode, SinkKind::Parquet, threads);
            let reader = ParquetRecordBatchReaderBuilder::try_new(
                File::open(&output).expect("open parquet"),
            )
            .expect("read parquet")
            .build()
            .expect("build reader");
            let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().expect("read batches");
            assert_eq!(batches.len(), 1, "one row, one batch");
            let batch = batches.into_iter().next().expect("a batch");
            assert_eq!(batch.num_columns(), COLUMNS);
            assert_eq!(batch.num_rows(), 1);
            for field in batch.schema().fields() {
                assert_eq!(
                    field
                        .metadata()
                        .get(SAS_LOGICAL_TYPE_KEY)
                        .map(String::as_str),
                    Some("TIME"),
                    "{} keeps its TIME tag under {time_mode:?}",
                    field.name()
                );
            }
            batch
        })
        .collect()
}

fn column<T: 'static>(batch: &RecordBatch, index: usize) -> &T {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<T>()
        .unwrap_or_else(|| panic!("column {index} is {}", batch.column(index).data_type()))
}

#[test]
fn native_parquet_is_a_nanosecond_duration() {
    for batch in parquet_batches(TimeMode::Native) {
        for index in 0..COLUMNS {
            assert_eq!(
                batch.column(index).data_type(),
                &DataType::Duration(TimeUnit::Nanosecond)
            );
            assert_eq!(
                column::<DurationNanosecondArray>(&batch, index).value(0),
                36_610_000_000_000
            );
        }
    }
}

#[test]
fn text_parquet_is_hh_mm_ss() {
    for batch in parquet_batches(TimeMode::Text) {
        for index in 0..COLUMNS {
            assert_eq!(column::<StringArray>(&batch, index).value(0), "10:10:10");
        }
    }
}

#[test]
fn seconds_parquet_is_the_sas_value() {
    for batch in parquet_batches(TimeMode::Seconds) {
        for index in 0..COLUMNS {
            let values = column::<Float64Array>(&batch, index);
            assert!(values.is_valid(0));
            assert!((values.value(0) - SECONDS).abs() < f64::EPSILON);
        }
    }
}

#[test]
fn time_parquet_is_a_microsecond_clock_time() {
    for batch in parquet_batches(TimeMode::Time) {
        for index in 0..COLUMNS {
            assert_eq!(
                column::<Time64MicrosecondArray>(&batch, index).value(0),
                36_610_000_000
            );
        }
    }

    // What a reader that ignores the Arrow schema sees: Parquet's own TIME(MICROS), the
    // annotation DuckDB maps to `TIME`.
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("out.parquet");
    convert(&output, TimeMode::Time, SinkKind::Parquet, None);
    let reader = SerializedFileReader::new(File::open(&output).expect("open parquet"))
        .expect("read parquet footer");
    let schema = reader.metadata().file_metadata().schema_descr();
    for index in 0..schema.num_columns() {
        assert!(
            matches!(
                schema.column(index).logical_type_ref(),
                Some(LogicalType::Time(time)) if matches!(time.unit, ParquetTimeUnit::MICROS)
            ),
            "column {index}: {:?}",
            schema.column(index).logical_type_ref()
        );
    }
}

fn csv_row(time_mode: TimeMode) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("out.csv");
    convert(&output, time_mode, SinkKind::Csv, None);
    let text = std::fs::read_to_string(&output).expect("read csv");
    let mut lines = text.lines();
    assert_eq!(
        lines.next().map(|header| header.split(',').count()),
        Some(COLUMNS)
    );
    lines.next().expect("one data row").to_owned()
}

#[test]
fn csv_follows_the_mode() {
    let row = |cell: &str| [cell; COLUMNS].join(",");
    assert_eq!(csv_row(TimeMode::Native), row("10:10:10"));
    assert_eq!(csv_row(TimeMode::Text), row("10:10:10"));
    assert_eq!(csv_row(TimeMode::Seconds), row("36610"));
    assert_eq!(csv_row(TimeMode::Time), row("10:10:10"));
}
