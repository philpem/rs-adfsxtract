//! Opt-in validation against a local corpus of real disc images (e.g. the
//! `/mnt/nfs` archive). Unlike the committed fixtures - which must be
//! authoritative and small - this scans whatever is on disk, opens and
//! verifies every recognisable Acorn disc, and reports which *format cells*
//! are actually exercised. Nothing is committed and no hash is pinned, so it
//! is a broad, ongoing real-media sweep.
//!
//! It self-skips unless `ACORNFS_CORPUS_DIR` is set to a directory to scan.
//! Flux/raw-floppy formats (`.scp`/`.dfi`/`.hfe`) are skipped, as are clearly
//! non-Acorn images; only ADFS/FileCore and DFS candidates are attempted.
//!
//! This is report-only (like the format-coverage matrix): it never gates CI,
//! but the summary tells us what real media is out there and whether we still
//! recognise it.

use std::path::{Path, PathBuf};

use crate::diagnostics::Diagnostics;
use crate::extract::report::{build_dfs_report, build_report};
use crate::format::dfs::DfsFs;
use crate::format::filecore::FileCoreFs;
use crate::verify::{verify, verify_filecore_volume};

/// Directories whose contents are (almost) all flux images; skip descent to
/// avoid walking enormous flux-only trees on a slow mount.
const SKIP_DIRS: &[&str] = &["beeb-hfe-scarybeasts"];

fn candidate(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "adf" | "adl" | "ssd" | "dsd" | "hdf" | "img" | "dd"
    )
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            if SKIP_DIRS.contains(&p.file_name().and_then(|s| s.to_str()).unwrap_or("")) {
                continue;
            }
            walk(&p, out);
        } else if candidate(&p) {
            out.push(p);
        }
    }
}

#[test]
fn scan_corpus_for_format_coverage() {
    let Ok(dir) = std::env::var("ACORNFS_CORPUS_DIR") else {
        eprintln!("skipping: ACORNFS_CORPUS_DIR not set");
        return;
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        eprintln!("skipping: {dir:?} is not a directory");
        return;
    }

    let mut files = Vec::new();
    walk(&dir, &mut files);
    eprintln!(
        "corpus scan: {} candidate image(s) under {}",
        files.len(),
        dir.display()
    );

    let mut recognized = 0usize;
    let mut error = 0usize;
    let mut broken = 0usize;
    // Tally the format cells actually exercised on real media.
    let mut fc_new_new = 0usize;
    let mut fc_new_big = 0usize;
    let mut fc_old_new = 0usize;
    let mut fc_old_old = 0usize;
    let mut dfs_ssd = 0usize;
    let mut dfs_dsd = 0usize;

    for path in &files {
        let label = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
        let file = || std::fs::File::open(path).unwrap_or_else(|e| panic!("open {label}: {e}"));
        // FileCore first; fall through to DFS only on non-recognition.
        if let Ok(mut fs) = FileCoreFs::open(file()) {
            if let Ok(report) = build_report(&mut fs) {
                let mut diag = Diagnostics::default();
                let _ = verify_filecore_volume(&fs, &mut diag);
                let unreadable = verify(&mut fs, &mut diag)
                    .map(|v| v.unreadable)
                    .unwrap_or(usize::MAX);
                match (report.map_type, report.dir_type) {
                    ("new", "new") => fc_new_new += 1,
                    ("new", "big") => fc_new_big += 1,
                    ("old", "new") => fc_old_new += 1,
                    ("old", "old") => fc_old_old += 1,
                    _ => {}
                }
                if unreadable != 0 || !report.root_check_byte_ok {
                    broken += 1;
                    eprintln!(
                        "  [broken] {label}: map={} dir={} unreadable={unreadable} root_check={}",
                        report.map_type, report.dir_type, report.root_check_byte_ok
                    );
                } else {
                    recognized += 1;
                }
                continue;
            }
            // report failed: count as error rather than fall through to DFS.
            error += 1;
            eprintln!("  [err] {label}: FileCore report failed");
            continue;
        }
        match DfsFs::open(file()) {
            Ok(mut fs) => {
                if build_dfs_report(&mut fs).is_ok() {
                    if fs.double_sided {
                        dfs_dsd += 1;
                    } else {
                        dfs_ssd += 1;
                    }
                    recognized += 1;
                } else {
                    error += 1;
                    eprintln!("  [err] {label}: DFS report failed");
                }
            }
            Err(_) => {
                error += 1;
                eprintln!("  [err] {label}: not recognised as FileCore or DFS");
            }
        }
    }

    eprintln!("corpus result: recognized={recognized} error={error} broken={broken}");
    eprintln!(
        "  FileCore new|new={fc_new_new} new|big={fc_new_big} old|new={fc_old_new} old|old={fc_old_old} | DFS ssd={dfs_ssd} dsd={dfs_dsd}"
    );
    assert!(
        recognized > 0,
        "corpus scan found no recognisable Acorn disc images"
    );
}
