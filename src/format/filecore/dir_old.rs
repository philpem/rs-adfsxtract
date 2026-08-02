//! Old (S/M/L) and new (D/E/F) directory decode (guide §3.3), including
//! broken-directory detection (guide §A.2/§A.5) - detection and recovery
//! are separate concerns here: this module always does a best-effort parse
//! and reports anomalies; the caller (`extract::walker`) applies the
//! `--on-broken-directory` policy using that report.

use crate::error::Result;
use crate::format::filecore::checksums::{ChecksumRegion, dir_checksum_accumulate, dir_checksum_fold};
use crate::format::filecore::detect::MapType;
use crate::format::filecore::map_new::NewMapIndex;
use crate::format::filecore::sml_geometry::SmlGeometry;
use crate::model::object::{
    ATTR_DIRECTORY, ATTR_LOCKED, ATTR_OWNER_READ, ATTR_OWNER_WRITE, Extent, Object, truncate_extents,
};

pub const SMALL_DIR_SIZE: usize = 0x500;
pub const LARGE_DIR_SIZE: usize = 0x800;
pub(crate) const ENTRY_SIZE: usize = 26;
pub(crate) const HEADER_SIZE: usize = 5;
pub(crate) const SMALL_MAX_ENTRIES: usize = 47;
pub(crate) const LARGE_MAX_ENTRIES: usize = 77;
pub(crate) const SMALL_TAIL_START: usize = 0x4CB;
pub(crate) const LARGE_TAIL_START: usize = 0x7D7;

/// Bit 6 of the standard attribute byte is reserved; reused internally for
/// the S/M/L "E" (execute-only) flag, which has no equivalent among the
/// standard owner/public read/write/locked/directory bits.
pub const ATTR_EXEC_ONLY: u32 = 1 << 6;

#[derive(Debug, Default)]
pub struct DirDecodeResult {
    pub objects: Vec<Object>,
    pub title: String,
    pub parent_raw: u32,
    pub is_broken: bool,
    pub check_byte_ok: bool,
    pub anomalies: Vec<String>,
}

fn decode_name(raw: &[u8; 10], mask_top_bit: bool) -> (String, u32) {
    let mut attrs = 0u32;
    if mask_top_bit {
        if raw[0] & 0x80 != 0 {
            attrs |= ATTR_OWNER_READ;
        }
        if raw[1] & 0x80 != 0 {
            attrs |= ATTR_OWNER_WRITE;
        }
        if raw[2] & 0x80 != 0 {
            attrs |= ATTR_LOCKED;
        }
        if raw[3] & 0x80 != 0 {
            attrs |= ATTR_DIRECTORY;
        }
        if raw[4] & 0x80 != 0 {
            attrs |= ATTR_EXEC_ONLY;
        }
    }
    let chars: Vec<u8> = raw
        .iter()
        .map(|&b| if mask_top_bit { b & 0x7F } else { b })
        .collect();
    let end = chars
        .iter()
        .position(|&b| b == 0 || b == 0x0D)
        .unwrap_or(chars.len());
    (crate::xlate::charset::decode(&chars[..end]), attrs)
}

fn read_u24_le(b: &[u8]) -> u32 {
    b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16
}

pub(crate) struct TailLayout {
    pub(crate) tail_start: usize,
    pub(crate) dir_len: usize,
    pub(crate) end_marker: usize,
    pub(crate) parent: usize,
    pub(crate) title: (usize, usize),
    pub(crate) name: (usize, usize),
    pub(crate) end_seq: usize,
    pub(crate) end_validation: (usize, usize),
    pub(crate) check_byte: usize,
}

pub(crate) fn tail_layout(small: bool) -> TailLayout {
    if small {
        TailLayout {
            tail_start: SMALL_TAIL_START,
            dir_len: SMALL_DIR_SIZE,
            end_marker: SMALL_TAIL_START,
            parent: SMALL_TAIL_START + 0x0B,
            title: (SMALL_TAIL_START + 0x0E, 19),
            name: (SMALL_TAIL_START + 0x01, 10),
            end_seq: SMALL_TAIL_START + 0x2F,
            end_validation: (SMALL_TAIL_START + 0x30, 4),
            check_byte: SMALL_TAIL_START + 0x34,
        }
    } else {
        TailLayout {
            tail_start: LARGE_TAIL_START,
            dir_len: LARGE_DIR_SIZE,
            end_marker: LARGE_TAIL_START,
            parent: LARGE_TAIL_START + 0x03,
            title: (LARGE_TAIL_START + 0x06, 19),
            name: (LARGE_TAIL_START + 0x19, 10),
            end_seq: LARGE_TAIL_START + 0x23,
            end_validation: (LARGE_TAIL_START + 0x24, 4),
            check_byte: LARGE_TAIL_START + 0x28,
        }
    }
}

/// Resolves one entry's raw 3-byte SIN/sector field to disc extents,
/// truncated to `length` bytes. `sml_geometry` is `Some` only for S/M/L
/// (old-map, old-directory) discs, where the logical sector number needs
/// translating to the image file's physically-interleaved layout (see
/// `sml_geometry.rs`) - D-format (also old-map) uses interleaved logical
/// addressing already, so its raw sector numbers are already valid file
/// byte offsets.
#[allow(clippy::too_many_arguments)]
fn resolve_entry_extents(
    raw_sin: u32,
    length: u64,
    map_type: MapType,
    new_map: Option<&NewMapIndex>,
    sharing_unit: u64,
    sml_geometry: Option<&SmlGeometry>,
    anomalies: &mut Vec<String>,
    name: &str,
) -> Vec<Extent> {
    match map_type {
        MapType::Old => {
            let logical_addr = raw_sin as u64 * 256;
            let extents = match sml_geometry {
                Some(geom) => geom.translate(logical_addr, length),
                None => vec![Extent { disc_addr: logical_addr, len: length }],
            };
            truncate_extents(extents, length)
        }
        MapType::New => {
            let fragment_id = (raw_sin >> 8) & 0xFFFF;
            if fragment_id == 0 {
                return Vec::new();
            }
            let Some(index) = new_map else {
                anomalies.push(format!("{name}: new-map entry but no map index available"));
                return Vec::new();
            };
            let sharing_offset = raw_sin & 0xFF;
            match crate::format::filecore::map_new::resolve_fragment(
                index,
                fragment_id,
                sharing_offset,
                sharing_unit,
            ) {
                Some(extents) => truncate_extents(extents, length),
                None => {
                    anomalies.push(format!(
                        "{name}: fragment id {fragment_id} not found in zone map (lost object?)"
                    ));
                    Vec::new()
                }
            }
        }
    }
}

pub fn decode_dir(
    data: &[u8],
    small: bool,
    map_type: MapType,
    new_map: Option<&NewMapIndex>,
    sharing_unit: u64,
    sml_geometry: Option<&SmlGeometry>,
) -> Result<DirDecodeResult> {
    let mut anomalies = Vec::new();
    let expected_len = if small { SMALL_DIR_SIZE } else { LARGE_DIR_SIZE };
    if data.len() < expected_len {
        anomalies.push(format!(
            "directory data truncated: expected {expected_len} bytes, got {}",
            data.len()
        ));
    }

    let header_seq = data[0];
    let header_validation = &data[1..5];
    let tail = tail_layout(small);

    // If the loop below never finds an empty (name-byte-0) slot or runs
    // into the tail, `used_entries` stays at `max_entries` - a directory
    // at exactly its structural capacity legitimately has no room for a
    // terminator, so that's the correct count, not a sign of corruption.
    let max_entries = if small { SMALL_MAX_ENTRIES } else { LARGE_MAX_ENTRIES };
    let mut used_entries = max_entries;
    for i in 0..max_entries {
        let off = HEADER_SIZE + i * ENTRY_SIZE;
        if off >= tail.tail_start || data[off] == 0 {
            used_entries = i;
            break;
        }
    }

    let mut objects = Vec::with_capacity(used_entries);
    for i in 0..used_entries {
        let off = HEADER_SIZE + i * ENTRY_SIZE;
        let entry = &data[off..off + ENTRY_SIZE];
        let name_raw: [u8; 10] = entry[0..10].try_into().unwrap();
        let (name, name_attrs) = decode_name(&name_raw, small);
        let load = u32::from_le_bytes(entry[0x0A..0x0E].try_into().unwrap());
        let exec = u32::from_le_bytes(entry[0x0E..0x12].try_into().unwrap());
        let length = u32::from_le_bytes(entry[0x12..0x16].try_into().unwrap()) as u64;
        let sin_raw = read_u24_le(&entry[0x16..0x19]);

        let (attrs, is_directory) = if small {
            (name_attrs, name_attrs & ATTR_DIRECTORY != 0)
        } else {
            let a = entry[0x19] as u32;
            (a, a & ATTR_DIRECTORY != 0)
        };

        // A subdirectory's "extents" are its own directory structure's
        // extents (readable again as directory data by the caller), rather
        // than a length-truncated file extent list.
        let extents = if is_directory {
            match map_type {
                MapType::Old => {
                    let dir_len = if small { SMALL_DIR_SIZE as u64 } else { LARGE_DIR_SIZE as u64 };
                    let logical_addr = sin_raw as u64 * 256;
                    match sml_geometry {
                        Some(geom) => geom.translate(logical_addr, dir_len),
                        None => vec![Extent { disc_addr: logical_addr, len: dir_len }],
                    }
                }
                MapType::New => {
                    // S/M/L (`small`) is always old-map, so a `New`-map
                    // directory here is always the large (0x800) layout.
                    let fragment_id = (sin_raw >> 8) & 0xFFFF;
                    let sharing_offset = sin_raw & 0xFF;
                    match new_map.and_then(|idx| {
                        crate::format::filecore::map_new::resolve_fragment(
                            idx,
                            fragment_id,
                            sharing_offset,
                            sharing_unit,
                        )
                    }) {
                        Some(e) => truncate_extents(e, LARGE_DIR_SIZE as u64),
                        None => {
                            anomalies.push(format!(
                                "{name}: subdirectory fragment id {fragment_id} not found in zone map"
                            ));
                            Vec::new()
                        }
                    }
                }
            }
        } else {
            resolve_entry_extents(
                sin_raw,
                length,
                map_type,
                new_map,
                sharing_unit,
                sml_geometry,
                &mut anomalies,
                &name,
            )
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

    if data.get(tail.end_marker).copied() != Some(0) {
        anomalies.push("tail end-marker byte is not 0x00".into());
    }

    let seq_match = data.get(tail.end_seq).copied() == Some(header_seq);
    let validation_match =
        data.get(tail.end_validation.0..tail.end_validation.0 + tail.end_validation.1) == Some(header_validation);
    if !seq_match {
        anomalies.push(format!(
            "sequence number mismatch: header={header_seq:#04x} tail={:#04x}",
            data.get(tail.end_seq).copied().unwrap_or(0)
        ));
    }
    if !validation_match {
        anomalies.push("start/end validation name mismatch (Broken directory)".into());
    }

    let end_of_entries = HEADER_SIZE + used_entries * ENTRY_SIZE;
    let regions = [
        ChecksumRegion { start: 0, end: end_of_entries, words_first: true },
        ChecksumRegion { start: tail.tail_start + 1, end: tail.dir_len - 4, words_first: false },
    ];
    let checksum = dir_checksum_fold(dir_checksum_accumulate(data, &regions));
    let check_byte_ok = data.get(tail.check_byte).copied() == Some(checksum);
    if !check_byte_ok {
        anomalies.push(format!(
            "directory check byte mismatch: computed={checksum:#04x} stored={:#04x}",
            data.get(tail.check_byte).copied().unwrap_or(0)
        ));
    }

    let (title_off, title_len) = tail.title;
    let title_bytes = &data[title_off..title_off + title_len];
    let title_end = title_bytes.iter().position(|&b| b == 0).unwrap_or(title_bytes.len());
    let title = crate::xlate::charset::decode(&title_bytes[..title_end]);

    let (name_off, name_len) = tail.name;
    let _dir_own_name_bytes = &data[name_off..name_off + name_len]; // parent-visible name; unused here

    let parent_raw = read_u24_le(&data[tail.parent..tail.parent + 3]);

    // Not a broken directory by the terminator-missing anomaly alone if the
    // validation/seq/checksum all check out - but do surface it.
    let is_broken = !seq_match || !validation_match || !check_byte_ok;

    Ok(DirDecodeResult {
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

    #[test]
    fn small_dir_name_attribute_bits() {
        let mut raw = [0u8; 10];
        raw[0] = b'F' | 0x80; // R
        raw[1] = b'r' | 0x80; // W
        raw[2] = b'e' | 0x80; // L
        raw[3] = b'd'; // D clear
        raw[4] = b'\0';
        let (name, attrs) = decode_name(&raw, true);
        assert_eq!(name, "Fred");
        assert_eq!(attrs, ATTR_OWNER_READ | ATTR_OWNER_WRITE | ATTR_LOCKED);
    }
}
