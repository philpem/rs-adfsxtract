//! Acorn DFS (8-bit Disc Filing System) backend: a `FileSystem` implementation
//! alongside `format::filecore`, sharing every layer above the trait
//! boundary (translation, sidecar writing, extraction orchestration,
//! logging, bad-sector handling).
//!
//! DFS is architecturally much simpler than FileCore - a flat, two-sector
//! catalogue per side, no directory tree - but has no checksum at all, so
//! "broken directory" here means only "an entry's data run doesn't fit on
//! the disc" (§ `catalogue::DfsEntry::in_bounds`), not a failed integrity
//! check. `--on-broken-directory fail` triggers only on that; there's
//! nothing else to detect.
//!
//! Each side's catalogue is a genuinely flat namespace (no real
//! subdirectories - the "directory character" on each entry is a one-byte
//! filter tag, not a catalogue of its own), so `list()` on a side returns
//! every file directly rather than inventing a nested tree the medium
//! doesn't have. A double-sided image's two sides *are* independent
//! catalogues, though (a file can exist under the same name on both), so
//! those still need one real level of separation - `root()` exposes them as
//! two synthetic directory objects, distinguished internally by a reserved
//! attrs bit (not by name, since a real DFS filename could coincidentally
//! collide with any string sentinel chosen here).

pub mod catalogue;
pub mod detect;
pub mod geometry;

use crate::error::Result;
use crate::format::dfs::catalogue::{DfsCatalogue, DfsEntry, SECTOR_SIZE, read_catalogue};
use crate::format::dfs::geometry::DfsGeometry;
use crate::format::fs::{FileSystem, ListResult};
use crate::io::SectorSource;
use crate::model::object::{ATTR_DIRECTORY, ATTR_LOCKED, Object};

pub use detect::DfsDetection;

/// Reserved in `Object::attrs` to mark a synthetic per-side directory
/// object, with the side index packed into bits 8-15. Real DFS attributes
/// (and the generic `ATTR_*` constants, including `ATTR_DIRECTORY` which is
/// set alongside this marker) only ever occupy bits 0-5, so this range is
/// never touched by a genuine file entry.
const SIDE_MARKER_BIT: u32 = 1 << 31;
const SIDE_INDEX_SHIFT: u32 = 8;

pub struct DfsFs<S: SectorSource> {
    source: S,
    geometry: DfsGeometry,
    pub double_sided: bool,
    pub catalogues: Vec<DfsCatalogue>,
    /// Actual byte length of the disc image. The catalogue's `total_sectors`
    /// is unreliable on real media (e.g. Watford discs report a small value on
    /// an 800-sector image), so file data runs are bounded by this instead.
    pub(crate) image_len: u64,
}

fn leaf_name(e: &DfsEntry) -> String {
    if e.dir_char == '$' || e.dir_char == ' ' {
        e.name.clone()
    } else {
        format!("{}.{}", e.dir_char, e.name)
    }
}

/// The leaf name as raw RISC OS bytes, mirroring [`leaf_name`] but on the
/// raw byte sequences so the `.inf` sidecar can encode them exactly as DIM
/// does. A `$`/space directory character omits the prefix (root), otherwise
/// the directory-character byte is followed by a `.` and the name bytes.
fn leaf_name_bytes(e: &DfsEntry) -> Vec<u8> {
    if e.dir_char == '$' || e.dir_char == ' ' {
        e.name_bytes.clone()
    } else {
        let mut v = Vec::with_capacity(e.name_bytes.len() + 2);
        v.push(e.dir_char_byte);
        v.push(b'.');
        v.extend_from_slice(&e.name_bytes);
        v
    }
}

fn side_object(side: u8) -> Object {
    let name = format!("Side{side}");
    Object {
        name: name.clone(),
        name_bytes: name.into_bytes(),
        load: 0,
        exec: 0,
        length: 0,
        attrs: ATTR_DIRECTORY | SIDE_MARKER_BIT | ((side as u32) << SIDE_INDEX_SHIFT),
        is_directory: true,
        extents: vec![],
        sin: None,
        modified_unix_secs: None,
    }
}

impl<S: SectorSource> DfsFs<S> {
    pub fn open(mut source: S) -> Result<Self> {
        let detection = detect::detect(&mut source)?;
        let geometry = DfsGeometry {
            double_sided: detection.double_sided,
        };

        let mut catalogues = vec![read_catalogue(&mut source, &geometry, 0)?];
        if detection.double_sided {
            catalogues.push(read_catalogue(&mut source, &geometry, 1)?);
        }
        let image_len = source.total_len()?;

        Ok(Self {
            source,
            geometry,
            double_sided: detection.double_sided,
            catalogues,
            image_len,
        })
    }

    pub fn into_source(self) -> S {
        self.source
    }

    fn list_side_files(&self, side: u8) -> ListResult {
        let cat = &self.catalogues[side as usize];
        let mut objects = Vec::with_capacity(cat.entries.len());
        let mut anomalies = Vec::new();

        for e in &cat.entries {
            // Bound the file's data run by the *actual* image byte length, not
            // the catalogue's declared total_sectors. Real media (notably
            // Watford discs) record a total that is smaller than the physical
            // image, so a valid file at a high sector must not be rejected.
            let end = e.start_sector as u64 * SECTOR_SIZE as u64 + e.length;
            if end > self.image_len {
                anomalies.push(format!(
                    "{}: start sector {} + length {:#x} exceeds disc size ({} bytes)",
                    leaf_name(e),
                    e.start_sector,
                    e.length,
                    self.image_len
                ));
                continue;
            }
            let logical_addr = e.start_sector as u64 * SECTOR_SIZE as u64;
            let extents = self.geometry.translate(side, logical_addr, e.length);
            objects.push(Object {
                name: leaf_name(e),
                name_bytes: leaf_name_bytes(e),
                load: e.load,
                exec: e.exec,
                length: e.length,
                attrs: if e.locked { ATTR_LOCKED } else { 0 },
                is_directory: false,
                extents,
                sin: None,
                modified_unix_secs: None,
            });
        }

        let is_broken = !anomalies.is_empty();
        ListResult {
            objects,
            title: cat.title.clone(),
            is_broken,
            anomalies,
            warnings: vec![],
        }
    }
}

impl<S: SectorSource> FileSystem for DfsFs<S> {
    fn root(&mut self) -> Result<Object> {
        Ok(Object {
            name: "$".to_string(),
            name_bytes: b"$".to_vec(),
            load: 0,
            exec: 0,
            length: 0,
            attrs: ATTR_DIRECTORY,
            is_directory: true,
            extents: vec![],
            sin: None,
            modified_unix_secs: None,
        })
    }

    fn list(&mut self, dir: &Object) -> Result<ListResult> {
        if dir.attrs & SIDE_MARKER_BIT != 0 {
            let side = ((dir.attrs >> SIDE_INDEX_SHIFT) & 0xFF) as u8;
            return Ok(self.list_side_files(side));
        }

        if self.double_sided {
            let title = self.catalogues[0].title.clone();
            let objects = (0..self.catalogues.len() as u8).map(side_object).collect();
            return Ok(ListResult {
                objects,
                title,
                is_broken: false,
                anomalies: vec![],
                warnings: vec![],
            });
        }

        Ok(self.list_side_files(0))
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
