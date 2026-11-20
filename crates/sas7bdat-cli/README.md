# sas7bdat-cli

A command-line tool for inspecting SAS7BDAT files, the binary format written by
SAS, and converting them to Parquet, CSV or TSV. It is a single Rust binary built on
the [`sas7bdat`](https://crates.io/crates/sas7bdat) reader, and needs no SAS
installation or C library.

## Installation

```sh
pip install sas7bdat-cli
# or, as an isolated tool:
uv tool install sas7bdat-cli
```

Wheels are built for Linux (x86-64 and ARM64), Windows (x86-64) and macOS (Apple
Silicon). On x86-64 the binary needs a CPU with AVX2: Intel Haswell (2013) or AMD
Excavator (2015) and later.

The wheel ships a single executable, `sas7bdat`. It is a plain binary wheel
(`py3-none-<platform>`), so it works with any Python version and has no
dependencies; pip is only the way it is delivered.

Rust users can install the same tool from source instead:

```sh
cargo install --git https://github.com/tkragholm/sas7bdat-parser-rs sas7bdat-cli
```

## Usage

```sh
sas7bdat info data.sas7bdat        # columns, types, labels, and a sample
sas7bdat head data.sas7bdat        # first rows as a table
sas7bdat convert data.sas7bdat     # -> data.parquet (Zstd)
sas7bdat convert data.sas7bdat --out out.csv        # format inferred from the extension
sas7bdat convert ./folder --out-dir out             # whole tree, mirrored
sas7bdat completions zsh           # shell completion script
```

Run `sas7bdat <command> --help` for the full options of a command.

### Metadata in the Parquet file

Two opt-in flags of `convert` write entries into the Parquet file's key-value
metadata, where plain Parquet readers can find them without the original file:

- `--parquet-metadata` stores the SAS labels, formats, kinds and widths as JSON
  under `sas7bdat.metadata`.
- `--parquet-metadata-additional STRING` stores `STRING` as given, not parsed or
  validated, under `sas7bdat.additional`: a small tag of your own, typically JSON.

```sh
sas7bdat convert sales.sas7bdat --out-dir out --parquet-metadata \
  --parquet-metadata-additional '{"id": 101, "roles": ["admin", "editor"]}'
duckdb -c "SELECT decode(key), decode(value) FROM parquet_kv_metadata('out/sales.parquet')"
```

### TIME columns

SAS stores a time as signed seconds since midnight with no upper bound, so a TIME
column can hold durations and negative offsets as well as clock times.
`convert --time-mode` picks how such a column is written, in every format:

| Mode | Parquet (as DuckDB reads it) | CSV/TSV | Out-of-range values |
|---|---|---|---|
| `native` (default) | nanosecond duration (`BIGINT`) | `10:10:10` | kept |
| `text` | `10:10:10` (`VARCHAR`) | `10:10:10` | kept, e.g. `-01:30:00`, `25:00:00` |
| `seconds` | `36610.0` (`DOUBLE`) | `36610` | kept |
| `time` | `10:10:10` (`TIME`) | `10:10:10` | null |

## Related packages

- [`sas7bdat-polars`](https://pypi.org/project/sas7bdat-polars/): a Polars plugin
  built on the same reader, with lazy scans and projection pushdown.
  `pip install "sas7bdat-polars[cli]"` installs it together with this CLI.
- [`sas7bdat`](https://crates.io/crates/sas7bdat): the Rust crate.

## License

MIT. See the [repository](https://github.com/tkragholm/sas7bdat-parser-rs) for
third-party notices and the SAS trademark note.
