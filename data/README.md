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

## Solidisk fixtures

Two genuine **Solidisk DDFS** discs, posted by Stephen Harris (**sweh**) to the
Stardot archive-submissions thread
(https://stardot.org.uk/forums/viewtopic.php?t=5011): side A is the Solidisk
DDFS **utility set** (`FORMAT`, `DISCOPY`, `ARCHIVE`, `CATALL`, `PASSWD`,
`PROTECT`, `RECOVER`, `RESTORE`, `SPECIFY`, `SDRVBAK`, `PARK`, ...), side B
carries ADFS/DFS 2.1 system files. Solidisk DDFS is a density enhancement, not
a layout change, so the catalogue layout is standard Acorn-DFS-compatible; the
discs are single-density single-sided and provide real third-party media for
the `dfs/solidisk` format cell.

These are effectively the **driver/utilities disc that shipped with the action
Solidisk hardware**, i.e. freely redistributable; the images are committed
in-repo compressed, so a Stardot login is *not* required to reproduce the
tests (the original `/tmp/stl9.zip` attachment is citable for provenance).

Side A doubles as a detector regression: its single-sided file data lands at
the interleaved side-1 catalogue offset, which previously caused the
double-sided probe to report it as double-sided (inflating disc_size to 438272
and inventing a phantom side 1). The detector now requires real evidence of a
second side before trusting it. See `src/solidisk_reference_media_tests.rs`.

> Known Solidisk-specific variants not yet covered by a fixture (noted from
> sweh's MMB_Utils): 320 KB double-density discs record an 11-bit start sector
> (bit stolen from the load high bits), and more-than-31-file Solidisk discs
> use a *chained* catalogue rather than a fixed second block. Neither is
> exercised by the committed 200 KB fixtures; the 11-bit disk-size field *is*
> now decoded (see the three-high-bit fix and its regression test).

| Fixture | Format | Raw image SHA-256 | Content |
|---------|--------|-------------------|---------|
| `solidisk_utils_side_a.ssd.gz` | Acorn-DFS-layout single-sided, "stl9a" | `7a6d3d0a6b7c957407a84b4e567bceaa91fd6de0c8ded1b4055d1bec02428189` | Solidisk DDFS utilities (19 files) |
| `solidisk_utils_side_b.ssd.gz` | Acorn-DFS-layout single-sided, "stl9b" | `ef53506676a1077eeae50e33c9249b0fbfee6a2acf6b6b92514fe93f5793c8e3` | ADFS/DFS 2.1 system files |

## Old-map hard disc fixture

`winchester_adfs_rodime.gz` is a genuine **old-map ADFS hard disc**: an Acorn
**Winchester File Server** drive from 1984 (a Rodime drive, 256-byte sectors,
old map, old directories, `OLDFS` containing "(C) 1984 Acorn"). Supplied by
Phil Pemberton from archived ST506/MFM hard-drive dumps
(`rodime_datafile2`, at `mdfs.net`-style archive /temp drive images).

This is the real media the extension guide (§1.1) said was missing ("none of
the sample images behind this guide is an old-map hard disc"). It corrects the
guide's assumptions: old-map hard discs here use old/small (`0x500`)
directories at `0x200` (not the `0x800` new dirs at `0x400` the guide implied),
are addressed **linearly** (not via the S/M/L floppy interleave), and their
free-space map's `total_sectors` records a chunk/cylinder count (594) rather
than the image size - so `disc_size` is taken from the image length, and their
8-bit directories legitimately carry an uncomputed (zero) check byte. See
`src/oldmap_hard_disc_tests.rs`.

| Fixture | Format | Raw image SHA-256 | Content |
|---------|--------|-------------------|---------|
| `winchester_adfs_rodime.gz` | Old-map ADFS hard disc, 256 B sectors, old dirs, 13,567,488 bytes | `04034978cbd26848bcb82f035b213ff14efdaf64178f8422a6beaed774c5ce63` | Acorn Winchester File Server (20 files) |

