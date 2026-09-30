//! Disc metadata gathering, shared by the `info` and `extract` subcommands.
//! Deliberately structured output (not free text to grep), so a future
//! worker patch can parse fields instead of matching stdout substrings the
//! way it currently does against DIM's `report` output.

use serde::Serialize;

use crate::error::Result;
use crate::format::dfs::DfsFs;
use crate::format::filecore::{DirType, FileCoreFs, MapType};
use crate::format::fs::FileSystem;
use crate::io::SectorSource;

#[derive(Debug, Clone, Serialize)]
pub struct DiscReport {
    pub filesystem: &'static str,
    pub map_type: &'static str,
    pub dir_type: &'static str,
    pub disc_name: Option<String>,
    pub disc_id: Option<u32>,
    pub disc_size: Option<u64>,
    pub sector_size: Option<u32>,
    pub boot_option: Option<u8>,
    pub root_title: String,
    pub boot_block_present: bool,
    pub boot_block_checksum_ok: Option<bool>,
    pub zone_checksum_ok_count: Option<usize>,
    pub zone_checksum_total: Option<usize>,
    pub cross_check_ok: Option<bool>,
    pub root_check_byte_ok: bool,
    /// RISC OS path (e.g. `$.!Calendar`) of every directory whose listing
    /// is structurally broken, found by walking the whole tree - ADFS/FSCK
    /// report these as "broken directory" while the volume-level checks
    /// above can pass.
    pub broken_directories: Vec<String>,
    /// Non-fatal directory quirks (`<path>: <warning>`), e.g. entries out of
    /// collation order or a wrong tail NewDirParent. ADFS/FSCK flag these
    /// too, but they don't stop a best-effort read.
    pub directory_warnings: Vec<String>,
}

pub fn build_report<S: SectorSource>(fs: &mut FileCoreFs<S>) -> Result<DiscReport> {
    let map_type = fs.map_type;
    let dir_type = fs.dir_type;

    let (disc_name, disc_id, disc_size, sector_size, boot_option) = match map_type {
        MapType::Old => {
            let m = fs.old_free_space_map()?;
            // S/M/L floppies record 640/1280/2560 total sectors; an old-map
            // hard disc records a chunk/cylinder count that is NOT the image
            // size, so for those report the actual image length.
            let total = m.total_sectors as u64;
            let disc_size = if matches!(total, 640 | 1280 | 2560) {
                Some(total * 256)
            } else {
                Some(fs.total_bytes()?)
            };
            (
                Some(m.disc_name),
                Some(m.disc_id as u32),
                disc_size,
                None,
                Some(m.boot_option),
            )
        }
        MapType::New => {
            // Prefer the boot-block disc record (authoritative for geometry),
            // which open() has already had the zone-0 *identity* fields merged
            // into it. The raw `zone0_disc_record` may be a zeroed/unreliable
            // copy (a disc whose zone map sits elsewhere); only fall back to it
            // when no boot-block record exists.
            let dr = fs
                .disc_record
                .as_ref()
                .or_else(|| fs.new_map.as_ref().map(|m| &m.zone0_disc_record))
                .expect("new-map FileCoreFs always has a disc record");
            (
                Some(dr.disc_name_str()),
                Some(dr.disc_id as u32),
                Some(dr.disc_size_bytes()),
                Some(dr.sector_size()),
                Some(dr.boot_option),
            )
        }
    };

    let boot_block_present = fs.boot_block.is_some();
    let boot_block_checksum_ok = fs.boot_block.as_ref().map(|b| b.checksum_ok);
    let (zone_checksum_ok_count, zone_checksum_total, cross_check_ok) = match &fs.new_map {
        Some(nm) => (
            Some(nm.zone_check_ok.iter().filter(|&&ok| ok).count()),
            Some(nm.zone_check_ok.len()),
            Some(nm.cross_check_xor == 0xFF),
        ),
        None => (None, None, None),
    };

    let root = fs.root()?;
    let root_list = fs.list_with_parent(&root, root.sin)?;
    let (broken_directories, directory_warnings) = collect_directory_health(fs);

    Ok(DiscReport {
        filesystem: "FileCore",
        map_type: match map_type {
            MapType::Old => "old",
            MapType::New => "new",
        },
        dir_type: match dir_type {
            DirType::Old => "old",
            DirType::New => "new",
            DirType::Big => "big",
        },
        disc_name,
        disc_id,
        disc_size,
        sector_size,
        boot_option,
        root_title: root_list.title,
        boot_block_present,
        boot_block_checksum_ok,
        zone_checksum_ok_count,
        zone_checksum_total,
        cross_check_ok,
        root_check_byte_ok: !root_list.is_broken,
        broken_directories,
        directory_warnings,
    })
}

/// Walks the whole tree listing each directory, collecting (a) the RISC OS
/// path of any structurally-broken directory and (b) any non-fatal quirk as
/// `<path>: <warning>`. This is what lets `info` surface a problem in a
/// non-root directory (e.g. `$.!Calendar`) that has nothing to do with the
/// volume-level checks, matching what ADFS/FSCK report on mount. An explicit
/// stack plus an extent fingerprint set guards against a directory cycle on
/// a corrupt disc.
fn collect_directory_health<S: SectorSource>(fs: &mut FileCoreFs<S>) -> (Vec<String>, Vec<String>) {
    use crate::format::fs::FileSystem;
    use std::collections::HashSet;

    let mut broken = Vec::new();
    let mut warnings = Vec::new();
    let Ok(root) = fs.root() else {
        return (broken, warnings);
    };
    let root_sin = root.sin;
    let mut stack = vec![(root, "$".to_string(), root_sin)];
    let mut visited: HashSet<Vec<(u64, u64)>> = HashSet::new();

    while let Some((dir_obj, path, parent_sin)) = stack.pop() {
        let fingerprint: Vec<(u64, u64)> = dir_obj
            .extents
            .iter()
            .map(|e| (e.disc_addr, e.len))
            .collect();
        if !fingerprint.is_empty() && !visited.insert(fingerprint) {
            continue;
        }
        let Ok(listing) = fs.list_with_parent(&dir_obj, parent_sin) else {
            broken.push(path.clone());
            continue;
        };
        if listing.is_broken {
            broken.push(path.clone());
        }
        for w in &listing.warnings {
            warnings.push(format!("{path}: {w}"));
        }
        for obj in listing.objects {
            let child_path = format!("{path}.{}", obj.name);
            if obj.is_directory {
                stack.push((obj, child_path, dir_obj.sin));
            }
        }
    }
    (broken, warnings)
}

/// DFS has no map/directory-type distinction and no checksum of any kind,
/// so most of the structural-health fields are simply absent rather than a
/// placeholder value. `dir_type` is repurposed to carry sidedness, since
/// that's the one piece of DFS-specific structure worth surfacing here.
pub fn build_dfs_report<S: SectorSource>(fs: &mut DfsFs<S>) -> Result<DiscReport> {
    let cat0 = &fs.catalogues[0];
    let disc_name = cat0.title.clone();
    let boot_option = cat0.boot_option;
    let total_sectors: u64 = fs.catalogues.iter().map(|c| c.total_sectors as u64).sum();

    let root = fs.root()?;
    let root_list = fs.list_with_parent(&root, root.sin)?;

    // The catalogue's `total_sectors` is advisory and unreliable on real media
    // (Watford discs record a smaller value than the physical image). Surface
    // a non-fatal warning when it diverges from the image size, rather than
    // silently accepting it - the divergence is why file bounds are checked
    // against the image length instead of this value.
    let declared_bytes = total_sectors * 256;
    let mut directory_warnings = Vec::new();
    if declared_bytes != fs.image_len {
        directory_warnings.push(format!(
            "catalogue total_sectors ({total_sectors} sectors / {declared_bytes} bytes) \
             diverges from the image size ({} bytes)",
            fs.image_len
        ));
    }

    Ok(DiscReport {
        filesystem: "DFS",
        map_type: "n/a",
        dir_type: if fs.double_sided {
            "double-sided"
        } else {
            "single-sided"
        },
        disc_name: Some(disc_name),
        disc_id: None,
        disc_size: Some(total_sectors * 256),
        sector_size: Some(256),
        boot_option: Some(boot_option),
        root_title: root_list.title,
        boot_block_present: false,
        boot_block_checksum_ok: None,
        zone_checksum_ok_count: None,
        zone_checksum_total: None,
        cross_check_ok: None,
        root_check_byte_ok: !root_list.is_broken,
        broken_directories: Vec::new(),
        directory_warnings,
    })
}

impl DiscReport {
    pub fn to_text(&self) -> String {
        let mut lines = vec![
            format!("Filesystem:      {}", self.filesystem),
            format!("Map type:        {}", self.map_type),
            format!("Directory type:  {}", self.dir_type),
        ];
        if let Some(n) = &self.disc_name {
            lines.push(format!("Disc name:       {n}"));
        }
        if let Some(id) = self.disc_id {
            lines.push(format!("Disc id:         {id:#06x}"));
        }
        if let Some(sz) = self.disc_size {
            lines.push(format!("Disc size:       {sz} bytes"));
        }
        if let Some(ss) = self.sector_size {
            lines.push(format!("Sector size:     {ss} bytes"));
        }
        if let Some(bo) = self.boot_option {
            lines.push(format!("Boot option:     {bo}"));
        }
        lines.push(format!("Root title:      {}", self.root_title));
        lines.push(format!(
            "Boot block:      {}",
            if self.boot_block_present {
                "present"
            } else {
                "absent"
            }
        ));
        if let Some(ok) = self.boot_block_checksum_ok {
            lines.push(format!("Boot block ok:   {ok}"));
        }
        if let (Some(ok), Some(total)) = (self.zone_checksum_ok_count, self.zone_checksum_total) {
            lines.push(format!("Zone checksums:  {ok}/{total} ok"));
        }
        if let Some(ok) = self.cross_check_ok {
            lines.push(format!("Cross-check ok:  {ok}"));
        }
        lines.push(format!("Root dir ok:     {}", self.root_check_byte_ok));
        lines.push(format!(
            "Broken dirs:     {}",
            if self.broken_directories.is_empty() {
                "none".to_string()
            } else {
                self.broken_directories.join(", ")
            }
        ));
        lines.push(format!(
            "Dir warnings:    {}",
            if self.directory_warnings.is_empty() {
                "none".to_string()
            } else {
                self.directory_warnings.join("; ")
            }
        ));
        lines.join("\n")
    }
}
