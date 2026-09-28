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
