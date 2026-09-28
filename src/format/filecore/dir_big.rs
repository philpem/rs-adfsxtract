//! Big directory (E+/F+/G) decode (guide §3.4). The 4-byte widened SIN uses
//! the same split as the 3-byte form, just widened: fragment ID =
//! `value >> 8`, sharing offset = `value & 0xFF`.

use crate::error::Result;
use crate::format::filecore::checksums::{ChecksumRegion, dir_checksum_accumulate, dir_checksum_fold};
use crate::format::filecore::map_new::NewMapIndex;
use crate::model::object::{ATTR_DIRECTORY, Extent, Object, truncate_extents};

pub(crate) const HEADER_FIXED_SIZE: usize = 0x1C;
pub(crate) const ENTRY_SIZE: usize = 28;
pub(crate) const START_NAME: &[u8; 4] = b"SBPr";
pub(crate) const END_NAME: &[u8; 4] = b"oven";

#[derive(Debug, Default)]
pub struct BigDirDecodeResult {
    pub objects: Vec<Object>,
    pub title: String,
    pub parent_raw: u32,
    pub is_broken: bool,
    pub check_byte_ok: bool,
    pub anomalies: Vec<String>,
}

pub(crate) fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

fn resolve_big_sin(
    sin: u32,
    length: u64,
    new_map: &NewMapIndex,
    sharing_unit: u64,
    anomalies: &mut Vec<String>,
    name: &str,
) -> Vec<Extent> {
    let fragment_id = sin >> 8;
    if fragment_id == 0 {
        return Vec::new();
    }
    let sharing_offset = sin & 0xFF;
    match crate::format::filecore::map_new::resolve_fragment(new_map, fragment_id, sharing_offset, sharing_unit) {
        Some(extents) => {
            if fragment_id > 0xFFFF {
                anomalies.push(format!(
                    "{name}: big-dir fragment id {fragment_id:#x} exceeds 16 bits"
                ));
            }
            truncate_extents(extents, length)
        }
        None => {
            anomalies.push(format!("{name}: fragment id {fragment_id} not found in zone map"));
            Vec::new()
        }
    }
}

pub fn decode_big_dir(data: &[u8], new_map: &NewMapIndex, sharing_unit: u64) -> Result<BigDirDecodeResult> {
    let mut anomalies = Vec::new();

    if data.len() < HEADER_FIXED_SIZE {
        // Every fixed-header field below is read by direct offset; a
        // resolved extent shorter than the header itself (e.g. a
        // subdirectory whose zone-map fragment is smaller than what its
        // own `BigDirSize`/`BigDirEntries` fields would imply - these are
        // two independently-sourced numbers and nothing guarantees they
        // agree on a corrupted disc) has nothing safe to parse. Report it
        // as broken instead of indexing past the end of `data`.
        anomalies.push(format!(
            "directory data too short to contain a header: expected at least {HEADER_FIXED_SIZE} bytes, got {}",
            data.len()
        ));
        return Ok(BigDirDecodeResult { is_broken: true, anomalies, ..Default::default() });
    }

    let start_seq = data[0];
    let start_name_ok = &data[4..8] == START_NAME;
    if !start_name_ok {
        anomalies.push("big directory start signature is not \"SBPr\"".into());
    }

    let name_len = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    let big_dir_size = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;
    let n_entries = u32::from_le_bytes(data[16..20].try_into().unwrap()) as usize;
    let names_size = u32::from_le_bytes(data[20..24].try_into().unwrap()) as usize;
    let parent_raw = u32::from_le_bytes(data[24..28].try_into().unwrap());

    let name_padded = pad4(name_len + 1);
    let name_bytes = &data[HEADER_FIXED_SIZE..HEADER_FIXED_SIZE + name_len.min(data.len() - HEADER_FIXED_SIZE)];
    let title = crate::xlate::charset::decode(name_bytes);

    let entries_start = HEADER_FIXED_SIZE + name_padded;
    let heap_start = entries_start + n_entries * ENTRY_SIZE;
    let region1_end = heap_start + names_size;

    let mut objects = Vec::new();
    if data.len() >= region1_end {
        // Only allocate once `n_entries` is known to be consistent with
        // the buffer we actually have - `region1_end` grows with
        // `n_entries`, so this bounds it implicitly. A corrupted
        // `BigDirEntries` field (up to u32::MAX) read directly into
        // `Vec::with_capacity` before this check would abort the process
        // trying to allocate an absurd amount of memory instead of being
        // handled as the anomaly it is.
        objects.reserve_exact(n_entries);
        for i in 0..n_entries {
            let off = entries_start + i * ENTRY_SIZE;
            let entry = &data[off..off + ENTRY_SIZE];
            let load = u32::from_le_bytes(entry[0..4].try_into().unwrap());
            let exec = u32::from_le_bytes(entry[4..8].try_into().unwrap());
            let length = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as u64;
            let sin_raw = u32::from_le_bytes(entry[12..16].try_into().unwrap());
            let attrs = u32::from_le_bytes(entry[16..20].try_into().unwrap());
            let obj_name_len = u32::from_le_bytes(entry[20..24].try_into().unwrap()) as usize;
            let obj_name_ptr = u32::from_le_bytes(entry[24..28].try_into().unwrap()) as usize;

            let name = if heap_start + obj_name_ptr + obj_name_len <= data.len() {
                crate::xlate::charset::decode(&data[heap_start + obj_name_ptr..heap_start + obj_name_ptr + obj_name_len])
            } else {
                anomalies.push(format!("entry {i}: name heap offset out of range"));
                String::new()
            };

            let is_directory = attrs & ATTR_DIRECTORY != 0;
            let extents = if is_directory {
                let fragment_id = sin_raw >> 8;
                let sharing_offset = sin_raw & 0xFF;
                match crate::format::filecore::map_new::resolve_fragment(new_map, fragment_id, sharing_offset, sharing_unit) {
                    Some(e) => e,
                    None => {
                        anomalies.push(format!("{name}: subdirectory fragment id {fragment_id} not found in zone map"));
                        Vec::new()
                    }
                }
            } else {
                resolve_big_sin(sin_raw, length, new_map, sharing_unit, &mut anomalies, &name)
            };

            objects.push(Object {
                name,
                load,
                exec,
                length,
                attrs,
                is_directory,
                extents,
            });
        }
    } else {
        anomalies.push(format!(
            "directory data truncated: need {region1_end} bytes for entries+heap, got {}",
            data.len()
        ));
    }

    let tail_start = big_dir_size.saturating_sub(8);
    let end_seq_ok;
    let end_name_ok;
    let check_byte_ok;
    if data.len() >= big_dir_size && tail_start >= region1_end {
        end_name_ok = &data[tail_start..tail_start + 4] == END_NAME;
        end_seq_ok = data[tail_start + 4] == start_seq;
        if !end_name_ok {
            anomalies.push("big directory end signature is not \"oven\"".into());
        }
        if !end_seq_ok {
            anomalies.push(format!(
                "sequence number mismatch: header={start_seq:#04x} tail={:#04x}",
                data[tail_start + 4]
            ));
        }

        let regions = [
            ChecksumRegion { start: 0, end: region1_end, words_first: true },
            ChecksumRegion { start: tail_start, end: tail_start + 7, words_first: true },
        ];
        let checksum = dir_checksum_fold(dir_checksum_accumulate(data, &regions));
        check_byte_ok = data.get(tail_start + 7).copied() == Some(checksum);
        if !check_byte_ok {
            anomalies.push(format!(
                "directory check byte mismatch: computed={checksum:#04x} stored={:#04x}",
                data.get(tail_start + 7).copied().unwrap_or(0)
            ));
        }
    } else {
        anomalies.push("directory data too short to contain a valid tail".into());
        end_name_ok = false;
        end_seq_ok = false;
        check_byte_ok = false;
    }

    let is_broken = !start_name_ok || !end_name_ok || !end_seq_ok || !check_byte_ok;

    Ok(BigDirDecodeResult {
        objects,
        title,
        parent_raw,
        is_broken,
        check_byte_ok,
        anomalies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::filecore::disc_record::parse_disc_record;
    use crate::format::filecore::map_new::NewMapIndex;

    #[test]
    fn truncated_buffer_is_reported_broken_not_panicked() {
        // Shorter than HEADER_FIXED_SIZE - every header field below is read
        // by direct offset, so this must not panic.
        let short = vec![0u8; 10];
        let dr = parse_disc_record(&[0u8; 60]).unwrap();
        let new_map = NewMapIndex::empty_for_test(dr);
        let result = decode_big_dir(&short, &new_map, 0).unwrap();
        assert!(result.is_broken);
        assert!(result.objects.is_empty());
        assert!(!result.anomalies.is_empty());
    }

    #[test]
    fn empty_buffer_is_reported_broken_not_panicked() {
        let dr = parse_disc_record(&[0u8; 60]).unwrap();
        let new_map = NewMapIndex::empty_for_test(dr);
        let result = decode_big_dir(&[], &new_map, 0).unwrap();
        assert!(result.is_broken);
    }

    #[test]
    fn huge_n_entries_does_not_attempt_a_huge_allocation() {
        // n_entries claims u32::MAX entries, but the buffer is nowhere near
        // big enough to back that - `data.len() >= region1_end` must fail
        // before any allocation sized by n_entries happens.
        let mut data = vec![0u8; HEADER_FIXED_SIZE];
        data[4..8].copy_from_slice(START_NAME);
        data[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        let dr = parse_disc_record(&[0u8; 60]).unwrap();
        let new_map = NewMapIndex::empty_for_test(dr);
        let result = decode_big_dir(&data, &new_map, 0).unwrap();
        assert!(result.objects.is_empty());
        assert!(result.anomalies.iter().any(|a| a.contains("truncated")));
    }
}
