//! End to end: `parquet_metadata` writes the SAS metadata into the Parquet file's
//! key-value metadata under `sas7bdat.metadata`, `parquet_metadata_additional` stores a
//! caller's string verbatim under `sas7bdat.additional`, and without them the file has
//! neither key.
//!
//! The unit tests in `sas_metadata.rs` cover the JSON shape of one column. These cover what
//! a reader of the written file sees: the key is there (or not), the JSON describes the
//! dataset it came from, and a column projection is reflected in output order.

use parquet::file::reader::{FileReader, SerializedFileReader};
use sas7bdat::Dataset;
use sas7bdat_convert::sas_metadata::{PARQUET_ADDITIONAL_METADATA_KEY, PARQUET_METADATA_KEY};
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

/// Every value stored under `key` in the file's key-value metadata, in footer order.
fn kv_values(parquet: &Path, key: &str) -> Vec<Option<String>> {
    let reader = SerializedFileReader::new(File::open(parquet).expect("open parquet"))
        .expect("read parquet footer");
    reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .map(|kv| {
            kv.iter()
                .filter(|kv| kv.key == key)
                .map(|kv| kv.value.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The value stored under [`PARQUET_METADATA_KEY`], if any.
fn sas_metadata(parquet: &Path) -> Option<Value> {
    let values = kv_values(parquet, PARQUET_METADATA_KEY);
    assert!(
        values.len() <= 1,
        "at most one {PARQUET_METADATA_KEY} entry"
    );
    let json = values.into_iter().next()?.expect("metadata value is set");
    Some(serde_json::from_str(&json).expect("metadata value is JSON"))
}

/// The value stored under [`PARQUET_ADDITIONAL_METADATA_KEY`], if any.
fn additional_metadata(parquet: &Path) -> Option<String> {
    let values = kv_values(parquet, PARQUET_ADDITIONAL_METADATA_KEY);
    assert!(
        values.len() <= 1,
        "at most one {PARQUET_ADDITIONAL_METADATA_KEY} entry"
    );
    Some(values.into_iter().next()?.expect("additional value is set"))
}

fn convert_with_additional(additional: &str, parquet_metadata: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = ConvertOptions {
        parquet_metadata,
        parquet_metadata_additional: Some(additional.to_owned()),
        ..ConvertOptions::default()
    };
    convert(&fixture(), &dir.path().join("out.parquet"), &options);
    dir
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
    assert!(
        additional_metadata(&output).is_none(),
        "no sas7bdat.additional key without parquet_metadata_additional"
    );
}

#[test]
fn stores_the_additional_string_verbatim() {
    // Not JSON-normalised: spacing, key order, escapes and non-ASCII text survive as given.
    let additional = r#"{"id": 101, "nom": "Dupont", "prénom": "Élodie", "actif": true, "roles": ["admin", "éditeur"], "note": "il a dit \"oui\" ; 'non'"}"#;
    let dir = convert_with_additional(additional, false);
    let output = dir.path().join("out.parquet");

    assert_eq!(additional_metadata(&output).as_deref(), Some(additional));
    assert!(
        sas_metadata(&output).is_none(),
        "parquet_metadata_additional does not imply parquet_metadata"
    );
}

#[test]
fn stores_a_string_that_is_not_json() {
    let additional = "  libre, pas du JSON {  ";
    let dir = convert_with_additional(additional, false);
    assert_eq!(
        additional_metadata(&dir.path().join("out.parquet")).as_deref(),
        Some(additional)
    );
}

#[test]
fn stores_an_empty_string() {
    let dir = convert_with_additional("", false);
    assert_eq!(
        additional_metadata(&dir.path().join("out.parquet")).as_deref(),
        Some("")
    );
}

#[test]
fn keeps_both_keys_when_both_are_asked_for() {
    let additional = r#"{"id": 101}"#;
    let dir = convert_with_additional(additional, true);
    let output = dir.path().join("out.parquet");

    assert_eq!(additional_metadata(&output).as_deref(), Some(additional));
    let json = sas_metadata(&output).expect("sas7bdat.metadata key is present");
    let ds = Dataset::open(fixture()).expect("open dataset");
    assert_eq!(json["row_count"], ds.metadata().row_count);
    assert_eq!(json["column_count"], ds.columns().len());
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
