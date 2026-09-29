//! Synthetic FileCore disc image builder for tests. No real disc images are
//! committed to this repo; every test that needs an image constructs one
//! here, directly from the guide's Appendix C procedures, so tests can
//! target exact edge cases (fragmentation, corruption, boundary formats)
//! that may not exist in whatever real sample happens to be at hand.

use std::collections::HashMap;
use std::io::Cursor;

use crate::format::dfs::geometry::DfsGeometry;
use crate::format::filecore::checksums::{
    ChecksumRegion, dir_checksum_accumulate, dir_checksum_fold, zone_check,
};
use crate::format::filecore::dir_old;
use crate::model::filetype;
use crate::model::object::ATTR_DIRECTORY;
use crate::model::riscos_time::RiscOsTimestamp;

pub struct SynthFile {
    pub name: String,
    pub load: u32,
    pub exec: u32,
    pub attrs: u32,
    pub content: Vec<u8>,
}

impl SynthFile {
    pub fn plain(name: &str, content: &[u8]) -> Self {
        Self {
            name: name.to_string(),
            load: 0x8000,
            exec: 0x8000,
            attrs: 0x03,
            content: content.to_vec(),
        }
    }

    pub fn typed(name: &str, filetype: u16, unix_secs: i64, content: &[u8]) -> Self {
        let (load, exec) = filetype::encode(
            filetype,
            RiscOsTimestamp {
                unix_secs,
                nanos: 0,
            },
        );
        Self {
            name: name.to_string(),
            load,
            exec,
            attrs: 0x03,
            content: content.to_vec(),
        }
    }
}

pub enum SynthEntry {
    File(SynthFile),
    Dir {
        name: String,
        attrs: u32,
        children: Vec<SynthEntry>,
    },
}

impl SynthEntry {
    pub fn dir(name: &str, children: Vec<SynthEntry>) -> Self {
        SynthEntry::Dir {
            name: name.to_string(),
            attrs: 0x03,
            children,
        }
    }
}

pub struct BuiltImage {
    pub bytes: Vec<u8>,
}

impl BuiltImage {
    pub fn cursor(&self) -> Cursor<Vec<u8>> {
        Cursor::new(self.bytes.clone())
    }
}

/// Inverts `xlate::charset::decode_byte`, built once per call - fine for
/// test-only, small-scale use.
fn encode_charset_str(s: &str) -> Vec<u8> {
    let mut rev: HashMap<char, u8> = HashMap::new();
    for b in 0u16..=255 {
        let c = crate::xlate::charset::decode_byte(b as u8);
        rev.entry(c).or_insert(b as u8);
    }
    s.chars().map(|c| *rev.get(&c).unwrap_or(&b'?')).collect()
}

fn pad_to(mut v: Vec<u8>, len: usize) -> Vec<u8> {
    v.truncate(len);
    v.resize(len, 0);
    v
}

// ---------------------------------------------------------------------
// New-map (E/F/E+/F+/G-style) builder
// ---------------------------------------------------------------------

pub struct NewMapConfig {
    pub log2_sector_size: u8,
    pub idlen: u8,
    pub log2_bpmb: u8,
    pub big_dirs: bool,
    pub disc_name: String,
}

impl Default for NewMapConfig {
    fn default() -> Self {
        Self {
            log2_sector_size: 12, // 4096 bytes: generous room for small test trees
            idlen: 8,
            log2_bpmb: 4, // 16 bytes/unit
            big_dirs: false,
            disc_name: "TestDisc".to_string(),
        }
    }
}

/// Metadata for one directory entry, after its own (and, if a directory,
/// its children's) bytes have already been placed into `all_objects`.
struct ChildMeta {
    name: String,
    load: u32,
    exec: u32,
    attrs: u32,
    fragment_id: u32,
    content_len: usize,
}

fn entry_name(e: &SynthEntry) -> &str {
    match e {
        SynthEntry::File(f) => &f.name,
        SynthEntry::Dir { name, .. } => name,
    }
}

/// Recursively allocates a fragment ID for every object (file or
/// directory) at any depth, flattening them all into `all_objects` (which
/// `build_new_map_disc` places on disk one-for-one) - a directory's own
/// serialized bytes reference its children only by fragment ID, so the
/// children's *data* must be placed separately, not nested inside the
/// parent's bytes on disk. `parent_sin` is the containing directory's SIN,
/// written into each directory's tail NewDirParent (the root points back to
/// its own SIN). Entries are emitted in ADFS case-insensitive collation
/// order, as RISC OS requires and the reader now enforces.
fn allocate_objects(
    entries: Vec<SynthEntry>,
    next_id: &mut u32,
    big_dirs: bool,
    all_objects: &mut Vec<(u32, Vec<u8>)>,
    parent_sin: u32,
) -> Vec<ChildMeta> {
    let mut entries = entries;
    entries.sort_by(|a, b| {
        crate::format::filecore::dir_old::name_collation_cmp(
            &encode_charset_str(entry_name(a)),
            &encode_charset_str(entry_name(b)),
        )
    });
    let mut result = Vec::new();
    for e in entries {
        match e {
            SynthEntry::File(f) => {
                let id = *next_id;
                *next_id += 1;
                let len = f.content.len();
                all_objects.push((id, f.content));
                result.push(ChildMeta {
                    name: f.name,
                    load: f.load,
                    exec: f.exec,
                    attrs: f.attrs,
                    fragment_id: id,
                    content_len: len,
                });
            }
            SynthEntry::Dir {
                name,
                attrs,
                children,
            } => {
                // Allocate this directory's own fragment ID before
                // serializing its children - they must reference this
                // directory's SIN as their parent, and that ID isn't known
                // until we assign it here.
                let id = *next_id;
                *next_id += 1;
                let self_sin = id << 8;
                let child_meta =
                    allocate_objects(children, next_id, big_dirs, all_objects, self_sin);
                let data = if big_dirs {
                    serialize_big_dir(&name, &child_meta, parent_sin)
                } else {
                    serialize_new_dir(&name, &child_meta, b"Nick", parent_sin)
                };
                let len = data.len();
                all_objects.push((id, data));
                result.push(ChildMeta {
                    name,
                    load: 0,
                    exec: 0,
                    attrs: attrs | ATTR_DIRECTORY,
                    fragment_id: id,
                    content_len: len,
                });
            }
        }
    }
    result
}

fn serialize_new_dir(
    name: &str,
    children: &[ChildMeta],
    validation: &[u8; 4],
    parent_sin: u32,
) -> Vec<u8> {
    let dir_len = dir_old::LARGE_DIR_SIZE;
    let mut buf = vec![0u8; dir_len];
    buf[0] = 1;
    buf[1..5].copy_from_slice(validation);

    for (i, child) in children.iter().enumerate() {
        let off = dir_old::HEADER_SIZE + i * dir_old::ENTRY_SIZE;
        let name_bytes = pad_to(encode_charset_str(&child.name), 10);
        buf[off..off + 10].copy_from_slice(&name_bytes);
        buf[off + 0x0A..off + 0x0E].copy_from_slice(&child.load.to_le_bytes());
        buf[off + 0x0E..off + 0x12].copy_from_slice(&child.exec.to_le_bytes());
        buf[off + 0x12..off + 0x16].copy_from_slice(&(child.content_len as u32).to_le_bytes());
        let sin = if child.content_len == 0 {
            0u32
        } else {
            child.fragment_id << 8
        };
        buf[off + 0x16] = (sin & 0xFF) as u8;
        buf[off + 0x17] = ((sin >> 8) & 0xFF) as u8;
        buf[off + 0x18] = ((sin >> 16) & 0xFF) as u8;
        buf[off + 0x19] = (child.attrs & 0xFF) as u8;
    }

    let tail = dir_old::tail_layout(false);
    // The tail NewDirParent must hold the containing directory's SIN (the
    // root points back to its own SIN), not a byte address.
    buf[tail.parent] = (parent_sin & 0xFF) as u8;
    buf[tail.parent + 1] = ((parent_sin >> 8) & 0xFF) as u8;
    buf[tail.parent + 2] = ((parent_sin >> 16) & 0xFF) as u8;
    let title_bytes = pad_to(encode_charset_str(name), tail.title.1);
    buf[tail.title.0..tail.title.0 + tail.title.1].copy_from_slice(&title_bytes);
    let name_bytes = pad_to(encode_charset_str(name), tail.name.1);
    buf[tail.name.0..tail.name.0 + tail.name.1].copy_from_slice(&name_bytes);
    buf[tail.end_seq] = 1;
    buf[tail.end_validation.0..tail.end_validation.0 + tail.end_validation.1]
        .copy_from_slice(validation);

    let end_of_entries = dir_old::HEADER_SIZE + children.len() * dir_old::ENTRY_SIZE;
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

fn serialize_big_dir(name: &str, children: &[ChildMeta], parent_sin: u32) -> Vec<u8> {
    use crate::format::filecore::dir_big::{ENTRY_SIZE, HEADER_FIXED_SIZE};

    let dir_name_bytes = encode_charset_str(name);
    let dir_name_len = dir_name_bytes.len();
    let mut dir_name_padded = dir_name_bytes.clone();
    dir_name_padded.push(0x0D);
    while !dir_name_padded.len().is_multiple_of(4) {
        dir_name_padded.push(0);
    }

    let entries_start = HEADER_FIXED_SIZE + dir_name_padded.len();

    let mut heap = Vec::new();
    let mut name_meta = Vec::with_capacity(children.len());
    for child in children {
        let ptr = heap.len();
        let mut nb = encode_charset_str(&child.name);
        let namelen = nb.len();
        nb.push(0x0D);
        while !nb.len().is_multiple_of(4) {
            nb.push(0);
        }
        heap.extend(nb);
        name_meta.push((ptr, namelen));
    }
    let names_size = heap.len();

    let region1_end = entries_start + children.len() * ENTRY_SIZE + names_size;
    let tail_start = region1_end;
    let big_dir_size = tail_start + 8;

    let mut buf = vec![0u8; big_dir_size];
    buf[0] = 1;
    buf[4..8].copy_from_slice(b"SBPr");
    buf[8..12].copy_from_slice(&(dir_name_len as u32).to_le_bytes());
    buf[12..16].copy_from_slice(&(big_dir_size as u32).to_le_bytes());
    buf[16..20].copy_from_slice(&(children.len() as u32).to_le_bytes());
    buf[20..24].copy_from_slice(&(names_size as u32).to_le_bytes());
    buf[24..28].copy_from_slice(&parent_sin.to_le_bytes());
    buf[HEADER_FIXED_SIZE..HEADER_FIXED_SIZE + dir_name_padded.len()]
        .copy_from_slice(&dir_name_padded);

    for (i, child) in children.iter().enumerate() {
        let off = entries_start + i * ENTRY_SIZE;
        buf[off..off + 4].copy_from_slice(&child.load.to_le_bytes());
        buf[off + 4..off + 8].copy_from_slice(&child.exec.to_le_bytes());
        buf[off + 8..off + 12].copy_from_slice(&(child.content_len as u32).to_le_bytes());
        let sin = if child.content_len == 0 {
            0u32
        } else {
            child.fragment_id << 8
        };
        buf[off + 12..off + 16].copy_from_slice(&sin.to_le_bytes());
        buf[off + 16..off + 20].copy_from_slice(&child.attrs.to_le_bytes());
        let (ptr, namelen) = name_meta[i];
        buf[off + 20..off + 24].copy_from_slice(&(namelen as u32).to_le_bytes());
        buf[off + 24..off + 28].copy_from_slice(&(ptr as u32).to_le_bytes());
    }

    let heap_start = entries_start + children.len() * ENTRY_SIZE;
    buf[heap_start..heap_start + names_size].copy_from_slice(&heap);

    buf[tail_start..tail_start + 4].copy_from_slice(b"oven");
    buf[tail_start + 4] = 1;

    let regions = [
        ChecksumRegion {
            start: 0,
            end: region1_end,
            words_first: true,
        },
        ChecksumRegion {
            start: tail_start,
            end: tail_start + 7,
            words_first: true,
        },
    ];
    let checksum = dir_checksum_fold(dir_checksum_accumulate(&buf, &regions));
    buf[tail_start + 7] = checksum;

    buf
}

struct BitWriter {
    buf: Vec<u8>,
}

impl BitWriter {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn set_bit(&mut self, pos: usize, val: u8) {
        let byte_idx = pos / 8;
        while self.buf.len() <= byte_idx {
            self.buf.push(0);
        }
        if val == 1 {
            self.buf[byte_idx] |= 1 << (pos % 8);
        }
    }

    /// Writes a fragment descriptor: `idlen`-bit id (LSB-first), zero
    /// padding, terminating 1 bit, totalling exactly `total_units` bits.
    fn write_fragment(&mut self, bit_pos: &mut usize, id: u32, idlen: u32, total_units: u64) {
        for i in 0..idlen {
            self.set_bit(*bit_pos, ((id >> i) & 1) as u8);
            *bit_pos += 1;
        }
        let padding = total_units as usize - idlen as usize - 1;
        for _ in 0..padding {
            self.set_bit(*bit_pos, 0);
            *bit_pos += 1;
        }
        self.set_bit(*bit_pos, 1);
        *bit_pos += 1;
    }

    fn into_bytes(mut self, min_len: usize) -> Vec<u8> {
        while self.buf.len() < min_len {
            self.buf.push(0);
        }
        self.buf
    }
}

#[allow(clippy::too_many_arguments)]
fn build_disc_record_bytes(
    log2_sector_size: u8,
    idlen: u8,
    log2_bpmb: u8,
    zone_spare: u16,
    root_dir: u32,
    disc_size: u32,
    disc_name: &str,
    format_version: u32,
    root_size: u32,
) -> [u8; 60] {
    let mut b = [0u8; 60];
    b[0] = log2_sector_size;
    b[4] = idlen;
    b[5] = log2_bpmb;
    b[9] = 1; // nzones lo
    b[0x0A..0x0C].copy_from_slice(&zone_spare.to_le_bytes());
    b[0x0C..0x10].copy_from_slice(&root_dir.to_le_bytes());
    b[0x10..0x14].copy_from_slice(&disc_size.to_le_bytes());
    b[0x14..0x16].copy_from_slice(&0xABCDu16.to_le_bytes());
    let name_bytes = pad_to(encode_charset_str(disc_name), 10);
    b[0x16..0x20].copy_from_slice(&name_bytes);
    b[0x2C..0x30].copy_from_slice(&format_version.to_le_bytes());
    b[0x30..0x34].copy_from_slice(&root_size.to_le_bytes());
    b
}

/// Builds a single-zone new-map disc image (E-style): zone map (double
/// copied) + root directory as fragment ID 2, everything else placed
/// sequentially after it.
pub fn build_new_map_disc(root_children: Vec<SynthEntry>, cfg: &NewMapConfig) -> BuiltImage {
    let sector_size = 1usize << cfg.log2_sector_size;
    let bpmb = 1u64 << cfg.log2_bpmb;
    let idlen = cfg.idlen as u32;
    let min_units = idlen as u64 + 1;

    let mut next_id = 3u32;
    let mut all_objects: Vec<(u32, Vec<u8>)> = Vec::new();
    // The root directory's own SIN: fragment 2 (the system object), sharing
    // offset 3 (it starts at byte 2*sector_size). The root's NewDirParent
    // points back to this own SIN.
    let root_sin = (2u32 << 8) | 3;
    let root_children_meta = allocate_objects(
        root_children,
        &mut next_id,
        cfg.big_dirs,
        &mut all_objects,
        root_sin,
    );
    let root_data = if cfg.big_dirs {
        serialize_big_dir("$", &root_children_meta, root_sin)
    } else {
        serialize_new_dir("$", &root_children_meta, b"Hugo", root_sin)
    };

    let root_len = root_data.len() as u64;
    let system_object_bytes = 2 * sector_size as u64 + root_len;
    let system_units = system_object_bytes.div_ceil(bpmb).max(min_units);

    let mut addr = system_units * bpmb;
    let mut placements: Vec<(u32, u64, u64)> = Vec::new();
    for (id, data) in &all_objects {
        if data.is_empty() {
            continue;
        }
        let units = (data.len() as u64).div_ceil(bpmb).max(min_units);
        placements.push((*id, addr, units));
        addr += units * bpmb;
    }

    let zone_spare: u64 = 32;
    let zone0_bits = (sector_size as u64 * 8) - zone_spare - 480;
    let used_units = system_units + placements.iter().map(|(_, _, u)| u).sum::<u64>();
    assert!(
        used_units < zone0_bits,
        "synthetic image too small: increase log2_sector_size or reduce content ({used_units} >= {zone0_bits})"
    );
    let free_units = zone0_bits - used_units;

    let mut bw = BitWriter::new();
    let mut bit_pos = 64 * 8;
    bw.write_fragment(&mut bit_pos, 2, idlen, system_units);
    for (id, _addr, units) in &placements {
        bw.write_fragment(&mut bit_pos, *id, idlen, *units);
    }
    let free_frag_start_bit = bit_pos;
    bw.write_fragment(&mut bit_pos, 0, idlen, free_units);
    let free_link_value = ((free_frag_start_bit - 8) as u16) | 0x8000;

    let bitstream = bw.into_bytes(sector_size);

    let dr_bytes = build_disc_record_bytes(
        cfg.log2_sector_size,
        cfg.idlen,
        cfg.log2_bpmb,
        zone_spare as u16,
        (2u32 << 8) | 3, // root dir starts at byte 2*sector_size within fragment 2 -> sharing_offset=3
        addr as u32,
        &cfg.disc_name,
        if cfg.big_dirs { 1 } else { 0 },
        if cfg.big_dirs {
            root_data.len() as u32
        } else {
            0
        },
    );

    let mut zone0 = vec![0u8; sector_size];
    zone0[1..3].copy_from_slice(&free_link_value.to_le_bytes());
    zone0[3] = 0xFF; // CrossCheck: single zone, XOR of all zones must be 0xFF
    zone0[4..64].copy_from_slice(&dr_bytes);
    zone0[64..].copy_from_slice(&bitstream[64..sector_size]);
    zone0[0] = zone_check(&zone0);

    let disc_size = addr as usize;
    let mut disc = vec![0u8; disc_size];
    disc[0..sector_size].copy_from_slice(&zone0);
    disc[sector_size..2 * sector_size].copy_from_slice(&zone0);
    disc[2 * sector_size..2 * sector_size + root_data.len()].copy_from_slice(&root_data);

    let data_by_id: HashMap<u32, &Vec<u8>> = all_objects.iter().map(|(id, d)| (*id, d)).collect();
    for (id, paddr, _units) in &placements {
        let data = data_by_id[id];
        let start = *paddr as usize;
        disc[start..start + data.len()].copy_from_slice(data);
    }

    BuiltImage { bytes: disc }
}

/// Builds a new-map disc with a deliberately fragmented file: two separate
/// same-ID fragments in zone 0, separated by another object's data
/// (mirroring what §4.3's "extend, can't do in place" leaves behind).
/// Returns the image plus the expected reassembled content, for the test
/// to compare against.
pub fn build_fragmented_file_disc(
    cfg: &NewMapConfig,
    part_a: &[u8],
    filler: &[u8],
    part_b: &[u8],
) -> (BuiltImage, Vec<u8>) {
    let sector_size = 1usize << cfg.log2_sector_size;
    let bpmb = 1u64 << cfg.log2_bpmb;
    let idlen = cfg.idlen as u32;
    let min_units = idlen as u64 + 1;

    let file_id = 3u32;
    let filler_id = 4u32;

    // Directory has one entry: the fragmented file, total length = a+b.
    let total_len = (part_a.len() + part_b.len()) as u32;
    let mut root = vec![0u8; dir_old::LARGE_DIR_SIZE];
    root[0] = 1;
    root[1..5].copy_from_slice(b"Hugo");
    let off = dir_old::HEADER_SIZE;
    let name = pad_to(encode_charset_str("Frag"), 10);
    root[off..off + 10].copy_from_slice(&name);
    root[off + 0x12..off + 0x16].copy_from_slice(&total_len.to_le_bytes());
    let sin = file_id << 8;
    root[off + 0x16] = (sin & 0xFF) as u8;
    root[off + 0x17] = ((sin >> 8) & 0xFF) as u8;
    root[off + 0x18] = ((sin >> 16) & 0xFF) as u8;
    root[off + 0x19] = 0x03;
    let tail = dir_old::tail_layout(false);
    let root_sin = (2u32 << 8) | 3;
    root[tail.parent] = (root_sin & 0xFF) as u8;
    root[tail.parent + 1] = ((root_sin >> 8) & 0xFF) as u8;
    root[tail.parent + 2] = ((root_sin >> 16) & 0xFF) as u8;
    root[tail.name.0..tail.name.0 + tail.name.1]
        .copy_from_slice(&pad_to(encode_charset_str("$"), tail.name.1));
    root[tail.end_seq] = 1;
    root[tail.end_validation.0..tail.end_validation.0 + 4].copy_from_slice(b"Hugo");
    let end_of_entries = dir_old::HEADER_SIZE + dir_old::ENTRY_SIZE;
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
    root[tail.check_byte] = dir_checksum_fold(dir_checksum_accumulate(&root, &regions));

    let root_len = root.len() as u64;
    let system_units = (2 * sector_size as u64 + root_len)
        .div_ceil(bpmb)
        .max(min_units);
    let mut addr = system_units * bpmb;

    let a_units = (part_a.len() as u64).div_ceil(bpmb).max(min_units);
    let a_addr = addr;
    addr += a_units * bpmb;
    let filler_units = (filler.len() as u64).div_ceil(bpmb).max(min_units);
    let filler_addr = addr;
    addr += filler_units * bpmb;
    let b_units = (part_b.len() as u64).div_ceil(bpmb).max(min_units);
    let b_addr = addr;
    addr += b_units * bpmb;

    let zone_spare: u64 = 32;
    let zone0_bits = (sector_size as u64 * 8) - zone_spare - 480;
    let used = system_units + a_units + filler_units + b_units;
    assert!(
        used < zone0_bits,
        "synthetic fragmented-file image too small"
    );
    let free_units = zone0_bits - used;

    let mut bw = BitWriter::new();
    let mut bit_pos = 64 * 8;
    bw.write_fragment(&mut bit_pos, 2, idlen, system_units);
    bw.write_fragment(&mut bit_pos, file_id, idlen, a_units); // first fragment of the file
    bw.write_fragment(&mut bit_pos, filler_id, idlen, filler_units); // unrelated data in between
    bw.write_fragment(&mut bit_pos, file_id, idlen, b_units); // second fragment, same id
    let free_start = bit_pos;
    bw.write_fragment(&mut bit_pos, 0, idlen, free_units);
    let free_link_value = ((free_start - 8) as u16) | 0x8000;

    let bitstream = bw.into_bytes(sector_size);
    let dr_bytes = build_disc_record_bytes(
        cfg.log2_sector_size,
        cfg.idlen,
        cfg.log2_bpmb,
        zone_spare as u16,
        (2u32 << 8) | 3,
        addr as u32,
        &cfg.disc_name,
        0,
        0,
    );
    let mut zone0 = vec![0u8; sector_size];
    zone0[1..3].copy_from_slice(&free_link_value.to_le_bytes());
    zone0[3] = 0xFF;
    zone0[4..64].copy_from_slice(&dr_bytes);
    zone0[64..].copy_from_slice(&bitstream[64..sector_size]);
    zone0[0] = zone_check(&zone0);

    let mut disc = vec![0u8; addr as usize];
    disc[0..sector_size].copy_from_slice(&zone0);
    disc[sector_size..2 * sector_size].copy_from_slice(&zone0);
    disc[2 * sector_size..2 * sector_size + root.len()].copy_from_slice(&root);
    disc[a_addr as usize..a_addr as usize + part_a.len()].copy_from_slice(part_a);
    disc[filler_addr as usize..filler_addr as usize + filler.len()].copy_from_slice(filler);
    disc[b_addr as usize..b_addr as usize + part_b.len()].copy_from_slice(part_b);

    let mut expected = part_a.to_vec();
    expected.extend_from_slice(part_b);
    (BuiltImage { bytes: disc }, expected)
}

// ---------------------------------------------------------------------
// Old-map (S/M/L/D-style) builder - flat root directory only.
// ---------------------------------------------------------------------

pub fn build_old_map_disc(files: Vec<SynthFile>, small: bool, disc_name: &str) -> BuiltImage {
    use crate::format::filecore::checksums::old_map_checksum;

    let dir_len = if small {
        dir_old::SMALL_DIR_SIZE
    } else {
        dir_old::LARGE_DIR_SIZE
    };
    let root_addr: u64 = if small { 0x200 } else { 0x400 };
    let mut next_addr_unit = (root_addr + dir_len as u64) / 256;

    let mut root = vec![0u8; dir_len];
    root[0] = 1;
    root[1..5].copy_from_slice(b"Hugo");

    let mut placements = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let off = dir_old::HEADER_SIZE + i * dir_old::ENTRY_SIZE;
        let addr_units = next_addr_unit;
        let len_units = (f.content.len() as u64).div_ceil(256).max(1);
        next_addr_unit += len_units;
        placements.push((addr_units * 256, &f.content));

        if small {
            let mut name_bytes = pad_to(encode_charset_str(&f.name), 10);
            for (bit, mask) in [(0, 0x01u32), (1, 0x02), (2, 0x04), (3, 0x08)] {
                if f.attrs & mask != 0 {
                    name_bytes[bit] |= 0x80;
                }
            }
            root[off..off + 10].copy_from_slice(&name_bytes);
            root[off + 0x19] = 1; // per-entry sequence number (unused by the reader)
        } else {
            let name_bytes = pad_to(encode_charset_str(&f.name), 10);
            root[off..off + 10].copy_from_slice(&name_bytes);
            root[off + 0x19] = (f.attrs & 0xFF) as u8;
        }
        root[off + 0x0A..off + 0x0E].copy_from_slice(&f.load.to_le_bytes());
        root[off + 0x0E..off + 0x12].copy_from_slice(&f.exec.to_le_bytes());
        root[off + 0x12..off + 0x16].copy_from_slice(&(f.content.len() as u32).to_le_bytes());
        let addr_u24 = addr_units as u32;
        root[off + 0x16] = (addr_u24 & 0xFF) as u8;
        root[off + 0x17] = ((addr_u24 >> 8) & 0xFF) as u8;
        root[off + 0x18] = ((addr_u24 >> 16) & 0xFF) as u8;
    }

    let tail = dir_old::tail_layout(small);
    let name_bytes = pad_to(encode_charset_str("$"), tail.name.1);
    root[tail.name.0..tail.name.0 + tail.name.1].copy_from_slice(&name_bytes);
    root[tail.end_seq] = 1;
    root[tail.end_validation.0..tail.end_validation.0 + 4].copy_from_slice(b"Hugo");

    let end_of_entries = dir_old::HEADER_SIZE + files.len() * dir_old::ENTRY_SIZE;
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
    root[tail.check_byte] = dir_checksum_fold(dir_checksum_accumulate(&root, &regions));

    let disc_size = (next_addr_unit * 256) as usize;
    let mut disc = vec![0u8; disc_size];
    disc[root_addr as usize..root_addr as usize + dir_len].copy_from_slice(&root);
    for (addr, content) in &placements {
        disc[*addr as usize..*addr as usize + content.len()].copy_from_slice(content);
    }

    // Old free-space map (guide §2.5) - one extent covering everything
    // after the root dir, for a plausible (if not perfectly accurate once
    // files are placed) report; not used for file resolution.
    let mut s0 = [0u8; 256];
    let mut s1 = [0u8; 256];
    let name10 = pad_to(encode_charset_str(disc_name), 10);
    for i in 0..5 {
        s0[0xF7 + i] = name10[i * 2];
        s1[0xF6 + i] = name10[i * 2 + 1];
    }
    let total_sectors = (disc_size / 256) as u32;
    s0[0xFC] = (total_sectors & 0xFF) as u8;
    s0[0xFD] = ((total_sectors >> 8) & 0xFF) as u8;
    s0[0xFE] = ((total_sectors >> 16) & 0xFF) as u8;
    s1[0xFE] = 3; // one extent entry (3 bytes)
    s0[0xFF] = old_map_checksum(&s0);
    s1[0xFF] = old_map_checksum(&s1);
    disc[0x000..0x100].copy_from_slice(&s0);
    disc[0x100..0x200].copy_from_slice(&s1);

    BuiltImage { bytes: disc }
}

// ---------------------------------------------------------------------
// DFS (Disc Filing System) builder
// ---------------------------------------------------------------------

pub struct DfsFile {
    pub name: String,
    pub dir_char: char,
    pub locked: bool,
    pub load: u32,
    pub exec: u32,
    pub content: Vec<u8>,
}

impl DfsFile {
    /// A file in the `$` directory with a plain (non-date-stamped) address
    /// pair. `load`/`exec` must each fit the DFS-representable shape: low
    /// 16 bits free, bits 16-23 one of `0x00/0x55/0xAA/0xFF` (the only
    /// values the 2-bit hardware field can reproduce), bits 24-31 zero.
    pub fn plain(name: &str, content: &[u8]) -> Self {
        Self {
            name: name.to_string(),
            dir_char: '$',
            locked: false,
            load: 0x1900,
            exec: 0x1900,
            content: content.to_vec(),
        }
    }

    pub fn in_dir(name: &str, dir_char: char, content: &[u8]) -> Self {
        assert!(
            dir_char.is_ascii(),
            "test dir chars are kept ASCII for simplicity"
        );
        Self {
            name: name.to_string(),
            dir_char,
            locked: false,
            load: 0x1900,
            exec: 0x1900,
            content: content.to_vec(),
        }
    }

    pub fn locked(mut self) -> Self {
        self.locked = true;
        self
    }
}

pub struct DfsSideSpec {
    pub title: String,
    pub boot_option: u8,
    pub files: Vec<DfsFile>,
}

impl DfsSideSpec {
    pub fn new(title: &str, files: Vec<DfsFile>) -> Self {
        Self {
            title: title.to_string(),
            boot_option: 0,
            files,
        }
    }
}

/// 8-bit high byte (bits 16-23 of a load/exec address) -> the 2-bit field
/// DFS actually stores, inverting `catalogue::replicate_2bit`.
fn encode_2bit(high_byte: u8) -> u8 {
    match high_byte {
        0x00 => 0,
        0x55 => 1,
        0xAA => 2,
        0xFF => 3,
        other => panic!(
            "DFS load/exec high byte must be one of 0x00/0x55/0xAA/0xFF (the 2-bit field is \
             pattern-replicated to a full byte), got {other:#04x}"
        ),
    }
}

fn write_logical(
    disc: &mut [u8],
    geometry: &DfsGeometry,
    side: u8,
    logical_addr: u64,
    data: &[u8],
) {
    let extents = geometry.translate(side, logical_addr, data.len() as u64);
    let mut offset = 0usize;
    for e in extents {
        let len = e.len as usize;
        let start = e.disc_addr as usize;
        disc[start..start + len].copy_from_slice(&data[offset..offset + len]);
        offset += len;
    }
}

struct PlacedFile<'a> {
    file: &'a DfsFile,
    start_sector: u32,
}

fn place_side_files(spec: &DfsSideSpec) -> (Vec<PlacedFile<'_>>, u32) {
    let watford = spec.files.len() > 31;
    let mut next_sector = if watford { 4 } else { 2 };
    let mut placed = Vec::with_capacity(spec.files.len());
    for f in &spec.files {
        let start_sector = next_sector;
        let len_sectors = (f.content.len() as u32).div_ceil(256).max(1);
        next_sector += len_sectors;
        placed.push(PlacedFile {
            file: f,
            start_sector,
        });
    }
    (placed, next_sector)
}

fn encode_entry_info(f: &DfsFile, start_sector: u32) -> [u8; 8] {
    let length = f.content.len();
    assert!(length < (1 << 18), "DFS file length must fit in 18 bits");
    assert!(start_sector < 1024, "DFS start sector must fit in 10 bits");
    assert_eq!(
        f.load >> 24,
        0,
        "DFS load must fit in 24 bits (2-bit extension + 16-bit low word)"
    );
    assert_eq!(f.exec >> 24, 0, "DFS exec must fit in 24 bits");

    let load_hi = encode_2bit(((f.load >> 16) & 0xFF) as u8);
    let exec_hi = encode_2bit(((f.exec >> 16) & 0xFF) as u8);
    let length_hi = ((length >> 16) & 0x3) as u8;
    let sector_hi = ((start_sector >> 8) & 0x3) as u8;
    let ext = sector_hi | (load_hi << 2) | (length_hi << 4) | (exec_hi << 6);

    let mut info = [0u8; 8];
    info[0..2].copy_from_slice(&(f.load as u16).to_le_bytes());
    info[2..4].copy_from_slice(&(f.exec as u16).to_le_bytes());
    info[4..6].copy_from_slice(&(length as u16).to_le_bytes());
    info[6] = ext;
    info[7] = (start_sector & 0xFF) as u8;
    info
}

fn write_side_catalogue(
    disc: &mut [u8],
    geometry: &DfsGeometry,
    side: u8,
    spec: &DfsSideSpec,
    total_sectors: u32,
    placed: &[PlacedFile],
) {
    let watford = placed.len() > 31;
    let mut s0 = [0u8; 256];
    let mut s1 = [0u8; 256];

    let title_bytes = pad_to(encode_charset_str(&spec.title), 12);
    s0[0..8].copy_from_slice(&title_bytes[0..8]);
    s1[0..4].copy_from_slice(&title_bytes[8..12]);

    let std_count = placed.len().min(31);
    s1[5] = (std_count * 8) as u8;
    s1[6] = ((spec.boot_option & 0x3) << 4) | (((total_sectors >> 8) & 0x3) as u8);
    s1[7] = (total_sectors & 0xFF) as u8;

    for (i, pf) in placed.iter().take(31).enumerate() {
        let off = 8 + i * 8;
        let mut name_field = [b' '; 7];
        for (j, b) in encode_charset_str(&pf.file.name)
            .into_iter()
            .take(7)
            .enumerate()
        {
            name_field[j] = b;
        }
        s0[off..off + 7].copy_from_slice(&name_field);
        s0[off + 7] = pf.file.dir_char as u8 | if pf.file.locked { 0x80 } else { 0 };
        s1[off..off + 8].copy_from_slice(&encode_entry_info(pf.file, pf.start_sector));
    }

    write_logical(disc, geometry, side, 0x000, &s0);
    write_logical(disc, geometry, side, 0x100, &s1);

    if watford {
        let mut s2 = [0u8; 256];
        let mut s3 = [0u8; 256];
        s2[0..8].copy_from_slice(&[0xAA; 8]);
        let extra = &placed[31..];
        s3[5] = (extra.len() * 8) as u8;
        s3[6] = s1[6];
        s3[7] = s1[7];
        for (i, pf) in extra.iter().enumerate() {
            let off = 8 + i * 8;
            let mut name_field = [b' '; 7];
            for (j, b) in encode_charset_str(&pf.file.name)
                .into_iter()
                .take(7)
                .enumerate()
            {
                name_field[j] = b;
            }
            s2[off..off + 7].copy_from_slice(&name_field);
            s2[off + 7] = pf.file.dir_char as u8 | if pf.file.locked { 0x80 } else { 0 };
            s3[off..off + 8].copy_from_slice(&encode_entry_info(pf.file, pf.start_sector));
        }
        write_logical(disc, geometry, side, 0x200, &s2);
        write_logical(disc, geometry, side, 0x300, &s3);
    }
}

/// Builds a synthetic DFS image: one side for `.ssd`, two for `.dsd`. File
/// content is written at its *physically interleaved* location for a
/// double-sided image (via `DfsGeometry::translate`, the same code path the
/// reader uses), not at a flat logical offset - a synthetic image that
/// skipped this would never exercise the track-boundary-split logic the way
/// a real `.dsd` does.
pub fn build_dfs_disc(sides: Vec<DfsSideSpec>) -> BuiltImage {
    assert!(
        (1..=2).contains(&sides.len()),
        "DFS images are single- or double-sided"
    );
    let double_sided = sides.len() == 2;
    let geometry = DfsGeometry { double_sided };

    let mut per_side = Vec::with_capacity(sides.len());
    let mut max_logical_sectors: u32 = 0;
    for spec in &sides {
        let (placed, needed) = place_side_files(spec);
        max_logical_sectors = max_logical_sectors.max(needed);
        per_side.push(placed);
    }

    let total_sectors = max_logical_sectors;
    let physical_sectors = if double_sided {
        total_sectors.div_ceil(10) * 20
    } else {
        total_sectors
    };
    let mut disc = vec![0u8; physical_sectors as usize * 256];

    for (side_idx, spec) in sides.iter().enumerate() {
        let side = side_idx as u8;
        let placed = &per_side[side_idx];
        write_side_catalogue(&mut disc, &geometry, side, spec, total_sectors, placed);
        for pf in placed {
            let logical_addr = pf.start_sector as u64 * 256;
            write_logical(&mut disc, &geometry, side, logical_addr, &pf.file.content);
        }
    }

    BuiltImage { bytes: disc }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::filecore::FileCoreFs;
    use crate::format::fs::FileSystem;

    #[test]
    fn new_map_round_trip() {
        let cfg = NewMapConfig::default();
        let image = build_new_map_disc(
            vec![
                SynthEntry::File(SynthFile::plain("Fred", b"hello world")),
                SynthEntry::File(SynthFile::typed(
                    "Data",
                    0xFFD,
                    1_700_000_000,
                    b"typed content",
                )),
            ],
            &cfg,
        );
        let mut fs = FileCoreFs::open(image.cursor()).expect("should recognise synthetic image");
        let root = fs.root().unwrap();
        let listing = fs.list(&root).unwrap();
        assert!(!listing.is_broken, "anomalies: {:?}", listing.anomalies);
        assert_eq!(listing.objects.len(), 2);
        let fred = listing.objects.iter().find(|o| o.name == "Fred").unwrap();
        let mut collected = Vec::new();
        fs.read_object(fred, &mut |_addr, chunk| {
            collected.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
        assert_eq!(collected, b"hello world");
    }

    #[test]
    fn fragmented_file_reassembles_in_order() {
        let cfg = NewMapConfig::default();
        // Each fragment's allocation rounds up to at least (idlen+1)*bpmb
        // bytes; use exactly that size for both parts so truncating the
        // resolved extents to the directory entry's exact length doesn't
        // cut into the second fragment's padding instead of its real data.
        let unit_bytes = (cfg.idlen as usize + 1) * (1usize << cfg.log2_bpmb);
        let part_a = vec![b'A'; unit_bytes];
        let part_b = vec![b'B'; unit_bytes];
        let (image, expected) = build_fragmented_file_disc(&cfg, &part_a, b"IGNORE ME", &part_b);
        let mut fs = FileCoreFs::open(image.cursor()).unwrap();
        let root = fs.root().unwrap();
        let listing = fs.list(&root).unwrap();
        assert!(!listing.is_broken, "anomalies: {:?}", listing.anomalies);
        let frag = &listing.objects[0];
        assert_eq!(frag.extents.len(), 2, "expected two fragments");
        let mut collected = Vec::new();
        fs.read_object(frag, &mut |_addr, chunk| {
            collected.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
        assert_eq!(collected, expected);
    }

    #[test]
    fn old_map_small_round_trip() {
        let image = build_old_map_disc(
            vec![SynthFile::plain("Fred", b"old map content")],
            true,
            "OldDisc",
        );
        let mut fs = FileCoreFs::open(image.cursor()).unwrap();
        let root = fs.root().unwrap();
        let listing = fs.list(&root).unwrap();
        assert!(!listing.is_broken, "anomalies: {:?}", listing.anomalies);
        assert_eq!(listing.objects.len(), 1);
        assert_eq!(listing.objects[0].name, "Fred");
    }

    #[test]
    fn big_dir_round_trip() {
        let cfg = NewMapConfig {
            big_dirs: true,
            ..NewMapConfig::default()
        };
        let image = build_new_map_disc(
            vec![SynthEntry::File(SynthFile::plain(
                "LongFileName",
                b"big dir content",
            ))],
            &cfg,
        );
        let mut fs = FileCoreFs::open(image.cursor()).unwrap();
        let root = fs.root().unwrap();
        let listing = fs.list(&root).unwrap();
        assert!(!listing.is_broken, "anomalies: {:?}", listing.anomalies);
        assert_eq!(listing.objects[0].name, "LongFileName");
    }

    #[test]
    fn dfs_ssd_round_trip() {
        use crate::format::dfs::DfsFs;

        let image = build_dfs_disc(vec![DfsSideSpec::new(
            "MYDISC",
            vec![
                DfsFile::plain("BOOT", b"boot text"),
                DfsFile::in_dir("CODE", 'L', b"code bytes").locked(),
            ],
        )]);
        let mut fs = DfsFs::open(image.cursor()).unwrap();
        assert!(!fs.double_sided);
        let root = fs.root().unwrap();
        let listing = fs.list(&root).unwrap();
        assert!(!listing.is_broken, "anomalies: {:?}", listing.anomalies);
        assert_eq!(listing.title, "MYDISC");
        assert_eq!(listing.objects.len(), 2);

        let boot = listing.objects.iter().find(|o| o.name == "BOOT").unwrap();
        let mut collected = Vec::new();
        fs.read_object(boot, &mut |_addr, chunk| {
            collected.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
        assert_eq!(collected, b"boot text");

        let code = listing.objects.iter().find(|o| o.name == "L.CODE").unwrap();
        assert!(code.is_locked());
    }

    #[test]
    fn dfs_dsd_round_trip_with_track_spanning_file() {
        use crate::format::dfs::DfsFs;

        // Pad side 0 with a filler file that exactly fills sectors 2-7, so
        // the real content file starts at sector 8 (still track 0, which
        // ends at sector 9) and its 4-sector run crosses into track 1 -
        // this is the scenario that would silently corrupt if the
        // interleave split in `geometry.rs` were wrong.
        let filler = vec![DfsFile::plain("FILLER", &[0xEE; 6 * 256])];
        let content: Vec<u8> = (0..(3 * 256 + 40)).map(|i| (i % 256) as u8).collect();
        let mut side0_files = filler;
        side0_files.push(DfsFile::plain("SPAN", &content));

        let image = build_dfs_disc(vec![
            DfsSideSpec::new("SIDE0", side0_files),
            DfsSideSpec::new("SIDE1", vec![DfsFile::plain("OTHER", b"side 1 content")]),
        ]);
        let mut fs = DfsFs::open(image.cursor()).unwrap();
        assert!(fs.double_sided);

        let root = fs.root().unwrap();
        let root_listing = fs.list(&root).unwrap();
        assert_eq!(root_listing.objects.len(), 2);
        let side0_obj = root_listing
            .objects
            .iter()
            .find(|o| o.name == "Side0")
            .unwrap();
        let side0_listing = fs.list(side0_obj).unwrap();
        assert!(
            !side0_listing.is_broken,
            "anomalies: {:?}",
            side0_listing.anomalies
        );

        let span = side0_listing
            .objects
            .iter()
            .find(|o| o.name == "SPAN")
            .unwrap();
        assert!(
            span.extents.len() >= 2,
            "expected the track-crossing file to split into multiple extents"
        );
        let mut collected = Vec::new();
        fs.read_object(span, &mut |_addr, chunk| {
            collected.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            collected, content,
            "reassembled bytes must match despite the track-boundary split"
        );

        let side1_obj = root_listing
            .objects
            .iter()
            .find(|o| o.name == "Side1")
            .unwrap();
        let side1_listing = fs.list(side1_obj).unwrap();
        assert_eq!(side1_listing.objects.len(), 1);
        assert_eq!(side1_listing.objects[0].name, "OTHER");
    }

    #[test]
    fn dfs_watford_extension_round_trip() {
        use crate::format::dfs::DfsFs;

        let files: Vec<DfsFile> = (0..40)
            .map(|i| DfsFile::plain(&format!("F{i}"), b"x"))
            .collect();
        let image = build_dfs_disc(vec![DfsSideSpec::new("WATFORD", files)]);
        let mut fs = DfsFs::open(image.cursor()).unwrap();
        let root = fs.root().unwrap();
        let listing = fs.list(&root).unwrap();
        assert!(!listing.is_broken, "anomalies: {:?}", listing.anomalies);
        assert_eq!(listing.objects.len(), 40);
        assert!(listing.objects.iter().any(|o| o.name == "F39"));
    }
}
