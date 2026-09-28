//! `verify` implementation: a read-only structural walk that reports typed
//! `Fault`s instead of aborting or silently skipping. It counts what it
//! could not read and why, for damaged-media triage. Unlike `extract`, it
//! does not write anything and applies no recovery policy beyond "report".

use std::collections::HashSet;

use crate::diagnostics::{Diagnostic, Diagnostics, Fault};
use crate::error::Result;
use crate::format::filecore::FileCoreFs;
use crate::format::fs::FileSystem;
use crate::io::SectorSource;
use crate::model::object::Object;

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub directories: usize,
    pub files: usize,
    pub unreadable: usize,
}

/// Walks the tree via the `FileSystem` trait, counting directories and files
/// and recording any that cannot be read or whose directory structure is
/// broken. An explicit stack plus an extent fingerprint set guards against a
/// directory cycle on a corrupt disc.
pub fn verify<FS: FileSystem>(fs: &mut FS, diag: &mut Diagnostics) -> Result<VerifyReport> {
    let mut report = VerifyReport::default();
    let mut stack = vec![(fs.root()?, "$".to_string())];
    let mut visited: HashSet<Vec<(u64, u64)>> = HashSet::new();

    while let Some((dir_obj, riscos_path)) = stack.pop() {
        let fingerprint: Vec<(u64, u64)> = dir_obj
            .extents
            .iter()
            .map(|e| (e.disc_addr, e.len))
            .collect();
        if !fingerprint.is_empty() && !visited.insert(fingerprint) {
            diag.push(Diagnostic::from_fault(Fault::DirectoryCycle).at_path(&riscos_path));
            report.unreadable += 1;
            continue;
        }
        report.directories += 1;

        let listing = match fs.list(&dir_obj) {
            Ok(l) => l,
            Err(e) => {
                diag.push(
                    Diagnostic::from_fault(Fault::UnreadableDirectory {
                        details: e.to_string(),
                    })
                    .at_path(&riscos_path),
                );
                report.unreadable += 1;
                continue;
            }
        };

        if listing.is_broken {
            diag.push(
                Diagnostic::from_fault(Fault::BrokenDirectory {
                    details: listing.anomalies.join("; "),
                })
                .at_path(&riscos_path),
            );
            report.unreadable += 1;
        }

        for obj in listing.objects {
            let child_path = format!("{riscos_path}.{}", obj.name);
            if obj.is_directory {
                stack.push((obj, child_path));
            } else {
                report.files += 1;
                verify_file(&mut *fs, &obj, &child_path, &mut *diag, &mut report)?;
            }
        }
    }
    Ok(report)
}

fn verify_file<FS: FileSystem>(
    fs: &mut FS,
    obj: &Object,
    path: &str,
    diag: &mut Diagnostics,
    report: &mut VerifyReport,
) -> Result<()> {
    if obj.extents.is_empty() && obj.length > 0 {
        diag.push(
            Diagnostic::from_fault(Fault::UnreadableFile {
                details: "no disc extents resolved (missing fragment?)".into(),
            })
            .at_path(path),
        );
        report.unreadable += 1;
        return Ok(());
    }
    // A file with valid extents is only unreadable if the bytes cannot be
    // read back (e.g. an extent falls outside the image). Streaming the
    // read verifies that without retaining the data.
    match fs.read_object(obj, &mut |_disc_addr, _bytes| Ok(())) {
        Ok(()) => Ok(()),
        Err(e) => {
            diag.push(
                Diagnostic::from_fault(Fault::UnreadableFile {
                    details: e.to_string(),
                })
                .at_path(path),
            );
            report.unreadable += 1;
            Ok(())
        }
    }
}

/// Volume-level structural health for FileCore media. The generic tree walk
/// above cannot see the zone map or boot block, so these are checked here
/// against the decoded data `FileCoreFs::open` already gathered.
pub fn verify_filecore_volume<S: SectorSource>(
    fs: &FileCoreFs<S>,
    diag: &mut Diagnostics,
) -> Result<()> {
    if let Some(new_map) = &fs.new_map {
        for (zone, &ok) in new_map.zone_check_ok.iter().enumerate() {
            if !ok {
                diag.push(Diagnostic::from_fault(Fault::ZoneChecksumMismatch { zone }));
            }
        }
        if new_map.cross_check_xor != 0xFF {
            diag.push(Diagnostic::from_fault(Fault::ZoneCrossCheckMismatch {
                actual: new_map.cross_check_xor,
                expected: 0xFF,
            }));
        }
        if let Some(dr) = &fs.disc_record {
            for (field, boot, zone0) in dr.geometry_mismatches(&new_map.zone0_disc_record) {
                diag.push(Diagnostic::from_fault(Fault::DiscRecordGeometryMismatch {
                    field: field.to_string(),
                    boot,
                    zone0,
                }));
            }
        }
    }
    if let Some(bb) = &fs.boot_block {
        let stored = bb.raw[0x1FF];
        let computed = crate::format::filecore::checksums::boot_block_checksum(&bb.raw);
        if stored != computed {
            diag.push(Diagnostic::from_fault(Fault::BootBlockChecksumMismatch {
                stored,
                computed,
            }));
        }
    }
    Ok(())
}
