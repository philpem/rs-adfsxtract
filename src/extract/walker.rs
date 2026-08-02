use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{FcError, Result};
use crate::extract::log::{ExtractionLog, LogEntry};
use crate::format::fs::FileSystem;
use crate::io::rescue::{BadSectorPolicy, RangeStatus, RescueMap};
use crate::model::object::Object;
use crate::sidecar::inf::{InfFields, build_inf_line};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokenDirPolicy {
    Fail,
    Skip,
    Recover,
}

pub struct ExtractOptions {
    pub output_dir: PathBuf,
    pub write_inf: bool,
    pub dry_run: bool,
    pub broken_dir_policy: BrokenDirPolicy,
    pub bad_sector_policy: BadSectorPolicy,
    pub rescue_map: Option<RescueMap>,
}

#[derive(Debug, Default)]
pub struct ExtractSummary {
    pub files_extracted: u64,
    pub files_skipped: u64,
    pub dirs_created: u64,
    pub total_bytes: u64,
}

/// Walks the object tree via the filesystem-agnostic `FileSystem` trait and
/// extracts it to `opts.output_dir`. Uses an explicit stack, not recursion,
/// so a directory cycle on a corrupt disc can't blow the call stack - it's
/// caught by the `visited` fingerprint set instead and logged.
pub fn walk_and_extract<FS: FileSystem>(
    fs: &mut FS,
    opts: &ExtractOptions,
    log: &mut ExtractionLog,
) -> Result<ExtractSummary> {
    let root = fs.root()?;
    let mut summary = ExtractSummary::default();
    let mut stack = vec![(root, opts.output_dir.clone(), "$".to_string())];
    let mut visited: HashSet<Vec<(u64, u64)>> = HashSet::new();

    while let Some((dir_obj, host_dir, riscos_path)) = stack.pop() {
        let fingerprint: Vec<(u64, u64)> = dir_obj.extents.iter().map(|e| (e.disc_addr, e.len)).collect();
        if !fingerprint.is_empty() && !visited.insert(fingerprint) {
            log.push(LogEntry::Warning {
                message: format!("directory cycle detected at {riscos_path}, skipping"),
            });
            continue;
        }

        if !opts.dry_run {
            std::fs::create_dir_all(&host_dir)?;
        }
        summary.dirs_created += 1;

        let listing = fs.list(&dir_obj)?;
        if listing.is_broken {
            match opts.broken_dir_policy {
                BrokenDirPolicy::Fail => {
                    return Err(FcError::BrokenDirectory(format!(
                        "{riscos_path}: {}",
                        listing.anomalies.join("; ")
                    )));
                }
                BrokenDirPolicy::Skip => {
                    log.push(LogEntry::BrokenDirectory {
                        path: riscos_path.clone(),
                        anomalies: listing.anomalies.clone(),
                        action: "skip".into(),
                    });
                    continue;
                }
                BrokenDirPolicy::Recover => {
                    log.push(LogEntry::BrokenDirectory {
                        path: riscos_path.clone(),
                        anomalies: listing.anomalies.clone(),
                        action: "recover".into(),
                    });
                }
            }
        } else {
            for a in &listing.anomalies {
                log.push(LogEntry::Warning { message: format!("{riscos_path}: {a}") });
            }
        }

        for obj in listing.objects {
            let riscos_child = format!("{riscos_path}.{}", obj.name);
            let host_leaf = build_host_leafname(&obj);
            let host_child = host_dir.join(&host_leaf);

            if obj.is_directory {
                stack.push((obj, host_child, riscos_child));
            } else {
                extract_file(fs, &obj, &host_child, &riscos_child, opts, log, &mut summary)?;
            }
        }
    }

    Ok(summary)
}

fn build_host_leafname(obj: &Object) -> String {
    let host = crate::xlate::dosfs::leafname_to_host(&obj.name);
    let le = obj.load_exec();
    crate::xlate::dosfs::append_filetype_suffix(&host, le.filetype, obj.is_directory)
}

fn extract_file<FS: FileSystem>(
    fs: &mut FS,
    obj: &Object,
    host_path: &Path,
    riscos_path: &str,
    opts: &ExtractOptions,
    log: &mut ExtractionLog,
    summary: &mut ExtractSummary,
) -> Result<()> {
    let total_len = obj.total_extent_len();

    let mut bad_ranges: Vec<(u64, u64)> = Vec::new();
    let mut good_bytes: u64 = 0;
    if let Some(map) = &opts.rescue_map {
        for extent in &obj.extents {
            for (start, len, status) in map.segments(extent.disc_addr, extent.len) {
                if status == RangeStatus::Bad {
                    bad_ranges.push((start, len));
                } else {
                    good_bytes += len;
                }
            }
        }
    } else {
        good_bytes = total_len;
    }

    if total_len > 0 && good_bytes == 0 {
        log.push(LogEntry::SkippedWhollyBad { path: riscos_path.to_string() });
        summary.files_skipped += 1;
        return Ok(());
    }

    if !bad_ranges.is_empty() {
        log.push(LogEntry::BadSectors {
            path: riscos_path.to_string(),
            ranges: bad_ranges,
            policy: format!("{:?}", opts.bad_sector_policy),
        });
    }

    if opts.dry_run {
        summary.files_extracted += 1;
        summary.total_bytes += total_len;
        return Ok(());
    }

    let mut out = File::create(host_path)?;
    let mut hasher = crc32fast::Hasher::new();
    let mut written: u64 = 0;
    let rescue_map = opts.rescue_map.as_ref();
    let policy = opts.bad_sector_policy;

    fs.read_object(obj, &mut |disc_addr, chunk| {
        if let Some(map) = rescue_map {
            let mut offset = 0usize;
            for (_seg_start, seg_len, status) in map.segments(disc_addr, chunk.len() as u64) {
                let seg_len = seg_len as usize;
                let slice = &chunk[offset..offset + seg_len];
                match (status, policy) {
                    (RangeStatus::Good, _) => {
                        out.write_all(slice).map_err(FcError::from)?;
                        hasher.update(slice);
                        written += seg_len as u64;
                    }
                    (RangeStatus::Bad, BadSectorPolicy::Skip) => {}
                    (RangeStatus::Bad, BadSectorPolicy::NullFill) => {
                        let zeros = vec![0u8; seg_len];
                        out.write_all(&zeros).map_err(FcError::from)?;
                        hasher.update(&zeros);
                        written += seg_len as u64;
                    }
                    (RangeStatus::Bad, BadSectorPolicy::MarkerFill) => {
                        let mut marker = vec![0u8; seg_len];
                        crate::io::rescue::fill_marker(&mut marker);
                        out.write_all(&marker).map_err(FcError::from)?;
                        hasher.update(&marker);
                        written += seg_len as u64;
                    }
                }
                offset += seg_len;
            }
        } else {
            out.write_all(chunk).map_err(FcError::from)?;
            hasher.update(chunk);
            written += chunk.len() as u64;
        }
        Ok(())
    })?;

    drop(out);

    if let Some(ts) = obj.load_exec().timestamp {
        let ft = filetime::FileTime::from_unix_time(ts.unix_secs, ts.nanos);
        let _ = filetime::set_file_mtime(host_path, ft);
    }

    if opts.write_inf {
        let crc = hasher.finalize();
        let le = obj.load_exec();
        let fields = InfFields {
            riscos_name: &obj.name,
            load: obj.load,
            exec: obj.exec,
            length: written,
            attrs: obj.attrs,
            crc32: Some(crc),
            datetime_unix_secs: le.timestamp.map(|t| t.unix_secs),
        };
        let inf_path = PathBuf::from(format!("{}.inf", host_path.display()));
        let mut inf_file = File::create(&inf_path)?;
        writeln!(inf_file, "{}", build_inf_line(&fields))?;
    }

    log.push(LogEntry::Extracted { path: riscos_path.to_string(), bytes: written });
    summary.files_extracted += 1;
    summary.total_bytes += written;
    Ok(())
}
