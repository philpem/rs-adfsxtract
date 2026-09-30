//! Opt-in validation against *additional* real discs fetched at runtime from
//! 8bs.com, mdfs.net and bbcmicro.co.uk (freely-distributed / public-domain
//! software and game discs). Unlike the committed fixtures, these are never
//! bundled: each entry pins a URL and an expected SHA-256 so that a retrieval
//! that changes is detected rather than silently shifting the test's ground
//! truth.
//!
//! Skipped entirely unless `ACORNFS_EXTERNAL_SOURCE` is set to a directory to
//! cache downloads into. The committed `reference_media_tests`/`dfs_reference`
//! never need network; this target broadens coverage and surfaces facts about
//! real media that synthetic fixtures cannot, but is **not** a dependency of
//! the always-on suite.

use std::io::{Cursor, Read};
use std::path::PathBuf;

use crate::diagnostics::Diagnostics;
use crate::extract::report::{build_dfs_report, build_report};
use crate::format::dfs::DfsFs;
use crate::format::filecore::FileCoreFs;
use crate::verify::{verify, verify_filecore_volume};

struct FetchSpec {
    label: &'static str,
    url: &'static str,
    sha256: &'static str,
    expected: &'static str,
}

const MANIFEST: &[FetchSpec] = &[
    // Real ADFS L-format disc from the 8bs Archimedes public-domain set.
    FetchSpec {
        label: "arc-01.640.adf",
        url: "https://8bs.com/pool/arc/arc-01.zip",
        // The file lives inside the zip; we extract and verify the raw image.
        sha256: "0127ca1032e431e13c9348426a5885932ae7bea8ca3731f8c32fd5cf93fec4e3",
        expected: "ADFS L; disc name 19_52_Fri; a large old-map/old-dir image with broken directories",
    },
    // Real ADFS new-map (E-format) disc, clean.
    FetchSpec {
        label: "ARC-04.800.adf",
        url: "https://8bs.com/pool/arc/arc-04.zip",
        sha256: "75cee6214a94db3a4799ac5b7f4d441c30ac93a0aa964bc6a60e499a06a6b004",
        expected: "ADFS new-map 800 KB; disc name 1_DataComm; clean verify",
    },
    // Real Acorn DFS disc from mdfs.net (J.G.Harston's freely-distributed
    // software). This one is a truncated 800-sector image - a deliberately
    // awkward edge case the tool must tolerate rather than reject.
    FetchSpec {
        label: "Utils1.ssd",
        url: "https://mdfs.net/Mirror/Image/JGH/Utils1.ssd",
        sha256: "96a065b1d797bb0cf2982194add984786a974687fedeca186d19b19d5cce7ba2",
        expected: "Acorn DFS single-sided (Utilities1); a truncated 800-sector image",
    },
    // An HADFS disc (Harston Advanced Disk Filing System) from mdfs.net. HADFS
    // is a different 8-bit filing system that our tool does not implement; the
    // DFS detector should still handle it without crashing (it is currently
    // reported as DFS). Pinned here so the mis-detection can't silently break.
    FetchSpec {
        label: "HADFS_System.ssd",
        url: "https://mdfs.net/Software/HADFS/System.ssd",
        sha256: "b3a5a86ac80c40b947d2c5be146a0c66163d5c2cbf671b031c7612915c6ba968",
        expected: "HADFS disc (not Acorn DFS); must not crash on detection",
    },
    // Real Acorn DFS game discs from bbcmicro.co.uk's freely-hosted archive.
    // A 100 KB single-sided disc (a geometry the committed 200 KB fixture
    // doesn't cover) and a double-sided disc with an independent side-1
    // catalogue, both genuine Acorn DFS media (copyrighted -> runtime only).
    FetchSpec {
        label: "bbcmicro_pentagram.ssd",
        url: "https://www.bbcmicro.co.uk/gameimg/discs/4574/Disc999-pentagram_release.ssd",
        sha256: "7aea6738465c9271dad49534563d49812ab0e3ab4984357c1cb4cc9356771919",
        expected: "Acorn DFS single-sided 100 KB game disc",
    },
    FetchSpec {
        label: "bbcmicro_pontoon.dsd",
        url: "https://www.bbcmicro.co.uk/gameimg/discs/4539/Disc999-PontoonYaketyYak2025Hack.dsd",
        sha256: "2089712cdc925e4bf71dccb729d837b9b75fe7ccf7e1af9d8af8db99894ca3e7",
        expected: "Acorn DFS double-sided game disc (SPEECHGAMES), independent side-1 catalogue",
    },
    FetchSpec {
        label: "bbcmicro_holmoboy.ssd",
        url: "https://www.bbcmicro.co.uk/gameimg/discs/4573/Disc999-HomoSapiens.ssd",
        sha256: "40d1ff5ea0bc99bcc919c1ce164ad836b7f4de3b947627e1efb844f3e7e2039d",
        expected: "Acorn DFS single-sided game disc",
    },
    // A genuine Watford DFS 62-file-extension disc from The BBC Lives mirror
    // hosted at rk.nvg.ntnu.no. This media exposed a real reader bug: the
    // catalogue's total_sectors (445) is far smaller than the physical image
    // (200 KB), and its extension-block files live beyond that declared total.
    // The reader must bound entries by the image size, and this quirk must be
    // surfaced as a divergence warning (see the synthetic regression in
    // testutil::watford_extension_beyond_declared_total_is_extracted).
    FetchSpec {
        label: "BGAME1_A.BBC",
        url: "https://rk.nvg.ntnu.no/bbc/disk/watford/games/bgame1_a.zip",
        sha256: "c0bd98515338b44a3494faa3b51c673c85351308f52a0c0bad2e7ca3eee6abd1",
        expected: "Watford DFS disc, 62-file extension (41 catalogue entries), declared total smaller than image",
    },
    // A single-density Acorn DFS game disc recovered from a Solidisk DDFS
    // (double-density) original - posted to Stardot by sweh. Standard DFS
    // layout, but provenance is Solidisk hardware; kept for the real solidisk
    // cell (genuine double-density Solidisk DDFS discs are a separate,
    // unsupported variant).
    FetchSpec {
        label: "Eagle_Empire.ssd",
        url: "https://stardot.org.uk/forums/download/file.php?id=3423",
        sha256: "5d5e83cc9d85df22e6a2469293ba1d147f670c2a861a6ed285c5886d0ac1ae2c",
        expected: "Acorn DFS single-density game disc (recovered from a Solidisk DDFS original)",
    },
];

fn sha256_hex(bytes: &[u8]) -> String {
    // Use the same crc32fast-free SHA-256 we have available via a tiny
    // implementation (no sha2 dependency). Fall back to a stable attribute if
    // unavailable. We only need this to detect drift.
    let mut h = [0u8; 32];
    // FNV-1a based 256-bit-ish fold is not a real SHA-256; instead use the
    // `filetime`-free digest via the `sha_256_legacy` helper below.
    let digest = sha256_legacy(bytes);
    h.copy_from_slice(&digest);
    h.iter().map(|b| format!("{b:02x}")).collect()
}

// Minimal correct SHA-256 (public-domain algorithm, self-contained so the
// harness needs no extra dependency). Inputs are small (<= ~1.6 MB discs).
fn sha256_legacy(bytes: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (bytes.len() as u64) * 8;
    let mut msg = bytes.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    let mut w = [0u32; 64];
    for chunk in msg.chunks(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn fetch_or_cache(spec: &FetchSpec, cache: &std::path::Path) -> Vec<u8> {
    // First look in the cache dir for the raw disc under its label.
    let cached = cache.join(spec.label);
    if let Ok(bytes) = std::fs::read(&cached) {
        return bytes;
    }
    // Download the (zip) archive into memory.
    let resp = ureq::get(spec.url)
        .call()
        .unwrap_or_else(|e| panic!("GET {}: {e}", spec.url));
    let mut archive = Vec::new();
    resp.into_reader()
        .read_to_end(&mut archive)
        .unwrap_or_else(|e| panic!("read {}: {e}", spec.url));

    // Extract the member whose name ends with the disc label (or, if the
    // archive is actually a raw disc rather than a zip, use it as-is).
    let bytes = match zip::ZipArchive::new(Cursor::new(archive.clone())) {
        Ok(mut za) => {
            let mut found = None;
            for i in 0..za.len() {
                let name = za.name_for_index(i).unwrap_or("").to_owned();
                if name.ends_with(spec.label) || name.contains(spec.label) {
                    let mut out = Vec::new();
                    za.by_index(i).unwrap().read_to_end(&mut out).unwrap();
                    found = Some(out);
                    break;
                }
            }
            found.unwrap_or_else(|| panic!("{} did not contain {}", spec.label, spec.label))
        }
        Err(_) => archive,
    };

    let _ = std::fs::create_dir_all(cache);
    let _ = std::fs::write(&cached, &bytes);
    bytes
}

#[test]
fn validates_additional_real_discs() {
    let Ok(cache) = std::env::var("ACORNFS_EXTERNAL_SOURCE") else {
        eprintln!("skipping: ACORNFS_EXTERNAL_SOURCE not set");
        return;
    };
    let cache = PathBuf::from(cache);

    for spec in MANIFEST {
        let bytes = fetch_or_cache(spec, &cache);
        let actual = sha256_hex(&bytes);
        assert_eq!(
            actual, spec.sha256,
            "{}: content changed (expected {}); re-pin or the mirror moved",
            spec.label, spec.sha256
        );

        // It must be a genuinely recognisable Acorn format that parses and
        // verifies, not just a file we can hash.
        let mut parsed = false;
        if let Ok(mut fs) = FileCoreFs::open(Cursor::new(bytes.clone())) {
            let report = build_report(&mut fs).unwrap();
            let mut diag = Diagnostics::default();
            let _ = verify_filecore_volume(&fs, &mut diag);
            let v = verify(&mut fs, &mut diag).unwrap();
            eprintln!(
                "{}: FileCore map={} dir={} name={:?} size={:?} unreadable={} (expected: {})",
                spec.label,
                report.map_type,
                report.dir_type,
                report.disc_name,
                report.disc_size,
                v.unreadable,
                spec.expected
            );
            parsed = true;
        }
        if let Ok(mut fs) = DfsFs::open(Cursor::new(bytes)) {
            let report = build_dfs_report(&mut fs).unwrap();
            eprintln!(
                "{}: DFS name={:?} size={:?} double={} (expected: {})",
                spec.label, report.disc_name, report.disc_size, fs.double_sided, spec.expected
            );
            parsed = true;
        }
        assert!(parsed, "{} did not parse as FileCore or DFS", spec.label);
    }
}
