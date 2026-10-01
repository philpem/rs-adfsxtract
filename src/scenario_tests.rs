//! End-to-end scenario tests: full `extract::walker` pipeline (not just
//! `FileSystem::list`) against synthetic images, covering the specific
//! cases called out in the implementation plan.

use std::path::Path;

use tempfile::tempdir;

use crate::extract::log::ExtractionLog;
use crate::extract::walker::{BrokenDirPolicy, ExtractOptions, walk_and_extract};
use crate::format::dfs::DfsFs;
use crate::format::filecore::FileCoreFs;
use crate::io::rescue::{BadSectorPolicy, RescueMap};
use crate::testutil::{
    DfsFile, DfsSideSpec, NewMapConfig, SynthEntry, SynthFile, build_dfs_disc, build_new_map_disc,
};

fn default_opts(output_dir: &Path) -> ExtractOptions {
    ExtractOptions {
        output_dir: output_dir.to_path_buf(),
        write_inf: false,
        dry_run: false,
        broken_dir_policy: BrokenDirPolicy::Recover,
        bad_sector_policy: BadSectorPolicy::NullFill,
        rescue_map: None,
    }
}

#[test]
fn dosfs_characters_translated_end_to_end() {
    // Non-big directory entries only hold a 10-character name field, so
    // this uses just the 7 DOSFS-swapped characters, not a realistic
    // full filename.
    let cfg = NewMapConfig::default();
    let image = build_new_map_disc(
        vec![SynthEntry::File(SynthFile::plain("/?<>+=;", b"x"))],
        &cfg,
    );
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &default_opts(dir.path()), &mut log).unwrap();

    let expected = dir.path().join(".#$^&@%");
    assert!(
        expected.exists(),
        "expected {expected:?} to exist; log: {:?}",
        log.entries
    );
}

#[test]
fn riscos_charset_high_byte_name_end_to_end() {
    let cfg = NewMapConfig::default();
    // '€' (U+20AC) decodes from RISC OS byte 0x80; round-trips through the
    // synthetic encoder's reverse-lookup table.
    let name = "Price\u{20AC}File";
    let image = build_new_map_disc(
        vec![SynthEntry::File(SynthFile::plain(name, b"money"))],
        &cfg,
    );
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &default_opts(dir.path()), &mut log).unwrap();

    assert!(dir.path().join(name).exists(), "log: {:?}", log.entries);
}

#[test]
fn high_byte_name_inf_encodes_raw_riscos_bytes() {
    let cfg = NewMapConfig::default();
    // 'é' (U+00E9) encodes to a single RISC OS byte 0xE9. The .inf sidecar
    // must record that raw byte as %E9, not the two-byte UTF-8 sequence
    // ("Caf%C3%A9File") that encoding the *decoded* string would produce.
    let name = "Caf\u{E9}File";
    let image = build_new_map_disc(vec![SynthEntry::File(SynthFile::plain(name, b"x"))], &cfg);
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.write_inf = true;
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &opts, &mut log).unwrap();

    let inf_path = dir.path().join(format!("{name}.inf"));
    let inf_content = std::fs::read_to_string(&inf_path).unwrap();
    assert!(
        inf_content.starts_with("\"Caf%E9File\" "),
        "inf must encode raw byte 0xE9, not UTF-8: {inf_content}"
    );
}

#[test]
fn typed_file_gets_suffix_plain_file_does_not() {
    let cfg = NewMapConfig::default();
    let image = build_new_map_disc(
        vec![
            SynthEntry::File(SynthFile::typed("Typed", 0xFEB, 1_600_000_000, b"typed")),
            SynthEntry::File(SynthFile::plain("Untyped", b"plain")),
        ],
        &cfg,
    );
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &default_opts(dir.path()), &mut log).unwrap();

    assert!(
        dir.path().join("Typed,feb").exists(),
        "log: {:?}",
        log.entries
    );
    assert!(
        dir.path().join("Untyped").exists(),
        "log: {:?}",
        log.entries
    );
    assert!(!dir.path().join("Untyped,000").exists());
}

#[test]
fn zero_length_file_extracts_as_empty() {
    let cfg = NewMapConfig::default();
    let image = build_new_map_disc(vec![SynthEntry::File(SynthFile::plain("Empty", b""))], &cfg);
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &default_opts(dir.path()), &mut log).unwrap();

    let path = dir.path().join("Empty");
    assert!(path.exists());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
}

#[test]
fn nested_directories_walk_correctly() {
    let cfg = NewMapConfig::default();
    let image = build_new_map_disc(
        vec![SynthEntry::dir(
            "Outer",
            vec![SynthEntry::dir(
                "Inner",
                vec![SynthEntry::File(SynthFile::plain("Deep", b"deep content"))],
            )],
        )],
        &cfg,
    );
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &default_opts(dir.path()), &mut log).unwrap();

    let deep_path = dir.path().join("Outer").join("Inner").join("Deep");
    assert_eq!(std::fs::read(&deep_path).unwrap(), b"deep content");
    assert_eq!(summary.files_extracted, 1);
    assert_eq!(summary.dirs_created, 3); // root, Outer, Inner
}

#[test]
fn nested_directories_are_well_formed_not_reported_broken() {
    // The synthetic builder must emit valid tail NewDirParent references at
    // every depth (each pointing at its containing directory's SIN), so a
    // `verify` walk reports no broken directory. This guards against the
    // builder regression that would otherwise be required if the parent-SIN
    // validation were ever skipped.
    use crate::diagnostics::Diagnostics;
    use crate::verify::verify;

    let cfg = NewMapConfig::default();
    let image = build_new_map_disc(
        vec![SynthEntry::dir(
            "Outer",
            vec![SynthEntry::dir(
                "Inner",
                vec![SynthEntry::File(SynthFile::plain("Deep", b"deep content"))],
            )],
        )],
        &cfg,
    );
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let mut diag = Diagnostics::default();
    let report = verify(&mut fs, &mut diag).unwrap();
    assert!(diag.is_empty(), "diagnostics: {diag:?}");
    assert_eq!(
        (report.directories, report.files, report.unreadable),
        (3, 1, 0)
    );
}

#[test]
fn info_and_verify_surface_a_bad_parent_sin() {
    // The issue: `info`/`verify` reported a disc with a bad tail NewDirParent
    // as healthy. Corrupt the root directory's parent SIN to a wrong value
    // and assert both surfaces now flag it - as a warning, since the tree is
    // still readable.
    use crate::diagnostics::Diagnostics;
    use crate::extract::report::build_report;
    use crate::verify::verify;

    let cfg = NewMapConfig::default();
    let image = bad_root_parent_disc(&cfg);

    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let report = build_report(&mut fs).unwrap();
    assert!(
        report
            .directory_warnings
            .iter()
            .any(|w| w.contains("parent SIN mismatch")),
        "info should surface the parent-SIN warning: {:?}",
        report.directory_warnings
    );
    assert!(
        report.broken_directories.is_empty(),
        "a wrong parent SIN is a warning, not a structural break: {:?}",
        report.broken_directories
    );

    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let mut diag = Diagnostics::default();
    let report = verify(&mut fs, &mut diag).unwrap();
    assert!(
        diag.iter().any(|d| d.fault.code() == "directory_warning"),
        "verify should report a DirectoryWarning fault: {diag:?}"
    );
    assert!(
        !diag.iter().any(|d| d.fault.code() == "broken_directory"),
        "a wrong parent SIN must not be reported as a broken directory: {diag:?}"
    );
    // The directory is still walked and its file counted.
    assert_eq!(report.files, 1);
}

#[test]
fn parent_sin_warning_does_not_abort_strict_extraction() {
    // Non-fatal quirks are warnings, so even `Fail` (strict) mode extracts
    // the files rather than aborting the whole disc.
    let cfg = NewMapConfig::default();
    let image = bad_root_parent_disc(&cfg);
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.broken_dir_policy = BrokenDirPolicy::Fail;
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &opts, &mut log).unwrap();
    assert_eq!(summary.files_extracted, 1);
    assert!(dir.path().join("Fred").exists());
    assert!(
        log.entries.iter().any(|e| matches!(
            e,
            crate::extract::log::LogEntry::Warning { message } if message.contains("parent SIN")
        )),
        "expected a parent-SIN warning, got: {:?}",
        log.entries
    );
}

#[test]
fn inf_sidecar_written_when_requested() {
    let cfg = NewMapConfig::default();
    let image = build_new_map_disc(
        vec![SynthEntry::File(SynthFile::typed(
            "Doc",
            0xFFF,
            1_650_000_000,
            b"contents",
        ))],
        &cfg,
    );
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.write_inf = true;
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &opts, &mut log).unwrap();

    let inf_path = dir.path().join("Doc,fff.inf");
    let inf_content = std::fs::read_to_string(&inf_path).unwrap();
    assert!(
        inf_content.starts_with("Doc "),
        "inf content: {inf_content}"
    );
    assert!(inf_content.contains("CRC32="));
    assert!(
        !inf_content.contains('&'),
        "hex fields must be bare, not &-prefixed: {inf_content}"
    );
}

fn corrupt_root_check_byte(image_bytes: &mut [u8], sector_size: usize) {
    let tail = crate::format::filecore::dir_old::tail_layout(false);
    let root_addr = 2 * sector_size;
    image_bytes[root_addr + tail.check_byte] ^= 0xFF;
}

/// Builds a one-file new-map disc whose root `NewDirParent` is wrong, with
/// the check byte recomputed so the only anomaly is the parent SIN - a
/// non-fatal warning. The parent field lies inside the checksum region, so
/// corrupting it without fixing the byte would also be a structural break.
fn bad_root_parent_disc(cfg: &NewMapConfig) -> crate::testutil::BuiltImage {
    let mut image = build_new_map_disc(vec![SynthEntry::File(SynthFile::plain("Fred", b"x"))], cfg);
    let sector = 1 << cfg.log2_sector_size;
    let tail = crate::format::filecore::dir_old::tail_layout(false);
    let root_addr = 2 * sector;
    let dir_len = crate::format::filecore::dir_old::LARGE_DIR_SIZE;
    {
        let root = &mut image.bytes[root_addr..root_addr + dir_len];
        // correct value is the root's own SIN: fragment 2 << 8 | sharing 3.
        root[tail.parent] = 0;
        root[tail.parent + 1] = 0;
        root[tail.parent + 2] = 0;
        use crate::format::filecore::checksums::{
            ChecksumRegion, dir_checksum_accumulate, dir_checksum_fold,
        };
        use crate::format::filecore::dir_old::{ENTRY_SIZE, HEADER_SIZE};
        let end_of_entries = HEADER_SIZE + ENTRY_SIZE; // one entry
        let regions = [
            ChecksumRegion {
                start: 0,
                end: end_of_entries,
                words_first: true,
            },
            ChecksumRegion {
                start: tail.tail_start + 1,
                end: tail.dir_len - 4,
                words_first: false,
            },
        ];
        let checksum = dir_checksum_fold(dir_checksum_accumulate(root, &regions));
        root[tail.check_byte] = checksum;
    }
    image
}

#[test]
fn broken_directory_fail_policy_aborts() {
    let cfg = NewMapConfig::default();
    let mut image =
        build_new_map_disc(vec![SynthEntry::File(SynthFile::plain("Fred", b"x"))], &cfg);
    corrupt_root_check_byte(&mut image.bytes, 1 << cfg.log2_sector_size);

    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.broken_dir_policy = BrokenDirPolicy::Fail;
    let mut log = ExtractionLog::default();
    let result = walk_and_extract(&mut fs, &opts, &mut log);
    assert!(result.is_err());
}

#[test]
fn broken_directory_skip_policy_extracts_nothing_from_it() {
    let cfg = NewMapConfig::default();
    let mut image =
        build_new_map_disc(vec![SynthEntry::File(SynthFile::plain("Fred", b"x"))], &cfg);
    corrupt_root_check_byte(&mut image.bytes, 1 << cfg.log2_sector_size);

    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.broken_dir_policy = BrokenDirPolicy::Skip;
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &opts, &mut log).unwrap();
    assert_eq!(summary.files_extracted, 0);
    assert!(!dir.path().join("Fred").exists());
}

#[test]
fn broken_directory_recover_policy_still_extracts() {
    let cfg = NewMapConfig::default();
    let mut image =
        build_new_map_disc(vec![SynthEntry::File(SynthFile::plain("Fred", b"x"))], &cfg);
    corrupt_root_check_byte(&mut image.bytes, 1 << cfg.log2_sector_size);

    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.broken_dir_policy = BrokenDirPolicy::Recover;
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &opts, &mut log).unwrap();
    assert_eq!(summary.files_extracted, 1);
    assert!(dir.path().join("Fred").exists());
    assert!(
        log.entries
            .iter()
            .any(|e| matches!(e, crate::extract::log::LogEntry::BrokenDirectory { .. })),
        "expected a BrokenDirectory log entry"
    );
}

fn file_disc_addr(fs: &mut FileCoreFs<std::io::Cursor<Vec<u8>>>, name: &str) -> (u64, u64) {
    use crate::format::fs::FileSystem;
    let root = fs.root().unwrap();
    let listing = fs.list(&root).unwrap();
    let obj = listing.objects.iter().find(|o| o.name == name).unwrap();
    let e = obj.extents[0];
    (e.disc_addr, e.len)
}

fn rescue_map_marking_bad(start: u64, len: u64, total_bad: bool, total_len: u64) -> RescueMap {
    let text = if total_bad {
        format!("0 + 1\n{start} {total_len} -\n")
    } else {
        format!("0 + 1\n{start} {len} -\n")
    };
    RescueMap::parse(&text).unwrap()
}

#[test]
fn bad_sector_null_fill() {
    let cfg = NewMapConfig::default();
    let content = vec![b'X'; (cfg.idlen as usize + 1) * (1 << cfg.log2_bpmb)];
    let image = build_new_map_disc(
        vec![SynthEntry::File(SynthFile::plain("Fred", &content))],
        &cfg,
    );
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let (addr, len) = file_disc_addr(&mut fs, "Fred");
    // mark the first half of the file's extent bad
    let bad_len = len / 2;

    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.bad_sector_policy = BadSectorPolicy::NullFill;
    opts.rescue_map = Some(rescue_map_marking_bad(addr, bad_len, false, len));
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &opts, &mut log).unwrap();

    let bytes = std::fs::read(dir.path().join("Fred")).unwrap();
    assert_eq!(bytes.len(), content.len());
    assert!(bytes[..bad_len as usize].iter().all(|&b| b == 0));
    assert!(bytes[bad_len as usize..].iter().all(|&b| b == b'X'));
}

#[test]
fn bad_sector_marker_fill() {
    let cfg = NewMapConfig::default();
    let content = vec![b'X'; (cfg.idlen as usize + 1) * (1 << cfg.log2_bpmb)];
    let image = build_new_map_disc(
        vec![SynthEntry::File(SynthFile::plain("Fred", &content))],
        &cfg,
    );
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let (addr, len) = file_disc_addr(&mut fs, "Fred");
    let bad_len = len / 2;

    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.bad_sector_policy = BadSectorPolicy::MarkerFill;
    opts.rescue_map = Some(rescue_map_marking_bad(addr, bad_len, false, len));
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &opts, &mut log).unwrap();

    let bytes = std::fs::read(dir.path().join("Fred")).unwrap();
    assert_eq!(bytes.len(), content.len());
    assert_ne!(&bytes[..bad_len as usize], &content[..bad_len as usize]);
    assert!(bytes[..4].starts_with(b"BAD "));
}

#[test]
fn bad_sector_skip_shortens_output() {
    let cfg = NewMapConfig::default();
    let content = vec![b'X'; (cfg.idlen as usize + 1) * (1 << cfg.log2_bpmb)];
    let image = build_new_map_disc(
        vec![SynthEntry::File(SynthFile::plain("Fred", &content))],
        &cfg,
    );
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let (addr, len) = file_disc_addr(&mut fs, "Fred");
    let bad_len = len / 2;

    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.bad_sector_policy = BadSectorPolicy::Skip;
    opts.rescue_map = Some(rescue_map_marking_bad(addr, bad_len, false, len));
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &opts, &mut log).unwrap();

    let bytes = std::fs::read(dir.path().join("Fred")).unwrap();
    assert_eq!(bytes.len() as u64, len - bad_len);
}

#[test]
fn wholly_bad_file_is_not_extracted() {
    let cfg = NewMapConfig::default();
    let content = vec![b'X'; (cfg.idlen as usize + 1) * (1 << cfg.log2_bpmb)];
    let image = build_new_map_disc(
        vec![SynthEntry::File(SynthFile::plain("Fred", &content))],
        &cfg,
    );
    let mut fs = FileCoreFs::open(image.cursor()).unwrap();
    let (addr, len) = file_disc_addr(&mut fs, "Fred");

    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.rescue_map = Some(rescue_map_marking_bad(addr, len, true, len));
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &opts, &mut log).unwrap();

    assert_eq!(summary.files_skipped, 1);
    assert_eq!(summary.files_extracted, 0);
    assert!(!dir.path().join("Fred").exists());
    assert!(
        log.entries
            .iter()
            .any(|e| matches!(e, crate::extract::log::LogEntry::SkippedWhollyBad { .. })),
        "expected a SkippedWhollyBad log entry"
    );
}

#[test]
fn dfs_ssd_extraction_uses_dirchar_prefix_and_locked_attr() {
    let image = build_dfs_disc(vec![DfsSideSpec::new(
        "DISC",
        vec![
            DfsFile::plain("BOOT", b"boot text"),
            DfsFile::in_dir("CODE", 'L', b"code").locked(),
        ],
    )]);
    let mut fs = DfsFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.write_inf = true;
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &opts, &mut log).unwrap();

    assert!(dir.path().join("BOOT").exists(), "log: {:?}", log.entries);
    assert!(dir.path().join("L.CODE").exists(), "log: {:?}", log.entries);

    let inf = std::fs::read_to_string(dir.path().join("L.CODE.inf")).unwrap();
    let parts: Vec<&str> = inf.split_whitespace().collect();
    assert_eq!(parts[0], "L.CODE");
    assert_eq!(
        parts[4], "04",
        "attrs field should carry ATTR_LOCKED: {inf}"
    );
}

#[test]
fn dfs_dsd_extraction_creates_side_directories() {
    let image = build_dfs_disc(vec![
        DfsSideSpec::new("SIDE0", vec![DfsFile::plain("ALPHA", b"alpha content")]),
        DfsSideSpec::new("SIDE1", vec![DfsFile::plain("BETA", b"beta content")]),
    ]);
    let mut fs = DfsFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &default_opts(dir.path()), &mut log).unwrap();

    assert!(
        dir.path().join("Side0").join("ALPHA").exists(),
        "log: {:?}",
        log.entries
    );
    assert!(
        dir.path().join("Side1").join("BETA").exists(),
        "log: {:?}",
        log.entries
    );
}

/// Pushes the second file's catalogue entry (`sector1` offset `0x117`, the
/// start-sector byte of entry index 1) out of the disc's bounds - DFS's
/// only integrity signal, since there's no checksum to corrupt instead.
fn dfs_image_with_one_out_of_bounds_entry() -> Vec<u8> {
    let image = build_dfs_disc(vec![DfsSideSpec::new(
        "DISC",
        vec![DfsFile::plain("GOOD", b"fine"), DfsFile::plain("BAD", b"x")],
    )]);
    let mut bytes = image.bytes;
    bytes[0x117] = 250;
    bytes
}

#[test]
fn dfs_broken_entry_recover_policy_extracts_good_files() {
    let mut fs = DfsFs::open(std::io::Cursor::new(
        dfs_image_with_one_out_of_bounds_entry(),
    ))
    .unwrap();
    let dir = tempdir().unwrap();
    let mut log = ExtractionLog::default();
    let summary = walk_and_extract(&mut fs, &default_opts(dir.path()), &mut log).unwrap();

    assert!(dir.path().join("GOOD").exists(), "log: {:?}", log.entries);
    assert!(!dir.path().join("BAD").exists());
    assert_eq!(summary.files_extracted, 1);
    assert!(
        log.entries
            .iter()
            .any(|e| matches!(e, crate::extract::log::LogEntry::BrokenDirectory { .. })),
        "expected the out-of-bounds entry to be logged: {:?}",
        log.entries
    );
}

#[test]
fn dfs_broken_entry_fail_policy_aborts() {
    let mut fs = DfsFs::open(std::io::Cursor::new(
        dfs_image_with_one_out_of_bounds_entry(),
    ))
    .unwrap();
    let dir = tempdir().unwrap();
    let mut opts = default_opts(dir.path());
    opts.broken_dir_policy = BrokenDirPolicy::Fail;
    let mut log = ExtractionLog::default();
    let result = walk_and_extract(&mut fs, &opts, &mut log);
    assert!(
        result.is_err(),
        "Fail policy should abort on an out-of-bounds DFS entry"
    );
}

#[test]
fn dangerous_name_does_not_escape_output_directory() {
    // A name field of "//" translates to ".." via the DOSFS '/'->'.' swap.
    // Nothing in the on-disk format forbids this; a corrupted or crafted
    // image can produce it trivially (DFS's 7-byte name field holds it with
    // room to spare). Left unhandled, `host_dir.join("..")` would write
    // this file's content into the *parent* of the output directory.
    let image = build_dfs_disc(vec![DfsSideSpec::new(
        "DISC",
        vec![DfsFile::plain("//", b"escaped?")],
    )]);
    let mut fs = DfsFs::open(image.cursor()).unwrap();
    let dir = tempdir().unwrap();
    // The output directory's parent is the shared system temp dir, which other
    // parallel tests churn with their own tempdirs (directories). Count only
    // *files* so the assertion is about what extraction might leak, not about
    // unrelated concurrently-created sibling directories.
    let count_files = |p: &Path| {
        std::fs::read_dir(p)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .map(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                    .unwrap_or(false)
            })
            .count()
    };
    let parent_before = count_files(dir.path().parent().expect("tempdir has a parent"));
    let mut log = ExtractionLog::default();
    walk_and_extract(&mut fs, &default_opts(dir.path()), &mut log).unwrap();

    // Nothing should have been written into the output directory's parent -
    // its listing must be unchanged (still just the tempdir itself).
    let parent_after = count_files(dir.path().parent().expect("tempdir has a parent"));
    assert_eq!(
        parent_before, parent_after,
        "extraction must not add anything outside --output"
    );

    // The file must exist *inside* the output directory under a safe,
    // substituted name, not silently dropped and not named "..".
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one substituted file, got {entries:?}"
    );
    let substituted_name = entries[0].to_str().unwrap();
    assert_ne!(substituted_name, "..");
    assert_ne!(substituted_name, ".");
    let content = std::fs::read(dir.path().join(substituted_name)).unwrap();
    assert_eq!(content, b"escaped?");

    assert!(
        log.entries.iter().any(|e| matches!(e, crate::extract::log::LogEntry::Warning { message } if message.contains("unsafe"))),
        "expected a warning about the substituted name: {:?}",
        log.entries
    );
}

#[test]
fn newmap_sequential_track_order_refused() {
    // New-map discs normally use interleaved track order; a disc with
    // `DiscRecord_SequenceSides_Flag` (bit 6 of `low_sector`) set requests
    // sequential ordering, which this backend deliberately refuses rather
    // than mis-reading bytes (guide §1.1/§2.1). Detection must recognise the
    // image as new-map and then reject it cleanly with `Unsupported` - not
    // panic, not silently extract wrong data, and not misidentify it as
    // something else.
    use crate::error::FcError;

    let cfg = NewMapConfig::default();
    let mut image = build_new_map_disc(
        vec![SynthEntry::File(SynthFile::plain("Seq", b"sequential"))],
        &cfg,
    );

    // The disc record copy detection and open() read lives at byte 0x04; the
    // low_sector field is at offset +0x08 within it. Set bit 6.
    const LOW_SECTOR_ABS: usize = 0x04 + 0x08;
    image.bytes[LOW_SECTOR_ABS] |= 0x40;
    // Keep the zone-0 checksum consistent (zone_check is a folding XOR), or the
    // still-plausible detection for the (unused, since refusal happens first)
    // zone map would be left stale; not strictly required to reach the refusal
    // but keeps the fixture internally valid.
    let sector_size = 1usize << cfg.log2_sector_size;
    for base in [0, sector_size] {
        image.bytes[base] =
            crate::format::filecore::checksums::zone_check(&image.bytes[base..base + sector_size]);
    }

    let result = FileCoreFs::open(image.cursor());
    let err = match result {
        Ok(_) => panic!("sequential-order new-map must be refused"),
        Err(e) => e,
    };
    assert!(
        matches!(err, FcError::Unsupported(_)),
        "expected Unsupported, got {err:?}"
    );
}

#[test]
fn truncated_dump_is_flagged_in_info() {
    // A truncated/partial capture (the image is shorter than the size recorded
    // on disc) must be surfaced as a non-fatal warning in `info`, not silently
    // accepted. (Observed on a real drive: a 7.5 MB image whose disc record
    // claims 13 MB.)
    use crate::extract::report::build_report;

    let cfg = NewMapConfig::default();
    let mut image = build_new_map_disc(vec![SynthEntry::File(SynthFile::plain("F", b"x"))], &cfg);
    let full = image.bytes.len();
    image.bytes.truncate(full - 1); // simulate an aborted capture

    let mut fs = FileCoreFs::open(image.cursor()).expect("still opens after truncation");
    let report = build_report(&mut fs).unwrap();
    assert!(
        report
            .directory_warnings
            .iter()
            .any(|w| w.contains("truncated")),
        "expected a truncation warning, got {:?}",
        report.directory_warnings
    );
}
