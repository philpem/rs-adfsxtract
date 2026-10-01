//! Shared plumbing for the opt-in real-media harnesses (`afs_real_media_tests`
//! and `external_real_media_tests`).
//!
//! Local development fetches the genuine discs from their sources at run time
//! and caches them; CI should not poll the forums/mirrors, so the GitHub
//! Actions workflow sets `ACORNFS_CI=1`, which disables the network fetch
//! (a missing file is then skipped rather than downloaded). Hashing uses the
//! `sha2` crate rather than a hand-rolled digest.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// A disc to validate: how to find it on disk, and where to fetch it from if
/// local development and the file is not cached.
pub struct Spec<'a> {
    pub label: &'a str,
    pub url: &'a str,
    /// Whether the disc is inside a downloadable zip (so the fetch instructions
    /// mention extracting it) versus a raw file.
    pub in_zip: bool,
}

/// SHA-256 hex digest, via the `sha2` crate.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Whether network fetches are disabled (we are running as CI). The GitHub
/// Actions workflow sets `ACORNFS_CI=1` so CI never polls a forum or mirror.
pub fn network_disabled() -> bool {
    std::env::var("ACORNFS_CI")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Returns the disc bytes, or `None` only when network is disabled **and** the
/// file is not already cached (the caller skips that entry). With network
/// enabled, a missing file is downloaded (extracting the member from a zip if
/// necessary) and written to `cache`/`label` before being returned.
pub fn obtain(spec: &Spec, cache: &Path) -> Option<Vec<u8>> {
    let path = cache.join(spec.label);
    if let Ok(bytes) = std::fs::read(&path) {
        return Some(bytes);
    }
    if network_disabled() {
        return None;
    }
    // Fetch and cache. A transient forum/mirror failure (or a source that has
    // moved) is non-fatal: report the manual download and skip, rather than
    // failing a local run.
    let bytes = match fetch(spec) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("could not fetch {}: {e}", spec.label);
            eprintln!("  {}", instruction(spec, cache));
            return None;
        }
    };

    let _ = std::fs::create_dir_all(cache);
    let _ = std::fs::write(&path, &bytes);
    Some(bytes)
}

fn fetch(spec: &Spec) -> std::result::Result<Vec<u8>, String> {
    let resp = ureq::get(spec.url)
        .set("User-Agent", "acornfsextract-test")
        .call()
        .map_err(|e| e.to_string())?;
    let mut archive = Vec::new();
    resp.into_reader()
        .read_to_end(&mut archive)
        .map_err(|e| e.to_string())?;

    match zip::ZipArchive::new(std::io::Cursor::new(archive.clone())) {
        Ok(mut za) => {
            for i in 0..za.len() {
                let name = za.name_for_index(i).unwrap_or("").to_owned();
                if name.ends_with(spec.label) || name.contains(spec.label) {
                    let mut out = Vec::new();
                    za.by_index(i)
                        .unwrap()
                        .read_to_end(&mut out)
                        .map_err(|e| e.to_string())?;
                    return Ok(out);
                }
            }
            Err(format!("{} did not contain '{}'", spec.url, spec.label))
        }
        Err(_) => Ok(archive),
    }
}

/// Human-readable instruction for downloading a disc manually.
pub fn instruction(spec: &Spec, cache: &Path) -> String {
    let dest = cache.join(spec.label);
    if spec.in_zip {
        format!(
            "download {} and extract '{}' to {}",
            spec.url,
            spec.label,
            dest.display()
        )
    } else {
        format!("download {} to {}", spec.url, dest.display())
    }
}
