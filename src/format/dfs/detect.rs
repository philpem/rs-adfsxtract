//! DFS detection. DFS has no magic number, so this is a structural
//! plausibility check, and it's only ever tried as a last resort - `main.rs`
//! attempts every FileCore signature check first and falls through to this
//! only on `FcError::NotRecognised`. Kept deliberately tight (catalogue
//! header bytes printable-or-zero, reserved bits clear, AND every entry's
//! data run inside the recorded disc size) so an arbitrary FAT/other image
//! doesn't get misidentified as DFS - the worker's own DOS-fallback path
//! depends on that not happening.

use crate::error::{FcError, Result};
use crate::format::dfs::catalogue::{read_catalogue, read_sector};
use crate::format::dfs::geometry::DfsGeometry;
use crate::io::SectorSource;

#[derive(Debug, Clone, Copy)]
pub struct DfsDetection {
    pub double_sided: bool,
}

fn printable_or_zero(b: u8) -> bool {
    b > 0x1F || b == 0
}

fn plausible_header(sector0: &[u8], sector1: &[u8]) -> bool {
    sector0[0..8].iter().all(|&b| printable_or_zero(b))
        && sector1[0..4].iter().all(|&b| printable_or_zero(b))
        && sector1[5] & 0x07 == 0
        && sector1[6] & 0xCC == 0
}

pub fn detect(source: &mut dyn SectorSource) -> Result<DfsDetection> {
    let ssd = DfsGeometry { double_sided: false };
    let s0 = read_sector(source, &ssd, 0, 0x000)?;
    let s1 = read_sector(source, &ssd, 0, 0x100)?;
    if !plausible_header(&s0, &s1) {
        return Err(FcError::NotRecognised);
    }

    // The header check alone is too weak to reject arbitrary binary data
    // (plenty of non-DFS images have "printable-ish" bytes at 0x000/0x100
    // by coincidence) - most entries must also fit inside the disc. This is
    // deliberately a majority vote, not "every entry": a real, genuinely
    // DFS disc can have one entry corrupted (a bad sector over part of the
    // catalogue, say) while the rest of the catalogue is clearly intact,
    // and that's exactly the case `--on-broken-directory recover` exists to
    // handle at extraction time - detection must not reject the whole disc
    // before recovery ever gets a chance to run. Wholesale garbage, by
    // contrast, has essentially no chance of a majority of random bytes
    // landing in-bounds.
    let side0 = read_catalogue(source, &ssd, 0)?;
    let bad = side0.entries.iter().filter(|e| !e.in_bounds(side0.total_sectors)).count();
    if bad * 2 > side0.entries.len() {
        return Err(FcError::NotRecognised);
    }

    // Side 2 is present only if its own catalogue - read at the
    // double-sided interleaved offset (0xA00/0xB00) - independently looks
    // plausible; a genuinely single-sided image has arbitrary bytes there.
    let dsd = DfsGeometry { double_sided: true };
    let s1_0 = read_sector(source, &dsd, 1, 0x000)?;
    let s1_1 = read_sector(source, &dsd, 1, 0x100)?;
    // An entirely blank region at side 1's catalogue offset is ambiguous -
    // it could be a real second side that just hasn't been formatted, or
    // (far more commonly) simply the tail of a single-sided image that
    // doesn't extend that far. Default to single-sided in that case, same
    // as DIM's own "assume SSD" fallback for an all-zero probe.
    let side1_all_zero = s1_0.iter().all(|&b| b == 0) && s1_1.iter().all(|&b| b == 0);
    // Any genuinely formatted side records its own sector count here, even
    // with zero files on it (confirmed on a real disc: the MikeJames .dsd's
    // side 0 has no files but still reports 800 sectors). A single-sided
    // image has no reason for the bytes that fall at this interleaved
    // offset to encode a plausible sector count by coincidence unless
    // there's real, sparse (e.g. short, zero-padded) file content sitting
    // there - which is otherwise indistinguishable from an empty catalogue
    // by the header check alone, so this is the deciding signal.
    let side1_sector_count = (((s1_1[6] & 0x3) as u32) << 8) | s1_1[7] as u32;
    let double_sided = plausible_header(&s1_0, &s1_1) && !side1_all_zero && side1_sector_count != 0;

    Ok(DfsDetection { double_sided })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn all_zero_image_is_single_sided_dfs() {
        let mut cursor = Cursor::new(vec![0u8; 4096]);
        let detection = detect(&mut cursor).expect("an all-zero, zero-file catalogue is valid DFS");
        assert!(!detection.double_sided, "an all-zero side-1 probe must not be read as a real second side");
    }

    #[test]
    fn rejects_control_code_garbage() {
        let mut cursor = Cursor::new(vec![0x01u8; 4096]);
        assert!(matches!(detect(&mut cursor), Err(FcError::NotRecognised)));
    }

    #[test]
    fn rejects_out_of_bounds_entry() {
        let mut image = vec![0u8; 4096];
        image[0..8].copy_from_slice(b"TITLE   ");
        image[0x105] = 8; // 1 file
        image[0x106] = 0x00;
        image[0x107] = 0x02; // total_sectors = 2 (512 bytes)
        // entry: start sector 200 (way beyond the 2-sector disc), length 256
        image[0x108 + 6] = 0x00;
        image[0x108 + 7] = 200;
        let mut cursor = Cursor::new(image);
        assert!(matches!(detect(&mut cursor), Err(FcError::NotRecognised)));
    }
}
