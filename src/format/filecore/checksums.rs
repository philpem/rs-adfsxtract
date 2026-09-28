//! Checksum algorithms from the FileCore guide. Each on-disk structure uses
//! a different algorithm; see guide §2.2, §2.5, §A.1, §A.2.

/// Boot block checksum (guide §2.2): ascending add-with-carry over bytes
/// `0x000..0x1FE` (511 bytes), stored at `+0x1FF`.
pub fn boot_block_checksum(bb: &[u8]) -> u8 {
    debug_assert!(bb.len() >= 0x200);
    let mut sum: u32 = 0;
    for &b in &bb[..0x1FF] {
        sum += b as u32;
        if sum > 255 {
            sum = (sum + 1) & 255;
        }
    }
    (sum & 0xFF) as u8
}

/// Old free-space map sector checksum (guide §2.5): starts at 255, sums
/// bytes 254 down to 0 (descending), carry propagated *before* each add.
/// Covers everything except the checksum byte itself at `+0xFF`.
pub fn old_map_checksum(sector: &[u8]) -> u8 {
    debug_assert!(sector.len() >= 256);
    let mut sum: u32 = 255;
    for a in (0..=254).rev() {
        if sum > 255 {
            sum = (sum + 1) & 255;
        }
        sum += sector[a] as u32;
    }
    (sum & 0xFF) as u8
}

/// New-map zone `ZoneCheck` (guide §A.1): add-with-carry running sum of the
/// sector's 32-bit words in reverse order, with the accumulated carry
/// discarded; then subtract the stored check byte and fold the 32-bit
/// result to 8 bits via two sequential XOR-shifts. Summation order matters
/// and a byte-at-a-time or one's-complement pass will not reproduce it.
pub fn zone_check(sector: &[u8]) -> u8 {
    let mut sum: u32 = 0;
    let mut carry: u32 = 0;
    for chunk in sector.as_chunks::<4>().0.iter().rev() {
        let word = u32::from_le_bytes(*chunk);
        let (s1, c1) = sum.overflowing_add(word);
        let (s2, c2) = s1.overflowing_add(carry);
        sum = s2;
        carry = (c1 || c2) as u32;
    }
    let mut s = sum;
    s = s.wrapping_sub(sector[0] as u32);
    s ^= s >> 16;
    s ^= s >> 8;
    (s & 0xFF) as u8
}

fn rotr13(x: u32) -> u32 {
    x.rotate_right(13)
}

/// One region to accumulate into a directory check byte. `words_first`
/// selects which of the two orderings guide §A.2 describes: old/new
/// directories process whole-words-then-leftover-bytes for the entry
/// region, but leftover-bytes-then-whole-words for the tail region (having
/// skipped the tail's leading end-marker byte first).
pub struct ChecksumRegion {
    pub start: usize,
    pub end: usize,
    pub words_first: bool,
}

/// Directory check byte accumulator (guide §A.2): rotate(13)-XOR over the
/// given regions, each processed as whole 32-bit LE words plus leftover
/// bytes at a boundary aligned to the start of `data` (offset 0), in the
/// order `words_first` specifies. Caller folds and takes `& 0xFF`.
pub fn dir_checksum_accumulate(data: &[u8], regions: &[ChecksumRegion]) -> u32 {
    let mut checksum: u32 = 0;
    for region in regions {
        let (start, end) = (region.start, region.end);
        if region.words_first {
            let word_end = start + (end - start) / 4 * 4;
            let mut off = start;
            while off < word_end {
                let w = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
                checksum = w ^ rotr13(checksum);
                off += 4;
            }
            for &b in &data[word_end..end] {
                checksum = (b as u32) ^ rotr13(checksum);
            }
        } else {
            let aligned = (start.div_ceil(4) * 4).min(end);
            for &b in &data[start..aligned] {
                checksum = (b as u32) ^ rotr13(checksum);
            }
            let mut off = aligned;
            while off + 4 <= end {
                let w = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
                checksum = w ^ rotr13(checksum);
                off += 4;
            }
        }
    }
    checksum
}

/// Fold a 32-bit directory checksum accumulator to the final check byte.
/// Must be two *sequential* steps (guide §A.2) - collapsing to one
/// expression silently drops the `>>24` term.
pub fn dir_checksum_fold(mut checksum: u32) -> u8 {
    checksum ^= checksum >> 16;
    checksum ^= checksum >> 8;
    (checksum & 0xFF) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_block_checksum_known_value() {
        // Empty defect list word 0x20000000 at offset 0, rest zero.
        let mut bb = vec![0u8; 512];
        bb[0..4].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        let chk = boot_block_checksum(&bb);
        // sum of bytes 0..0x1FE = 0x20 (byte 3 of the LE word, the rest 0)
        assert_eq!(chk, 0x20);
    }

    #[test]
    fn old_map_checksum_matches_reference_algorithm() {
        let mut sector = [0u8; 256];
        for (i, b) in sector.iter_mut().enumerate().take(255) {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        let mut sum: u32 = 255;
        for a in (0..=254).rev() {
            if sum > 255 {
                sum = (sum + 1) & 255;
            }
            sum += sector[a] as u32;
        }
        assert_eq!(old_map_checksum(&sector), (sum & 0xFF) as u8);
    }

    #[test]
    fn zone_check_self_consistent() {
        let mut sector = vec![0u8; 512];
        sector[1] = 0x18;
        sector[2] = 0x80;
        sector[3] = 0x42;
        for (i, b) in sector.iter_mut().enumerate().skip(4) {
            *b = ((i * 31) & 0xFF) as u8;
        }
        let chk = zone_check(&sector);
        sector[0] = chk;
        assert_eq!(zone_check(&sector), chk);
    }

    /// Golden value from an independent Python re-implementation of the
    /// ADCS add-with-carry loop (not derived from this Rust code), over a
    /// sector whose running sum crosses 2^32 partway through - self-
    /// consistency alone (above) can't distinguish the correct
    /// add-with-carry accumulation from the simpler wrapping-sum bug this
    /// caught only when checked against real disc images
    /// (`adfs1600F.adf`/`adfs800E.adf` had `ZoneCheck` mismatches until
    /// this was fixed).
    #[test]
    fn zone_check_matches_independent_reference() {
        let mut sector = vec![0u8; 512];
        sector[1] = 0x18;
        sector[2] = 0x80;
        sector[3] = 0x42;
        for (i, b) in sector.iter_mut().enumerate().skip(4) {
            *b = ((i * 31) & 0xFF) as u8;
        }
        assert_eq!(zone_check(&sector), 0xa0);
    }
}
