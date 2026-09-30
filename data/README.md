# Reference disc images

These four gzip-compressed FileCore disc images are test fixtures for the
regression suite in `src/reference_media_tests.rs`. They are the canonical
media the FileCore guide and this extractor were developed against, so they
exercise the format matrix rather than one format: S/M/L (`.adl`), D, E and
F. They were created with ADFS itself, not with a synthetic builder.

| Fixture | Format | Image |
|---------|--------|-------|
| `adfs640L.adl.gz` | old map, old (small) directory | 640 KB double-sided |
| `adfs800D.adf.gz` | old map, new (large) directory | 800 KB |
| `adfs800E.adf.gz` | new map, new directory, single zone | 800 KB |
| `adfs1600F.adf.gz` | new map, new directory, multi-zone | 1.6 MB |

Every image contains the same two files, which is deliberate: it lets the
tests assert byte-identical extraction across all four formats, which also
validates the S/M/L sequential-to-interleaved sector translation - a file
that spans track 0 would read incorrectly without it.

## Contents and provenance

- `qtm149.txt` - QTheMusic (QTM) v1.49 release notes, (c) Steve Harrison
  (Quantum), 1993-2023. Text file, so reproduced here purely as test data.
- `hostfs.txt` - the Arculator HostFS placeholder file.

The disc images themselves are created with ADFS (RISC OS), not by any of
these tools.

## DFS fixtures

Two genuine Acorn DFS discs, re-downloaded from **8bs.com** (The BBC and
Master Computer Public Domain Library - freely-licensed / public-domain
software; https://8bs.com/catalogue.htm and `/pool/...`). They are committed
as the *only* non-synthetic ground truth for the DFS backend: the in-repo DFS
builder shares its author with the reader, so a shared comprehension gap (the
SSD/DSD interleave would be the obvious one) must not be able to pass a
builder+reader round-trip. These provide real media for the SSD and DSD paths
and a genuine `G`/`U` directory-character mix.

| Fixture | Format | Raw image SHA-256 | Content |
|---------|--------|-------------------|---------|
| `apd01_ssd.ssd.gz` | Acorn DFS, single-sided, 800 sectors, "A_Programs 1" | `9cc46c78dbf71850afce98475352d86b5f41c7933a0b62d43a4a839d979981e2` | 30 files, incl. `!BOOT`, `G.JUNGLE`, `U.WORDPRO` |
| `8bs0_dsd.dsd.gz` | Acorn DFS, double-sided, "8BS-00" | `bcc73394ad6ce071448af94d7b40e4b8cd319f0a284fd7b4f4cdafdcd6e361ce` | 8 files (side 0) + 17 files (side 1), incl. `Side0/!BOOT`, `Side1/ISSUES1` |

The raw disc bytes are decompressed from `.gz` for the tests; the per-file
CRC-32 and byte-length assertions in `src/dfs_reference_media_tests.rs` were
derived from these exact images.

