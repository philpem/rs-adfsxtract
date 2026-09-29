//! Optional stress harness against the Arcology corpus. Exercises the
//! extractor against many real disc images to shake out crashes/regressions
//! that a handful of validated fixtures never will.
//!
//! Skipped entirely unless `ACORNFS_ARCOLOGY_KEY` is set; the in-repo
//! `reference_media_tests` never needs it. The corpus images are
//! copyrighted, so they are not committed - only this key-gated test pulls
//! them at runtime.
//!
//! The corpus stores disc images as zstd-compressed raw-sector files and
//! attaches artefacts to the item record, so this fetches the item detail
//! (which embeds artefacts), filters to FileCore disc-image names (an .adf /
//! .adl / .dd / .img / .hdf, possibly with a trailing .zst), decompresses
//! .zst, and runs open/verify on each.
//!
//! Env vars:
//! - `ACORNFS_ARCOLOGY_KEY`    - required; API key (sent via `X-API-Key`).
//! - `ACORNFS_ARCOLOGY_API`    - base URL, default `https://arco-staging.philpem.me.uk/api`.
//! - `ACORNFS_ARCOLOGY_MAX`    - cap on artefacts processed (default 15, a sample).
//! - `ACORNFS_ARCOLOGY_EXT`    - comma-separated extensions to sample
//!   (default `adf,adl,dd,img,hdf` - the FileCore images this tool targets).
//! - `ACORNFS_ARCOLOGY_MAXSIZE` - skip artefacts larger than this many bytes
//!   (default 2147483648 = 2 GiB) to avoid OOM on very large dumps.

use std::fmt::Write as _;
use std::io::{Cursor, Read};

use serde::Deserialize;

use crate::diagnostics::Diagnostics;
use crate::extract::report::build_report;
use crate::format::filecore::FileCoreFs;
use crate::verify::{verify, verify_filecore_volume};

const DEFAULT_BASE: &str = "https://arco-staging.philpem.me.uk/api";
const DEFAULT_MAX: u64 = 15;
const DEFAULT_MAXSIZE: u64 = 2 * 1024 * 1024 * 1024;
const PAGE_SIZE: u64 = 100;
const DEFAULT_EXT: &[&str] = &["adf", "adl", "dd", "img", "hdf"];

#[derive(Deserialize)]
struct ItemsPage {
    items: Vec<ItemRef>,
}

#[derive(Deserialize)]
struct ItemRef {
    uuid: String,
}

#[derive(Deserialize)]
struct ItemDetail {
    artefacts: Vec<Artefact>,
}

#[derive(Deserialize)]
struct Artefact {
    uuid: String,
    original_filename: String,
    #[serde(default)]
    is_restricted: bool,
    #[serde(default)]
    file_size: u64,
}

fn api_get_json<T: serde::de::DeserializeOwned>(base: &str, key: &str, path: &str) -> Result<T, String> {
    let url = format!("{base}{path}");
    let resp = ureq::get(&url)
        .set("X-API-Key", key)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    resp.into_json::<T>().map_err(|e| format!("JSON decode {url}: {e}"))
}

fn api_get_bytes(base: &str, key: &str, path: &str) -> Result<Vec<u8>, String> {
    let url = format!("{base}{path}");
    let resp = ureq::get(&url)
        .set("X-API-Key", key)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read {url}: {e}"))?;
    Ok(bytes)
}

fn sample_extensions() -> Vec<String> {
    match std::env::var("ACORNFS_ARCOLOGY_EXT") {
        Ok(v) => v
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
        Err(_) => DEFAULT_EXT.iter().map(|s| s.to_string()).collect(),
    }
}

/// A FileCore disc image is one of the supported extensions, optionally
/// zstd-compressed (`.zst`). Returns (is_image, needs_decompress).
fn classify_image(filename: &str, exts: &[String]) -> Option<bool> {
    let mut name = filename.to_ascii_lowercase();
    let compressed = name.ends_with(".zst");
    if compressed {
        name = name[..name.len() - 4].to_string();
    }
    let ext = name.rsplit('.').next().unwrap_or("");
    if exts.iter().any(|e| e == ext) {
        Some(compressed)
    } else {
        None
    }
}

fn zstd_decode(data: &[u8], label: &str) -> Result<Vec<u8>, String> {
    let mut dec = ruzstd::StreamingDecoder::new(Cursor::new(data))
        .map_err(|e| format!("zstd init {label}: {e}"))?;
    let mut out = Vec::new();
    dec.read_to_end(&mut out).map_err(|e| format!("zstd decode {label}: {e}"))?;
    Ok(out)
}

#[test]
fn stress_against_arcology_corpus() {
    let Ok(key) = std::env::var("ACORNFS_ARCOLOGY_KEY") else {
        eprintln!("skipping: ACORNFS_ARCOLOGY_KEY not set");
        return;
    };
    if key.trim().is_empty() {
        eprintln!("skipping: ACORNFS_ARCOLOGY_KEY is empty");
        return;
    }
    let base = std::env::var("ACORNFS_ARCOLOGY_API").unwrap_or_else(|_| DEFAULT_BASE.into());
    let max = std::env::var("ACORNFS_ARCOLOGY_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX);
    let max_size = std::env::var("ACORNFS_ARCOLOGY_MAXSIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAXSIZE);
    let exts = sample_extensions();

    let mut candidates = 0usize;
    let mut not_filecore = 0usize;
    let mut clean = 0usize;
    let mut faults = 0usize;
    let mut too_big = 0usize;
    let mut summaries = Vec::new();

    'outer: for page in 1.. {
        let items: ItemsPage =
            api_get_json(&base, &key, &format!("/items?page={page}&per_page={PAGE_SIZE}"))
                .unwrap_or_else(|e| panic!("list items page {page}: {e}"));
        for item in items.items {
            let detail: ItemDetail =
                api_get_json(&base, &key, &format!("/items/{}", item.uuid))
                    .unwrap_or_else(|e| panic!("get item {}: {e}", item.uuid));
            for art in detail.artefacts {
                let Some(needs_decompress) = classify_image(&art.original_filename, &exts) else {
                    continue;
                };
                if art.is_restricted {
                    continue;
                }
                if art.file_size > max_size {
                    too_big += 1;
                    continue;
                }
                let label = art.original_filename.clone();
                let raw = api_get_bytes(&base, &key, &format!("/artefacts/{}/download", art.uuid))
                    .unwrap_or_else(|e| panic!("download {label}: {e}"));
                let data = if needs_decompress {
                    zstd_decode(&raw, &label).unwrap_or_else(|e| panic!("{e}"))
                } else {
                    raw
                };
                candidates += 1;

                let mut summary = format!("{label}: ");
                let status = if let Ok(mut fs) = FileCoreFs::open(Cursor::new(data)) {
                    let _ = build_report(&mut fs).map(|r| {
                        let _ = write!(
                            summary,
                            "map={} dir={} name={:?} size={:?} ",
                            r.map_type, r.dir_type, r.disc_name, r.disc_size
                        );
                    });
                    let mut diag = Diagnostics::default();
                    let _ = verify_filecore_volume(&fs, &mut diag);
                    let unreadable = verify(&mut fs, &mut diag).map(|v| v.unreadable).unwrap_or(usize::MAX);
                    let _ = write!(summary, "unreadable={unreadable}");
                    if diag.is_empty() && unreadable == 0 {
                        clean += 1;
                        "clean".to_string()
                    } else {
                        faults += 1;
                        "HAS_FAULTS".to_string()
                    }
                } else {
                    not_filecore += 1;
                    "not_filecore".to_string()
                };
                let _ = writeln!(summary, " [{status}]");
                summaries.push(summary);

                if candidates as u64 >= max {
                    break 'outer;
                }
            }
        }
    }

    eprintln!(
        "arcology stress: candidates={candidates} clean={clean} has_faults={faults} non_filecore={not_filecore} too_big={too_big}"
    );
    for s in &summaries {
        eprint!("{s}");
    }
    assert!(candidates > 0, "arcology stress ran but found no disc-image artefacts");
}
