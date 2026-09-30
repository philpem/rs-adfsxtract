//! Always-on regression tests against committed *real* Acorn DFS disc images.
//!
//! These are the only non-synthetic ground truth for the DFS backend: the
//! synthetic builder in `testutil` is written by the same author as the
//! reader, so a shared comprehension gap (the same class of hazard the ADFS
//! S/M/L and DSD interleave caused) would silently pass a builder+reader
//! round-trip. These images are genuine Acorn DFS media - the single-sided
//! disc exposes the standard catalogue and a `G`/`U` directory-character
//! mix, and the double-sided disc exercises real per-track interleave with a
//! populated side 1 - so the reader cannot pass by mirroring its own builder.
//!
//! Provenance and licence: re-downloaded from 8bs.com ("The BBC and Master
//! Computer Public Domain Library"), which distributes freely-licensed
//! software. See `data/README.md` for source, SHA-256, and how the hashes
//! below were derived.

use std::io::{Cursor, Read};
use std::path::Path;

use tempfile::tempdir;

use crate::extract::log::ExtractionLog;
use crate::extract::report::build_dfs_report;
use crate::extract::walker::{BrokenDirPolicy, ExtractOptions, walk_and_extract};
use crate::format::dfs::DfsFs;
use crate::io::rescue::BadSectorPolicy;

const DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/");

fn load_fixture(name: &str) -> Vec<u8> {
    let path = Path::new(DATA_DIR).join(name);
    let file = std::fs::File::open(&path)
        .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()));
    let mut gz = flate2::read::GzDecoder::new(file);
    let mut bytes = Vec::new();
    gz.read_to_end(&mut bytes).unwrap();
    bytes
}

fn opts(dir: &Path) -> ExtractOptions {
    ExtractOptions {
        output_dir: dir.to_path_buf(),
        write_inf: false,
        dry_run: false,
        broken_dir_policy: BrokenDirPolicy::Fail,
        bad_sector_policy: BadSectorPolicy::NullFill,
        rescue_map: None,
    }
}

fn assert_content(dir: &Path, name: &str, expected_len: usize, expected_crc: u32) {
    let bytes = std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("read {name}: {e}"));
    assert_eq!(bytes.len(), expected_len, "unexpected length for {name}");
    assert_eq!(
        crc32fast::hash(&bytes),
        expected_crc,
        "content mismatch for {name}"
    );
}

#[test]
fn acorn_dfs_ssd_real_media() {
    // Standard single-sided Acorn DFS disc ("A_Programs 1", Acorn DFS from
    // the 8bs public-domain library).
    let mut fs =
        DfsFs::open(Cursor::new(load_fixture("apd01_ssd.ssd.gz"))).expect("recognised as DFS");
    assert!(!fs.double_sided, "this is a single-sided image");

    let report = build_dfs_report(&mut fs).unwrap();
    assert_eq!(report.disc_name.as_deref(), Some("A_Programs 1"));
    assert_eq!(report.disc_size, Some(204_800));
    assert_eq!(report.boot_option, Some(3));

    // The catalogue is a flat namespace here; entries carry a directory
    // character (G/U) that the extractor exposes as a `dirchar.name` prefix.
    assert_eq!(fs.catalogues[0].entries.len(), 30);

    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &opts(dir.path()), &mut log).unwrap();
    assert_eq!(summary.files_extracted, 30, "log: {:?}", log.entries);
    assert_eq!(summary.dirs_created, 1);
    assert_content(dir.path(), "!BOOT", 20, 0xb2d2810e);
    assert_content(dir.path(), "G.JUNGLE", 3730, 0xdcbcf829);
    assert_content(dir.path(), "U.WORDPRO", 4647, 0x71ea4510);
}

#[test]
fn acorn_dfs_dsd_real_media_interleave() {
    // A real double-sided DFS disc ("8BS-00", 8bs public-domain library).
    // Side 1 is populated independently and the reader must interleave the
    // two sides per-track to read catalogue and file data - the precise
    // hazard that synthetic DSD tests can no longer vouch for on their own.
    let mut fs =
        DfsFs::open(Cursor::new(load_fixture("8bs0_dsd.dsd.gz"))).expect("recognised as DFS");
    assert!(fs.double_sided, "this is a double-sided image");

    let report = build_dfs_report(&mut fs).unwrap();
    assert_eq!(report.disc_name.as_deref(), Some("8BS-00"));
    assert_eq!(report.disc_size, Some(409_600));
    assert_eq!(report.boot_option, Some(3));

    assert_eq!(fs.catalogues[0].entries.len(), 8, "side 0");
    assert_eq!(fs.catalogues[1].entries.len(), 17, "side 1");

    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &opts(dir.path()), &mut log).unwrap();
    assert_eq!(summary.files_extracted, 25, "log: {:?}", log.entries);
    assert_eq!(summary.dirs_created, 3);

    // Side 0 content (including a !BOOT that chains into Menu).
    assert_content(dir.path(), "Side0/!BOOT", 21, 0x40a2f7d6);
    assert_content(dir.path(), "Side0/Menu", 5559, 0xcac1c0a6);
    // Side 1 content, reached only via the per-track interleave translation.
    assert_content(dir.path(), "Side1/ISSUES1", 16812, 0x9555d1c9);
    assert_content(dir.path(), "Side1/Join", 697, 0x2ca766e0);
}

#[test]
fn dfs_fixtures_are_gzip_deterministic() {
    // Guard against a fixture being silently regenerated with different
    // bytes: verify against the known raw image sizes.
    for (fixture, sz) in [
        ("apd01_ssd.ssd.gz", 71424u64),
        ("8bs0_dsd.dsd.gz", 243200u64),
    ] {
        assert_eq!(load_fixture(fixture).len() as u64, sz, "{fixture}");
    }
}
