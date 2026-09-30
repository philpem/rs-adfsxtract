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
| `--partition <label>` | Extract only the named partition (see below) |
| `--format text\|json` | Output format |

## Partitions

An image may contain more than one filesystem volume. A **hybrid** disc is an
ADFS filesystem that also carries an embedded Acorn File Server (AFS0)
partition; such an image exposes two partitions, `ADFS` and `AFS`. The
`info` and `verify` commands report every partition found. `extract`, when
more than one partition is present, extracts each into a subdirectory named
after that partition (e.g. `<dir>/ADFS/...` and `<dir>/AFS/...`). Use
`--partition ADFS` (or `--partition AFS`) to extract a single partition
directly into the output directory (the file layout then matches the
single-volume case).

FileCore and AFS are both signature/structurally-detected and always
attempted; DFS - which has no magic number, only structural plausibility -
is only tried as a final fallback.

Note that the `--format json` output for `info`/`verify`/`extract` is always
a **list** (one element per partition), even for a single-volume image, so
tooling should not expect a bare object.



## Tests

```sh
cargo test
```

The suite includes always-on regression tests against real disc images
committed to `data/` - the ADFS S/M/L, D, E and F format fixtures, plus two
genuine Acorn DFS discs (`apd01_ssd` and `8bs0_dsd`). The ADFS fixtures all
contain the same files, which lets the tests assert byte-identical extraction
across formats and thereby validate the S/M/L sector-interleave translation.
The DFS fixtures are the only non-synthetic ground truth for the DFS backend
and cover single- and double-sided media plus a real `G`/`U`
directory-character mix. See `data/README.md` for provenance.

A **format-coverage matrix** (report-only) is printed by
`src/format_coverage_tests.rs`; it documents which on-disk format cells are
covered by real media versus synthetic-only versus uncovered, and is the
authoritative statement of format breadth (see `docs/FORMAT_COVERAGE.md`).

An optional stress harness pulls copyrighted corpus images at runtime only
when an Arcology API key is set (it self-skips otherwise, so CI never needs
network or a key):

```sh
ACORNFS_ARCOLOGY_KEY=<key> cargo test stress_against_arcology_corpus
# Real new-map fragmentation coverage (many sampled .dd hard discs contain
# fragmented files; nothing is pinned or committed):
ACORNFS_ARCOLOGY_KEY=<key> cargo test hard_disc_fragmentation_against_arcology_corpus
```

Configurable via `ACORNFS_ARCOLOGY_API` (base URL), `ACORNFS_ARCOLOGY_MAX`
(sample size), `ACORNFS_ARCOLOGY_EXT` (extensions to sample) and
`ACORNFS_ARCOLOGY_MAXSIZE` (skip larger artefacts).

Separately, an opt-in target validates additional pinned real discs fetched
from 8bs.com and mdfs.net (URL + SHA-256, self-skips unless a cache dir is
given):

```sh
ACORNFS_EXTERNAL_SOURCE=/tmp/cache cargo test --lib validates_additional_real_discs
```

A broader opt-in sweep against any local corpus of real disc images (e.g. an
archive under `/mnt/nfs`) opens and verifies every recognisable Acorn disc and
reports which format cells are actually exercised - no files are committed and
no hash is pinned, so it is purely a real-media coverage/health check:

```sh
ACORNFS_CORPUS_DIR=/mnt/nfs cargo test --lib scan_corpus_for_format_coverage
```

An opt-in **differential harness** (report-only, never a CI gate) runs this
reader and an independent second reader, [ejmount/acorn-dfs](https://github.com/ejmount/acorn-dfs),
over the same committed corpus and synthetic matrix and reports where their
recovered file inventories diverge. Because the other reader is *independent*
(a different author and parse path), a divergence is genuine cross-validation
rather than the self-confirming builder round-trip; on a divergence it dumps
both readers' raw readings so a cell can be adjudicated rather than assumed.
It requires fetching the git dependency, so it is feature-gated:

```sh
cargo test --features differential --test differential -- --nocapture
```

An optional coverage tool (report-only, not a CI gate) is available:

```sh
cargo llvm-cov --summary-only --lib   # requires cargo-llvm-cov + llvm-tools-preview
```

Golden images (real media created with official Acorn tools) can be produced
and validated by following `tools/GOLDEN_AUTHORING.md`; the donor images those
steps need are emitted by `ACORNFS_DONOR_OUT=/path cargo test --lib emit_golden_donors_if_requested`.
