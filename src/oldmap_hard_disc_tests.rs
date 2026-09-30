//! Always-on regression tests against a committed **real old-map ADFS hard
//! disc** (an Acorn Winchester File Server drive from 1984 - a Rodime drive,
//! 256-byte sectors, old map, old directories).
//!
//! The extension guide (§1.1) explicitly notes that no old-map hard disc was
//! behind it ("none of the sample images behind this guide is an old-map hard
//! disc, so the conclusion has not been checked against real media"). This
//! fixture is that real media and corrects the assumptions:
//!
//! - Old-map hard discs use **old/small (0x500) directories** here (attrs
//!   still carried in the name high bits), not the `0x800` new directories the
//!   guide implied, and the root is at `0x200` (`L_Root`), not `0x400`.
//! - They are addressed **linearly**, not through the S/M/L sequential->
//!   interleaved translation (a floppy geometry). The reader must not apply
//!   SML translation to a disc whose recorded total isn't a floppy size
//!   (640/1280/2560 sectors).
//! - The free-space map's `total_sectors` here records a chunk/cylinder count
//!   (594), not the image size, so `disc_size` must come the image length.
//! - 8-bit ADFS old directories legitimately have an **uncomputed (zero)**
//!   directory check byte; they must not be reported as broken.
//!
//! The disc is a genuine Acorn Winchester File Server (OLDFS 34560 bytes
//! contains "(C) 1984 Acorn"; `UTILS/Verify` is a valid BASIC program), so the
//! extracted bytes are a real correctness check, not a self-consistent one.

use std::io::{Cursor, Read};
use std::path::Path;

use tempfile::tempdir;

use crate::extract::log::ExtractionLog;
use crate::extract::report::build_report;
use crate::extract::walker::{BrokenDirPolicy, ExtractOptions, walk_and_extract};
use crate::format::filecore::FileCoreFs;
use crate::io::rescue::BadSectorPolicy;

const DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/");
const DISC_BYTES: u64 = 13_567_488;

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
fn oldmap_hard_disc_extracts_the_winchester_fileserver() {
    let bytes = load_fixture("winchester_adfs_rodime.gz");
    assert_eq!(bytes.len() as u64, DISC_BYTES);

    let mut fs = FileCoreFs::open(Cursor::new(bytes)).expect("recognised as FileCore");
    assert_eq!(fs.map_type, crate::format::filecore::MapType::Old);
    assert_eq!(fs.dir_type, crate::format::filecore::DirType::Old);

    let report = build_report(&mut fs).unwrap();
    assert_eq!(report.filesystem, "FileCore");
    assert_eq!(report.map_type, "old");
    assert_eq!(report.dir_type, "old");
    // disc_size must be the image length, not the map's chunk count (594).
    assert_eq!(report.disc_size, Some(DISC_BYTES));
    assert!(
        report.root_check_byte_ok,
        "8-bit old directories have an uncomputed (zero) check byte; must not be flagged broken"
    );
    assert!(
        report.broken_directories.is_empty(),
        "broken: {:?}",
        report.broken_directories
    );

    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &opts(dir.path()), &mut log).unwrap();
    assert_eq!(summary.files_extracted, 20, "log: {:?}", log.entries);
    assert_eq!(summary.dirs_created, 4);

    for name in [
        "!BOOT", "BOOT", "FileServ", "OLDFS", "Format", "LIBRARY", "UTILS",
    ] {
        assert!(dir.path().join(name).exists(), "missing {name}");
    }
    for ut in [
        "Weditor",
        "Verify",
        "SuperForm",
        "Rtrve.1",
        "HardError,f1b",
        "GetLost",
        "Exall",
        "CopyFiles",
        "Catall",
        "Bakup.1",
    ] {
        assert!(
            dir.path().join("UTILS").join(ut).exists(),
            "missing UTILS/{ut}"
        );
    }
    // Real content checks (an actual 1984 Acorn Winchester File Server).
    assert_content(dir.path(), "!BOOT", 56, 0xa97295f6);
    assert_content(dir.path(), "FileServ", 34512, 0x3bc14f78);
    assert_content(dir.path(), "OLDFS", 34560, 0x4f7e3e18);
    assert_content(dir.path(), "UTILS/Verify", 730, 0x5ef515c3);
    assert_content(dir.path(), "UTILS/Weditor", 6406, 0x2af51285);
    assert_content(dir.path(), "Format/SuperForm", 9950, 0x9a1e476a);

    // And it is genuinely an OLDFS Winchester fileserver, not junk.
    let ol = std::fs::read(dir.path().join("OLDFS")).unwrap();
    let has_banner = ol.windows(b"Winchester".len()).any(|w| w == b"Winchester");
    let has_copyright = ol
        .windows(b"(C) 1984 Acorn".len())
        .any(|w| w == b"(C) 1984 Acorn");
    assert!(
        has_banner && has_copyright,
        "OLDFS should contain the Acorn Winchester File Server banner"
    );
}

#[test]
fn oldmap_hard_disc_fixture_is_gzip_deterministic() {
    assert_eq!(
        load_fixture("winchester_adfs_rodime.gz").len() as u64,
        DISC_BYTES
    );
}
