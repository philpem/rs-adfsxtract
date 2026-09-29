# acornfsextract

A read-only extractor and inspector for Acorn FileCore (ADFS) and DFS disc
images, written in Rust. It understands the full FileCore format matrix -
old-map S/M/L floppies, D, E/F floppies, big-directory (E+/F+/G) discs and
raw hard-disc images - as well as DFS single- and double-sided images.

The FileCore format is documented externally and is **not** vendored here.
The authoritative specification is the FileCore Technical Guide, section
references in the code (e.g. "guide §2.4") point at it:

- https://github.com/philpem/arcology/blob/master/doc/format_info/acorn32bit/filecore_guide.md

## Usage

```sh
# Disc metadata (format, disc name, size, structural health)
acornfsextract info <image> [--format text|json]

# Full structural verification with typed diagnostics
acornfsextract verify <image> [--format text|json] [--diagnostics <path>]

# Extract every file into a directory tree
acornfsextract extract <image> --output <dir> [options]
```

`extract` options:

| Flag | Meaning |
|------|---------|
| `--output <dir>` | Destination directory (required) |
| `--rescue-map <path>` | ddrescue/gddrescue mapfile marking known bad sectors |
| `--bad-sectors skip\|null\|marker` | How to handle bad sectors (default `null`) |
| `--on-broken-directory fail\|skip\|recover` | Policy for a broken directory (default `recover`) |
| `--inf` | Write a `.inf` sidecar per extracted file |
| `--log <path>` | Write an extraction log (bad sectors, skips, anomalies) |
| `--dry-run` | List what would be extracted without writing |
| `--format text\|json` | Output format |

FileCore is detected first; DFS is only tried as a fallback since it has no
magic number.

## Tests

```sh
cargo test
```

The suite includes always-on regression tests against real disc images
committed to `data/` (S/M/L, D, E and F format fixtures, all containing the
same files - which lets the tests assert byte-identical extraction across
formats and thereby validate the S/M/L sector-interleave translation). See
`data/README.md` for provenance.

An optional stress harness pulls copyrighted corpus images at runtime only
when an Arcology API key is set (it self-skips otherwise, so CI never needs
network or a key):

```sh
ACORNFS_ARCOLOGY_KEY=<key> cargo test stress_against_arcology_corpus
```

Configurable via `ACORNFS_ARCOLOGY_API` (base URL), `ACORNFS_ARCOLOGY_MAX`
(sample size), `ACORNFS_ARCOLOGY_EXT` (extensions to sample) and
`ACORNFS_ARCOLOGY_MAXSIZE` (skip larger artefacts).
