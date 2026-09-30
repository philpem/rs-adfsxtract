//! New (zone bitstream) map (guide §2.4, §3.1, §3.2).

use std::collections::HashMap;

use crate::error::Result;
use crate::format::filecore::checksums::zone_check;
use crate::format::filecore::disc_record::DiscRecord;
use crate::io::BitReader;
use crate::io::SectorSource;
use crate::model::object::Extent;

#[derive(Debug, Clone, Copy)]
pub struct ZoneHeader {
    pub zone_check: u8,
    pub free_link: u16,
    pub cross_check: u8,
}

pub fn parse_zone_header(sector: &[u8]) -> ZoneHeader {
    ZoneHeader {
        zone_check: sector[0],
        free_link: u16::from_le_bytes([sector[1], sector[2]]),
        cross_check: sector[3],
    }
}

fn header_bits(zone: usize) -> usize {
    if zone == 0 { 64 * 8 } else { 4 * 8 }
}

/// Real allocation-unit coverage of a zone (guide §2.4): zone 0 loses an
/// extra 480 bits to its embedded 60-byte disc record copy.
fn zone_bits(zone: usize, sector_size: u64, zone_spare: u64) -> u64 {
    let base = sector_size * 8 - zone_spare;
    if zone == 0 {
        base.saturating_sub(480)
    } else {
        base
    }
}

/// Cumulative allocation-unit base for each zone (guide §2.4): also used to
/// locate the map's own disc address, since it's stored starting at the
/// disc address of zone `nzones/2` in its own numbering.
pub fn zone_base_units(dr: &DiscRecord) -> Vec<u64> {
    let sector_size = dr.sector_size() as u64;
    let zone_spare = dr.zone_spare as u64;
    let nzones = dr.nzones() as usize;
    let mut bases = Vec::with_capacity(nzones);
    let mut acc = 0u64;
    for z in 0..nzones {
        bases.push(acc);
        acc += zone_bits(z, sector_size, zone_spare);
    }
    bases
}

pub fn map_disc_addr(dr: &DiscRecord) -> u64 {
    let bases = zone_base_units(dr);
    let map_zone = dr.nzones() as usize / 2;
    bases[map_zone] * dr.bpmb() as u64
}

#[derive(Debug, Clone)]
struct FragmentRecord {
    zone: u32,
    id: u32,
    start_unit: u64,
    len_units: u64,
    is_free: bool,
}

fn free_chain_positions(free_link_raw: u16, idlen: u32, reader: &BitReader) -> Vec<usize> {
    if free_link_raw == 0x8000 {
        return Vec::new();
    }
    let masked = (free_link_raw & 0x7FFF) as usize;
    let mut positions = Vec::new();
    let mut pos = 8 + masked;
    loop {
        if positions.len() > 100_000 || positions.contains(&pos) {
            break;
        }
        positions.push(pos);
        let id = match reader.bits(pos, idlen) {
            Some(v) => v,
            None => break,
        };
        if id == 0 {
            break;
        }
        pos += id as usize;
    }
    positions
}

/// Decodes the fragment bitstream across all zones (guide §3.1), including
/// the (spec-flagged-untested) cross-zone `pending_span` continuation and
/// free-chain exclusion (§3.2). Pure function over already-read zone
/// sector bytes, for unit testing without a `SectorSource`.
fn decode_all_zones(zones_raw: &[Vec<u8>], dr: &DiscRecord) -> Vec<FragmentRecord> {
    let idlen = dr.idlen as u32;
    let sector_size = dr.sector_size() as usize;
    let zone_spare = dr.zone_spare as u64;
    let nzones = zones_raw.len();

    let mut records: Vec<FragmentRecord> = Vec::new();
    let mut pending_span: Option<usize> = None;
    let mut free_starts_by_zone: Vec<Vec<usize>> = Vec::with_capacity(nzones);

    for (z, data) in zones_raw.iter().enumerate() {
        let reader = BitReader::new(data);
        let hbits = header_bits(z);
        let zone_end = sector_size * 8;
        let zbits = zone_bits(z, sector_size as u64, zone_spare) as usize;
        let extent_end = hbits + zbits;
        let mut bit_pos = hbits;

        let header = parse_zone_header(data);
        free_starts_by_zone.push(free_chain_positions(header.free_link, idlen, &reader));

        if let Some(rec_idx) = pending_span.take() {
            let span_start = bit_pos;
            while bit_pos < zone_end && (bit_pos - span_start) < zone_spare as usize {
                let b = reader.bit(bit_pos).unwrap_or(1);
                bit_pos += 1;
                if b == 1 {
                    break;
                }
            }
            records[rec_idx].len_units += (bit_pos - span_start) as u64;
        }

        while bit_pos < zone_end {
            let frag_start_bit = bit_pos;
            let id = match reader.bits(bit_pos, idlen) {
                Some(v) => v,
                None => break,
            };
            bit_pos += idlen as usize;
            let mut found_terminator = false;
            while bit_pos < zone_end {
                let b = reader.bit(bit_pos).unwrap();
                bit_pos += 1;
                if b == 1 {
                    found_terminator = true;
                    break;
                }
            }
            let frag_len_bits = (bit_pos - frag_start_bit) as u64;
            let start_unit = (frag_start_bit - hbits) as u64;

            if !found_terminator {
                if frag_start_bit >= extent_end {
                    break; // pure slack, not a real fragment
                }
                records.push(FragmentRecord {
                    zone: z as u32,
                    id: id as u32,
                    start_unit,
                    len_units: frag_len_bits,
                    is_free: false,
                });
                if z + 1 < nzones {
                    pending_span = Some(records.len() - 1);
                }
                break;
            }
            records.push(FragmentRecord {
                zone: z as u32,
                id: id as u32,
                start_unit,
                len_units: frag_len_bits,
                is_free: false,
            });
        }
    }

    for rec in &mut records {
        let hbits = header_bits(rec.zone as usize);
        let frag_start_bit = hbits + rec.start_unit as usize;
        if free_starts_by_zone[rec.zone as usize].contains(&frag_start_bit) {
            rec.is_free = true;
        }
    }

    records
}

#[derive(Debug, Clone)]
pub struct NewMapIndex {
    pub map_addr: u64,
    pub zone_check_ok: Vec<bool>,
    pub cross_check_xor: u8,
    /// The disc record copy embedded in zone 0's map sector, preferred over
    /// the boot block copy for metadata (guide §A.4: the boot block copy's
    /// extended fields - disc_id/disc_name/disc_type - are zeroed on real
    /// media, while zone 0's copy carries the real values).
    pub zone0_disc_record: DiscRecord,
    fragment_index: HashMap<u32, Vec<Extent>>,
}

impl NewMapIndex {
    /// All extents (in bit-position, then zone, order) for a fragment ID.
    /// Excludes free-chain fragments and never includes ID 0/1/2's
    /// bookkeeping semantics beyond returning whatever matched literally.
    pub fn extents_for(&self, fragment_id: u32) -> Option<&[Extent]> {
        self.fragment_index.get(&fragment_id).map(|v| v.as_slice())
    }

    /// A fragment-less index, for tests in sibling modules (e.g. `dir_big`)
    /// that need *a* `NewMapIndex` to call into but don't exercise fragment
    /// resolution - `fragment_index` has no public constructor otherwise.
    #[cfg(test)]
    pub(crate) fn empty_for_test(dr: DiscRecord) -> Self {
        Self {
            map_addr: 0,
            zone_check_ok: Vec::new(),
            cross_check_xor: 0,
            zone0_disc_record: dr,
            fragment_index: HashMap::new(),
        }
    }
}

/// Resolves a 3-byte SIN (fragment ID in bits 8-23, sharing offset in bits
/// 0-7) to disc extents (guide §3.2). Returns `None` for fragment ID 0
/// (caller must special-case that as a zero-length file before calling, per
/// the guide - it is not "free space" in this context) or an unknown ID.
pub fn resolve_sin(index: &NewMapIndex, sin: u32, sharing_unit: u64) -> Option<Vec<Extent>> {
    let fragment_id = (sin >> 8) & 0xFFFF;
    let sharing_offset = sin & 0xFF;
    resolve_fragment(index, fragment_id, sharing_offset, sharing_unit)
}

pub fn resolve_fragment(
    index: &NewMapIndex,
    fragment_id: u32,
    sharing_offset: u32,
    sharing_unit: u64,
) -> Option<Vec<Extent>> {
    if fragment_id == 0 {
        return None;
    }
    let extents = index.extents_for(fragment_id)?.to_vec();
    if sharing_offset == 0 {
        return Some(extents);
    }
    let skip = (sharing_offset as u64 - 1) * sharing_unit;
    Some(crate::model::object::trim_extents_from(extents, skip))
}

fn build_index(records: &[FragmentRecord], dr: &DiscRecord) -> HashMap<u32, Vec<Extent>> {
    let bases = zone_base_units(dr);
    let bpmb = dr.bpmb() as u64;
    let mut index: HashMap<u32, Vec<Extent>> = HashMap::new();
    for rec in records {
        if rec.is_free || rec.id == 0 {
            continue;
        }
        let disc_addr = (bases[rec.zone as usize] + rec.start_unit) * bpmb;
        let len = rec.len_units * bpmb;
        index
            .entry(rec.id)
            .or_default()
            .push(Extent { disc_addr, len });
    }
    index
}

/// Reads and decodes the whole new map from the image, with per-zone
/// fallback to the backup copy (guide §2.4: "double-copied... allows
/// recovery if one copy is damaged") when the primary copy's `ZoneCheck`
/// doesn't verify.
pub fn read_new_map(source: &mut dyn SectorSource, dr: &DiscRecord) -> Result<NewMapIndex> {
    let sector_size = dr.sector_size() as usize;
    let nzones = dr.nzones() as usize;
    let map_addr = map_disc_addr(dr);

    let mut zones_raw: Vec<Vec<u8>> = Vec::with_capacity(nzones);
    let mut zone_check_ok = Vec::with_capacity(nzones);
    for z in 0..nzones {
        let primary_addr = map_addr + (z as u64) * sector_size as u64;
        let mut primary = vec![0u8; sector_size];
        source.read_at(primary_addr, &mut primary)?;
        let primary_ok = zone_check(&primary) == primary[0];

        if primary_ok {
            zone_check_ok.push(true);
            zones_raw.push(primary);
        } else {
            let backup_addr = map_addr + ((nzones + z) as u64) * sector_size as u64;
            let mut backup = vec![0u8; sector_size];
            source.read_at(backup_addr, &mut backup)?;
            let backup_ok = zone_check(&backup) == backup[0];
            zone_check_ok.push(backup_ok);
            zones_raw.push(if backup_ok { backup } else { primary });
        }
    }

    let cross_check_xor = zones_raw
        .iter()
        .fold(0u8, |acc, z| acc ^ parse_zone_header(z).cross_check);

    let zone0_disc_record =
        crate::format::filecore::disc_record::parse_disc_record(&zones_raw[0][4..64])
            .unwrap_or_else(|_| dr.clone());

    let records = decode_all_zones(&zones_raw, dr);
    let fragment_index = build_index(&records, dr);

    Ok(NewMapIndex {
        map_addr,
        zone_check_ok,
        cross_check_xor,
        zone0_disc_record,
        fragment_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::filecore::disc_record::parse_disc_record;

    fn make_dr(
        sector_size_log2: u8,
        idlen: u8,
        bpmb_log2: u8,
        nzones: u8,
        zone_spare: u16,
    ) -> DiscRecord {
        let mut b = vec![0u8; 60];
        b[0] = sector_size_log2;
        b[4] = idlen;
        b[5] = bpmb_log2;
        b[9] = nzones;
        b[0x0A..0x0C].copy_from_slice(&zone_spare.to_le_bytes());
        parse_disc_record(&b).unwrap()
    }

    fn write_fragment(
        bits: &mut Vec<u8>,
        bit_pos: &mut usize,
        id: u32,
        idlen: u32,
        total_units: u64,
    ) {
        for i in 0..idlen {
            set_bit(bits, *bit_pos, ((id >> i) & 1) as u8);
            *bit_pos += 1;
        }
        let padding = total_units as usize - idlen as usize - 1;
        for _ in 0..padding {
            set_bit(bits, *bit_pos, 0);
            *bit_pos += 1;
        }
        set_bit(bits, *bit_pos, 1);
        *bit_pos += 1;
    }

    fn set_bit(bits: &mut Vec<u8>, pos: usize, val: u8) {
        let byte_idx = pos / 8;
        while bits.len() <= byte_idx {
            bits.push(0);
        }
        if val == 1 {
            bits[byte_idx] |= 1 << (pos % 8);
        }
    }

    #[test]
    fn single_zone_two_fragments() {
        // Zone 0 always has a 64-byte header (4-byte zone header + 60-byte
        // disc record copy), so the sector must be big enough to hold that
        // plus a small allocation bit stream: 128 bytes total.
        let mut sector = vec![0u8; 128];
        sector[1..3].copy_from_slice(&0x8000u16.to_le_bytes()); // FreeLink: no free space
        let mut bit_pos = 64 * 8;
        write_fragment(&mut sector, &mut bit_pos, 5, 4, 10); // fragment id=5, 10 units
        write_fragment(&mut sector, &mut bit_pos, 0, 4, 118); // rest free (id 0)

        let dr = make_dr(7, 4, 7, 1, 0); // sector_size=128
        let records = decode_all_zones(&[sector], &dr);
        let real: Vec<_> = records.iter().filter(|r| !r.is_free && r.id != 0).collect();
        assert_eq!(real.len(), 1);
        assert_eq!(real[0].id, 5);
        assert_eq!(real[0].start_unit, 0);
        assert_eq!(real[0].len_units, 10);
    }

    #[test]
    fn cross_zone_span_is_continued_and_joined() {
        // A fragment whose terminator is never found before the end of zone 0
        // must be carried into zone 1 by the `pending_span` logic (guide §3.1)
        // rather than being dropped or left truncated at zone 0's edge. Zone 0
        // has a 64-byte header (512 bits); zone >=1 has only a 4-byte header
        // (32 bits). Build zone 0 with an id-3 fragment that runs to the very
        // end with NO terminator (id bits, then zeros), so it is genuinely
        // pending; zone 1 supplies a terminator.
        let mut z0 = vec![0u8; 128];
        z0[1..3].copy_from_slice(&0x8000u16.to_le_bytes()); // FreeLink: no free space
        // id = 3 (idlen 4), LSB-first: bits at 512 and 513 set.
        set_bit(&mut z0, 512, 1);
        set_bit(&mut z0, 513, 1);
        // bits 514..1023 remain zero: no terminator inside zone 0 -> pending.

        let mut z1 = vec![0u8; 128];
        z1[1..3].copy_from_slice(&0x8000u16.to_le_bytes());
        // A single 1 bit at the start of zone 1's allocatable area terminates
        // the continuing span almost immediately.
        set_bit(&mut z1, 4 * 8, 1);

        let dr = make_dr(7, 4, 7, 2, 8); // 2 zones, sector_size=128, zone_spare=8
        let records = decode_all_zones(&[z0, z1], &dr);
        let real: Vec<_> = records.iter().filter(|r| !r.is_free && r.id != 0).collect();
        // The pending span from zone 0 must be joined with its zone-1 tail so
        // the file is one logical fragment across the zone boundary, not two.
        assert_eq!(
            real.len(),
            1,
            "pending span must be joined, not split: {records:?}"
        );
        assert_eq!(real[0].id, 3);
        assert_eq!(
            real[0].zone, 0,
            "joined fragment retains its originating zone"
        );
        // Zone 0's unterminated run alone would be (1024-512)=512 units; the
        // continuation must add at least the zone-1 bytes that were consumed.
        assert!(
            real[0].len_units > 512,
            "len_units must include the zone-1 continuation: {:?}",
            real[0]
        );
    }
}
