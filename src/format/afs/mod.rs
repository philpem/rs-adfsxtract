//! Acorn File Server (AFS0) filesystem support.
//!
//! Implements both the **Level 2** and **Level 3** File Server filesystems
//! (mdfs.net/Docs/Comp/Disk/Format/AFS0) - 256-byte logical sectors. L2FS uses
//! a single 12-bit per-sector allocation map and chains an object through it;
//! L3FS uses per-object "JesMap" allocation blocks and may coexist with an
//! ADFS partition (a hybrid disc). Detection: an `AFS0` signature at sector 0
//! identifies L2; for L3/hybrid the signature is at the sector pointed to by
//! the ADFS free-space map's FileServer start pointer (`&0F6`).
//!
//! Interleave handling: real FileStore media could be reformatted SEQ/INT/MUX.
//! We auto-detect by attempting each order and validating that the root
//! directory decodes cleanly, falling back to SEQ. Sequential images (the
//! common case for bitstream captures) will always be read as-is.

pub mod info;
pub mod read;

use crate::error::{FcError, Result};
use crate::format::afs::info::{AFS0_SIG, DiskInfo, SECTOR_SIZE};
use crate::format::afs::read::Interleave;
use crate::format::fs::{FileSystem, ListResult};
use crate::io::SectorSource;
use crate::model::object::{ATTR_DIRECTORY, Extent, Object, truncate_extents};

pub struct AfsFs<S: SectorSource> {
    source: S,
    pub info: DiskInfo,
    interleave: Interleave,
    spt: u16,
    l2_map: Vec<u8>,
}

fn u24le(b: &[u8], off: usize) -> u32 {
    b[off] as u32 | (b[off + 1] as u32) << 8 | (b[off + 2] as u32) << 16
}

/// Level-2 single-density sectors per track (the default L2 geometry; L2 is
/// not double-density on the standard file server).
const L2_SECTORS_PER_TRACK: u16 = 10;

fn read_l2_map<S: SectorSource>(
    source: &mut S,
    info: &DiskInfo,
    spt: u16,
    il: Interleave,
) -> Result<Vec<u8>> {
    let total = (info.sectors_per_side as usize) * 2;
    let bytes = total * 2 + 5;
    let sectors = bytes.div_ceil(SECTOR_SIZE);
    // Read both candidate maps and keep the one with the higher "current map"
    // indicator (byte 0) - the backup copy is older.
    let read_map = |source: &mut S, sin: u32| -> Result<Vec<u8>> {
        let mut out = vec![0u8; bytes];
        for i in 0..sectors {
            let phys = read::translate(sin + i as u32, spt, il);
            let mut buf = [0u8; SECTOR_SIZE];
            source.read_at(phys as u64 * SECTOR_SIZE as u64, &mut buf)?;
            let dst = i * SECTOR_SIZE;
            let n = (bytes - dst).min(SECTOR_SIZE);
            out[dst..dst + n].copy_from_slice(&buf[..n]);
        }
        Ok(out)
    };
    let map_a = read_map(source, info.map_a_sin)?;
    let map_b = read_map(source, info.map_b_sin)?;
    Ok(if map_b[0] > map_a[0] { map_b } else { map_a })
}

fn l2_entry(map: &[u8], sector: u32) -> u16 {
    let idx = (sector as usize) * 2 + 5;
    if idx + 2 > map.len() {
        return 0;
    }
    u16::from_le_bytes([map[idx], map[idx + 1]])
}

impl<S: SectorSource> AfsFs<S> {
    /// Opens an AFS0 image (see the free [`open`] function).
    pub fn open(source: S) -> Result<Self> {
        crate::format::afs::open(source)
    }

    fn translate(&self, logical: u32) -> u32 {
        read::translate(logical, self.spt, self.interleave)
    }

    fn read_sector(&mut self, logical: u32) -> Result<[u8; SECTOR_SIZE]> {
        let phys = self.translate(logical);
        let mut buf = [0u8; SECTOR_SIZE];
        self.source
            .read_at(phys as u64 * SECTOR_SIZE as u64, &mut buf)?;
        Ok(buf)
    }

    fn read_extents(&mut self, extents: &[Extent]) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(extents.iter().map(|e| e.len as usize).sum());
        for e in extents {
            let mut chunk = vec![0u8; e.len as usize];
            self.source.read_at(e.disc_addr, &mut chunk)?;
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    /// Resolves an object's allocation to disc extents (physical byte
    /// offsets) plus its byte length. `is_dir` controls whether the final
    /// partial sector is counted (directories are whole sectors). L2FS walks
    /// the 12-bit allocation-map chain; L3FS follows the JesMap group list
    /// (including any chained allocation sectors).
    fn resolve_object(&mut self, sin: u32, is_dir: bool) -> Result<(Vec<Extent>, u64)> {
        match self.info.level {
            AfsLevel::Level2 => self.resolve_l2(sin, is_dir),
            AfsLevel::Level3 => self.resolve_l3(sin, is_dir),
        }
    }

    fn resolve_l2(&mut self, sin: u32, is_dir: bool) -> Result<(Vec<Extent>, u64)> {
        let mut extents = Vec::new();
        let mut total: u64 = 0;
        let mut cur = sin & 0xFFF;
        let max_sectors = (self.info.sectors_per_side as u64) * 2 + 4;
        let mut guard = 0u64;
        loop {
            guard += 1;
            if guard > max_sectors {
                break;
            }
            let entry = l2_entry(&self.l2_map, cur);
            let last = entry & 0x4000 != 0;
            let empty = entry & 0x1000 != 0;
            let phys = self.translate(cur);
            let mut len = SECTOR_SIZE as u64;
            if last && !is_dir {
                let lb = (entry & 0xFF) as u64;
                len = if lb == 0 { SECTOR_SIZE as u64 } else { lb };
            } else if empty {
                len = 0;
            }
            if len > 0 {
                extents.push(Extent {
                    disc_addr: phys as u64 * SECTOR_SIZE as u64,
                    len,
                });
                total += len;
            }
            if last {
                break;
            }
            let next = (entry & 0xFFF) as u32;
            if next == cur {
                break;
            }
            cur = next;
        }
        let extents = if is_dir {
            extents
        } else {
            truncate_extents(extents, total)
        };
        Ok((extents, total))
    }

    fn resolve_l3(&mut self, sin: u32, is_dir: bool) -> Result<(Vec<Extent>, u64)> {
        let mut extents = Vec::new();
        let mut total: u64 = 0;
        let mut map_sin = sin;
        let mut low_byte: Option<u8> = None;
        let mut guard = 0;
        let max_maps = 8;
        loop {
            guard += 1;
            if guard > max_maps {
                break;
            }
            let buf = self.read_sector(map_sin)?;
            if low_byte.is_none() {
                low_byte = buf.get(8).copied();
            }
            let (groups, _) = read::decode_jes_map(&buf);
            for g in groups {
                let phys = self.translate(g.sector);
                let len = g.count as u64 * SECTOR_SIZE as u64;
                extents.push(Extent {
                    disc_addr: phys as u64 * SECTOR_SIZE as u64,
                    len,
                });
                total += len;
            }
            let next = read::jes_map_next(&buf);
            if next == 0 || next == map_sin {
                break;
            }
            map_sin = next;
        }
        let actual = if is_dir {
            total
        } else if let Some(lb) = low_byte {
            if lb != 0 {
                total.saturating_sub((SECTOR_SIZE as u64) - lb as u64)
            } else {
                total
            }
        } else {
            total
        };
        let extents = if is_dir {
            extents
        } else {
            truncate_extents(extents, actual)
        };
        Ok((extents, actual))
    }

    fn root_object(&mut self) -> Result<Object> {
        let (extents, _len) = self.resolve_object(self.info.root_sin, true)?;
        Ok(Object {
            name: "$".to_string(),
            name_bytes: b"$".to_vec(),
            load: 0,
            exec: 0,
            length: 0,
            attrs: ATTR_DIRECTORY,
            is_directory: true,
            extents,
            sin: Some(self.info.root_sin),
            modified_unix_secs: None,
        })
    }

    /// Reclaims the underlying sector source after a failed detection attempt
    /// so another interleave candidate can be tried with the same image.
    pub fn into_source(self) -> S {
        self.source
    }

    /// The interleave order selected by detection (or the SEQ fallback).
    pub fn interleave(&self) -> Interleave {
        self.interleave
    }

    /// Whether the filesystem opens to a structurally-plausible root
    /// directory. Used by interleave auto-detection. A mis-translated
    /// ordering reads the root allocation/sectors from the wrong physical
    /// location, which yields an implausible decode. A plain zero sector
    /// decodes as an "empty" directory (no name, no entries), which would be
    /// indistinguishable from a genuine empty disc - so we additionally
    /// require that the root carries a name or entries. This rejects the
    /// zeroed read of a mis-interleaved disc while accepting a genuine
    /// (named) empty directory.
    fn valid_root(&mut self) -> bool {
        let Ok(root) = self.root() else {
            return false;
        };
        match self.list(&root) {
            Ok(listing) => {
                !listing.is_broken && (!listing.title.is_empty() || !listing.objects.is_empty())
            }
            Err(_) => false,
        }
    }
}

impl<S: SectorSource> FileSystem for AfsFs<S> {
    fn root(&mut self) -> Result<Object> {
        self.root_object()
    }

    fn list(&mut self, dir: &Object) -> Result<ListResult> {
        let mut result = ListResult::default();
        let buf = match self.read_extents(&dir.extents) {
            Ok(b) => b,
            Err(e) => {
                result.is_broken = true;
                result.anomalies.push(format!("cannot read directory: {e}"));
                return Ok(result);
            }
        };
        let dec = read::decode_directory(&buf);
        result.is_broken = dec.is_broken;
        result.anomalies = dec.anomalies;
        result.title = dec.title.clone();

        for d in &dec.entries {
            let (extents, length) = match self.resolve_object(d.sin, d.is_directory) {
                Ok(v) => v,
                Err(e) => {
                    result
                        .anomalies
                        .push(format!("{}: cannot resolve allocation: {e}", d.name));
                    continue;
                }
            };
            result.objects.push(Object {
                name: d.name.clone(),
                name_bytes: d.name_bytes.clone(),
                load: d.load,
                exec: d.exec,
                length,
                attrs: d.attrs,
                is_directory: d.is_directory,
                extents,
                sin: Some(d.sin),
                modified_unix_secs: d.modified_unix_secs,
            });
        }
        Ok(result)
    }

    fn read_object(
        &mut self,
        obj: &Object,
        sink: &mut dyn FnMut(u64, &[u8]) -> Result<()>,
    ) -> Result<()> {
        const CHUNK: usize = 256 * 1024;
        for extent in &obj.extents {
            let mut remaining = extent.len;
            let mut addr = extent.disc_addr;
            while remaining > 0 {
                let n = remaining.min(CHUNK as u64) as usize;
                let mut buf = vec![0u8; n];
                self.source.read_at(addr, &mut buf)?;
                sink(addr, &buf)?;
                addr += n as u64;
                remaining -= n as u64;
            }
        }
        Ok(())
    }
}

/// Opens an AFS0 image, detecting the level and (best-effort) interleave.
/// Returns `NotRecognised` if no `AFS0` signature can be found.
pub fn open<SS: SectorSource>(mut source: SS) -> Result<AfsFs<SS>> {
    let mut s0 = [0u8; SECTOR_SIZE];
    source.read_at(0, &mut s0)?;

    if s0[0..4] == AFS0_SIG {
        // Level 2: the Disk Information Block is sector 0.
        let info = info::parse_level2(&s0);
        open_l2(source, info)
    } else {
        // Level 3 / hybrid: the info block is at the sector pointed to by the
        // ADFS free-space map's FileServer start pointer (`&0F6`).
        let start = u24le(&s0, 0x0F6);
        open_l3(source, start)
    }
}

/// Attempts to open a Level 2 filesystem under each candidate interleave,
/// validating that the root directory decodes cleanly. SEQ is tried first
/// (the common ordering in captured images); a genuine interleaved disc only
/// validates under INT or MUX. Falls back to a best-effort SEQ open so a
/// blank/edge-case disc is still recognised rather than rejected.
fn open_l2<SS: SectorSource>(mut source: SS, info: DiskInfo) -> Result<AfsFs<SS>> {
    for il in Interleave::all() {
        let map = match read_l2_map(&mut source, &info, L2_SECTORS_PER_TRACK, il) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mut fs = AfsFs {
            source,
            info: info.clone(),
            interleave: il,
            spt: L2_SECTORS_PER_TRACK,
            l2_map: map,
        };
        if fs.valid_root() {
            return Ok(fs);
        }
        source = fs.into_source();
    }
    // Best-effort SEQ fallback.
    let map =
        read_l2_map(&mut source, &info, L2_SECTORS_PER_TRACK, Interleave::Seq).unwrap_or_default();
    Ok(AfsFs {
        source,
        info,
        interleave: Interleave::Seq,
        spt: L2_SECTORS_PER_TRACK,
        l2_map: map,
    })
}

/// Attempts to open a Level 3 filesystem. The info block is located via the
/// FileServer start pointer, but its position (and every data sector) depends
/// on the interleave, so each candidate order is probed by looking for the
/// `AFS0` signature there and then validating that the root directory walks
/// cleanly. For a hybrid disc the pointer may point to an early track that is
/// interleave-independent, but the root/data sectors are not - validation is
/// therefore the authoritative check.
fn open_l3<SS: SectorSource>(mut source: SS, start: u32) -> Result<AfsFs<SS>> {
    for il in Interleave::all() {
        for spt in [16u16, 10] {
            let phys = read::translate(start, spt, il);
            let mut b = [0u8; SECTOR_SIZE];
            if source
                .read_at(phys as u64 * SECTOR_SIZE as u64, &mut b)
                .is_err()
            {
                continue;
            }
            if b[0..4] != AFS0_SIG {
                continue;
            }
            let info = info::parse_level3(&b);
            let real_spt = if info.sectors_per_track != 0 {
                info.sectors_per_track
            } else {
                spt
            };
            let mut fs = AfsFs {
                source,
                info,
                interleave: il,
                spt: real_spt,
                l2_map: Vec::new(),
            };
            if fs.valid_root() {
                return Ok(fs);
            }
            source = fs.into_source();
        }
    }
    Err(FcError::NotRecognised)
}

pub use info::AfsLevel;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::log::ExtractionLog;
    use crate::extract::walker::{BrokenDirPolicy, ExtractOptions, walk_and_extract};
    use crate::io::rescue::BadSectorPolicy;
    use std::io::Cursor;

    const TS: usize = SECTOR_SIZE;
    const SECTORS: usize = 8;

    fn put16(buf: &mut [u8], off: usize, v: u16) {
        buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }

    #[allow(clippy::too_many_arguments)]
    fn write_entry(
        dir: &mut [u8],
        off: usize,
        name: &str,
        load: u32,
        exec: u32,
        access: u8,
        mdate: u16,
        sin: u32,
        next: u16,
    ) {
        let e = &mut dir[off..off + 0x1A];
        e[0x00..0x02].copy_from_slice(&next.to_le_bytes());
        e[0x02..0x02 + name.len()].copy_from_slice(name.as_bytes());
        e[0x0C..0x10].copy_from_slice(&load.to_le_bytes());
        e[0x10..0x14].copy_from_slice(&exec.to_le_bytes());
        e[0x14] = access;
        e[0x15..0x17].copy_from_slice(&mdate.to_le_bytes());
        e[0x17..0x1A].copy_from_slice(&sin.to_le_bytes()[..3]);
    }

    /// Builds a small, self-consistent Level 2 AFS image (SEQ geometry) with a
    /// root directory containing two files that exercise both single-sector
    /// and chained allocation.
    fn build_l2() -> Vec<u8> {
        let mut img = vec![0u8; TS * SECTORS];
        // Sector 0: Level 2 disk info block.
        img[0..4].copy_from_slice(b"AFS0");
        img[4..0x14].copy_from_slice(b"TESTDISC        ");
        img[0x14] = 0x04; // sectors per side
        img[0x16] = 0x03; // root_sin = 3
        img[0x1B] = 0x01; // map A sin = 1
        img[0x1E] = 0x02; // map B sin = 2

        // Sector 1: allocation map A (current, byte0=1). Sector 2: map B (be).
        let ma = TS;
        let mb = TS * 2;
        img[ma] = 1;
        img[mb] = 0;
        let e = |s: usize| ma + 5 + s * 2;
        put16(&mut img, e(3), 0x4000); // root dir, last (whole sectors)
        put16(&mut img, e(4), 0x400D); // HELLO, last, 13 bytes
        put16(&mut img, e(5), 0x0006); // DATA -> next sector 6
        put16(&mut img, e(6), 0x4003); // DATA, last, 3 bytes

        // Sector 3: root directory, 2 entries.
        let rd = TS * 3;
        img[rd] = 0x11;
        img[rd + 0x01] = 0x00;
        img[rd + 0x02] = 0x11; // cycle
        img[rd + 0x03..rd + 0x0D].fill(b' ');
        img[rd + 0x03] = b'$';
        img[rd + 0x0F] = 2;
        write_entry(&mut img, rd + 0x11, "HELLO", 0, 0, 0x0C, 0x01_01, 4, 0x2B);
        write_entry(&mut img, rd + 0x2B, "DATA", 0, 0, 0x0C, 0x01_01, 5, 0);

        // File data.
        img[TS * 4..TS * 4 + 13].copy_from_slice(b"Hello, world!");
        img[TS * 5..TS * 5 + 256].fill(b'A');
        img[TS * 6..TS * 6 + 3].copy_from_slice(b"XYZ");
        img
    }

    #[test]
    fn l2_open_detects_and_lists_files() {
        let mut fs = AfsFs::open(Cursor::new(build_l2())).unwrap();
        assert_eq!(fs.info.level, AfsLevel::Level2);
        assert_eq!(fs.info.title, "TESTDISC");
        let root = fs.root().unwrap();
        let list = fs.list(&root).unwrap();
        assert!(!list.is_broken, "{:?}", list.anomalies);
        assert_eq!(list.objects.len(), 2);

        let hello = list.objects.iter().find(|o| o.name == "HELLO").unwrap();
        assert!(!hello.is_directory);
        assert_eq!(hello.length, 13);
        let mut out = Vec::new();
        fs.read_object(hello, &mut |_a, b| {
            out.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        assert_eq!(out, b"Hello, world!");
    }

    /// Builds a small Level 3 AFS image (SEQ geometry): an ADFS free-space
    /// map at sector 0 pointing to the L3 disk info block at sector 4, a root
    /// directory driven by a JesMap at sector 5, and a single file object.
    fn build_l3() -> Vec<u8> {
        let mut img = vec![0u8; TS * 16];
        // Sector 0: ADFS free-space map with the FileServer start pointer at
        // &0F6 = the L3 info block sector (4).
        img[0x0F6] = 4;
        // Sector 4: Level 3 disk info block.
        let inf = TS * 4;
        img[inf..inf + 4].copy_from_slice(b"AFS0");
        img[inf + 4..inf + 0x14].copy_from_slice(b"L3DISK          ");
        img[inf + 0x14] = 40; // cylinders
        img[inf + 0x16] = 16; // total sectors (low)
        img[inf + 0x19] = 1; // partitions
        img[inf + 0x1A] = 16; // spt (low)
        img[inf + 0x1F] = 5; // root_sin
        // Sector 5: root directory JesMap pointing at data sector 6.
        let rj = TS * 5;
        img[rj..rj + 6].copy_from_slice(b"JesMap");
        img[rj + 6] = 1;
        img[rj + 0xFF] = 1;
        img[rj + 0x0A..rj + 0x0F].copy_from_slice(&[6, 0, 0, 1, 0]);
        // Sector 7: HELLO file JesMap pointing at data sector 8, length 13.
        let fj = TS * 7;
        img[fj..fj + 6].copy_from_slice(b"JesMap");
        img[fj + 6] = 1;
        img[fj + 8] = 13; // object length low byte
        img[fj + 0xFF] = 1;
        img[fj + 0x0A..fj + 0x0F].copy_from_slice(&[8, 0, 0, 1, 0]);

        // Sector 6: root directory data with one entry.
        let rd = TS * 6;
        img[rd] = 0x11;
        img[rd + 0x02] = 0x11;
        img[rd + 0x03..rd + 0x0D].fill(b' ');
        img[rd + 0x03] = b'$';
        img[rd + 0x0F] = 1;
        write_entry(&mut img, rd + 0x11, "HELLO", 0, 0, 0x0C, 0x01_01, 7, 0);
        // Sector 8: file data.
        img[TS * 8..TS * 8 + 13].copy_from_slice(b"Hello, world!");
        img
    }

    #[test]
    fn l3_open_detects_via_adfs_pointer() {
        let mut fs = AfsFs::open(Cursor::new(build_l3())).unwrap();
        assert_eq!(fs.info.level, AfsLevel::Level3);
        assert_eq!(fs.info.title, "L3DISK");
        assert_eq!(fs.info.sectors_per_track, 16);
        let root = fs.root().unwrap();
        let list = fs.list(&root).unwrap();
        assert!(!list.is_broken, "{:?}", list.anomalies);
        assert_eq!(list.objects.len(), 1);
        assert_eq!(list.objects[0].name, "HELLO");
        assert_eq!(list.objects[0].length, 13);
    }

    #[test]
    fn l3_extracts_file_via_jesmap() {
        let mut fs = AfsFs::open(Cursor::new(build_l3())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let opts = ExtractOptions {
            output_dir: dir.path().to_path_buf(),
            write_inf: false,
            dry_run: false,
            broken_dir_policy: BrokenDirPolicy::Recover,
            bad_sector_policy: BadSectorPolicy::NullFill,
            rescue_map: None,
        };
        let mut log = ExtractionLog::default();
        let summary = walk_and_extract(&mut fs, &opts, &mut log).unwrap();
        assert_eq!(summary.files_extracted, 1);
        assert_eq!(
            std::fs::read(dir.path().join("HELLO")).unwrap(),
            b"Hello, world!"
        );
    }

    /// Builds a Level 2 image laid out under a *specific* interleave, with
    /// deliberately-broken directory bytes placed at the physical locations
    /// that the *other* candidate interleaves would read as the root
    /// directory - so detection must select `target`.
    fn build_l2_interleaved(target: Interleave) -> Vec<u8> {
        const IMG_SECTORS: usize = 900;
        let mut img = vec![0u8; TS * IMG_SECTORS];

        fn place(img: &mut [u8], il: Interleave, logical: u32, data: &[u8]) {
            let phys = read::translate(logical, 10, il) as usize;
            img[phys * TS..phys * TS + data.len()].copy_from_slice(data);
        }
        fn bad_dir(img: &mut [u8], il: Interleave, logical: u32) {
            let phys = read::translate(logical, 10, il) as usize;
            let mut d = vec![0u8; TS];
            d[0x0F] = 200; // implausible entry count -> decode is broken
            img[phys * TS..phys * TS + TS].copy_from_slice(&d);
        }

        // Info block at logical 0 (root_sin=11, maps at 1/2).
        let mut info = vec![0u8; TS];
        info[0..4].copy_from_slice(b"AFS0");
        info[4..0x14].copy_from_slice(b"INTDISC         ");
        info[0x14] = 25; // sectors per side (50 total)
        info[0x16] = 11; // root_sin
        info[0x1B] = 1;
        info[0x1E] = 2;
        place(&mut img, target, 0, &info);

        // Allocation map A at logical 1 (current), entries by logical sector.
        let mut mapa = vec![0u8; TS];
        mapa[0] = 1;
        let put16 = |buf: &mut [u8], off: usize, v: u16| {
            buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
        };
        put16(&mut mapa, 5 + 11 * 2, 0x4000); // root dir, last
        put16(&mut mapa, 5 + 22 * 2, 0x4013); // file, last, 19 bytes
        place(&mut img, target, 1, &mapa);
        let mut mapb = vec![0u8; TS];
        mapb[0] = 0;
        place(&mut img, target, 2, &mapb);

        // Root directory at logical 11.
        let mut rd = vec![0u8; TS];
        rd[0x00] = 0x11;
        rd[0x02] = 0x11;
        rd[0x03..0x0D].fill(b' ');
        rd[0x03] = b'$';
        rd[0x0F] = 1;
        write_entry(&mut rd, 0x11, "HELLO", 0, 0, 0x0C, 0x01_01, 22, 0);
        place(&mut img, target, 11, &rd);

        // File data at logical 22.
        let data = b"Hello, interleave!";
        place(&mut img, target, 22, data);

        // Break the root at every other candidate interleave's location.
        for cand in Interleave::all() {
            if cand == target {
                continue;
            }
            bad_dir(&mut img, cand, 11);
        }
        img
    }

    #[test]
    fn l2_detects_int_interleave() {
        let mut fs = AfsFs::open(Cursor::new(build_l2_interleaved(Interleave::Int))).unwrap();
        assert_eq!(fs.interleave(), Interleave::Int);
        let root = fs.root().unwrap();
        let list = fs.list(&root).unwrap();
        assert!(!list.is_broken, "{:?}", list.anomalies);
        assert_eq!(list.objects.len(), 1);
        assert_eq!(list.objects[0].name, "HELLO");
    }

    #[test]
    fn l2_detects_mux_interleave() {
        let mut fs = AfsFs::open(Cursor::new(build_l2_interleaved(Interleave::Mux))).unwrap();
        assert_eq!(fs.interleave(), Interleave::Mux);
        let root = fs.root().unwrap();
        let list = fs.list(&root).unwrap();
        assert!(!list.is_broken, "{:?}", list.anomalies);
        assert_eq!(list.objects.len(), 1);
        assert_eq!(list.objects[0].name, "HELLO");
    }

    #[test]
    fn l2_seq_fixture_stays_seq() {
        let fs = AfsFs::open(Cursor::new(build_l2())).unwrap();
        assert_eq!(fs.interleave(), Interleave::Seq);
    }

    #[test]
    fn l2_report_builds() {
        use crate::extract::report::build_afs_report;
        let mut fs = AfsFs::open(Cursor::new(build_l2())).unwrap();
        let rep = build_afs_report(&mut fs).unwrap();
        assert_eq!(rep.filesystem, "AFS2");
        assert_eq!(rep.disc_name.as_deref(), Some("TESTDISC"));
        assert_eq!(rep.disc_size, Some(2048));
        assert_eq!(rep.sector_size, Some(256));
    }

    #[test]
    fn l2_extracts_chained_file_with_inf() {
        let mut fs = AfsFs::open(Cursor::new(build_l2())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let opts = ExtractOptions {
            output_dir: dir.path().to_path_buf(),
            write_inf: true,
            dry_run: false,
            broken_dir_policy: BrokenDirPolicy::Recover,
            bad_sector_policy: BadSectorPolicy::NullFill,
            rescue_map: None,
        };
        let mut log = ExtractionLog::default();
        let summary = walk_and_extract(&mut fs, &opts, &mut log).unwrap();
        assert_eq!(summary.files_extracted, 2);

        assert_eq!(
            std::fs::read(dir.path().join("HELLO")).unwrap(),
            b"Hello, world!"
        );
        let data = std::fs::read(dir.path().join("DATA")).unwrap();
        assert_eq!(data.len(), 259);
        assert_eq!(&data[0..256], &vec![b'A'; 256][..]);
        assert_eq!(&data[256..], b"XYZ");
        // An .inf was written for the typed-free file (no filetype suffix).
        assert!(dir.path().join("HELLO.inf").exists());
    }
}
