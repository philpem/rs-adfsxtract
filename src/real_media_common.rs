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

/// Whether a real_media bundle ZIP is the approved source for every image.
/// The GitHub Actions workflow downloads a single bundle (hosted on the
/// maintainer's site) rather than polling each forum/mirror, so CI only ever
/// makes one network request.
///
/// If a `real-media.zip` is present in the cache its members are extracted
/// (see [`ensure_bundle_extracted`]); local development still falls back to
/// per-source fetching when the bundle is absent.
pub fn ensure_bundle_extracted(cache: &Path) -> bool {
    let zip_path = cache.join("real-media.zip");
    if !zip_path.exists() {
        return false;
    }
    // Always re-extract (idempotent overwrite). The workflow re-downloads the
    // bundle fresh each run, so the stale `.bundle_extracted` marker that a
    // persisted cache used to smuggle in is gone - a changed bundle is always
    // unpacked rather than silently skipped.
    (|| -> std::option::Option<()> {
        let f = std::fs::File::open(&zip_path).ok()?;
        let mut za = zip::ZipArchive::new(f).ok()?;
        for i in 0..za.len() {
            let name = za.name_for_index(i).unwrap_or("").to_owned();
            if name.is_empty() || name.ends_with('/') {
                continue;
            }
            let mut out = Vec::new();
            za.by_index(i).unwrap().read_to_end(&mut out).ok()?;
            let _ = std::fs::write(cache.join(&name), &out);
        }
        Some(())
    })()
    .is_some()
}

/// Extract the bundle at most once per process (see [`ensure_bundle_extracted`]);
/// a no-op when no bundle is present.
fn extract_bundle_once(cache: &Path) {
    static EXTRACTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    EXTRACTED.get_or_init(|| {
        let _ = ensure_bundle_extracted(cache);
    });
}

/// Returns the disc bytes, or `None` only when network is disabled (CI) and
/// the file is not available from the cache or bundle - the caller silently
/// skips that entry. With network enabled, a missing file is fetched per
/// source and written to `cache`/`label` before being returned.
pub fn obtain(spec: &Spec, cache: &Path) -> Option<Vec<u8>> {
    // If the hosted bundle is present, unpack it before any lookup so its
    // members always take precedence over (and overwrite) stale files from a
    // reused cache directory. Gated to once per process.
    extract_bundle_once(cache);

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

/// Obtain a disc, printing its human-readable description so the workflow log
/// shows what is being validated. Under CI (network disabled) a disc that is
/// not present in the real-media bundle is **fatal**: the bundle is incomplete
/// and the job must fail. Locally a failed fetch is non-fatal - it is logged
/// and `None` is returned so the caller can skip that entry.
pub fn obtain_disc(spec: &Spec, cache: &Path, description: &str) -> Option<Vec<u8>> {
    eprintln!("validating {}", spec.label);
    eprintln!("  {}", description);
    let bytes = obtain(spec, cache);
    if bytes.is_some() {
        return bytes;
    }
    if network_disabled() {
        panic!(
            "{}: disc is not present in the hosted real-media bundle (ACORNFS_EXTERNAL_SOURCE={}); regenerate real-media.zip",
            spec.label,
            cache.display()
        );
    }
    eprintln!("  skipping: could not obtain (see manual-download instruction above)");
    None
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
