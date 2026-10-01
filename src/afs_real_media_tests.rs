//! Opt-in validation against **real Acorn File Server (AFS0)** images.
//!
//! These genuinely formatted Level 2 / Level 3 (hybrid) discs exercise layouts
//! synthetic fixtures cannot: real interleaved media, the linked-list
//! directory structure and the ADFS+AFS hybrid partition. They are not
//! bundled. Local development fetches each from its source and caches it into
//! `ACORNFS_EXTERNAL_SOURCE`; CI sets `ACORNFS_CI=1` (see `real_media_common`)
//! so it never polls a forum - missing files are silently skipped there.

use std::path::PathBuf;

use crate::diagnostics::Diagnostics;
use crate::format::afs::AfsFs;
use crate::real_media_common::{Spec, obtain_disc, sha256_hex};
use crate::verify::verify;

struct FetchSpec {
    spec: Spec<'static>,
    sha256: &'static str,
    expected: &'static str,
}

const MANIFEST: &[FetchSpec] = &[
    // Mark Moxon's Level 2 file-server data disc, created with Acorn DSCMGR
    // and populated with Elite. Genuine INT-interleaved 400 KiB AFS0 media.
    FetchSpec {
        spec: Spec {
            label: "econet_level_2_elite.dsd",
            url: "https://www.stardot.org.uk/forums/download/file.php?id=105300",
            in_zip: false,
        },
        sha256: "6de15f03e8911d7995195e0a4294f169d9dddc7a450e281e2578e7494f37ebfb",
        expected: "Level 2 (AFS2) populated data disc - ELITE, INT interleave, 7 dirs / 56 files",
    },
    // BeebMaster's Level 3 combined software+storage disc: an ADFS floppy that
    // carries an embedded Level 3 (NFS/"AFS0") partition - the hybrid layout.
    FetchSpec {
        spec: Spec {
            label: "L3Utils3.adf",
            url: "https://www.beebmaster.co.uk/Downloads/L3Utils3.zip",
            in_zip: true,
        },
        sha256: "cfc9b7c2160a570ce8869381a1b43d1f2f5c55d943b83878e0d46d42a334254c",
        expected: "Level 3 (AFS3) ADFS+AFS hybrid - AFS0 partition not at byte zero",
    },
];

#[test]
fn validates_real_afs_images() {
    let Ok(cache) = std::env::var("ACORNFS_EXTERNAL_SOURCE") else {
        eprintln!("skipping: ACORNFS_EXTERNAL_SOURCE not set");
        return;
    };
    let cache = PathBuf::from(cache);

    for entry in MANIFEST {
        // Prints the disc description, panics if the bundle is missing the disc
        // (CI), or returns None for a non-fatal local fetch failure (skip).
        let Some(bytes) = obtain_disc(&entry.spec, &cache, entry.expected) else {
            continue;
        };
        let actual = sha256_hex(&bytes);
        assert_eq!(
            actual, entry.sha256,
            "{}: content changed (expected {}); re-pin or the file is a different build",
            entry.spec.label, entry.sha256
        );

        let mut fs = match AfsFs::open(std::io::Cursor::new(bytes.clone())) {
            Ok(fs) => fs,
            Err(e) => panic!("{} did not open as AFS0: {e}", entry.spec.label),
        };
        let mut diag = Diagnostics::default();
        let report = verify(&mut fs, &mut diag).unwrap();
        eprintln!(
            "{}: level={} interleave={:?} name={:?} dirs={} files={} unreadable={}",
            entry.spec.label,
            fs.info.level.as_str(),
            fs.interleave(),
            fs.info.title,
            report.directories,
            report.files,
            report.unreadable
        );
        assert!(
            report.files > 0,
            "{}: no files extracted (expected {}): interleave={:?}",
            entry.spec.label,
            entry.expected,
            fs.interleave()
        );
        assert_eq!(
            report.unreadable, 0,
            "{}: unreadable files",
            entry.spec.label
        );
    }
}
