//! S/M/L (old-map, old-directory) physical geometry translation.
//!
//! Guide §1.1 states S/M/L uses *sequential* logical track ordering
//! (`sector + track×16 + side×(tracks×16)`) while D/E/F uses *interleaved*
//! ordering (`sector + (track×2+side)×sectors_per_track`), and frames this
//! purely as "how a driver seeks the physical drive". It does not state -
//! and this was missed entirely on first implementation, only caught by
//! testing against a real `adfs640L.adl` image - that **the `.ADL`/`.ADF`
//! image file's own byte layout is always the interleaved one**, even for
//! S/M/L. A logical disc address (as encoded as a "sector number" in an
//! old-map SIN field, which the rest of the spec treats as a plain byte
//! offset) therefore needs converting from sequential to interleaved
//! sector order before it's a valid offset into the image file - except
//! for anything within track 0, where the two orderings coincide, which is
//! why this was easy to miss: the root directory (fixed at `0x200`) and
//! any file placed early on disk read correctly either way.
//!
//! Because interleaving reorders whole tracks, a logically-contiguous file
//! that spans a track boundary is *not* contiguous in the image file, so
//! translation can yield multiple extents from one logical run.

use crate::model::object::Extent;

const SML_SECTORS_PER_TRACK: u64 = 16;
const SML_SECTOR_SIZE: u64 = 256;

#[derive(Debug, Clone, Copy)]
pub struct SmlGeometry {
    pub tracks_per_side: u64,
    pub heads: u64,
}

impl SmlGeometry {
    /// Old-map floppies have no disc record (guide §2.1: "the disc record
    /// concept doesn't exist"), so geometry isn't stored anywhere on disk -
    /// it's implicit in the format letter (§1.1's format matrix: S = 1
    /// side/40 tracks, M = 1 side/80 tracks, L = 2 sides/80 tracks).
    /// Inferred here from the old free-space map's total sector count
    /// (640/1280/2560 respectively), which is the only on-disk source of
    /// disc size for this format.
    pub fn from_total_sectors(total_sectors: u64) -> Self {
        match total_sectors {
            640 => Self { tracks_per_side: 40, heads: 1 },
            1280 => Self { tracks_per_side: 80, heads: 1 },
            2560 => Self { tracks_per_side: 80, heads: 2 },
            _ => Self {
                tracks_per_side: (total_sectors / SML_SECTORS_PER_TRACK).max(1),
                heads: 1,
            },
        }
    }

    fn physical_sector(&self, logical_sector: u64) -> u64 {
        let sectors_per_side = self.tracks_per_side * SML_SECTORS_PER_TRACK;
        let side = logical_sector / sectors_per_side.max(1);
        let rem = logical_sector % sectors_per_side.max(1);
        let track = rem / SML_SECTORS_PER_TRACK;
        let sector_in_track = rem % SML_SECTORS_PER_TRACK;
        sector_in_track + (track * self.heads + side) * SML_SECTORS_PER_TRACK
    }

    /// Translates a logically-contiguous byte range (sequential addressing)
    /// into one or more physical extents in the image file (interleaved
    /// addressing), splitting at logical track boundaries.
    pub fn translate(&self, logical_addr: u64, len: u64) -> Vec<Extent> {
        if len == 0 {
            return Vec::new();
        }
        let mut extents = Vec::new();
        let mut remaining = len;
        let mut addr = logical_addr;
        while remaining > 0 {
            let logical_sector = addr / SML_SECTOR_SIZE;
            let offset_in_sector = addr % SML_SECTOR_SIZE;
            let sector_in_track = logical_sector % SML_SECTORS_PER_TRACK;
            let bytes_to_track_end =
                (SML_SECTORS_PER_TRACK - sector_in_track) * SML_SECTOR_SIZE - offset_in_sector;
            let chunk_len = remaining.min(bytes_to_track_end);

            let phys_sector = self.physical_sector(logical_sector);
            let phys_addr = phys_sector * SML_SECTOR_SIZE + offset_in_sector;
            extents.push(Extent { disc_addr: phys_addr, len: chunk_len });

            addr += chunk_len;
            remaining -= chunk_len;
        }
        merge_adjacent(extents)
    }
}

fn merge_adjacent(extents: Vec<Extent>) -> Vec<Extent> {
    let mut out: Vec<Extent> = Vec::new();
    for e in extents {
        if let Some(last) = out.last_mut()
            && last.disc_addr + last.len == e.disc_addr {
                last.len += e.len;
                continue;
            }
        out.push(e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track0_is_unaffected() {
        let geom = SmlGeometry::from_total_sectors(2560); // L
        let extents = geom.translate(2 * 256, 5 * 256); // root dir: sectors 2-6
        assert_eq!(extents, vec![Extent { disc_addr: 2 * 256, len: 5 * 256 }]);
    }

    #[test]
    fn matches_real_adfs640l_hostfs_txt() {
        // Empirically confirmed against a real adfs640L.adl image: logical
        // sector 26 (track 1) has real content at physical sector 42.
        let geom = SmlGeometry::from_total_sectors(2560); // L: 80 tracks, 2 heads
        let extents = geom.translate(26 * 256, 104);
        assert_eq!(extents, vec![Extent { disc_addr: 42 * 256, len: 104 }]);
    }

    #[test]
    fn splits_at_track_boundary() {
        let geom = SmlGeometry::from_total_sectors(2560);
        // starts 1 sector before the end of track 0, runs 3 sectors into track 1
        let start = 15 * 256;
        let len = 4 * 256;
        let extents = geom.translate(start, len);
        assert_eq!(extents.len(), 2, "{extents:?}");
        assert_eq!(extents[0], Extent { disc_addr: 15 * 256, len: 256 });
        // track1 sectors 0,1,2 -> physical sector = 0 + (1*2+0)*16 = 32
        assert_eq!(extents[1], Extent { disc_addr: 32 * 256, len: 3 * 256 });
    }
}
