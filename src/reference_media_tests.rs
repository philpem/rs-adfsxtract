//! Always-on regression tests against the committed reference disc images
//! (see `data/README.md`). These must not require network, an env var, or an
//! API key: the fixtures are in-repo and gzip-compressed. They exist to catch
//! format-parameter regressions the synthetic tests (a builder that always
//! writes a consistent image) cannot - most importantly S/M/L
//! sequential-to-interleaved sector translation, which would silently read
//! the wrong bytes for any file past track 0.

use std::io::{Cursor, Read};
use std::path::Path;

use tempfile::tempdir;

use crate::diagnostics::Diagnostics;
use crate::extract::log::ExtractionLog;
use crate::extract::report::build_report;
use crate::extract::walker::{BrokenDirPolicy, ExtractOptions, walk_and_extract};
use crate::format::filecore::FileCoreFs;
use crate::io::rescue::BadSectorPolicy;
use crate::verify::{verify, verify_filecore_volume};

const DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/");

const HOSTFS_LEN: usize = 104;
const HOSTFS_CRC: u32 = 0xae39bd2d;
const QTM149_LEN: usize = 4634;
const QTM149_CRC: u32 = 0x17d27b1f;

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

fn assert_clean_verify(fs: &mut FileCoreFs<Cursor<Vec<u8>>>) {
    let mut diag = Diagnostics::default();
    verify_filecore_volume(fs, &mut diag).unwrap();
    let report = verify(fs, &mut diag).unwrap();
    assert_eq!((report.directories, report.files, report.unreadable), (1, 2, 0), "unexpected: {diag:?}");
    assert!(diag.is_empty(), "expected a clean image, diagnostics: {diag:?}");
}

fn assert_content(dir: &Path, name: &str, expected_len: usize, expected_crc: u32) {
    let bytes = std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("read {name}: {e}"));
    assert_eq!(bytes.len(), expected_len, "unexpected length for {name}");
    assert_eq!(crc32fast::hash(&bytes), expected_crc, "content mismatch for {name}");
}

fn extract(dir: &Path, bytes: &[u8]) {
    let mut fs = FileCoreFs::open(Cursor::new(bytes.to_vec())).expect("recognised as FileCore");
    assert_clean_verify(&mut fs);
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &opts(dir), &mut log).expect("extraction succeeds");
}

#[test]
fn all_four_formats_extract_identical_files() {
    // Every fixture contains the same two files, so the extracted bytes must
    // be identical across all four formats. That also transitively validates
    // the S/M/L geometry translation: the .adl image yields the same bytes as
    // the unambiguously-addressed D/E/F images, which is impossible without
    // the sequential-to-interleaved conversion.
    let mut qtm: Option<Vec<u8>> = None;
    let mut hostfs: Option<Vec<u8>> = None;
    for fixture in ["adfs640L.adl.gz", "adfs800D.adf.gz", "adfs800E.adf.gz", "adfs1600F.adf.gz"] {
        let dir = tempdir().unwrap();
        extract(dir.path(), &load_fixture(fixture));
        assert_content(dir.path(), "qtm149.txt,fff", QTM149_LEN, QTM149_CRC);
        assert_content(dir.path(), "hostfs.txt,fff", HOSTFS_LEN, HOSTFS_CRC);
        if qtm.is_none() {
            qtm = Some(std::fs::read(dir.path().join("qtm149.txt,fff")).unwrap());
            hostfs = Some(std::fs::read(dir.path().join("hostfs.txt,fff")).unwrap());
        } else {
            assert_eq!(
                std::fs::read(dir.path().join("qtm149.txt,fff")).unwrap().as_slice(),
                qtm.as_deref().unwrap(),
                "qtm149 differs between formats ({fixture})"
            );
            assert_eq!(
                std::fs::read(dir.path().join("hostfs.txt,fff")).unwrap().as_slice(),
                hostfs.as_deref().unwrap(),
                "hostfs differs between formats ({fixture})"
            );
        }
    }
}

#[test]
fn sml_interleaving_translates_correctly() {
    // A direct, dedicated check on the S/M/L image: qtm149.txt spans past
    // track 0, so a false positive here means the sequential-to-interleaved
    // translation has regressed.
    let dir = tempdir().unwrap();
    extract(dir.path(), &load_fixture("adfs640L.adl.gz"));
    assert_content(dir.path(), "qtm149.txt,fff", QTM149_LEN, QTM149_CRC);
}

#[test]
fn metadata_matches_known_values() {
    struct Expect<'a> {
        fixture: &'a str,
        map: &'a str,
        dir: &'a str,
        name: &'a str,
        size: u64,
        boot_present: bool,
        zone_total: Option<usize>,
    }
    let cases = [
        Expect { fixture: "adfs640L.adl.gz", map: "old", dir: "old", name: "00_05_Sun", size: 655360, boot_present: false, zone_total: None },
        Expect { fixture: "adfs800D.adf.gz", map: "old", dir: "new", name: "00_06_Sun", size: 819200, boot_present: false, zone_total: None },
        Expect { fixture: "adfs800E.adf.gz", map: "new", dir: "new", name: "00_07_Sun ", size: 819200, boot_present: false, zone_total: Some(1) },
        Expect { fixture: "adfs1600F.adf.gz", map: "new", dir: "new", name: "00_07_Sun ", size: 1638400, boot_present: true, zone_total: Some(4) },
    ];
    for case in cases {
        let mut fs = FileCoreFs::open(Cursor::new(load_fixture(case.fixture))).expect("recognised as FileCore");
        let report = build_report(&mut fs).unwrap();
        assert_eq!(report.filesystem, "FileCore", "{}", case.fixture);
        assert_eq!(report.map_type, case.map, "{}", case.fixture);
        assert_eq!(report.dir_type, case.dir, "{}", case.fixture);
        assert_eq!(report.disc_name.as_deref(), Some(case.name), "{}", case.fixture);
        assert_eq!(report.disc_size, Some(case.size), "{}", case.fixture);
        assert_eq!(report.boot_block_present, case.boot_present, "{}", case.fixture);
        assert_eq!(report.zone_checksum_total, case.zone_total, "{}", case.fixture);
        if let Some(total) = case.zone_total {
            assert_eq!(report.zone_checksum_ok_count, Some(total), "{}", case.fixture);
            assert_eq!(report.cross_check_ok, Some(true), "{}", case.fixture);
        }
        assert!(report.root_check_byte_ok, "{}", case.fixture);
    }
}

#[test]
fn fixtures_are_gzip_deterministic() {
    // Guard against a fixture being silently regenerated with different
    // bytes: verify against the known raw image sizes.
    for (fixture, sz) in [
        ("adfs640L.adl.gz", 655360u64),
        ("adfs800D.adf.gz", 819200u64),
        ("adfs800E.adf.gz", 819200u64),
        ("adfs1600F.adf.gz", 1638400u64),
    ] {
        assert_eq!(load_fixture(fixture).len() as u64, sz, "{fixture}");
    }
}
