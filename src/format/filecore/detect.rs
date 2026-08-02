//! Format identification (guide §1): map type, directory type, and (for
//! old-map discs, where the root directory address is fixed rather than
//! derived from a disc record) its location.

use crate::error::{FcError, Result};
use crate::format::filecore::disc_record::{
    BOOT_BLOCK_ADDR, BootBlock, DISC_RECORD_SIZE, DiscRecord, looks_plausible, parse_boot_block,
    parse_disc_record,
};
use crate::io::SectorSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapType {
    Old,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirType {
    Old,
    New,
    Big,
}

#[derive(Debug, Clone)]
pub struct Detection {
    pub map_type: MapType,
    pub dir_type: DirType,
    /// `None` only for old-map discs, which have no disc record structure.
    pub disc_record: Option<DiscRecord>,
    /// Fixed root directory address for old-map discs (`0x200` for S/M/L,
    /// `0x400` for D). New-map root directory addresses need the zone map
    /// resolved first (guide §2.3) and are not computed here.
    pub old_map_root_dir_addr: Option<u64>,
    pub boot_block: Option<BootBlock>,
}

const OLD_DIR_SML_ADDR: u64 = 0x200;
const OLD_DIR_D_ADDR: u64 = 0x400;

fn has_signature(buf: &[u8]) -> bool {
    &buf[1..5] == b"Hugo" || &buf[1..5] == b"Nick"
}

pub fn detect(source: &mut dyn SectorSource) -> Result<Detection> {
    let mut buf5 = [0u8; 5];

    source.read_at(OLD_DIR_SML_ADDR, &mut buf5)?;
    if has_signature(&buf5) {
        return Ok(Detection {
            map_type: MapType::Old,
            dir_type: DirType::Old,
            disc_record: None,
            old_map_root_dir_addr: Some(OLD_DIR_SML_ADDR),
            boot_block: None,
        });
    }

    source.read_at(OLD_DIR_D_ADDR, &mut buf5)?;
    if has_signature(&buf5) {
        return Ok(Detection {
            map_type: MapType::Old,
            dir_type: DirType::New,
            disc_record: None,
            old_map_root_dir_addr: Some(OLD_DIR_D_ADDR),
            boot_block: None,
        });
    }

    let mut bb_raw = [0u8; 512];
    source.read_at(BOOT_BLOCK_ADDR, &mut bb_raw)?;
    if let Ok(bb) = parse_boot_block(&bb_raw)
        && looks_plausible(&bb.disc_record) {
            return Ok(from_disc_record(bb.disc_record.clone(), Some(bb)));
        }

    let mut dr_bytes = [0u8; DISC_RECORD_SIZE];
    source.read_at(0x04, &mut dr_bytes)?;
    if let Ok(dr) = parse_disc_record(&dr_bytes)
        && looks_plausible(&dr) && !dr.is_old_map() {
            return Ok(from_disc_record(dr, None));
        }

    Err(FcError::NotRecognised)
}

fn from_disc_record(dr: DiscRecord, boot_block: Option<BootBlock>) -> Detection {
    let map_type = if dr.is_old_map() { MapType::Old } else { MapType::New };
    let dir_type = if dr.is_old_map() {
        DirType::Old
    } else if dr.is_big_dir() {
        DirType::Big
    } else {
        DirType::New
    };
    Detection {
        map_type,
        dir_type,
        disc_record: Some(dr),
        old_map_root_dir_addr: None,
        boot_block,
    }
}
