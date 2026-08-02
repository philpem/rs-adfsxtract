use winnow::Parser;
use winnow::binary::{le_u16, le_u32, u8 as bin_u8};
use winnow::combinator::seq;
use winnow::error::ContextError;
use winnow::token::take;

use crate::error::{FcError, Result};
use crate::format::filecore::checksums::boot_block_checksum;

/// The FileCore disc record (guide §2.1), always parsed as the full 60-byte
/// extended form; on pre-3.6 media the extended fields simply read as zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscRecord {
    pub log2_sector_size: u8,
    pub sectors_per_track: u8,
    pub heads: u8,
    pub density: u8,
    pub idlen: u8,
    pub log2_bpmb: u8,
    pub skew: u8,
    pub boot_option: u8,
    pub low_sector: u8,
    pub nzones_lo: u8,
    pub zone_spare: u16,
    pub root_dir: u32,
    pub disc_size: u32,
    pub disc_id: u16,
    pub disc_name: [u8; 10],
    pub disc_type: u32,
    pub disc_size_2: u32,
    pub share_size: u8,
    pub big_flag: u8,
    pub nzones_hi: u8,
    pub format_version: u32,
    pub root_size: u32,
}

pub const DISC_RECORD_SIZE: usize = 60;

impl DiscRecord {
    pub fn sector_size(&self) -> u32 {
        1u32 << self.log2_sector_size
    }

    pub fn bpmb(&self) -> u32 {
        1u32 << self.log2_bpmb
    }

    pub fn nzones(&self) -> u32 {
        self.nzones_lo as u32 | ((self.nzones_hi as u32) << 8)
    }

    pub fn is_old_map(&self) -> bool {
        self.idlen == 0
    }

    pub fn is_big_dir(&self) -> bool {
        self.format_version == 1
    }

    pub fn disc_size_bytes(&self) -> u64 {
        self.disc_size as u64 | ((self.disc_size_2 as u64) << 32)
    }

    /// Sharing unit in bytes (guide §3.2): `sector_size << share_size`.
    pub fn sharing_unit(&self) -> u64 {
        (self.sector_size() as u64) << self.share_size
    }

    pub fn disc_name_str(&self) -> String {
        let end = self
            .disc_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.disc_name.len());
        crate::xlate::charset::decode(&self.disc_name[..end])
    }
}

pub fn parse_disc_record(input: &[u8]) -> Result<DiscRecord> {
    let mut cursor: &[u8] = input;
    parse_disc_record_p
        .parse_next(&mut cursor)
        .map_err(|e: ContextError| FcError::Parse(format!("disc record: {e}")))
}

fn parse_disc_record_p(input: &mut &[u8]) -> winnow::Result<DiscRecord> {
    seq!(DiscRecord {
        log2_sector_size: bin_u8,
        sectors_per_track: bin_u8,
        heads: bin_u8,
        density: bin_u8,
        idlen: bin_u8,
        log2_bpmb: bin_u8,
        skew: bin_u8,
        boot_option: bin_u8,
        low_sector: bin_u8,
        nzones_lo: bin_u8,
        zone_spare: le_u16,
        root_dir: le_u32,
        disc_size: le_u32,
        disc_id: le_u16,
        disc_name: take(10usize).map(|b: &[u8]| b.try_into().unwrap()),
        disc_type: le_u32,
        disc_size_2: le_u32,
        share_size: bin_u8,
        big_flag: bin_u8,
        nzones_hi: bin_u8,
        _: bin_u8,
        format_version: le_u32,
        root_size: le_u32,
        _: take(8usize),
    })
    .parse_next(input)
}

/// The boot block (guide §2.2): 512 bytes at disc address `0xC00`, present
/// on hard discs and multi-zone (F-format) floppies only.
#[derive(Debug, Clone)]
pub struct BootBlock {
    pub raw: [u8; 512],
    pub disc_record: DiscRecord,
    pub checksum_ok: bool,
}

pub const BOOT_BLOCK_ADDR: u64 = 0xC00;
pub const BOOT_BLOCK_SIZE: usize = 512;
const DISC_RECORD_OFFSET_IN_BOOT_BLOCK: usize = 0x1C0;

pub fn parse_boot_block(raw: &[u8; 512]) -> Result<BootBlock> {
    let disc_record = parse_disc_record(
        &raw[DISC_RECORD_OFFSET_IN_BOOT_BLOCK..DISC_RECORD_OFFSET_IN_BOOT_BLOCK + DISC_RECORD_SIZE],
    )?;
    let checksum_ok = boot_block_checksum(raw) == raw[0x1FF];
    Ok(BootBlock {
        raw: *raw,
        disc_record,
        checksum_ok,
    })
}

/// True if a candidate boot block / zone-0 disc record buffer looks like a
/// real disc record rather than all-zero/uninitialised data (guide §1.4's
/// "if the boot block fields aren't plausible... fall through").
///
/// Also the only gate on `log2_bpmb` before it's used as a shift amount
/// throughout `map_new.rs` (`bpmb() = 1u32 << log2_bpmb`). Real media only
/// ever uses 7-10 (see the guide's §C.1/§C.4 worked examples); this allows
/// generous headroom above that while still ruling out the danger zone -
/// `log2_bpmb >= 32` overflows the shift, which panics in a debug build
/// but silently wraps to a masked (wrong) value in release, propagating a
/// corrupted allocation-unit size into every subsequent size calculation
/// instead of failing loudly.
pub fn looks_plausible(dr: &DiscRecord) -> bool {
    dr.log2_sector_size >= 8
        && dr.log2_sector_size <= 12
        && dr.log2_bpmb <= 20
        && !(dr.sectors_per_track == 0 && dr.heads == 0 && dr.root_dir == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bytes() -> Vec<u8> {
        let mut v = vec![0u8; DISC_RECORD_SIZE];
        v[0] = 9; // log2_sector_size = 512
        v[1] = 5; // sectors_per_track
        v[2] = 2; // heads
        v[4] = 15; // idlen
        v[5] = 10; // log2_bpmb
        v[9] = 4; // nzones lo
        v[0x0C..0x10].copy_from_slice(&0x0002_0203u32.to_le_bytes());
        v[0x10..0x14].copy_from_slice(&819200u32.to_le_bytes());
        v[0x16..0x20].copy_from_slice(b"TESTDISC\0\0");
        v
    }

    #[test]
    fn parses_fields() {
        let dr = parse_disc_record(&sample_bytes()).unwrap();
        assert_eq!(dr.sector_size(), 512);
        assert_eq!(dr.bpmb(), 1024);
        assert_eq!(dr.nzones(), 4);
        assert_eq!(dr.root_dir, 0x0002_0203);
        assert_eq!(dr.disc_size, 819200);
        assert_eq!(dr.disc_name_str(), "TESTDISC");
        assert!(!dr.is_old_map());
        assert!(!dr.is_big_dir());
    }

    #[test]
    fn old_map_has_zero_idlen() {
        let mut bytes = vec![0u8; DISC_RECORD_SIZE];
        bytes[0] = 8;
        let dr = parse_disc_record(&bytes).unwrap();
        assert!(dr.is_old_map());
    }

    #[test]
    fn rejects_log2_bpmb_that_would_overflow_a_shift() {
        let mut bytes = sample_bytes();
        bytes[5] = 200; // log2_bpmb, would overflow `1u32 << log2_bpmb`
        let dr = parse_disc_record(&bytes).unwrap();
        assert!(!looks_plausible(&dr));
    }
}
