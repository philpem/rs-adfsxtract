//! Old (S/M/L) and new (D/E/F) directory decode (guide §3.3), including
//! broken-directory detection (guide §A.2/§A.5) - detection and recovery
//! are separate concerns here: this module always does a best-effort parse
//! and reports anomalies; the caller (`extract::walker`) applies the
//! `--on-broken-directory` policy using that report.

use crate::error::Result;
use crate::format::filecore::checksums::{
    ChecksumRegion, dir_checksum_accumulate, dir_checksum_fold,
};
use crate::format::filecore::detect::MapType;
use crate::format::filecore::map_new::NewMapIndex;
use crate::format::filecore::sml_geometry::SmlGeometry;
use crate::model::object::{
    ATTR_DIRECTORY, ATTR_LOCKED, ATTR_OWNER_READ, ATTR_OWNER_WRITE, Extent, Object,
    truncate_extents,
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
    /// Fatal structural corruption (bad sequence/validation/checksum): the
    /// directory structure can't be trusted and `is_broken` is set.
    pub anomalies: Vec<String>,
    /// Non-fatal observations that ADFS/FSCK would flag but that don't stop
    /// us reading the tree (entries out of collation order, a wrong tail
    /// NewDirParent, a zero-length file carrying a real fragment). These are
    /// reported but never cause `is_broken`.
    pub warnings: Vec<String>,
}

fn decode_name(raw: &[u8; 10], mask_top_bit: bool) -> (String, Vec<u8>, u32) {
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
    let bytes = chars[..end].to_vec();
    (crate::xlate::charset::decode(&bytes), bytes, attrs)
}

fn read_u24_le(b: &[u8]) -> u32 {
    b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16
}

/// ADFS collation order for directory entries (guide §3.3): case-insensitive
/// on the raw RISC OS name bytes, compared byte-by-byte. Comparing the raw
/// bytes - not decoded Unicode code points - matters for the RISC OS
/// `0x80-0x9F` range, whose decode table maps to arbitrary high Unicode
/// points (e.g. 0x80 -> U+20AC) that would reorder them against `0xA0-0xFF`
/// relative to ADFS's on-disk byte order. High-bit/Latin-1 characters sort
/// above ASCII by byte value (not masked to 7-bit, which would fold 0xA4 to
/// '$' and sort it below the letters). ASCII letters fold to lower case.
pub(crate) fn name_collation_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    collation_key(a).cmp(&collation_key(b))
}

fn collation_key(name: &[u8]) -> Vec<u8> {
    name.iter().map(|&b| b.to_ascii_lowercase()).collect()
}

/// True if the given raw name byte strings are in non-decreasing ADFS
/// collation order.
pub(crate) fn names_in_collation_order(names: &[Vec<u8>]) -> bool {
    names
        .windows(2)
        .all(|w| name_collation_cmp(&w[0], &w[1]) != std::cmp::Ordering::Greater)
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
                None => vec![Extent {
                    disc_addr: logical_addr,
                    len: length,
                }],
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
    expected_parent_sin: Option<u32>,
) -> Result<DirDecodeResult> {
    let mut anomalies = Vec::new();
    let mut warnings = Vec::new();
    let expected_len = if small {
        SMALL_DIR_SIZE
    } else {
        LARGE_DIR_SIZE
    };
    if data.len() < expected_len {
        // Every offset below (header, entries, tail) assumes a
        // full-size buffer; a resolved extent shorter than that - a
        // corrupted fragment, a directory whose zone-map allocation
        // doesn't match its structural size - means there's nothing
        // safe to parse. Report it as broken rather than indexing past
        // the end of `data`, which would panic and abort the whole
        // extraction instead of just this one directory.
        anomalies.push(format!(
            "directory data truncated: expected {expected_len} bytes, got {}",
            data.len()
        ));
        return Ok(DirDecodeResult {
            is_broken: true,
            anomalies,
            ..Default::default()
        });
    }

    let header_seq = data[0];
    let header_validation = &data[1..5];
    let tail = tail_layout(small);

    // If the loop below never finds an empty (name-byte-0) slot or runs
    // into the tail, `used_entries` stays at `max_entries` - a directory
    // at exactly its structural capacity legitimately has no room for a
    // terminator, so that's the correct count, not a sign of corruption.
    let max_entries = if small {
        SMALL_MAX_ENTRIES
    } else {
        LARGE_MAX_ENTRIES
    };
    let mut used_entries = max_entries;
    for i in 0..max_entries {
        let off = HEADER_SIZE + i * ENTRY_SIZE;
        if off >= tail.tail_start || data[off] == 0 {
            used_entries = i;
            break;
        }
    }

    let mut objects = Vec::with_capacity(used_entries);
    let mut collation_names: Vec<Vec<u8>> = Vec::with_capacity(used_entries);
    for i in 0..used_entries {
        let off = HEADER_SIZE + i * ENTRY_SIZE;
        let entry = &data[off..off + ENTRY_SIZE];
        let name_raw: [u8; 10] = entry[0..10].try_into().unwrap();
        let (name, name_bytes, name_attrs) = decode_name(&name_raw, small);
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
                    let dir_len = if small {
                        SMALL_DIR_SIZE as u64
                    } else {
                        LARGE_DIR_SIZE as u64
                    };
                    let logical_addr = sin_raw as u64 * 256;
                    match sml_geometry {
                        Some(geom) => geom.translate(logical_addr, dir_len),
                        None => vec![Extent {
                            disc_addr: logical_addr,
                            len: dir_len,
                        }],
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

        // A zero-length file must carry a fragment-ID-0 SIN (i.e. "no disc
        // space", guide §3.2/§3.3); a real file of length 0 uses fragment 0
        // and allocates nothing. RISC OS's FSCK warns on any zero-length
        // file that was instead recorded with a real fragment. Non-fatal:
        // the file simply has no data, so we still extract it as empty.
        if map_type == MapType::New && !is_directory && length == 0 {
            let fragment_id = (sin_raw >> 8) & 0xFFFF;
            if fragment_id != 0 {
                warnings.push(format!(
                    "{name}: zero-length file recorded with fragment id {fragment_id} \
                     (must be 0, no disc space)"
                ));
            }
        }

        objects.push(Object {
            name,
            name_bytes,
            load,
            exec,
            length,
            attrs,
            is_directory,
            extents,
            sin: (map_type == MapType::New).then_some(sin_raw),
            modified_unix_secs: None,
        });

        // Raw name bytes for collation (the logical name, so S/M/L top bits
        // masked off), terminated at NUL/CR like `decode_name`.
        collation_names.push(
            name_raw
                .iter()
                .map(|&b| if small { b & 0x7F } else { b })
                .take_while(|&b| b != 0 && b != 0x0D)
                .collect(),
        );
    }

    // Entries must be in ADFS case-insensitive collation order (ADFS
    // binary-searches them). ADFS/FSCK report an unsorted directory as
    // "broken", but the entries themselves are all readable, so this is
    // surfaced as a warning rather than a fatal error - a "best effort"
    // extraction can still walk the tree.
    if !names_in_collation_order(&collation_names) {
        warnings.push("directory entries are not in case-insensitive collation order".into());
    }

    if data.get(tail.end_marker).copied() != Some(0) {
        anomalies.push("tail end-marker byte is not 0x00".into());
    }

    let seq_match = data.get(tail.end_seq).copied() == Some(header_seq);
    let validation_match = data
        .get(tail.end_validation.0..tail.end_validation.0 + tail.end_validation.1)
        == Some(header_validation);
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
        ChecksumRegion {
            start: 0,
            end: end_of_entries,
            words_first: true,
        },
        ChecksumRegion {
            start: tail.tail_start + 1,
            end: tail.dir_len - 4,
            words_first: false,
        },
    ];
    let checksum = dir_checksum_fold(dir_checksum_accumulate(data, &regions));
    let stored_check = data.get(tail.check_byte).copied().unwrap_or(0);
    // The guide (§A.2) notes that on 8-bit ADFS the directory check byte is
    // always zero - the 32-bit A.2 algorithm never runs. So a stored byte of 0
    // is the legitimate "not computed" value for 8-bit/old directories, and
    // must not be reported as corruption even though the recomputed value is
    // non-zero. (Adfs640L etc. store a real computed check byte and are still
    // verified; only the 8-bit zero convention is exempted.)
    let check_byte_ok = stored_check == 0 || stored_check == checksum;
    if !check_byte_ok {
        anomalies.push(format!(
            "directory check byte mismatch: computed={checksum:#04x} stored={stored_check:#04x}"
        ));
    }

    let (title_off, title_len) = tail.title;
    let title_bytes = &data[title_off..title_off + title_len];
    let title_end = title_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(title_bytes.len());
    let title = crate::xlate::charset::decode(&title_bytes[..title_end]);

    let (name_off, name_len) = tail.name;
    let _dir_own_name_bytes = &data[name_off..name_off + name_len]; // parent-visible name; unused here

    let parent_raw = read_u24_le(&data[tail.parent..tail.parent + 3]);

    // New-map (E/F) directories: the tail `NewDirParent` must hold the
    // containing directory's SIN (the root points back to its own SIN), not
    // a byte address. A mismatch is flagged, but the tree is still walkable:
    // each entry's own SIN (which we do trust) is used to resolve children,
    // so this is a warning rather than a fatal error. Old-map directories
    // are not validated (their parent field has different semantics).
    match (map_type, expected_parent_sin) {
        (MapType::New, Some(expected)) if parent_raw != expected => {
            warnings.push(format!(
                "directory parent SIN mismatch: stored {parent_raw:#06x}, expected {expected:#06x}"
            ));
        }
        _ => {}
    }

    // Fatal: only a structural integrity failure (bad sequence, validation
    // or checksum) means the directory contents can't be trusted. The
    // warnings above never set this.
    let is_broken = !seq_match || !validation_match || !check_byte_ok;

    Ok(DirDecodeResult {
        objects,
        title,
        parent_raw,
        is_broken,
        check_byte_ok,
        anomalies,
        warnings,
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
        let (name, name_bytes, attrs) = decode_name(&raw, true);
        assert_eq!(name, "Fred");
        assert_eq!(name_bytes, b"Fred");
        assert_eq!(attrs, ATTR_OWNER_READ | ATTR_OWNER_WRITE | ATTR_LOCKED);
    }

    #[test]
    fn truncated_buffer_is_reported_broken_not_panicked() {
        // A resolved extent shorter than the format's structural size (a
        // corrupted fragment, or a subdirectory whose zone-map allocation
        // came up short) must not panic - every offset in the real parse
        // path assumes a full-size buffer.
        let short = vec![0u8; 10];
        let result = decode_dir(&short, true, MapType::Old, None, 0, None, None).unwrap();
        assert!(result.is_broken);
        assert!(result.objects.is_empty());
        assert!(!result.anomalies.is_empty());
    }

    #[test]
    fn empty_buffer_is_reported_broken_not_panicked() {
        let result = decode_dir(&[], false, MapType::New, None, 0, None, None).unwrap();
        assert!(result.is_broken);
    }

    /// Builds a structurally-valid new-map (large) directory buffer with the
    /// given `(name, length, sin)` entries and a tail NewDirParent of
    /// `parent`. The header sequence/validation and checksum are computed
    /// correctly, so `decode_dir`'s `is_broken` is driven solely by the
    /// entry-order / parent / zero-length checks under test rather than by a
    /// mismatched checksum. Fragment resolution intentionally gets no map
    /// index, which yields a harmless anomaly but not a broken flag.
    fn build_new_dir(entries: &[(&str, u32, u32)], parent: u32) -> Vec<u8> {
        let mut buf = vec![0u8; LARGE_DIR_SIZE];
        buf[0] = 1;
        buf[1..5].copy_from_slice(b"Hugo");
        for (i, (name, length, sin)) in entries.iter().enumerate() {
            let off = HEADER_SIZE + i * ENTRY_SIZE;
            let mut nm = [0u8; 10];
            for (j, b) in name.bytes().take(10).enumerate() {
                nm[j] = b;
            }
            buf[off..off + 10].copy_from_slice(&nm);
            buf[off + 0x12..off + 0x16].copy_from_slice(&length.to_le_bytes());
            buf[off + 0x16] = (sin & 0xFF) as u8;
            buf[off + 0x17] = ((sin >> 8) & 0xFF) as u8;
            buf[off + 0x18] = ((sin >> 16) & 0xFF) as u8;
            buf[off + 0x19] = 0x03;
        }
        let tail = tail_layout(false);
        buf[tail.parent] = (parent & 0xFF) as u8;
        buf[tail.parent + 1] = ((parent >> 8) & 0xFF) as u8;
        buf[tail.parent + 2] = ((parent >> 16) & 0xFF) as u8;
        buf[tail.end_seq] = 1;
        buf[tail.end_validation.0..tail.end_validation.0 + 4].copy_from_slice(b"Hugo");
        let end_of_entries = HEADER_SIZE + entries.len() * ENTRY_SIZE;
        let regions = [
            ChecksumRegion {
                start: 0,
                end: end_of_entries,
                words_first: true,
            },
            ChecksumRegion {
                start: tail.tail_start + 1,
                end: tail.dir_len - 4,
                words_first: false,
            },
        ];
        let checksum = dir_checksum_fold(dir_checksum_accumulate(&buf, &regions));
        buf[tail.check_byte] = checksum;
        buf
    }

    fn decode_new(buf: &[u8], expected_parent: Option<u32>) -> DirDecodeResult {
        decode_dir(buf, false, MapType::New, None, 0, None, expected_parent).unwrap()
    }

    #[test]
    fn sorted_entries_are_not_reported_broken() {
        let buf = build_new_dir(&[("Alpha", 5, 0x300), ("beta", 6, 0x400)], 0x203);
        let r = decode_new(&buf, Some(0x203));
        assert!(!r.is_broken, "anomalies: {:?}", r.anomalies);
        assert!(r.warnings.is_empty(), "warnings: {:?}", r.warnings);
    }

    #[test]
    fn unsorted_entries_are_warned_but_not_fatal() {
        // Case-insensitively, "beta" sorts after "alpha"; writing them in
        // extraction order (beta first) is what RISC OS reports as broken.
        // The entries are all readable, so it is a warning, not a fatal
        // broken-directory - a best-effort extraction can still walk it.
        let buf = build_new_dir(&[("beta", 6, 0x400), ("Alpha", 5, 0x300)], 0x203);
        let r = decode_new(&buf, Some(0x203));
        assert!(!r.is_broken, "anomalies: {:?}", r.anomalies);
        assert!(
            r.warnings.iter().any(|a| a.contains("collation")),
            "warnings: {:?}",
            r.warnings
        );
    }

    #[test]
    fn collation_is_case_insensitive_and_code_point_aware() {
        // Case-insensitive: "Grape" sorts after "fIge" (fig < gra), not
        // after by raw byte value where uppercase 'G' (0x47) < 'f' (0x66).
        assert_eq!(
            name_collation_cmp(b"Grape", b"fIge"),
            std::cmp::Ordering::Greater
        );
        // High-bit / Latin-1 characters (e.g. 0xA4) sort above ASCII
        // alphanumerics by byte value, not below - masking to 7-bit would
        // fold 0xA4 to '$' and sort it before the letters.
        assert_eq!(name_collation_cmp(b"A", &[0xA4]), std::cmp::Ordering::Less);
        // RISC OS-specific high bytes are compared by their raw on-disk
        // value, not by their remapped Unicode code point: byte 0x80 sorts
        // below Latin-1 0xE9, matching ADFS, even though 0x80 decodes to
        // U+20AC (which is *above* U+00E9 as a code point).
        assert_eq!(
            name_collation_cmp(&[0x80], &[0xE9]),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn wrong_parent_sin_is_warned_but_not_fatal() {
        // The tree is still walkable using each entry's own SIN, so a wrong
        // tail NewDirParent is flagged without making the directory broken.
        let buf = build_new_dir(&[("Alpha", 5, 0x300)], 0x999);
        let r = decode_new(&buf, Some(0x203));
        assert!(!r.is_broken, "anomalies: {:?}", r.anomalies);
        assert!(
            r.warnings.iter().any(|a| a.contains("parent")),
            "warnings: {:?}",
            r.warnings
        );
    }

    #[test]
    fn zero_length_file_with_real_fragment_is_warned_but_not_fatal() {
        // length == 0 but fragment id 3 (nonzero) instead of fragment 0.
        // Non-fatal: the file has no data, so it still extracts as empty.
        let buf = build_new_dir(&[("Empty", 0, 0x300)], 0x203);
        let r = decode_new(&buf, Some(0x203));
        assert!(!r.is_broken, "anomalies: {:?}", r.anomalies);
        assert!(
            r.warnings.iter().any(|a| a.contains("zero-length")),
            "warnings: {:?}",
            r.warnings
        );
    }
}
