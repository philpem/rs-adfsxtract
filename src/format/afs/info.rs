//! Parsing of the AFS0 "Disk Information Block" (mdfs.net/Docs/Comp/Disk/
//! Format/AFS0). The layout differs between the Level 2 and Level 3 File
//! Server filesystems - the L3 block additionally declares the filesystem
//! geometry (tracks, sectors/track) that the L2 block implies from the
//! density. Only the fields needed for detection, metadata and traversal are
//! decoded here.

pub const SECTOR_SIZE: usize = 256;
pub const AFS0_SIG: [u8; 4] = *b"AFS0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfsLevel {
    /// Acorn File Server Level 2 (single-density floppies, 12-bit allocation
    /// map).
    Level2,
    /// Acorn File Server Level 3 (double-density; allocation maps are
    /// "JesMap" blocks; may coexist with an ADFS partition).
    Level3,
}

impl AfsLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            AfsLevel::Level2 => "AFS2",
            AfsLevel::Level3 => "AFS3",
        }
    }
}

fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u24le(b: &[u8], off: usize) -> u32 {
    b[off] as u32 | (b[off + 1] as u32) << 8 | (b[off + 2] as u32) << 16
}

/// Decoded fields common to both AFS levels' Disk Information Block.
#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub level: AfsLevel,
    pub title: String,
    pub title_bytes: Vec<u8>,
    pub root_sin: u32,
    pub init_date: u16,
    /// L2: sectors on one side of the disc. L3: zero.
    pub sectors_per_side: u16,
    /// L2 only: SIN of the first allocation map copy.
    pub map_a_sin: u32,
    /// L2 only: SIN of the second allocation map copy.
    pub map_b_sin: u32,
    /// L2 only: number of sectors used by the allocation map.
    pub map_sector_count: u8,
    /// L3 only: number of tracks/cylinders on the whole disc.
    pub cylinders: u16,
    /// L3 only: total number of sectors on the whole disc (24-bit).
    pub total_sectors: u32,
    /// L3 only: number of partitions (usually 1).
    pub partitions: u8,
    /// L3 only: sectors per track.
    pub sectors_per_track: u16,
    /// L3 only: first free cylinder.
    pub first_free_cylinder: u16,
}

fn decode_title(raw: &[u8]) -> (String, Vec<u8>) {
    let end = raw
        .iter()
        .position(|&b| b == b' ' || b == 0)
        .unwrap_or(raw.len());
    (
        crate::xlate::charset::decode(&raw[..end]),
        raw[..end].to_vec(),
    )
}

/// Parses a Level 2 Disk Information Block (sector 0). Caller must have
/// verified the `AFS0` signature at offset 0.
pub fn parse_level2(block: &[u8]) -> DiskInfo {
    let (title, title_bytes) = decode_title(&block[0x04..0x14]);
    DiskInfo {
        level: AfsLevel::Level2,
        title,
        title_bytes,
        root_sin: u24le(block, 0x16),
        init_date: u16le(block, 0x19),
        sectors_per_side: u16le(block, 0x14),
        map_a_sin: u24le(block, 0x1B),
        map_b_sin: u24le(block, 0x1E),
        map_sector_count: block[0x21],
        cylinders: 0,
        total_sectors: 0,
        partitions: 0,
        sectors_per_track: 0,
        first_free_cylinder: 0,
    }
}

/// Parses a Level 3 Disk Information Block. The block is not at sector 0 for
/// L3 (sector 0/1 hold an ADFS free-space map); it is at the sector pointed
/// to by the AFS start pointer in the ADFIC free-space map (`&0F6`), which is
/// one logical sector into the FileServer partition. Caller must have
/// verified the `AFS0` signature at offset 0.
pub fn parse_level3(block: &[u8]) -> DiskInfo {
    let (title, title_bytes) = decode_title(&block[0x04..0x14]);
    DiskInfo {
        level: AfsLevel::Level3,
        title,
        title_bytes,
        root_sin: u24le(block, 0x1F),
        init_date: u16le(block, 0x22),
        sectors_per_side: 0,
        map_a_sin: 0,
        map_b_sin: 0,
        map_sector_count: 0,
        cylinders: u16le(block, 0x14),
        total_sectors: u24le(block, 0x16),
        partitions: block[0x19],
        sectors_per_track: u16le(block, 0x1A),
        first_free_cylinder: u16le(block, 0x24),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_level2_info_block() {
        let mut b = [0u8; SECTOR_SIZE];
        b[0..4].copy_from_slice(b"AFS0");
        b[4..0x14].copy_from_slice(b"MYDISK          ");
        b[0x14] = 0x20;
        b[0x15] = 0x03;
        b[0x16] = 0x80;
        b[0x17] = 0x00;
        b[0x18] = 0x00;
        b[0x1B] = 0x00;
        b[0x1C] = 0xA0;
        b[0x1D] = 0x00;
        b[0x1E] = 0x00;
        b[0x1F] = 0xB0;
        b[0x20] = 0x00;
        let info = parse_level2(&b);
        assert_eq!(info.level, AfsLevel::Level2);
        assert_eq!(info.title, "MYDISK");
        assert_eq!(info.sectors_per_side, 0x0320);
        assert_eq!(info.root_sin, 0x80);
        assert_eq!(info.map_a_sin, 0xA000);
        assert_eq!(info.map_b_sin, 0xB000);
    }
}
