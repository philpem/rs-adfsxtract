//! Optional stress harness against the Arcology corpus. Exercises the
//! extractor against many real disc images to shake out crashes/regressions
//! that a handful of validated fixtures never will.
//!
//! Skipped entirely unless `ACORNFS_ARCOLOGY_KEY` is set; the in-repo
//! `reference_media_tests` never needs it. The corpus images are
//! copyrighted, so they are not committed - only this key-gated test pulls
//! them at runtime.
//!
//! Env vars:
//! - `ACORNFS_ARCOLOGY_KEY`  - required; API key (sent via `X-API-Key`).
//! - `ACORNFS_ARCOLOGY_API`  - base URL, default `https://arco-staging.philpem.me.uk/api`.
//! - `ACORNFS_ARCOLOGY_MAX`  - cap on artefacts processed (default 200).

use std::fmt::Write as _;
use std::io::{Cursor, Read};

use serde::Deserialize;

use crate::diagnostics::Diagnostics;
use crate::extract::report::build_report;
use crate::format::filecore::FileCoreFs;
use crate::verify::{verify, verify_filecore_volume};

const DEFAULT_BASE: &str = "https://arco-staging.philpem.me.uk/api";
const DEFAULT_MAX: u64 = 200;
const PAGE_SIZE: u64 = 100;

const DISC_EXTENSIONS: &[&str] = &["adf", "adl", "adf", "dd", "img", "hdf", "flp", "ssd", "dsd"];

#[derive(Deserialize)]
struct ItemsPage {
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    uuid: String,
}

#[derive(Deserialize)]
struct Artefact {
    uuid: String,
    original_filename: String,
    #[serde(default)]
    is_restricted: bool,
    #[serde(default)]
    artefact_type: String,
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

fn is_disc_image(filename: &str) -> bool {
    let ext = filename.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    DISC_EXTENSIONS.contains(&ext.as_str())
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

    let mut candidates = 0usize;
    let mut not_filecore = 0usize;
    let mut clean = 0usize;
    let mut faults = 0usize;
    let mut summaries = Vec::new();

    'outer: for page in 1.. {
        let items: ItemsPage =
            api_get_json(&base, &key, &format!("/items?page={page}&per_page={PAGE_SIZE}"))
                .unwrap_or_else(|e| panic!("list items page {page}: {e}"));
        for item in items.items {
            let artefacts: Vec<Artefact> =
                api_get_json(&base, &key, &format!("/items/{}/artefacts", item.uuid))
                    .unwrap_or_else(|e| panic!("list artefacts for {}: {e}", item.uuid));
            for art in artefacts {
                if art.is_restricted || !is_disc_image(&art.original_filename) {
                    continue;
                }
                let label = format!("{}/{}", art.original_filename, art.artefact_type);
                let data = api_get_bytes(&base, &key, &format!("/artefacts/{}/download", art.uuid))
                    .unwrap_or_else(|e| panic!("download {label}: {e}"));
                candidates += 1;

                let mut summary = format!("{label}: ");
                let status = if let Ok(mut fs) = FileCoreFs::open(Cursor::new(data.to_vec())) {
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
        "arcology stress: candidates={candidates} clean={clean} has_faults={faults} non_filecore={not_filecore}"
    );
    for s in &summaries {
        eprint!("{s}");
    }
    assert!(candidates > 0, "arcology stress ran but found no disc-image artefacts");
}
