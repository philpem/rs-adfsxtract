//! Always-on tests for the **old-map ADFS hard disc** handling, using synthetic
//! images built from public-domain content - so nothing copyrighted is
//! committed and the fixtures stay tiny (generated in-memory).
//!
//! An old-map hard disc (e.g. an Acorn Winchester File Server drive) has
//! 256-byte sectors, an old map, old/small directories at `0x200`, and - unlike
//! an S/M/L floppy - is **not** an ADFS floppy geometry:
//!
//! - Its recorded total sector count is not 640/1280/2560 (it is a
//!   chunk/cylinder count), so the S/M/L sequential->interleaved translation
//!   must not be applied; the disc is addressed linearly.
//! - The free-space map's `total_sectors` is not the image size, so
//!   `disc_size` must be reported from the image length, not `total_sectors*256`.
//! - The directories are 8-bit style with an **uncomputed (zero)** check byte -
//!   reader must not flag them as broken.
//!
//! These were confirmed against real media (see the opt-in corpus scan), but
//! the synthetic image below locks in the same behaviour offline and
//! unencumbered.

use std::io::Cursor;

use tempfile::tempdir;

use crate::extract::log::ExtractionLog;
use crate::extract::report::build_report;
use crate::extract::walker::{BrokenDirPolicy, ExtractOptions, walk_and_extract};
use crate::format::filecore::{DirType, FileCoreFs, MapType};
use crate::io::rescue::BadSectorPolicy;
use crate::testutil::{SynthFile, build_old_map_disc};

/// Public-domain prose used as file data (a Shakespeare passage, in the public
/// domain) - keeps the test unencumbered and the images compressible/random.
const PROSE_A: &[u8] = b"But soft, what light through yonder window breaks?\nIt is the east, and Juliet is the sun.\nArise, fair sun, and kill the envious moon,\nWho is already sick and pale with grief,\nThat thou her maid art far more fair than she:\n";
const PROSE_B: &[u8] = b"Her vestal livery is but sick and green, and none but fools do wear it; cast it off.\nIt is my lady, O, it is my love!\n";

/// Builds a small synthetic old-map disc whose recorded total sector count is
/// NOT an S/M/L floppy size, so it must be treated as a hard disc (linear
/// addressing) rather than through the floppy geometry.
fn build_synthetic_oldmap_hard_disc() -> Vec<u8> {
    let files = vec![
        SynthFile::plain("!BOOT", b"CLOSE#0\nCHAIN FileServ\n"),
        SynthFile::plain("FileServ", PROSE_A),
        SynthFile::plain("Verify", PROSE_B),
        SynthFile::plain("Format", b"*=FORMAT 40 or 80\n"),
    ];
    build_old_map_disc(files, true, "WinFileSrv").bytes
}

#[test]
fn oldmap_hard_disc_is_addressed_linearly_and_sized_from_image() {
    let bytes = build_synthetic_oldmap_hard_disc();
    // Sanity: this tiny synthetic disc must not accidentally report an S/M/L
    // floppy total (640/1280/2560) - that's what forces the hard-disc path.
    let total = bytes.len() as u64 / 256;
    assert!(
        !matches!(total, 640 | 1280 | 2560),
        "synthetic disc total {total} must not be an S/M/L floppy size"
    );

    let mut fs = FileCoreFs::open(Cursor::new(bytes.clone())).expect("recognised as FileCore");
    assert_eq!(fs.map_type, MapType::Old);
    assert_eq!(fs.dir_type, DirType::Old);

    let report = build_report(&mut fs).unwrap();
    assert_eq!(report.filesystem, "FileCore");
    assert_eq!(report.map_type, "old");
    assert_eq!(report.dir_type, "old");
    // disc_size must be the image length, NOT total_sectors*256 (total is a
    // chunk/cylinder count on an old-map hard disc, not the image size).
    assert_eq!(
        report.disc_size,
        Some(bytes.len() as u64),
        "old-map hard disc disc_size must be the image length"
    );
    assert!(
        report.root_check_byte_ok,
        "old-map hard disc dirs are 8-bit (uncomputed/zero check byte); must not be broken"
    );
    assert!(
        report.broken_directories.is_empty(),
        "broken: {:?}",
        report.broken_directories
    );

    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    let opts = ExtractOptions {
        output_dir: dir.path().to_path_buf(),
        write_inf: false,
        dry_run: false,
        broken_dir_policy: BrokenDirPolicy::Fail,
        bad_sector_policy: BadSectorPolicy::NullFill,
        rescue_map: None,
    };
    let summary = walk_and_extract(&mut fs, &opts, &mut log).unwrap();
    assert_eq!(summary.files_extracted, 4, "log: {:?}", log.entries);
    for name in ["!BOOT", "FileServ", "Verify", "Format"] {
        assert!(dir.path().join(name).exists(), "missing {name}");
    }
    assert_eq!(std::fs::read(dir.path().join("Verify")).unwrap(), PROSE_B);
}
