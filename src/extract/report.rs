//! Disc metadata gathering, shared by the `info` and `extract` subcommands.
//! Deliberately structured output (not free text to grep), so a future
//! worker patch can parse fields instead of matching stdout substrings the
//! way it currently does against DIM's `report` output.

use serde::Serialize;

use crate::error::Result;
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
}

pub fn build_report<S: SectorSource>(fs: &mut FileCoreFs<S>) -> Result<DiscReport> {
    let map_type = fs.map_type;
    let dir_type = fs.dir_type;

    let (disc_name, disc_id, disc_size, sector_size, boot_option) = match map_type {
        MapType::Old => {
            let m = fs.old_free_space_map()?;
            (
                Some(m.disc_name),
                Some(m.disc_id as u32),
                Some(m.total_sectors as u64 * 256),
                None,
                Some(m.boot_option),
            )
        }
        MapType::New => {
            let dr = fs
                .new_map
                .as_ref()
                .map(|m| &m.zone0_disc_record)
                .or(fs.disc_record.as_ref())
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
    let root_list = fs.list(&root)?;

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
        lines.push(format!("Boot block:      {}", if self.boot_block_present { "present" } else { "absent" }));
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
        lines.join("\n")
    }
}
