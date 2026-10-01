//! AFS0 pure decoding helpers: logical->physical sector-geometry translation,
//! directory decoding, and "JesMap" allocation-map parsing. Sector I/O and
//! the filesystem state live in `mod.rs`; the routines here are deterministic
//! over a byte buffer so they can be unit-tested without a disc image.

use crate::model::object::{
    ATTR_DIRECTORY, ATTR_LOCKED, ATTR_OWNER_READ, ATTR_OWNER_WRITE, ATTR_PUBLIC_READ,
    ATTR_PUBLIC_WRITE,
};

pub const SECTOR_SIZE: usize = 256;
const ENTRY_SIZE: usize = 0x1A;

/// The physical ordering of logical sectors on a floppy. "Seq" stores logical
/// sector N at physical N; the interleaved orders (AFSFiler's INT/MUX) were
/// used on real FileStore/FileServer media to stagger track access. A raw
/// image extracted by a modern bitstream reader is almost always Seq.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interleave {
    Seq,
    Int,
    Mux,
}

impl Interleave {
    pub fn all() -> [Interleave; 3] {
        [Interleave::Seq, Interleave::Int, Interleave::Mux]
    }
}

/// Translates a filesystem *logical* sector number to the physical sector
/// stored in the image, given the sectors-per-track geometry and interleave.
/// AFSFiler's reference transforms (SEQ/INT/MUX) are used verbatim.
pub fn translate(logical: u32, spt: u16, il: Interleave) -> u32 {
    if spt == 0 {
        return logical;
    }
    let spt = spt as u32;
    let track0 = logical / spt;
    let sector = logical % spt;
    match il {
        Interleave::Seq => logical,
        Interleave::Int => (track0 % 80) * 2 * spt + (track0 / 80) * spt + sector,
        Interleave::Mux => (track0 % 2) * 80 * spt + (track0 / 2) * spt + sector,
    }
}

/// NetFS internal access byte -> standard Acorn filing-system attribute bits.
/// The reference conversion (mdfs AFS0 doc) is:
/// `access' = (nfs & 3)*16 + (nfs & 12)/4 + (nfs & 16)/2`, giving standard
/// bits 01wrL0WR; the directory bit (nfs bit 5) is separate.
pub fn convert_access(nfs: u8) -> u32 {
    let mut attrs = 0;
    if nfs & 0x01 != 0 {
        attrs |= ATTR_PUBLIC_READ;
    }
    if nfs & 0x02 != 0 {
        attrs |= ATTR_PUBLIC_WRITE;
    }
    if nfs & 0x04 != 0 {
        attrs |= ATTR_OWNER_READ;
    }
    if nfs & 0x08 != 0 {
        attrs |= ATTR_OWNER_WRITE;
    }
    if nfs & 0x10 != 0 {
        attrs |= ATTR_LOCKED;
    }
    if nfs & 0x20 != 0 {
        attrs |= ATTR_DIRECTORY;
    }
    attrs
}

fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u24le(b: &[u8], off: usize) -> u32 {
    b[off] as u32 | (b[off + 1] as u32) << 8 | (b[off + 2] as u32) << 16
}

fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Days from the Unix epoch for a Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + (d as i64 - 1);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Decodes an AFS filing-system date (the standard 2-byte date format: day in
/// bits 0-4 of byte 0, month in bits 0-3 of byte 1 and the (year-1981) spread
/// across the remaining bits) into Unix seconds. Returns `None` for an
/// out-of-range month/day.
pub fn decode_afs_date(date: u16) -> Option<i64> {
    let byte0 = (date & 0xFF) as u8;
    let byte1 = ((date >> 8) & 0xFF) as u8;
    let day = (byte0 & 0x1F) as u32;
    let year_hi = ((byte0 >> 5) & 0x07) as u16;
    let month = (byte1 & 0x0F) as u32;
    let year_lo = (byte1 >> 4) & 0x0F;
    let year = 1981i64 + (year_hi << 4) as i64 + year_lo as i64;
    if month == 0 || month > 12 || day == 0 || day > 31 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86400)
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub name_bytes: Vec<u8>,
    pub load: u32,
    pub exec: u32,
    pub attrs: u32,
    pub is_directory: bool,
    pub sin: u32,
    pub modified_unix_secs: Option<i64>,
}

#[derive(Debug, Default)]
pub struct DirDecode {
    pub entries: Vec<DirEntry>,
    pub title: String,
    pub title_bytes: Vec<u8>,
    pub is_broken: bool,
    pub anomalies: Vec<String>,
}

fn trim_spaces(b: &[u8]) -> &[u8] {
    let end = b
        .iter()
        .position(|&c| c == b' ' || c == 0)
        .unwrap_or(b.len());
    &b[..end]
}

/// Decodes a directory buffer (assembled from the allocation-map data sectors,
/// up to 26 sectors / &1A00 bytes).
///
/// Entries are not at fixed offsets: the directory header's pointer at &0
/// gives the offset (into the buffer) of the first entry, and each entry's
/// first field is a pointer to the next, forming a case-insensitively sorted
/// linked list terminated by &0000 (a reserved parent entry has next=&FFFF
/// and is skipped). Following that list rather than assuming a stride is
/// essential - real FileServer discs place entries at arbitrary offsets (e.g.
/// the first entry often sits far from &0x11).
pub fn decode_directory(buf: &[u8]) -> DirDecode {
    let mut out = DirDecode::default();
    if buf.len() < 0x10 {
        out.is_broken = true;
        out.anomalies.push("directory data truncated".into());
        return out;
    }
    let count = buf[0x0F] as usize;
    out.title_bytes = trim_spaces(&buf[0x03..0x0D]).to_vec();
    out.title = crate::xlate::charset::decode(&out.title_bytes);

    if count > 255 {
        out.is_broken = true;
        out.anomalies
            .push(format!("implausible entry count {count}"));
        return out;
    }

    let mut off = u16le(buf, 0x00) as usize;
    // If the directory claims entries but the first-entry pointer is zero, the
    // structure is inconsistent (and is how a mis-translated/mis-interleaved
    // read is caught).
    if count > 0 && off == 0 {
        out.is_broken = true;
        out.anomalies
            .push("directory declares entries but has a zero first-entry pointer".into());
        return out;
    }

    let mut seen = 0usize;
    while off != 0 && seen <= count {
        if off + ENTRY_SIZE > buf.len() {
            out.is_broken = true;
            out.anomalies.push(format!(
                "entry at offset &{off:X} lies beyond directory data (&{:X})",
                buf.len()
            ));
            break;
        }
        let e = &buf[off..off + ENTRY_SIZE];
        let next = u16le(e, 0x00);
        let name_bytes = trim_spaces(&e[0x02..0x0C]).to_vec();
        if !name_bytes.is_empty() && next != 0xFFFF {
            let nfs_access = e[0x14];
            let attrs = convert_access(nfs_access);
            out.entries.push(DirEntry {
                name: crate::xlate::charset::decode(&name_bytes),
                name_bytes,
                load: u32le(e, 0x0C),
                exec: u32le(e, 0x10),
                attrs,
                is_directory: attrs & ATTR_DIRECTORY != 0,
                sin: u24le(e, 0x17),
                modified_unix_secs: decode_afs_date(u16le(e, 0x15)),
            });
        }
        if next == 0 || next == 0xFFFF {
            break;
        }
        off = next as usize;
        seen += 1;
    }
    out
}

/// A group of contiguous allocated sectors from a Level-3 "JesMap" block.
#[derive(Debug, Clone, Copy)]
pub struct JesGroup {
    pub sector: u32,
    pub count: u16,
}

/// Parses a JesMap allocation-map block. Returns the contiguous sector groups
/// and the object-length low byte (only meaningful in the first map of a
/// chain; the caller handles chaining via the `FA` chain pointer). Paths the
/// format's allocated-run descriptor (5 bytes per group, `sector`=3 bytes,
/// `count`=2 bytes) beginning at `0A`, terminated by a zero group or running
/// into the chain pointer at `FA`.
pub fn decode_jes_map(buf: &[u8]) -> (Vec<JesGroup>, Option<u8>) {
    if buf.len() < 6 || &buf[0..6] != b"JesMap" {
        return (Vec::new(), buf.get(8).copied());
    }
    if buf.len() < 0x10 {
        return (Vec::new(), buf.get(8).copied());
    }
    let mut groups = Vec::new();
    let mut ptr = 0x0A;
    while ptr + 5 <= 0xFA.min(buf.len()) {
        let sector = u24le(buf, ptr);
        let count = u16le(buf, ptr + 3);
        if sector == 0 || count == 0 {
            break;
        }
        groups.push(JesGroup { sector, count });
        ptr += 5;
    }
    (groups, buf.get(8).copied())
}

/// The logical sector of a next allocation-map block (chain pointer at `FA`),
/// or `0` when none.
pub fn jes_map_next(buf: &[u8]) -> u32 {
    if buf.len() < 0xFC {
        return 0;
    }
    u24le(buf, 0xFA)
}

/// Whether a JesMap block is valid (signature plus matching chain guard byte
/// at `06`/`FF`).
pub fn valid_jes_map(buf: &[u8]) -> bool {
    buf.len() >= 0x100 && &buf[0..6] == b"JesMap" && buf[0x06] == buf[0xFF]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_conversion_owner_only() {
        // NetFS "WR" = owner write+read -> standard owner write+read bits.
        let attrs = convert_access(0x0C);
        assert_eq!(attrs & ATTR_OWNER_READ, ATTR_OWNER_READ);
        assert_eq!(attrs & ATTR_OWNER_WRITE, ATTR_OWNER_WRITE);
        assert_eq!(attrs & ATTR_PUBLIC_READ, 0);
        assert_eq!(attrs & ATTR_PUBLIC_WRITE, 0);
    }

    #[test]
    fn access_conversion_public_and_dir() {
        // NetFS "wr" public + directory bit.
        let attrs = convert_access(0x23);
        assert_eq!(attrs & ATTR_PUBLIC_READ, ATTR_PUBLIC_READ);
        assert_eq!(attrs & ATTR_PUBLIC_WRITE, ATTR_PUBLIC_WRITE);
        assert_eq!(attrs & ATTR_DIRECTORY, ATTR_DIRECTORY);
    }

    #[test]
    fn seq_geometry_passthrough() {
        assert_eq!(translate(100, 16, Interleave::Seq), 100);
    }

    #[test]
    fn int_and_mux_reorder_tracks() {
        // Track 1 / sector 0 with 16 spt under INT maps to physical track 2.
        assert_eq!(translate(16, 16, Interleave::Int), 32);
        // MUX swaps disk face first.
        assert_eq!(translate(16, 16, Interleave::Mux), 1280);
    }

    #[test]
    fn decodes_date_and_rejects_invalid() {
        // 1990-01-15: year-1981=9 encoded as year_hi=0 (b5-7 of byte0) and
        // year_lo=9 (b4-7 of byte1); day=15 (b0-4 of byte0); month=1 (b0-3
        // of byte1).
        let date = decode_afs_date(0x91_0F);
        assert!(date.is_some());
        assert!(date.unwrap() > 0); // post-1970
        // Month 0 is impossible -> None.
        assert!(decode_afs_date(0x00_05).is_none());
    }

    #[test]
    fn directory_follows_linked_list() {
        // Build a directory using the real linked-list layout: the header's
        // &0 pointer gives the first entry's offset, and each entry chains to
        // the next via its &0 field, terminated by &0000. Entries are placed
        // at 0x1E5 onwards (as real FileServer discs do), not at fixed &11.
        let mut buf = vec![0u8; 0x400];
        buf[0x0F] = 19;
        // First entry pointer (little-endian) at &0.
        let first = 0x1E5usize;
        let mut offsets = Vec::new();
        for i in 0..19 {
            let off = first + i * ENTRY_SIZE;
            offsets.push(off);
            let name = format!("F{i:02}");
            buf[off + 2..off + 2 + name.len()].copy_from_slice(name.as_bytes());
            buf[off + 0x0C..off + 0x10].copy_from_slice(&1u32.to_le_bytes());
            buf[off + 0x14] = 0x0C; // owner WR
        }
        for i in 0..19 {
            let next = if i + 1 < 19 { offsets[i + 1] } else { 0 };
            buf[offsets[i]..offsets[i] + 2].copy_from_slice(&(next as u16).to_le_bytes());
        }
        buf[0..2].copy_from_slice(&(first as u16).to_le_bytes());

        let dec = decode_directory(&buf);
        assert!(!dec.is_broken, "{:?}", dec.anomalies);
        assert_eq!(dec.entries.len(), 19);
        assert_eq!(dec.entries[0].name, "F00");
        assert_eq!(dec.entries[18].name, "F18");
    }

    #[test]
    fn directory_zero_first_pointer_with_entries_is_broken() {
        let mut buf = vec![0u8; 0x400];
        buf[0x0F] = 3; // entries declared but &0 pointer is zero
        let dec = decode_directory(&buf);
        assert!(dec.is_broken);
    }
}
