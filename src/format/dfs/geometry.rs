//! Acorn DFS physical sector geometry.
//!
//! Single-sided (`.ssd`) images address sectors sequentially, so a catalogue
//! byte address is already a valid file offset. Double-sided (`.dsd`)
//! images interleave the two sides *per track*: side 0's track N and side
//! 1's track N are adjacent in the image file, then the drive head steps to
//! track N+1. This isn't stated in the mdfs.net DFS reference; it was
//! confirmed against a real `.dsd` (`MikeJames_...bbc80ds_scp.dsd`), whose
//! side-1 catalogue lands at absolute byte `0xA00`, exactly where this
//! formula predicts it (side 0's track 0 occupies physical sectors 0-9,
//! side 1's track 0 occupies physical sectors 10-19 - i.e. bytes
//! `0xA00..0xC00`) - before any DFS-specific code was written against it.
//! This is the same class of "logical vs physical" gap as `sml_geometry.rs`
//! for FileCore S/M/L, so it's treated as a known hazard here rather than a
//! surprise: a logically-contiguous file that spans a track boundary is
//! *not* contiguous in a `.dsd` file.

use crate::model::object::Extent;

const DFS_SECTORS_PER_TRACK: u64 = 10;
const DFS_SECTOR_SIZE: u64 = 256;

#[derive(Debug, Clone, Copy)]
pub struct DfsGeometry {
    pub double_sided: bool,
}

impl DfsGeometry {
    fn physical_sector(side: u8, logical_sector: u64) -> u64 {
        let track = logical_sector / DFS_SECTORS_PER_TRACK;
        let sector_in_track = logical_sector % DFS_SECTORS_PER_TRACK;
        sector_in_track + (track * 2 + side as u64) * DFS_SECTORS_PER_TRACK
    }

    /// Translates a logically-contiguous byte range on one side into one or
    /// more physical extents in the image file, splitting at logical track
    /// boundaries. `side` is ignored (and must be 0) for single-sided images.
    pub fn translate(&self, side: u8, logical_addr: u64, len: u64) -> Vec<Extent> {
        if len == 0 {
            return Vec::new();
        }
        if !self.double_sided {
            debug_assert_eq!(side, 0, "single-sided image has only side 0");
            return vec![Extent { disc_addr: logical_addr, len }];
        }

        let mut extents = Vec::new();
        let mut remaining = len;
        let mut addr = logical_addr;
        while remaining > 0 {
            let logical_sector = addr / DFS_SECTOR_SIZE;
            let offset_in_sector = addr % DFS_SECTOR_SIZE;
            let sector_in_track = logical_sector % DFS_SECTORS_PER_TRACK;
            let bytes_to_track_end =
                (DFS_SECTORS_PER_TRACK - sector_in_track) * DFS_SECTOR_SIZE - offset_in_sector;
            let chunk_len = remaining.min(bytes_to_track_end);

            let phys_sector = Self::physical_sector(side, logical_sector);
            let phys_addr = phys_sector * DFS_SECTOR_SIZE + offset_in_sector;
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
            && last.disc_addr + last.len == e.disc_addr
        {
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
    fn single_sided_is_identity() {
        let geom = DfsGeometry { double_sided: false };
        let extents = geom.translate(0, 0x0E00, 512);
        assert_eq!(extents, vec![Extent { disc_addr: 0x0E00, len: 512 }]);
    }

    #[test]
    fn double_sided_track0_side0_is_unaffected() {
        let geom = DfsGeometry { double_sided: true };
        // sector 0, side 0: physical sector 0, matches logical.
        let extents = geom.translate(0, 0, 256);
        assert_eq!(extents, vec![Extent { disc_addr: 0, len: 256 }]);
    }

    #[test]
    fn double_sided_side1_catalogue_lands_at_0xa00() {
        // Confirmed against a real .dsd image: side 1's catalogue sector 0
        // (a whole-sector read from logical addr 0) is at physical byte
        // 0xA00, and sector 1 (logical addr 0x100) at 0xB00.
        let geom = DfsGeometry { double_sided: true };
        assert_eq!(geom.translate(1, 0x000, 256), vec![Extent { disc_addr: 0xA00, len: 256 }]);
        assert_eq!(geom.translate(1, 0x100, 256), vec![Extent { disc_addr: 0xB00, len: 256 }]);
    }

    #[test]
    fn watford_extension_side1_lands_at_0xc00() {
        // The Watford 62-file extension catalogue (logical sectors 2/3) on
        // side 1 of a real .dsd was found at absolute 0xC00/0xD00.
        let geom = DfsGeometry { double_sided: true };
        assert_eq!(geom.translate(1, 0x200, 256), vec![Extent { disc_addr: 0xC00, len: 256 }]);
        assert_eq!(geom.translate(1, 0x300, 256), vec![Extent { disc_addr: 0xD00, len: 256 }]);
    }

    #[test]
    fn splits_at_track_boundary_on_side0() {
        let geom = DfsGeometry { double_sided: true };
        // starts 1 sector before end of track 0 (10 sectors/track), runs 3
        // sectors into track 1.
        let start = 9 * DFS_SECTOR_SIZE;
        let len = 4 * DFS_SECTOR_SIZE;
        let extents = geom.translate(0, start, len);
        assert_eq!(extents.len(), 2, "{extents:?}");
        assert_eq!(extents[0], Extent { disc_addr: 9 * DFS_SECTOR_SIZE, len: DFS_SECTOR_SIZE });
        // track1 side0 sectors 0,1,2 -> physical sector = 0 + (1*2+0)*10 = 20
        assert_eq!(extents[1], Extent { disc_addr: 20 * DFS_SECTOR_SIZE, len: 3 * DFS_SECTOR_SIZE });
    }

    #[test]
    fn splits_at_track_boundary_on_side1() {
        let geom = DfsGeometry { double_sided: true };
        let start = 9 * DFS_SECTOR_SIZE;
        let len = 4 * DFS_SECTOR_SIZE;
        let extents = geom.translate(1, start, len);
        assert_eq!(extents.len(), 2, "{extents:?}");
        // track0 side1 sector9 -> physical sector = 9 + (0*2+1)*10 = 19
        assert_eq!(extents[0], Extent { disc_addr: 19 * DFS_SECTOR_SIZE, len: DFS_SECTOR_SIZE });
        // track1 side1 sectors 0,1,2 -> physical sector = 0 + (1*2+1)*10 = 30
        assert_eq!(extents[1], Extent { disc_addr: 30 * DFS_SECTOR_SIZE, len: 3 * DFS_SECTOR_SIZE });
    }
}
