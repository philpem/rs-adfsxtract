# Disk-format coverage matrix

This is the authoritative statement of *which on-disk formats and format
features* `acornfsextract` is known to handle and how each is tested. It is
generated/reported from `src/format_coverage_tests.rs` (report-only: the test
always passes and just prints the matrix, so CI surfaces regressions in format
breadth without failing the build on a coverage shortfall).

Ground truth is **real media authored by the official Acorn tools**, never
this project's own builder and not a second reader (the `filecore-extract`
reference tool is retired). A synthetic/self-generated image is only ever
trusted as a *carrier* or an *edge-case*; its expected values are derived from
an official-tool golden before they are baked into a test.

## Legend

| Status | Meaning |
|--------|---------|
| `real` | Exercised against a real (official-tool) disc image. |
| `synthetic` | Exercised against an in-repo generated image, but cross-checked against an official golden. |
| `synthetic-only` | Exercised only against an in-repo generated image; no golden anchor yet. |
| `uncovered` | No test; risk of silent mis-parsing. |
| `refused` | Recognised but deliberately rejected (a supported-limitation), with an explicit test. |

## FileCore (ADFS) / FileCore structures

Columns: **map** (old/new) × **dir** (old/new/big) × **geometry** ×
**interleave** × **zones** × **fragmentation**.

| Cell | Map | Dir | Geometry | Interleave | Zones | Status | Fixture / test |
|------|-----|-----|----------|-----------|-------|--------|----------------|
| S | old | old | 160 KB, 40 trk × 1 side | sequential→interleaved | 1 | `synthetic-only` (golden pending) | `sml_geometry` unit 640; S/M reference (pending) |
| M | old | old | 320 KB, 80 trk × 1 side | sequential→interleaved | 1 | `synthetic-only` (golden pending) | `sml_geometry` unit 1280; S/M reference (pending) |
| L | old | old | 640 KB, 80 trk × 2 sides | sequential→interleaved | 1 | `real` | `data/adfs640L.adl.gz` |
| D | old | new | 800 KB, 80 trk × 2 sides, 1024 B sect | interleaved | 1 | `real` | `data/adfs800D.adf.gz` |
| E | new | new | 800 KB, 5 sect/trk | interleaved | 1 | `real` | `data/adfs800E.adf.gz` |
| F | new | new | 1.6 MB, 10 sect/trk | interleaved | 4 | `real` | `data/adfs1600F.adf.gz` |
| E+ | new | big | 800 KB | interleaved | 1 | `synthetic-only` | `testutil` `big_dirs` |
| F+ | new | big | 1.6 MB | interleaved | 4 | `synthetic-only` | `testutil` `big_dirs` |
| G | new | big | hard disc | interleaved | n | `synthetic-only` | `testutil` `big_dirs` |
| Old-map hard disc | old | new | 256 B sect, drive-geometry | interleaved | 1 | `uncovered` (golden TBD, RISC OS 2 `!HForm`) | reader branch in `detect`/`mod` |
| New-map sequential track order | new | new | any | sequential flag set | n | `refused` | synthetic refusal test |

### Fragmentation (new map)

| Feature | Status | Test |
|---------|--------|------|
| Single zone, file in 2 same-id fragments | `synthetic` | `testutil::fragmented_file_reassembles_in_order` |
| File in >2 fragments | `uncovered` | seeded random generator (pending) |
| Cross-zone fragment span (`pending_span`) | `uncovered` | seeded random generator (pending) |
| Sharing offset on a non-root object | `synthetic-only` | `build_new_map_disc`/big-dir paths |
| Fragment in a big directory | `synthetic-only` | `testutil` `big_dirs` |
| Free-chain exclusion interacting with real fragments | `synthetic` | `map_new` unit tests |
| Real-media fragmentation | `uncovered` (golden via RISC OS churn, TBD) | authoring recipe |

### Structural / metadata features

| Feature | Status |
|---------|--------|
| Boot block present (F format / hard disc) | `real` (`adfs1600F`) |
| Boot block absent (S/M/L/D, E) | `real` (L/D/E) |
| Disc-record zone-0 vs boot-block merge | `real` |
| ZoneCheck / cross-check verification | `real` |
| Directory check byte (A.2) | `real` |
| Old-map free-space map checksum | `real` |
| S/M/L attribute bits in name bytes | `synthetic` |
| DOSFS / RISC OS charset name translation | `synthetic` |
| `.inf` sidecar creation | `synthetic` |
| Bad-sector policies (null/skip/marker) + rescue map | `synthetic` |
| Broken-directory policies (fail/skip/recover) | `synthetic` |

## DFS (Disc Filing System)

Columns: **vendor** × **geometry** (SSD/DSD, 40/80 trk, sect/trk) × **catalogue features**.

| Cell | Vendor | Geometry | Status | Fixture / test |
|------|--------|----------|--------|----------------|
| Acorn DFS SSD | Acorn | single-sided | `real` | `data/apd01_ssd.ssd.gz` (`dfs_reference_media_tests`) |
| Acorn DFS DSD | Acorn | double-sided interleaved | `real` | `data/8bs0_dsd.dsd.gz` (`dfs_reference_media_tests`) |
| Watford 62-file extension | Watford | SSD or DSD | `synthetic` | `testutil` `dfs_watford_extension_round_trip` |
| Solidisk / Opus catalogue | Solidisk/Opus | — | `uncovered` | external/8bs golden (pending) |
| HADFS (Harston) | HADFS | — | `uncovered` | mdfs.net external target (pending) |
| dir-char / locked attributes on real media | any | — | `real` | `data/apd01_ssd.ssd.gz` (G/U dir chars) |
| Catalogue bounds / broken entry | any | — | `synthetic` | `scenario_tests` |

## External real-disc validation (opt-in)

`external_real_media_tests` pulls additional public-domain discs from 8bs.com
at runtime (URL + SHA-256 pinned, self-skips unless `ACORNFS_EXTERNAL_SOURCE`
is set to a cache directory). It validates the tool against more real media and
feeds real-media facts back into the matrix, but is **never** a committed-test
dependency.

## Authoring / independent generator

- `tools/GOLDEN_AUTHORING.md` - the recipe for creating real media with the
  official Acorn tools and handing goldens back for hashing.
- `tools/filecore_build.py` - an independent single-zone E-format builder used
  as a cross-check oracle and hard-disc/big-dir generator (see
  `tools/README.md`).

## Coverage tooling

`cargo llvm-cov` (with `llvm-tools-preview`) reports line/branch coverage as a
secondary, report-only metric. This matrix — not a coverage threshold — is the
authoritative statement of *format* breadth.
