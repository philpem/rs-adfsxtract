//! Opt-in regression tests against real DFS disc images. Not required to
//! pass in CI or by default (each test skips itself if its env var isn't
//! set) - the point is regression protection on this machine specifically,
//! following the same pattern the implementation plan described for
//! FileCore's big-directory sample.
//!
//! Every assertion here was independently cross-checked: the catalogue
//! facts (title, sector count, entry count, per-entry start
//! sector/length) were read directly from the raw image bytes in Python,
//! and every extracted file's exact byte content was confirmed present
//! verbatim in the raw image (including, for the double-sided image, files
//! that reassemble from multiple physical extents split across a real
//! track boundary) - all done before trusting these numbers enough to
//! bake them in as a regression test.
//!
//! Set `DFS_SSD_SAMPLE` to a single-sided `.ssd` matching `BB1.ssd`'s
//! layout (title "BBC TAPE", 10 files, 800 sectors) and/or
//! `DFS_DSD_SAMPLE` to a double-sided `.dsd` matching the MikeJames book
//! disc (title "M/James-book", side 0 empty, side 1 with 27 files) to run
//! these.

use tempfile::tempdir;

use crate::extract::log::ExtractionLog;
use crate::extract::report::build_dfs_report;
use crate::extract::walker::{BrokenDirPolicy, ExtractOptions, walk_and_extract};
use crate::format::dfs::DfsFs;
use crate::format::fs::FileSystem;
use crate::io::rescue::BadSectorPolicy;

fn default_opts(output_dir: &std::path::Path) -> ExtractOptions {
    ExtractOptions {
        output_dir: output_dir.to_path_buf(),
        write_inf: false,
        dry_run: false,
        broken_dir_policy: BrokenDirPolicy::Fail,
        bad_sector_policy: BadSectorPolicy::NullFill,
        rescue_map: None,
    }
}

#[test]
fn bb1_ssd_regression() {
    let Ok(path) = std::env::var("DFS_SSD_SAMPLE") else {
        eprintln!("skipping: DFS_SSD_SAMPLE not set");
        return;
    };
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut fs = DfsFs::open(file).expect("should be recognised as DFS");
    assert!(!fs.double_sided, "BB1.ssd is single-sided");

    let report = build_dfs_report(&mut fs).unwrap();
    assert_eq!(report.disc_name.as_deref(), Some("BBC TAPE"));
    assert_eq!(report.disc_size, Some(204_800));
    assert_eq!(report.boot_option, Some(3));

    let cat = &fs.catalogues[0];
    assert_eq!(cat.entries.len(), 10);
    assert_eq!(cat.total_sectors, 800);
    let boot = cat.entries.iter().find(|e| e.name == "!BOOT").expect("!BOOT entry present");
    assert_eq!(boot.start_sector, 141);
    assert_eq!(boot.length, 11);

    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &default_opts(dir.path()), &mut log).unwrap();
    assert_eq!(summary.files_extracted, 10, "log: {:?}", log.entries);
    assert_eq!(summary.total_bytes, 34_118);
    assert_eq!(
        std::fs::read(dir.path().join("!BOOT")).unwrap(),
        b"CH.\"MAKER\"\r",
        "!BOOT's exact bytes (a BASIC chain command), confirmed present in the raw image at 0x8d00"
    );
}

#[test]
fn mikejames_dsd_regression() {
    let Ok(path) = std::env::var("DFS_DSD_SAMPLE") else {
        eprintln!("skipping: DFS_DSD_SAMPLE not set");
        return;
    };
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut fs = DfsFs::open(file).expect("should be recognised as DFS");
    assert!(fs.double_sided, "the MikeJames disc is double-sided");

    let report = build_dfs_report(&mut fs).unwrap();
    assert_eq!(report.disc_name.as_deref(), Some("M/James-book"));
    assert_eq!(report.disc_size, Some(409_600));

    // The asymmetry (empty side 0, populated side 1) is a deliberately
    // sharper test than a symmetric disc would be: a geometry bug that
    // silently reads side 0's catalogue twice would still pass a
    // both-sides-equal test but fail this one.
    assert_eq!(fs.catalogues[0].entries.len(), 0, "side 0 has no files on this real disc");
    assert_eq!(fs.catalogues[1].entries.len(), 27, "side 1 has 27 files");

    let root = fs.root().unwrap();
    let root_list = fs.list(&root).unwrap();
    let side1 = root_list.objects.iter().find(|o| o.name == "Side1").unwrap().clone();
    let side1_list = fs.list(&side1).unwrap();
    let pge164 = side1_list.objects.iter().find(|o| o.name == "pge164").expect("pge164 present");
    assert!(
        pge164.extents.len() >= 2,
        "pge164 is known (from independent verification) to cross a real track boundary; \
         a single extent here means the interleave split silently stopped working"
    );

    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &default_opts(dir.path()), &mut log).unwrap();
    assert_eq!(summary.files_extracted, 27, "log: {:?}", log.entries);
    for name in ["pge210", "pge208", "pge164", "pge112", "pge132"] {
        assert!(dir.path().join("Side1").join(name).exists(), "missing Side1/{name}");
    }
}
