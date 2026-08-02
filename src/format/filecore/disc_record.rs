use winnow::Parser;
use winnow::binary::{le_u16, le_u32, u8 as bin_u8};
use winnow::combinator::seq;
use winnow::error::ContextError;
use winnow::token::take;

use crate::error::{FcError, Result};
use crate::format::filecore::checksums::boot_block_checksum;

/// The FileCore disc record (guide §2.1), always parsed as the full 60-byte
/// extended form; on pre-3.6 media the extended fields simply read as zero.
///
/// Fields marked "not consumed elsewhere" are parsed because they're part
/// of the fixed on-disk layout (skipping them would misalign every field
/// after), but nothing in this extractor's read-only, translation-only
/// pipeline needs their value - they're kept and documented anyway so the
/// struct is a complete, accurate description of the real structure, not
/// just the subset this tool happens to act on. `low_sector` was the one
/// exception found the hard way: parsed-but-unused turned out to mean
/// "should have been read" rather than "genuinely irrelevant" - see
/// `sequential_track_order` below and `format::filecore::mod::open`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscRecord {
    /// Log₂ of sector size in bytes (8 = 256, 9 = 512, 10 = 1024, ...).
    pub log2_sector_size: u8,
    /// Sectors per track (physical geometry) - not consumed elsewhere;
    /// extraction resolves file locations via the zone map/old free-space
    /// map, not by walking physical geometry directly.
    pub sectors_per_track: u8,
    /// Number of disc surfaces - but *n-1* on old ADFS floppy formats
    /// (`hdr/FileCore`). Not consumed elsewhere (old-map floppies have no
    /// disc record at all; new-map extraction doesn't need it).
    pub heads: u8,
    /// Encoding density (0 = hard disc, 1 = single, 2 = double, 3 =
    /// double+, 4 = quad, 8 = octal). Not consumed elsewhere - informational
    /// only for this tool's purposes.
    pub density: u8,
    /// Fragment ID width in bits, i.e. how many low bits of each SIN select
    /// a fragment rather than a sharing offset. 0 on old-map discs (which
    /// have no disc record and so no real `idlen` - `is_old_map()` treats a
    /// parsed value of 0 as the old-map signal for that reason). Max 15
    /// (new map), 19 (big map), or 21 (RISC OS 5).
    pub idlen: u8,
    /// Log₂ of bytes per map bit - the allocation unit size (`bpmb()`).
    /// Real media only ever uses 7-10; see `looks_plausible`'s bound on
    /// this field before it's used as a shift amount.
    pub log2_bpmb: u8,
    /// Track-to-track sector skew for head positioning (a write-time/
    /// formatting concern). Not consumed elsewhere - extraction never
    /// seeks a physical drive, only reads a disc image file.
    pub skew: u8,
    /// Boot action (0 = none, 1 = load, 2 = run, 3 = exec).
    pub boot_option: u8,
    /// Bits 0-5 (`DiscRecord_LowSector_Mask`): lowest sector number on a
    /// track - not consumed elsewhere. Bit 6
    /// (`DiscRecord_SequenceSides_Flag`) is read via
    /// [`sequential_track_order`](DiscRecord::sequential_track_order). Bit
    /// 7 (`DiscRecord_DoubleStep_Flag`, double stepping) is not consumed
    /// elsewhere.
    pub low_sector: u8,
    /// Low byte of the zone count; combine with `nzones_hi` via `nzones()`.
    pub nzones_lo: u8,
    /// Bits in each zone after zone 0's 32-bit header that aren't
    /// allocation-map bits (trailing slack, guide §3.1/SPEC-ERRATA item 2 -
    /// the Glossary's "leading slack" reading is wrong).
    pub zone_spare: u16,
    /// Disc address of the root directory (guide §2.3).
    pub root_dir: u32,
    /// Total disc size in bytes, low 32 bits (`disc_size_bytes()` combines
    /// this with `disc_size_2` for discs over 4 GB).
    pub disc_size: u32,
    /// Cycle ID, incremented on each write to the disc structure. Not
    /// consumed elsewhere - this tool never writes.
    pub disc_id: u16,
    /// Space-padded disc name; decode via `disc_name_str()`.
    pub disc_name: [u8; 10],
    /// Filing system number. Not consumed elsewhere - always FileCore in
    /// context here.
    pub disc_type: u32,
    /// High 32 bits of disc size, for discs over 4 GB.
    pub disc_size_2: u32,
    /// Log₂ of sharing granularity in sectors; combined with sector size
    /// via `sharing_unit()` to resolve a SIN's non-zero sharing offset.
    pub share_size: u8,
    /// Bit 0: set if the RISC OS partition is over 512 MB
    /// (`DiscRecord_BigMap_BigFlag`). Bits 1-7 reserved. Not consumed
    /// elsewhere - `is_big_dir()` uses `format_version` instead, which is
    /// the more direct signal for this tool's purposes.
    pub big_flag: u8,
    /// High byte of the zone count; see `nzones_lo`/`nzones()`.
    pub nzones_hi: u8,
    /// Disc format version (`DiscRecord_BigDir_DiscVersion`): 0 = old/new
    /// directories, 1 = big directories. See `is_big_dir()`.
    pub format_version: u32,
    /// Root directory size in bytes (`DiscRecord_BigDir_RootDirSize`),
    /// meaningful only when `format_version` selects big directories -
    /// only big directories store their own size here rather than it being
    /// implied by a fixed format (`SMALL_DIR_SIZE`/`LARGE_DIR_SIZE`).
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

    /// `DiscRecord_SequenceSides_Flag` (guide §1.1/§2.1): true if this
    /// disc's tracks are numbered sequentially (all of side 0, then all of
    /// side 1), false if interleaved (side 0 and side 1 alternate per
    /// track). *Not* implied by map type despite the correlation described
    /// in §1.1 - old-map floppies (which have no disc record at all) are
    /// sequential by convention, and new-map discs are interleaved by
    /// convention, but the guide is explicit that a new-map disc's actual
    /// ordering must be read from this bit, not assumed. See
    /// `format::filecore::mod::open` for where that matters.
    pub fn sequential_track_order(&self) -> bool {
        self.low_sector & 0x40 != 0
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
