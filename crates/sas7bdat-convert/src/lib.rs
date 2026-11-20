//! Convert SAS7BDAT files to Parquet, CSV or TSV — one file, or a directory tree
//! mirrored into an output root.
//!
//! This is the machinery behind the `sas7bdat convert` command, shared by the CLI and the
//! `fastsas.convert` R package. The R package needs conversion to happen entirely in Rust,
//! because the files it is pointed at are far too large to pass through an R session.
//!
//! The split is along the UI boundary. Everything here takes
//! plain options and returns plain outcomes; argument parsing, progress bars,
//! terminal styling and reporting stay with the caller. [`ConvertOptions`] is
//! deliberately a struct of values rather than a builder — callers construct it from
//! their own argument types, and adding a field is then a compile error at every call
//! site rather than a silently-defaulted behaviour change.

#![forbid(unsafe_code)]

pub mod catalog;
pub mod driver;
pub mod export;
pub mod parquet_pipeline;
pub mod paths;
pub mod sas_metadata;
pub mod selection;
pub mod time_mode;
pub mod values;

pub use driver::{
    ConvertObserver, ConvertOptions, ConvertOutcome, NoObserver, convert_file, convert_tree,
};

use std::path::PathBuf;

/// Output format.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum SinkKind {
    #[default]
    Parquet,
    Csv,
    Tsv,
}

impl SinkKind {
    /// The file extension this sink writes, without a leading dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Parquet => "parquet",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
        }
    }
}

/// Whether a directory input is walked past its top level.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum RecursionMode {
    #[default]
    Recursive,
    Never,
}

/// How a SAS TIME column is written.
///
/// SAS stores a time as signed seconds since midnight, with no bound: an elapsed duration or
/// a negative offset is as valid as a clock time. The default keeps every value; the other
/// modes trade that for a type a given reader handles better. A column counts as TIME when
/// SAS declared it with a time format.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum TimeMode {
    /// As each format writes it today: Arrow `Duration(Nanosecond)` in Parquet (a bare
    /// `INT64` to readers that ignore the Arrow schema), `HH:MM:SS` text in CSV/TSV.
    #[default]
    Native,
    /// `HH:MM:SS[.fff]` text in every format, the CSV rendering: sign and hours past 24
    /// are kept.
    Text,
    /// The SAS value itself, seconds since midnight, as a `DOUBLE` (`Float64`).
    Seconds,
    /// A clock time: Parquet `TIME` in microseconds (Arrow `Time64(Microsecond)`), which
    /// readers such as DuckDB load as `TIME`. A value outside `[00:00:00, 24:00:00)` has no
    /// such representation and is written as null.
    Time,
}

/// Parquet compression codec. Zstd by default: a strong size win over uncompressed
/// while staying fast to decode and broadly readable.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum CompressionCodec {
    /// Zstandard, level 3 — best size/speed balance.
    #[default]
    Zstd,
    /// LZ4 (`lz4_raw`) — fastest decode, larger than Zstd.
    Lz4,
    /// Snappy — widely compatible, weaker ratio.
    Snappy,
    /// No compression.
    None,
}

/// Where an output file goes, given the input it came from.
///
/// `root` is the directory an input was discovered under, and is what makes a mirrored
/// tree possible: the path below it is preserved under [`out_dir`](Self::out_dir).
#[derive(Clone, Debug, Default)]
pub struct OutputLayout {
    /// Root of the output tree. `None` writes beside each input.
    pub out_dir: Option<PathBuf>,
    /// Write every output directly into `out_dir`, discarding the input's tree.
    pub flatten: bool,
    /// Format, which also decides the file extension.
    pub sink: SinkKind,
}
