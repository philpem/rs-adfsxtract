//! Always-on regression tests against committed **real Solidisk DDFS media**.
//!
//! `stl9a/stl9b` are genuine Solidisk DDFS utility/system discs (from sweh's
//! archive, /tmp/stl9.zip provenance): side A carries the Solidisk DDFS utility
//! set (`FORMAT`, `DISCOPY`, `ARCHIVE`, `CATALL`, `PASSWD`, `PROTECT`,
//! `RECOVER`, `RESTORE`, `SPECIFY`, `SDRVBAK`, `PARK`, ...) and side B carries
//! ADFS/DFS 2.1 system files. They are standard single-density single-sided
//! Acorn-DFS-layout discs - Solidisk's DDFS is a density enhancement, not a
//! layout change - so they validate our DFS backend against real third-party
//! media, closing the `dfs/solidisk` format cell.
//!
//! Crucially, side A also guards a detector regression: its single-sided file
//! data happens to land at the interleaved side-1 catalogue offset, which
//! previously caused the doublesided probe to *falsely* report the disc as
//! double-sided (inflating disc_size to 438272 and inventing a garbage side 1
//! of `e.ååååååå` entries). The detector must now require evidence of a real
//! second side (sector count within the image + majority in-bounds entries)
//! before trusting it.

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
fn solidisk_ddfs_utilities_disc_is_single_sided() {
    // Real Solidisk DDFS utilities disc. Regression guard for the double-sided
    // false-positive: this single-sided disc's data at the side-1 offset must
    // not be misread as a second side.
    let mut fs = DfsFs::open(Cursor::new(load_fixture("solidisk_utils_side_a.ssd.gz")))
        .expect("recognised as DFS");
    assert!(!fs.double_sided, "stl9a is single-sided, not double-sided");

    let report = build_dfs_report(&mut fs).unwrap();
    assert_eq!(report.disc_name.as_deref(), Some("stl9a"));
    assert_eq!(
        report.disc_size,
        Some(204_800),
        "must not be inflated by a phantom side 1"
    );
    assert_eq!(report.boot_option, Some(3));
    assert!(report.directory_warnings.is_empty());

    // Exactly the Solidisk DDFS utility set, with no garbage phantom entries.
    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &opts(dir.path()), &mut log).unwrap();
    assert_eq!(summary.files_extracted, 19, "log: {:?}", log.entries);
    assert_eq!(summary.dirs_created, 1);
    for name in [
        "FORMAT", "DISCOPY", "ARCHIVE", "Catall", "PASSWD", "PROTECT", "RECOVER", "RESTORE",
        "SPECIFY", "SDRVBAK", "PARK", "PARK10", "Dirall", "Exall", "Destall", "FOR_ALT", "Movedfs",
        "RITEFIL", "SAFE",
    ] {
        assert!(
            dir.path().join(name).exists(),
            "missing Solidisk utility {name}"
        );
    }
    assert_content(dir.path(), "FORMAT", 3906, 0xf4d45fe1);
    assert_content(dir.path(), "DISCOPY", 1063, 0x51357f58);
    assert_content(dir.path(), "SDRVBAK", 1172, 0x6f0d51e3);
}

#[test]
fn solidisk_system_disc_carries_adfs_dfs21() {
    // The companion disc carries ADFS/DFS 2.1 system files.
    let mut fs = DfsFs::open(Cursor::new(load_fixture("solidisk_utils_side_b.ssd.gz")))
        .expect("recognised as DFS");
    assert!(!fs.double_sided);
    let report = build_dfs_report(&mut fs).unwrap();
    assert_eq!(report.disc_name.as_deref(), Some("stl9b"));
    assert_eq!(report.disc_size, Some(204_800));

    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &opts(dir.path()), &mut log).unwrap();
    assert_eq!(summary.files_extracted, 5, "log: {:?}", log.entries);
    assert_content(dir.path(), "DFS21_1", 16384, 0x2a3af556);
    assert_content(dir.path(), "ADF21_1", 16384, 0xa4daa200);
}

#[test]
fn solidisk_fixtures_are_gzip_deterministic() {
    for (fixture, sz) in [
        ("solidisk_utils_side_a.ssd.gz", 204800u64),
        ("solidisk_utils_side_b.ssd.gz", 204800u64),
    ] {
        assert_eq!(load_fixture(fixture).len() as u64, sz, "{fixture}");
    }
}
