//! Opt-in validation against **additional real discs** fetched from
//! freely-distributed / public-domain sources (8bs.com, mdfs.net,
//! bbcmicro.co.uk, Stardot, The BBC Lives mirror).
//!
//! These are not bundled. Local development fetches each from its source and
//! caches it into `ACORNFS_EXTERNAL_SOURCE`; CI sets `ACORNFS_CI=1` (see
//! `real_media_common`) so it never polls a forum - missing files are silently
//! skipped there.

use std::path::PathBuf;

use crate::diagnostics::Diagnostics;
use crate::extract::report::{build_dfs_report, build_report};
use crate::format::dfs::DfsFs;
use crate::format::filecore::FileCoreFs;
use crate::real_media_common::{Spec, obtain_disc, sha256_hex};
use crate::verify::{verify, verify_filecore_volume};

struct FetchSpec {
    spec: Spec<'static>,
    sha256: &'static str,
    expected: &'static str,
}

const MANIFEST: &[FetchSpec] = &[
    // Real ADFS L-format disc from the 8bs Archimedes public-domain set.
    FetchSpec {
        spec: Spec {
            label: "arc-01.640.adf",
            url: "https://8bs.com/pool/arc/arc-01.zip",
            in_zip: true,
        },
        sha256: "0127ca1032e431e13c9348426a5885932ae7bea8ca3731f8c32fd5cf93fec4e3",
        expected: "ADFS L; disc name 19_52_Fri; a large old-map/old-dir image with broken directories",
    },
    // Real ADFS new-map (E-format) disc, clean.
    FetchSpec {
        spec: Spec {
            label: "ARC-04.800.adf",
            url: "https://8bs.com/pool/arc/arc-04.zip",
            in_zip: true,
        },
        sha256: "75cee6214a94db3a4799ac5b7f4d441c30ac93a0aa964bc6a60e499a06a6b004",
        expected: "ADFS new-map 800 KB; disc name 1_DataComm; clean verify",
    },
    // Real Acorn DFS disc from mdfs.net (J.G.Harston's freely-distributed
    // software). This one is a truncated 800-sector image - a deliberately
    // awkward edge case the tool must tolerate rather than reject.
    FetchSpec {
        spec: Spec {
            label: "Utils1.ssd",
            url: "https://mdfs.net/Mirror/Image/JGH/Utils1.ssd",
            in_zip: false,
        },
        sha256: "96a065b1d797bb0cf2982194add984786a974687fedeca186d19b19d5cce7ba2",
        expected: "Acorn DFS single-sided (Utilities1); a truncated 800-sector image",
    },
    // An HADFS disc (Harston Advanced Disk Filing System) from mdfs.net.
    FetchSpec {
        spec: Spec {
            label: "HADFS_System.ssd",
            url: "https://mdfs.net/Software/HADFS/System.ssd",
            in_zip: false,
        },
        sha256: "b3a5a86ac80c40b947d2c5be146a0c66163d5c2cbf671b031c7612915c6ba968",
        expected: "HADFS disc (not Acorn DFS); must not crash on detection",
    },
    // Real Acorn DFS game discs from bbcmicro.co.uk's freely-hosted archive.
    FetchSpec {
        spec: Spec {
            label: "bbcmicro_pentagram.ssd",
            url: "https://www.bbcmicro.co.uk/gameimg/discs/4574/Disc999-pentagram_release.ssd",
            in_zip: false,
        },
        sha256: "7aea6738465c9271dad49534563d49812ab0e3ab4984357c1cb4cc9356771919",
        expected: "Acorn DFS single-sided 100 KB game disc",
    },
    FetchSpec {
        spec: Spec {
            label: "bbcmicro_pontoon.dsd",
            url: "https://www.bbcmicro.co.uk/gameimg/discs/4539/Disc999-PontoonYaketyYak2025Hack.dsd",
            in_zip: false,
        },
        sha256: "2089712cdc925e4bf71dccb729d837b9b75fe7ccf7e1af9d8af8db99894ca3e7",
        expected: "Acorn DFS double-sided game disc (SPEECHGAMES), independent side-1 catalogue",
    },
    FetchSpec {
        spec: Spec {
            label: "bbcmicro_holmoboy.ssd",
            url: "https://www.bbcmicro.co.uk/gameimg/discs/4573/Disc999-HomoSapiens.ssd",
            in_zip: false,
        },
        sha256: "40d1ff5ea0bc99bcc919c1ce164ad836b7f4de3b947627e1efb844f3e7e2039d",
        expected: "Acorn DFS single-sided game disc",
    },
    // A genuine Watford DFS 62-file-extension disc from The BBC Lives mirror
    // hosted at rk.nvg.ntnu.no. This media exposed a real reader bug: the
    // catalogue's total_sectors (445) is far smaller than the physical image
    // (200 KB), and its extension-block files live beyond that declared total.
    FetchSpec {
        spec: Spec {
            label: "BGAME1_A.BBC",
            url: "https://rk.nvg.ntnu.no/bbc/disk/watford/games/bgame1_a.zip",
            in_zip: true,
        },
        sha256: "c0bd98515338b44a3494faa3b51c673c85351308f52a0c0bad2e7ca3eee6abd1",
        expected: "Watford DFS disc, 62-file extension (41 catalogue entries), declared total smaller than image",
    },
    // A single-density Acorn DFS game disc recovered from a Solidisk DDFS
    // (double-density) original - posted to Stardot by sweh.
    FetchSpec {
        spec: Spec {
            label: "Eagle_Empire.ssd",
            url: "https://www.stardot.org.uk/forums/download/file.php?id=3423",
            in_zip: false,
        },
        sha256: "5d5e83cc9d85df22e6a2469293ba1d147f670c2a861a6ed285c5886d0ac1ae2c",
        expected: "Acorn DFS single-density game disc (recovered from a Solidisk DDFS original)",
    },
];

#[test]
fn validates_additional_real_discs() {
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

        // It must be a genuinely recognisable Acorn format that parses and
        // verifies, not just a file we can hash.
        let mut parsed = false;
        if let Ok(mut fs) = FileCoreFs::open(std::io::Cursor::new(bytes.clone())) {
            let report = build_report(&mut fs).unwrap();
            let mut diag = Diagnostics::default();
            let _ = verify_filecore_volume(&fs, &mut diag);
            let v = verify(&mut fs, &mut diag).unwrap();
            eprintln!(
                "{}: FileCore map={} dir={} name={:?} size={:?} unreadable={}",
                entry.spec.label,
                report.map_type,
                report.dir_type,
                report.disc_name,
                report.disc_size,
                v.unreadable
            );
            parsed = true;
        }
        if let Ok(mut fs) = DfsFs::open(std::io::Cursor::new(bytes)) {
            let report = build_dfs_report(&mut fs).unwrap();
            eprintln!(
                "{}: DFS name={:?} size={:?} double={}",
                entry.spec.label, report.disc_name, report.disc_size, fs.double_sided
            );
            parsed = true;
        }
        assert!(
            parsed,
            "{} did not parse as FileCore or DFS",
            entry.spec.label
        );
    }
}
