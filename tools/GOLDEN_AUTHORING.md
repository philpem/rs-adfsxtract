# Golden-image authoring recipe

Ground truth for format support is **real media written by the official Acorn
tools**, not this project's builder and not a second reader. This document is
the step-by-step recipe for producing those goldens. It is deliberately split
by who does what:

- **You (human)** operate the emulators (formatting, copying, capturing).
- **The repo / me** generate the *donor* images, decode the captured golden,
  derive the expectation hashes, and bake them into tests.

The division arises because the emulator interactions (HForm clicks, DFS
`*FORM`/`*COPY`) need a GUI; the hashing/assertion side is entirely scriptable.

## What a golden is

A *golden* is a disc image created by real Acorn software whose contents are
entirely **freely-distributable** (public-domain prose, e.g. the text in
`testutil::PUBDOM_PROSE`). Once captured and decoded, only two things are
committed:

1. the **hashes** (disc metadata + per-file content CRC/length) baked into a
   reference test, and
2. the **reproducing builder** (Rust `testutil` functions) that regenerates an
   equivalent logical image, so CI never needs the original binary.

The golden image itself is normally **not** committed (only used transiently to
derive the hashes), although we keep one per structural class if a permanent
cross-check is desired.

## Generating donor images

You copy files FROM a donor, so first produce one. Run:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
ACORNFS_DONOR_OUT=/tmp/donors cargo test --lib emit_golden_donors_if_requested
```

This writes `donor_s.adl` (ADFS S), `donor_m.adl` (ADFS M), `donor_e.adf`
(ADFS E new-map), and `donor_dfs.ssd` (Acorn DFS SSD), each containing
`RomJul` (the public-domain prose) and `Readme`. These are what you mount as
the *source* drive.

> Note: the donor images are minimal (sized to their content). Most BBC and
> RISC OS emulators mount them fine; if yours rejects a non-standard size,
> pad/re-size to a standard geometry before mounting (e.g. a 200 K `.ssd`).

## ADFS S / M (8-bit, BBC Micro emulator)

Target: an S (160 KB) or M (320 KB) **old-map, old-directory** disc. Official
tool: the 8-bit **ADFS ROM** on a BBC Master / BBC Micro with ADFS.

1. Boot the BBC emulator with ADFS.
2. Mount `donor_s.adl` (or `donor_m.adl`) as drive 4.
3. `*FORM <drive> 1` (or `2` for M) on a freshly-formatted target drive to
   create the S/M image. *Optionally*, if you want a disc that already
   exercises the track-boundary interleave, format larger than 1 side is the
   L case - for S/M single-sided, content anywhere still only exercises the
   (identity) single-sided geometry.
4. `*COPY <donor>.RomJul` and `*COPY <donor>.Readme` to the target directory.
5. Capture the target drive's image file and save it (e.g. as `adfs320M.adl`).

Providing the image back lets me decode it, confirm the catalogue facts and
per-file content, and bake the hashes into an S/M reference test.

## Old-map hard disc (RISC OS 2 `!HForm`) - TBD

This cell needs **RISC OS 2** on an emulator (Arculator) and the RISC OS 2
`!HForm` application (from a RISC OS 2 applications disc, which still needs to
be located). It produces an **old-map, new-directory** hard disc (256-byte
sectors, `0x800`-byte / 77-entry directories, boot block at `0xC00`). The
reader side is already implemented; this golden is the missing independent
confirmation. Until found, the matrix marks `adfs/oldmap-hd` as uncovered.

## Big-directory disc (E+/F+/G, RISC OS 4 HForm) - easier path

Target: an **E+/F+/G big-directory** disc. Official tool: **RISC OS 4 `HForm`**
(accessible in RPCEmu).

1. Boot RPCEmu with RISC OS 4.
2. Run `HForm` to create a hard disc with big directories (E+/F+/G).
3. Mount/copy the public-domain files onto it.
4. Capture the resulting disc image and hand it back.

Big directories are currently covered only synthetically; this golden closes
the `adfs/eplus`/`fplus`/`g-hd` cells with real media.

## Fragmented disc (RISC OS file churn)

Fragmentation on a new-map disc arises from file churn, not formatting. To get
a genuine fragmented file on real media:

1. Create a disc, copy several large files onto it,
2. delete enough to fragment the free space,
3. copy a single file that must be split across the resulting non-contiguous
   free extents,
4. capture the image.

The hard part is knowing a file actually *is* fragmented. I can inspect the
captured image and confirm the target file resolves to >1 extent; if it isn't
fragmented, repeat with different copy/delete sizes. This closes `frag/real`.

## DFS vendor discs (Acorn / Watford / Solidisk)

Standard Acorn DFS: the BBC emulator `*FORM 40`/`*FORM 80` is the official
tool - you can author the disc and copy the public-domain files on. Vendor
variants (Watford 62-file extension, Solidisk) need the vendor's DFS ROM, which
is freely available from 8bs.com and stardot.org.uk; install it as the active
DFS and format/copy as usual. mdfs.net ALSO hosts HADFS (a non-Acorn variant).

Already committed as real Acorn media: `data/apd01_ssd.ssd.gz` and
`data/8bs0_dsd.dsd.gz` (see `data/README.md`).

## After you provide a golden

For each golden, I will:

1. `acornfsextract info`/`verify` and `extract --output` to obtain catalogue
   facts and per-file content.
2. Derive SHA-256 of the raw image + CRC-32/length of each file.
3. Bake them into a reference test (mirroring `dfs_reference_media_tests.rs`)
   and, if a reproducing builder is missing, add the corresponding
   `build_*_disc` function.
4. Update `docs/FORMAT_COVERAGE.md` and `data/README.md`.
