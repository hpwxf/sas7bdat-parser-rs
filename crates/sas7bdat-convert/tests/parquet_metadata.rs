//! End to end: `parquet_metadata` writes the SAS metadata into the Parquet file's
//! key-value metadata under `sas7bdat.metadata`, and leaves the file alone without it.
//!
//! The unit tests in `sas_metadata.rs` cover the JSON shape of one column. These cover what
//! a reader of the written file sees: the key is there (or not), the JSON describes the
//! dataset it came from, and a column projection is reflected in output order.

use parquet::file::reader::{FileReader, SerializedFileReader};
use sas7bdat::Dataset;
use sas7bdat_convert::sas_metadata::PARQUET_METADATA_KEY;
use sas7bdat_convert::{ConvertOptions, NoObserver, convert_file};
use serde_json::Value;
use std::fs::File;
use std::path::{Path, PathBuf};

/// A tracked fixture that carries SAS formats, so the `format` field is exercised.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sas7bdat/tests/fixtures/corpus/parso/date_formats.sas7bdat")
}

fn convert(input: &Path, output: &Path, options: &ConvertOptions) {
    let root = input.parent().expect("fixture has a parent directory");
    convert_file(root, input, Some(output), options, None, &NoObserver).expect("convert");
}

/// The value stored under [`PARQUET_METADATA_KEY`], if any.
fn sas_metadata(parquet: &Path) -> Option<Value> {
    let reader = SerializedFileReader::new(File::open(parquet).expect("open parquet"))
        .expect("read parquet footer");
    let kv = reader.metadata().file_metadata().key_value_metadata()?;
    let entry = kv.iter().find(|kv| kv.key == PARQUET_METADATA_KEY)?;
    let json = entry.value.as_deref().expect("metadata value is set");
    Some(serde_json::from_str(json).expect("metadata value is JSON"))
}

fn opt_str(value: Option<&String>) -> Value {
    value.map_or(Value::Null, |s| Value::String(s.clone()))
}

#[test]
fn embeds_dataset_and_column_metadata() {
    let input = fixture();
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("out.parquet");
    let options = ConvertOptions {
        parquet_metadata: true,
        ..ConvertOptions::default()
    };
    convert(&input, &output, &options);

    let json = sas_metadata(&output).expect("sas7bdat.metadata key is present");
    let ds = Dataset::open(&input).expect("open dataset");
    let meta = ds.metadata();
    let columns = ds.columns();

    assert_eq!(json["row_count"], meta.row_count);
    assert_eq!(json["column_count"], columns.len());
    assert_eq!(json["table_name"], opt_str(meta.table_name.as_ref()));
    assert_eq!(json["file_label"], opt_str(meta.file_label.as_ref()));
    assert_eq!(json["encoding"], opt_str(meta.encoding.as_ref()));

    let written = json["columns"].as_array().expect("columns is an array");
    assert_eq!(written.len(), columns.len());
    for (got, want) in written.iter().zip(columns) {
        assert_eq!(got["index"], want.index, "index of {}", want.name);
        assert_eq!(got["name"], want.name.as_str());
        assert_eq!(
            got["kind"],
            format!("{:?}", want.logical_type).to_lowercase(),
            "kind of {}",
            want.name
        );
        assert_eq!(got["width"], want.physical_width, "width of {}", want.name);
        assert_eq!(
            got["label"],
            opt_str(want.label.as_ref()),
            "label of {}",
            want.name
        );
        assert_eq!(
            got["format"],
            opt_str(want.format.as_ref()),
            "format of {}",
            want.name
        );
    }
    assert!(
        written.iter().any(|c| c["format"].is_string()),
        "the fixture should carry at least one SAS format"
    );
}

#[test]
fn omits_the_key_by_default() {
    let input = fixture();
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("out.parquet");
    convert(&input, &output, &ConvertOptions::default());

    assert!(
        sas_metadata(&output).is_none(),
        "no sas7bdat.metadata key without parquet_metadata"
    );
}

#[test]
fn describes_projected_columns_in_output_order() {
    let input = fixture();
    let ds = Dataset::open(&input).expect("open dataset");
    let columns = ds.columns();
    assert!(
        columns.len() >= 2,
        "the fixture needs two columns to reorder"
    );
    let first = &columns[0];
    let last = &columns[columns.len() - 1];

    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("out.parquet");
    let options = ConvertOptions {
        parquet_metadata: true,
        columns: Some(vec![last.name.clone(), first.name.clone()]),
        ..ConvertOptions::default()
    };
    convert(&input, &output, &options);

    let json = sas_metadata(&output).expect("sas7bdat.metadata key is present");
    assert_eq!(json["column_count"], 2);
    assert_eq!(json["row_count"], ds.metadata().row_count);
    let written = json["columns"].as_array().expect("columns is an array");
    let described: Vec<(&str, u64)> = written
        .iter()
        .map(|c| {
            (
                c["name"].as_str().expect("name"),
                c["index"].as_u64().expect("index"),
            )
        })
        .collect();
    assert_eq!(
        described,
        vec![
            (last.name.as_str(), last.index as u64),
            (first.name.as_str(), first.index as u64),
        ]
    );

    let reader = SerializedFileReader::new(File::open(&output).expect("open parquet"))
        .expect("read parquet footer");
    let schema = reader.metadata().file_metadata().schema_descr();
    let fields: Vec<String> = (0..schema.num_columns())
        .map(|i| schema.column(i).name().to_owned())
        .collect();
    assert_eq!(fields, vec![last.name.as_str(), first.name.as_str()]);
}
