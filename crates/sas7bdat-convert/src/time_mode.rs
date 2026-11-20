//! Writing a SAS TIME column under a [`TimeMode`].
//!
//! The reader's Arrow schema types a TIME column as `Duration(Nanosecond)` and tags the field
//! with `sas.logical_type: TIME`. [`TimeMode::Native`] keeps that. The other modes retype the
//! tagged fields ([`output_schema`]) and build those columns straight from the decoded SAS
//! seconds ([`record_batch`]), so `Seconds` writes the stored value rather than a nanosecond
//! round trip of it. Every other column goes through the reader's own conversion.
//!
//! The tag, not the `Duration` type, identifies the columns: it survives the retyping, so the
//! written file still says which columns SAS declared as times.

use crate::TimeMode;
use crate::values::{microseconds, write_sas_time};
use arrow_array::{ArrayRef, Float64Array, RecordBatch, StringArray, Time64MicrosecondArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use sas7bdat::{
    CellValue, Error, LogicalType, OwnedColumnBuffer, OwnedColumnarBatch, SAS_LOGICAL_TYPE_KEY,
};
use std::sync::Arc;

/// Microseconds in a day: a clock time is in `[0, MICROS_PER_DAY)`.
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Whether `field` is a column SAS declared with a TIME format.
#[must_use]
pub fn is_time_field(field: &Field) -> bool {
    field
        .metadata()
        .get(SAS_LOGICAL_TYPE_KEY)
        .is_some_and(|kind| kind == "TIME")
}

/// The Arrow type a TIME column is written as under `mode`; `None` keeps the reader's type.
const fn time_data_type(mode: TimeMode) -> Option<DataType> {
    match mode {
        TimeMode::Native => None,
        TimeMode::Text => Some(DataType::Utf8),
        TimeMode::Seconds => Some(DataType::Float64),
        TimeMode::Time => Some(DataType::Time64(TimeUnit::Microsecond)),
    }
}

/// `schema` with its TIME fields retyped for `mode`. Names, nullability and metadata, the
/// TIME tag included, are kept; every other field is untouched.
#[must_use]
pub fn output_schema(schema: SchemaRef, mode: TimeMode) -> SchemaRef {
    let Some(data_type) = time_data_type(mode) else {
        return schema;
    };
    let fields: Vec<Arc<Field>> = schema
        .fields()
        .iter()
        .map(|field| {
            if is_time_field(field) {
                Arc::new(field.as_ref().clone().with_data_type(data_type.clone()))
            } else {
                Arc::clone(field)
            }
        })
        .collect();
    Arc::new(Schema::new(fields).with_metadata(schema.metadata().clone()))
}

/// Convert a decoded batch into a record batch of `schema`, which must come from
/// [`output_schema`] with the same `mode`.
///
/// # Errors
///
/// Returns an error if a column does not convert to its field's type.
pub fn record_batch(
    batch: OwnedColumnarBatch,
    schema: &SchemaRef,
    mode: TimeMode,
) -> sas7bdat::Result<RecordBatch> {
    if mode == TimeMode::Native {
        return batch.into_arrow_record_batch(SchemaRef::clone(schema));
    }
    let rows = batch.row_count;
    let arrays = batch
        .columns
        .into_iter()
        .zip(schema.fields())
        .map(|(column, field)| {
            if is_time_field(field) {
                time_array(&column, rows, mode)
            } else {
                column.into_arrow_array_as(field.data_type())
            }
        })
        .collect::<sas7bdat::Result<Vec<_>>>()?;
    RecordBatch::try_new(SchemaRef::clone(schema), arrays)
        .map_err(|err| Error::arrow(err.to_string()))
}

/// A TIME column as the array `mode` writes.
fn time_array(
    column: &OwnedColumnBuffer,
    rows: usize,
    mode: TimeMode,
) -> sas7bdat::Result<ArrayRef> {
    let seconds = (0..rows)
        .map(|row| time_seconds(column, row))
        .collect::<sas7bdat::Result<Vec<Option<f64>>>>()?;
    let array: ArrayRef = match mode {
        TimeMode::Native => {
            return Err(Error::arrow(
                "native TIME columns use the reader's conversion",
            ));
        }
        TimeMode::Seconds => Arc::new(Float64Array::from(seconds)),
        TimeMode::Text => {
            let mut text = String::new();
            Arc::new(
                seconds
                    .into_iter()
                    .map(|value| {
                        value.map(|seconds| {
                            text.clear();
                            write_sas_time(&mut text, seconds);
                            text.clone()
                        })
                    })
                    .collect::<StringArray>(),
            )
        }
        TimeMode::Time => Arc::new(
            seconds
                .into_iter()
                .map(|value| value.and_then(clock_micros))
                .collect::<Time64MicrosecondArray>(),
        ),
    };
    Ok(array)
}

/// The SAS seconds at `row` of a TIME column, `None` when the cell is missing.
fn time_seconds(column: &OwnedColumnBuffer, row: usize) -> sas7bdat::Result<Option<f64>> {
    let value = match column {
        OwnedColumnBuffer::Time { values, valid } => {
            is_valid(valid.as_ref(), row).then(|| f64::from(values[row].seconds_since_midnight))
        }
        OwnedColumnBuffer::F64 { values, valid } => {
            is_valid(valid.as_ref(), row).then_some(values[row])
        }
        OwnedColumnBuffer::I64 { values, valid } => {
            #[allow(clippy::cast_precision_loss)]
            let value = is_valid(valid.as_ref(), row).then(|| values[row] as f64);
            value
        }
        OwnedColumnBuffer::I32 { values, valid } => {
            is_valid(valid.as_ref(), row).then(|| f64::from(values[row]))
        }
        _ => {
            return Err(Error::arrow(
                "a TIME column decoded to a buffer that holds no numbers",
            ));
        }
    };
    Ok(value.filter(|seconds| !seconds.is_nan()))
}

/// Whether row `row` is set in a bit-packed validity mask; no mask means every row is.
fn is_valid(valid: Option<&Vec<u64>>, row: usize) -> bool {
    valid.is_none_or(|words| {
        words
            .get(row / 64)
            .is_some_and(|word| word >> (row % 64) & 1 == 1)
    })
}

/// `seconds` as microseconds since midnight, if that is a clock time. Rounded to the
/// microsecond first, the way the text rendering is, so the two agree on the boundary.
fn clock_micros(seconds: f64) -> Option<i64> {
    if !seconds.is_finite() {
        return None;
    }
    let micros = microseconds(seconds);
    (0..MICROS_PER_DAY).contains(&micros).then_some(micros)
}

/// The cell to write, and the logical type to render it as, for a CSV/TSV TIME column.
///
/// `Native` and `Text` keep the `HH:MM:SS` rendering. `Seconds` writes the number, so its
/// type becomes `Float`. `Time` blanks a value that is not a clock time.
#[must_use]
pub fn delimited_cell(cell: CellValue<'_>, mode: TimeMode) -> (CellValue<'_>, LogicalType) {
    let seconds = match &cell {
        CellValue::Time(time) => Some(f64::from(time.seconds_since_midnight)),
        CellValue::Float64(value) => Some(*value),
        #[allow(clippy::cast_precision_loss)]
        CellValue::Int64(value) => Some(*value as f64),
        CellValue::Int32(value) => Some(f64::from(*value)),
        _ => None,
    };
    match (mode, seconds) {
        (TimeMode::Seconds, Some(seconds)) => (CellValue::Float64(seconds), LogicalType::Float),
        (TimeMode::Time, Some(seconds)) if clock_micros(seconds).is_none() => {
            (CellValue::Null, LogicalType::Time)
        }
        _ => (cell, LogicalType::Time),
    }
}

#[cfg(test)]
mod tests {
    use super::{delimited_cell, is_time_field, output_schema, record_batch};
    use crate::TimeMode;
    use arrow_array::{
        Array, DurationNanosecondArray, Float64Array, StringArray, Time64MicrosecondArray,
    };
    use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
    use sas7bdat::{
        CellValue, LogicalType, OwnedColumnBuffer, OwnedColumnarBatch, SAS_LOGICAL_TYPE_KEY,
        SasTime,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    fn time_field() -> Field {
        Field::new("T", DataType::Duration(TimeUnit::Nanosecond), true).with_metadata(
            HashMap::from([(SAS_LOGICAL_TYPE_KEY.to_owned(), "TIME".to_owned())]),
        )
    }

    fn schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            time_field(),
            Field::new("X", DataType::Float64, true),
        ]))
    }

    /// Rows: 10:10:10, missing, -1 s, 25 h, 23:59:59.9999996 (rounds to 24:00:00), 1.5 s.
    fn values() -> Vec<f64> {
        vec![36_610.0, 0.0, -1.0, 90_000.0, 86_399.999_999_6, 1.5]
    }

    /// Bit 1 cleared: the second row is missing.
    fn validity() -> Option<Vec<u64>> {
        Some(vec![0b11_1101])
    }

    fn batch(time: OwnedColumnBuffer) -> OwnedColumnarBatch {
        OwnedColumnarBatch {
            row_base: Default::default(),
            row_count: values().len(),
            columns: vec![
                time,
                OwnedColumnBuffer::F64 {
                    values: vec![7.0; values().len()],
                    valid: None,
                },
            ],
        }
    }

    fn f64_batch() -> OwnedColumnarBatch {
        batch(OwnedColumnBuffer::F64 {
            values: values(),
            valid: validity(),
        })
    }

    #[test]
    fn only_tagged_fields_are_retyped_and_the_tag_is_kept() {
        let out = output_schema(schema(), TimeMode::Seconds);
        assert_eq!(out.field(0).data_type(), &DataType::Float64);
        assert!(
            is_time_field(out.field(0)),
            "the TIME tag survives the retyping"
        );
        assert_eq!(out.field(1), schema().field(1));

        let native = output_schema(schema(), TimeMode::Native);
        assert_eq!(
            native.field(0).data_type(),
            &DataType::Duration(TimeUnit::Nanosecond)
        );
        assert_eq!(
            output_schema(schema(), TimeMode::Text).field(0).data_type(),
            &DataType::Utf8
        );
        assert_eq!(
            output_schema(schema(), TimeMode::Time).field(0).data_type(),
            &DataType::Time64(TimeUnit::Microsecond)
        );
    }

    #[test]
    fn native_keeps_the_readers_duration() {
        let schema = output_schema(schema(), TimeMode::Native);
        let out = record_batch(f64_batch(), &schema, TimeMode::Native).expect("convert");
        let column = out
            .column(0)
            .as_any()
            .downcast_ref::<DurationNanosecondArray>()
            .expect("Duration(ns)");
        assert_eq!(column.value(0), 36_610_000_000_000);
        assert!(column.is_null(1));
        assert_eq!(column.value(2), -1_000_000_000);
    }

    #[test]
    fn seconds_writes_the_sas_value() {
        let schema = output_schema(schema(), TimeMode::Seconds);
        let out = record_batch(f64_batch(), &schema, TimeMode::Seconds).expect("convert");
        let column = out
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("Float64");
        let got: Vec<Option<f64>> = column.iter().collect();
        assert_eq!(
            got,
            vec![
                Some(36_610.0),
                None,
                Some(-1.0),
                Some(90_000.0),
                Some(86_399.999_999_6),
                Some(1.5)
            ]
        );
        // The other column goes through the reader's conversion, untouched.
        assert_eq!(out.column(1).data_type(), &DataType::Float64);
    }

    #[test]
    fn text_renders_like_csv() {
        let schema = output_schema(schema(), TimeMode::Text);
        let out = record_batch(f64_batch(), &schema, TimeMode::Text).expect("convert");
        let column = out
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("Utf8");
        let got: Vec<Option<&str>> = column.iter().collect();
        assert_eq!(
            got,
            vec![
                Some("10:10:10"),
                None,
                Some("-00:00:01"),
                Some("25:00:00"),
                Some("24:00:00"),
                Some("00:00:01.500")
            ]
        );
    }

    #[test]
    fn time_nulls_what_is_not_a_clock_time() {
        let schema = output_schema(schema(), TimeMode::Time);
        let out = record_batch(f64_batch(), &schema, TimeMode::Time).expect("convert");
        let column = out
            .column(0)
            .as_any()
            .downcast_ref::<Time64MicrosecondArray>()
            .expect("Time64(us)");
        let got: Vec<Option<i64>> = column.iter().collect();
        assert_eq!(
            got,
            vec![
                Some(36_610_000_000),
                None,
                None,
                None,
                None,
                Some(1_500_000)
            ]
        );
    }

    #[test]
    fn whole_second_buffers_convert_too() {
        let time = OwnedColumnBuffer::Time {
            values: [36_610, 0, -1, 90_000, 86_399, 1]
                .into_iter()
                .map(|seconds_since_midnight| SasTime {
                    seconds_since_midnight,
                })
                .collect(),
            valid: validity(),
        };
        let schema = output_schema(schema(), TimeMode::Seconds);
        let out = record_batch(batch(time), &schema, TimeMode::Seconds).expect("convert");
        let column = out
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("Float64");
        let got: Vec<Option<f64>> = column.iter().collect();
        assert_eq!(
            got,
            vec![
                Some(36_610.0),
                None,
                Some(-1.0),
                Some(90_000.0),
                Some(86_399.0),
                Some(1.0)
            ]
        );
    }

    #[test]
    fn delimited_cells_follow_the_mode() {
        let ten = || {
            CellValue::Time(SasTime {
                seconds_since_midnight: 36_610,
            })
        };
        assert!(matches!(
            delimited_cell(ten(), TimeMode::Native),
            (CellValue::Time(_), LogicalType::Time)
        ));
        assert!(matches!(
            delimited_cell(ten(), TimeMode::Text),
            (CellValue::Time(_), LogicalType::Time)
        ));
        assert!(matches!(
            delimited_cell(ten(), TimeMode::Seconds),
            (CellValue::Float64(v), LogicalType::Float) if v == 36_610.0
        ));
        assert!(matches!(
            delimited_cell(ten(), TimeMode::Time),
            (CellValue::Time(_), LogicalType::Time)
        ));
        assert!(matches!(
            delimited_cell(CellValue::Float64(90_000.0), TimeMode::Time),
            (CellValue::Null, _)
        ));
        assert!(matches!(
            delimited_cell(CellValue::Null, TimeMode::Seconds),
            (CellValue::Null, _)
        ));
    }
}
