//! Acorn DFS catalogue: two 256-byte sectors (title, cycle number, boot
//! option, disc size, and up to 31 file entries), optionally followed by a
//! second 31-entry block (Watford DFS's 62-file extension) at sectors 2-3,
//! signalled by eight `0xAA` bytes at the start of sector 2.
//!
//! Layout and the entry's packed extension byte were confirmed against
//! `DiscImage_DFS.pas` (read for the on-disk facts only, not copied for
//! structure - same rule already applied to DIM elsewhere in this project)
//! and cross-checked by hand against a real image (`BB1.ssd`): entry 2
//! ("SETPAGE", extension byte `0xCC`) decodes to `load=0xFF0E00`,
//! `exec=0xFF802B` - the classic BBC "no explicit load address" `0xFFFFxxxx`
//! sentinel pattern falling out of real bytes, which confirms both the
//! field order and the 2-bit-to-8-bit pattern-replication below.

use crate::error::Result;
use crate::format::dfs::geometry::DfsGeometry;
use crate::io::SectorSource;
use crate::xlate::charset;

pub const SECTOR_SIZE: usize = 256;
const ENTRIES_PER_BLOCK: usize = 31;
const ENTRY_SIZE: usize = 8;

#[derive(Debug, Clone)]
pub struct DfsEntry {
    pub name: String,
    pub dir_char: char,
    pub locked: bool,
    pub load: u32,
    pub exec: u32,
    pub length: u64,
    pub start_sector: u32,
}

impl DfsEntry {
    /// Whether this entry's data run stays within the disc's recorded size
    /// - the only integrity signal DFS has (no checksum on the catalogue).
    pub fn in_bounds(&self, total_sectors: u32) -> bool {
        let end = self.start_sector as u64 * SECTOR_SIZE as u64 + self.length;
        end <= total_sectors as u64 * SECTOR_SIZE as u64
    }
}

#[derive(Debug, Clone)]
pub struct DfsCatalogue {
    pub title: String,
    pub cycle_bcd: u8,
    pub boot_option: u8,
    pub total_sectors: u32,
    pub entries: Vec<DfsEntry>,
    pub watford: bool,
}

/// 2-bit value -> replicated byte (0,1,2,3 -> 0x00,0x55,0xAA,0xFF). DFS packs
/// the top bits of a load/exec address this way so that the all-ones case
/// reproduces the BBC's conventional `0xFFFFxxxx` "no explicit address"
/// sentinel once the OS reads the low 16 bits back into a 32-bit register.
fn replicate_2bit(v: u8) -> u32 {
    (v as u32) * 0x55
}

fn decode_entries(names: &[u8], info: &[u8], count: usize) -> Vec<DfsEntry> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let n = &names[i * ENTRY_SIZE..i * ENTRY_SIZE + ENTRY_SIZE];
        let raw_dir = n[7];
        let locked = raw_dir & 0x80 != 0;
        let dir_char = charset::decode_byte(raw_dir & 0x7F);
        let name = charset::decode(&n[0..7])
            .trim_end_matches(['\0', ' '])
            .to_string();

        let e = &info[i * ENTRY_SIZE..i * ENTRY_SIZE + ENTRY_SIZE];
        let load_lo = u16::from_le_bytes([e[0], e[1]]) as u32;
        let exec_lo = u16::from_le_bytes([e[2], e[3]]) as u32;
        let length_lo = u16::from_le_bytes([e[4], e[5]]) as u32;
        let ext = e[6];
        let sector_lo = e[7] as u32;

        let load_hi = replicate_2bit((ext & 0x0C) >> 2);
        let exec_hi = replicate_2bit((ext & 0xC0) >> 6);
        let length_hi = ((ext & 0x30) >> 4) as u32;
        let sector_hi = (ext & 0x03) as u32;

        out.push(DfsEntry {
            name,
            dir_char,
            locked,
            load: (load_hi << 16) | load_lo,
            exec: (exec_hi << 16) | exec_lo,
            length: (((length_hi << 16) | length_lo) as u64),
            start_sector: (sector_hi << 8) | sector_lo,
        });
    }
    out
}

pub(crate) fn read_sector(
    source: &mut dyn SectorSource,
    geometry: &DfsGeometry,
    side: u8,
    logical_addr: u64,
) -> Result<[u8; SECTOR_SIZE]> {
    let extents = geometry.translate(side, logical_addr, SECTOR_SIZE as u64);
    debug_assert_eq!(
        extents.len(),
        1,
        "a whole-sector read never crosses a track boundary"
    );
    let mut buf = [0u8; SECTOR_SIZE];
    source.read_at(extents[0].disc_addr, &mut buf)?;
    Ok(buf)
}

/// Reads and decodes one side's catalogue (standard 31-entry block, plus the
/// Watford 62-file extension block if its signature is present).
pub fn read_catalogue(
    source: &mut dyn SectorSource,
    geometry: &DfsGeometry,
    side: u8,
) -> Result<DfsCatalogue> {
    let s0 = read_sector(source, geometry, side, 0x000)?;
    let s1 = read_sector(source, geometry, side, 0x100)?;
    let s2 = read_sector(source, geometry, side, 0x200)?;
    let s3 = read_sector(source, geometry, side, 0x300)?;

    // A title shorter than 12 chars is padded with NUL (confirmed on real
    // media - not spaces, unlike the per-entry name field below).
    let title = format!(
        "{}{}",
        charset::decode(&s0[0..8]),
        charset::decode(&s1[0..4])
    );
    let title = title.trim_end_matches(['\0', ' ']).to_string();

    let cycle_bcd = s1[4];
    let file_count = (s1[5] / 8) as usize;
    let byte6 = s1[6];
    let boot_option = (byte6 >> 4) & 0x3;
    // The disk-size field uses three high bits (bits 0-2) + the low byte, giving
    // an 11-bit sector count (up to 0x7FF = 2047). Double-density DFS (e.g.
    // Watford/Solidisk DDFS) discs - 320 KB (0x500 = 1280) and beyond - set bit
    // 2, which a 2-bit mask would silently drop and mis-size. Confirmed against
    // sweh's MMB_Utils (`$disk_size=($b[0]&7)*256+$b[1]`).
    let mut total_sectors = (((byte6 & 0x7) as u32) << 8) | s1[7] as u32;
    if total_sectors == 0 {
        // DFS's own convention for an unrecorded disc size: assume 200K.
        total_sectors = 0x320;
    }

    let mut entries = decode_entries(&s0[8..], &s1[8..], file_count.min(ENTRIES_PER_BLOCK));

    let watford = s2[0..8] == [0xAA; 8] && s3[0..4] == [0x00; 4];
    if watford {
        let extra_count = (s3[5] / 8) as usize;
        entries.extend(decode_entries(
            &s2[8..],
            &s3[8..],
            extra_count.min(ENTRIES_PER_BLOCK),
        ));
    }

    Ok(DfsCatalogue {
        title,
        cycle_bcd,
        boot_option,
        total_sectors,
        entries,
        watford,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_side0(
        title8: &[u8; 8],
        title4: &[u8; 4],
        entries: &[(&str, u8, [u8; 8])],
    ) -> ([u8; SECTOR_SIZE], [u8; SECTOR_SIZE]) {
        let mut s0 = [0u8; SECTOR_SIZE];
        let mut s1 = [0u8; SECTOR_SIZE];
        s0[0..8].copy_from_slice(title8);
        s1[0..4].copy_from_slice(title4);
        s1[5] = (entries.len() * 8) as u8;
        s1[6] = 0x03; // boot option 0, sector count high bits 3
        s1[7] = 0x20; // sector count low -> total 0x320 = 800
        for (i, (name, dirchar, info)) in entries.iter().enumerate() {
            let mut name_field = [b' '; 7];
            for (j, b) in name.bytes().enumerate() {
                name_field[j] = b;
            }
            s0[8 + i * 8..8 + i * 8 + 7].copy_from_slice(&name_field);
            s0[8 + i * 8 + 7] = *dirchar;
            s1[8 + i * 8..8 + i * 8 + 8].copy_from_slice(info);
        }
        (s0, s1)
    }

    #[test]
    fn decodes_setpage_entry_matching_real_media() {
        // Entry bytes for "SETPAGE" from a real BB1.ssd: 00 0e 2b 80 14 00 cc 8c
        let entries = decode_entries(
            b"SETPAGE$",
            &[0x00, 0x0e, 0x2b, 0x80, 0x14, 0x00, 0xcc, 0x8c],
            1,
        );
        let e = &entries[0];
        assert_eq!(e.name, "SETPAGE");
        assert_eq!(e.dir_char, '$');
        assert!(!e.locked);
        assert_eq!(e.load, 0xFF0E00);
        assert_eq!(e.exec, 0xFF802B);
        assert_eq!(e.length, 20);
        assert_eq!(e.start_sector, 140);
    }

    #[test]
    fn decodes_boot_entry_with_no_extension_bits() {
        // "!BOOT" from the same disc: 00 00 0b 00 00 8d 00 0e
        let entries = decode_entries(
            b"!BOOT  $",
            &[0x00, 0x00, 0x0b, 0x00, 0x00, 0x8d, 0x00, 0x0e],
            1,
        );
        let e = &entries[0];
        assert_eq!(e.name, "!BOOT");
        assert_eq!(e.dir_char, '$');
        assert_eq!(e.load, 0x0000);
        assert_eq!(e.exec, 0x000b);
        assert_eq!(e.length, 0x8d00);
        assert_eq!(e.start_sector, 14);
    }

    #[test]
    fn locked_flag_is_top_bit_of_dirchar() {
        let entries = decode_entries(b"LOCKED $", &[0; 8], 1);
        assert!(!entries[0].locked);
        let entries = decode_entries(b"LOCKED \xa4", &[0; 8], 1);
        assert!(entries[0].locked);
        assert_eq!(entries[0].dir_char, '$');
    }

    #[test]
    fn reads_full_catalogue_from_sector_source() {
        let (s0, s1) = make_side0(
            b"BBC TAPE",
            b"\0\0\0\0",
            &[(
                "!BOOT",
                b'$',
                [0x00, 0x00, 0x0b, 0x00, 0x00, 0x8d, 0x00, 0x0e],
            )],
        );
        let mut image = vec![0u8; SECTOR_SIZE * 4];
        image[0..SECTOR_SIZE].copy_from_slice(&s0);
        image[SECTOR_SIZE..SECTOR_SIZE * 2].copy_from_slice(&s1);
        let mut cursor = std::io::Cursor::new(image);
        let geometry = DfsGeometry {
            double_sided: false,
        };
        let cat = read_catalogue(&mut cursor, &geometry, 0).unwrap();
        assert_eq!(cat.title, "BBC TAPE");
        assert_eq!(cat.total_sectors, 800);
        assert_eq!(cat.entries.len(), 1);
        assert_eq!(cat.entries[0].name, "!BOOT");
        assert!(!cat.watford);
    }

    #[test]
    fn decodes_double_density_disk_size_using_three_high_bits() {
        // Double-density DFS (Watford/Solidisk DDFS) records a sector count
        // needing 11 bits, e.g. a 320 KB disc = 0x500 = 1280 sectors, whose
        // high bits (5 = 0b101) set bit 2 of the size byte. A 2-bit mask would
        // (incorrectly) read that as 0x100 = 256. The disk-size decode must use
        // all three high bits (confirmed against sweh's MMB_Utils).
        let mut s0 = [0u8; SECTOR_SIZE];
        let mut s1 = [0u8; SECTOR_SIZE];
        s0[0..8].copy_from_slice(b"DDDISK  ");
        s1[0..4].copy_from_slice(b"\0\0\0\0");
        // byte6: boot option 0 (bits 4-5) | disk-size high bits = 5 (bits 0-2)
        s1[6] = 0x05;
        s1[7] = 0x00; // low byte
        let mut image = vec![0u8; SECTOR_SIZE * 4];
        image[0..SECTOR_SIZE].copy_from_slice(&s0);
        image[SECTOR_SIZE..SECTOR_SIZE * 2].copy_from_slice(&s1);
        let mut cursor = std::io::Cursor::new(image);
        let geometry = DfsGeometry {
            double_sided: false,
        };
        let cat = read_catalogue(&mut cursor, &geometry, 0).unwrap();
        assert_eq!(
            cat.total_sectors, 0x500,
            "1280 sectors (320 KB) must be read correctly"
        );
    }

    #[test]
    fn detects_watford_signature() {
        let (s0, s1) = make_side0(b"TITLE   ", b"\0\0\0\0", &[]);
        let mut image = vec![0u8; SECTOR_SIZE * 4];
        image[0..SECTOR_SIZE].copy_from_slice(&s0);
        image[SECTOR_SIZE..SECTOR_SIZE * 2].copy_from_slice(&s1);
        image[SECTOR_SIZE * 2..SECTOR_SIZE * 2 + 8].copy_from_slice(&[0xAA; 8]);
        // sector 3: 4 zero bytes then a one-entry catalogue
        let extra_off = SECTOR_SIZE * 3;
        image[extra_off + 5] = 8; // 1 extra file
        image[extra_off + 6] = 0x03;
        image[extra_off + 7] = 0x20;
        let name_off = SECTOR_SIZE * 2 + 8;
        image[name_off..name_off + 8].copy_from_slice(b"EXTRA  $");
        let info_off = extra_off + 8;
        image[info_off..info_off + 8].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);

        let mut cursor = std::io::Cursor::new(image);
        let geometry = DfsGeometry {
            double_sided: false,
        };
        let cat = read_catalogue(&mut cursor, &geometry, 0).unwrap();
        assert!(cat.watford);
        assert_eq!(cat.entries.len(), 1);
        assert_eq!(cat.entries[0].name, "EXTRA");
    }

    #[test]
    fn bounds_check_catches_out_of_range_entry() {
        let e = DfsEntry {
            name: "X".into(),
            dir_char: '$',
            locked: false,
            load: 0,
            exec: 0,
            length: 300,
            start_sector: 799,
        };
        assert!(!e.in_bounds(800));
        let ok = DfsEntry {
            start_sector: 100,
            length: 256,
            ..e
        };
        assert!(ok.in_bounds(800));
    }
}
