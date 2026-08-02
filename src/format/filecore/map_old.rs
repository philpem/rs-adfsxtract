//! Old free-space map (guide §2.5): two 256-byte sectors at disc addresses
//! `0x000` and `0x100`. Not needed to resolve file extents (an old-map
//! directory entry's SIN field is already a direct sector address), only
//! for disc metadata (`info`) and integrity checking.

use crate::error::Result;
use crate::format::filecore::checksums::old_map_checksum;

pub const OLD_MAP_SECTOR0_ADDR: u64 = 0x000;
pub const OLD_MAP_SECTOR1_ADDR: u64 = 0x100;
pub const OLD_MAP_SECTOR_SIZE: usize = 256;

#[derive(Debug, Clone)]
pub struct OldFreeSpaceMap {
    /// (start, length), both in 256-byte units.
    pub extents: Vec<(u32, u32)>,
    pub disc_name: String,
    /// Total disc size in 256-byte units.
    pub total_sectors: u32,
    pub disc_id: u16,
    pub boot_option: u8,
    pub sector0_checksum_ok: bool,
    pub sector1_checksum_ok: bool,
}

fn read_u24_le(b: &[u8]) -> u32 {
    b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16
}

pub fn parse_old_map(sector0: &[u8; 256], sector1: &[u8; 256]) -> Result<OldFreeSpaceMap> {
    let sector0_checksum_ok = old_map_checksum(sector0) == sector0[0xFF];
    let sector1_checksum_ok = old_map_checksum(sector1) == sector1[0xFF];

    let end_ptr = sector1[0xFE] as usize;
    let num_extents = (end_ptr / 3).min(82);
    let mut extents = Vec::with_capacity(num_extents);
    for i in 0..num_extents {
        let start = read_u24_le(&sector0[i * 3..i * 3 + 3]);
        let len = read_u24_le(&sector1[i * 3..i * 3 + 3]);
        if len > 0 {
            extents.push((start, len));
        }
    }

    let mut name_bytes = [0u8; 10];
    for i in 0..5 {
        name_bytes[i * 2] = sector0[0xF7 + i];
        name_bytes[i * 2 + 1] = sector1[0xF6 + i];
    }
    let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(10);
    let disc_name = crate::xlate::charset::decode(&name_bytes[..end]);

    let total_sectors = read_u24_le(&sector0[0xFC..0xFF]);
    let disc_id = u16::from_le_bytes([sector1[0xFB], sector1[0xFC]]);
    let boot_option = sector1[0xFD];

    Ok(OldFreeSpaceMap {
        extents,
        disc_name,
        total_sectors,
        disc_id,
        boot_option,
        sector0_checksum_ok,
        sector1_checksum_ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_disc_name_interleaved() {
        let mut s0 = [0u8; 256];
        let mut s1 = [0u8; 256];
        // "00_05_Sun " interleaved: even chars in s0, odd in s1
        let name = b"00_05_Sun ";
        for i in 0..5 {
            s0[0xF7 + i] = name[i * 2];
            s1[0xF6 + i] = name[i * 2 + 1];
        }
        s0[0xFF] = old_map_checksum(&s0);
        s1[0xFF] = old_map_checksum(&s1);
        let map = parse_old_map(&s0, &s1).unwrap();
        assert_eq!(map.disc_name, "00_05_Sun ");
        assert!(map.sector0_checksum_ok);
        assert!(map.sector1_checksum_ok);
    }
}
