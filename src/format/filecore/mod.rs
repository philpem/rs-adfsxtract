pub mod checksums;
pub mod detect;
pub mod dir_big;
pub mod dir_old;
pub mod disc_record;
pub mod map_new;
pub mod map_old;
pub mod sml_geometry;

use crate::error::{FcError, Result};
use crate::format::fs::{FileSystem, ListResult};
use crate::io::SectorSource;
use crate::model::object::{ATTR_DIRECTORY, Extent, Object, truncate_extents};

pub use detect::{DirType, MapType};
pub use disc_record::DiscRecord;
pub use map_new::NewMapIndex;
pub use sml_geometry::SmlGeometry;

/// A resolved FileCore image: format detection plus (for new-map discs)
/// the fully-decoded fragment index, ready for directory traversal.
pub struct FileCoreFs<S: SectorSource> {
    source: S,
    pub map_type: MapType,
    pub dir_type: DirType,
    pub disc_record: Option<DiscRecord>,
    pub new_map: Option<NewMapIndex>,
    sml_geometry: Option<SmlGeometry>,
    sharing_unit: u64,
    root_extents: Vec<Extent>,
    pub boot_block: Option<disc_record::BootBlock>,
}

fn read_extents(source: &mut dyn SectorSource, extents: &[Extent]) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(extents.iter().map(|e| e.len as usize).sum());
    for e in extents {
        let mut chunk = vec![0u8; e.len as usize];
        source.read_at(e.disc_addr, &mut chunk)?;
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn resolve_root_dir(
    dr: &DiscRecord,
    dir_type: DirType,
    new_map: &NewMapIndex,
    sharing_unit: u64,
) -> Result<Vec<Extent>> {
    let fragment_id = (dr.root_dir >> 8) & 0xFFFF;
    let sharing_offset = dr.root_dir & 0xFF;
    let root_len = match dir_type {
        DirType::Big => dr.root_size as u64,
        _ => dir_old::LARGE_DIR_SIZE as u64,
    };
    if fragment_id == 2 {
        let base = map_new::map_disc_addr(dr);
        let off = if sharing_offset == 0 {
            0
        } else {
            (sharing_offset as u64 - 1) * sharing_unit
        };
        Ok(vec![Extent {
            disc_addr: base + off,
            len: root_len,
        }])
    } else {
        let extents = map_new::resolve_fragment(new_map, fragment_id, sharing_offset, sharing_unit)
            .ok_or_else(|| {
                FcError::Parse("root directory fragment not found in zone map".into())
            })?;
        Ok(if dir_type == DirType::Big {
            extents
        } else {
            truncate_extents(extents, root_len)
        })
    }
}

impl<S: SectorSource> FileCoreFs<S> {
    pub fn open(mut source: S) -> Result<Self> {
        let detection = detect::detect(&mut source)?;
        match detection.map_type {
            MapType::Old => {
                let root_addr = detection
                    .old_map_root_dir_addr
                    .expect("old-map detection always sets root_dir_addr");
                let root_len = if detection.dir_type == DirType::Old {
                    dir_old::SMALL_DIR_SIZE
                } else {
                    dir_old::LARGE_DIR_SIZE
                };
                // Only S/M/L (old directories) need the sequential->
                // interleaved geometry translation (sml_geometry.rs); D
                // format (old map, new/large directories) already uses
                // interleaved logical addressing.
                let sml_geometry = if detection.dir_type == DirType::Old {
                    let mut s0 = [0u8; map_old::OLD_MAP_SECTOR_SIZE];
                    let mut s1 = [0u8; map_old::OLD_MAP_SECTOR_SIZE];
                    source.read_at(map_old::OLD_MAP_SECTOR0_ADDR, &mut s0)?;
                    source.read_at(map_old::OLD_MAP_SECTOR1_ADDR, &mut s1)?;
                    let free_map = map_old::parse_old_map(&s0, &s1)?;
                    Some(SmlGeometry::from_total_sectors(
                        free_map.total_sectors as u64,
                    ))
                } else {
                    None
                };
                Ok(Self {
                    source,
                    map_type: MapType::Old,
                    dir_type: detection.dir_type,
                    disc_record: None,
                    new_map: None,
                    sharing_unit: 0,
                    root_extents: vec![Extent {
                        disc_addr: root_addr,
                        len: root_len as u64,
                    }],
                    boot_block: None,
                    sml_geometry,
                })
            }
            MapType::New => {
                let mut dr = detection
                    .disc_record
                    .clone()
                    .expect("new-map detection always sets disc_record");
                // Every new-map disc address this backend computes (zone
                // map, directory extents, file extents) assumes
                // interleaved track ordering, matching how new-map media
                // is conventionally formatted (guide §1.1). The guide is
                // explicit that this is a convention, not a structural
                // guarantee, and that the actual ordering must be read from
                // this flag. Refuse rather than mis-read bytes for the
                // untested case.
                if dr.sequential_track_order() {
                    return Err(FcError::Unsupported(
                        "this disc uses sequential track ordering (DiscRecord_SequenceSides_Flag set) \
                         on a new-map format; only the conventional interleaved ordering is supported"
                            .into(),
                    ));
                }
                let new_map = map_new::read_new_map(&mut source, &dr)?;
                // The zone-0 copy is authoritative for the extended fields
                // the boot-block copy zeroes on real media (guide §A.4).
                // Merge them before deriving the directory type, so a big
                // directory on such a disc is not misclassified from the
                // boot block's zeroed format_version (guide §1.3).
                dr.merge_zone0_metadata(&new_map.zone0_disc_record);
                let dir_type = if dr.is_big_dir() {
                    DirType::Big
                } else {
                    DirType::New
                };
                let sharing_unit = dr.sharing_unit();
                let root_extents = resolve_root_dir(&dr, dir_type, &new_map, sharing_unit)?;
                Ok(Self {
                    source,
                    map_type: MapType::New,
                    dir_type,
                    disc_record: Some(dr),
                    new_map: Some(new_map),
                    sharing_unit,
                    root_extents,
                    boot_block: detection.boot_block,
                    sml_geometry: None,
                })
            }
        }
    }

    /// The disc record copy preferred for metadata (guide §A.4): zone 0's
    /// copy carries real `disc_id`/`disc_name`/`disc_type` on real media,
    /// while the boot block copy's extended fields are zeroed.
    pub fn metadata_disc_record(&self) -> Option<&DiscRecord> {
        self.disc_record.as_ref()
    }

    pub fn into_source(self) -> S {
        self.source
    }

    /// Reads the old free-space map (guide §2.5) for disc metadata; only
    /// meaningful when `map_type == MapType::Old`.
    pub fn old_free_space_map(&mut self) -> Result<map_old::OldFreeSpaceMap> {
        let mut s0 = [0u8; map_old::OLD_MAP_SECTOR_SIZE];
        let mut s1 = [0u8; map_old::OLD_MAP_SECTOR_SIZE];
        self.source
            .read_at(map_old::OLD_MAP_SECTOR0_ADDR, &mut s0)?;
        self.source
            .read_at(map_old::OLD_MAP_SECTOR1_ADDR, &mut s1)?;
        map_old::parse_old_map(&s0, &s1)
    }
}

impl<S: SectorSource> FileSystem for FileCoreFs<S> {
    fn root(&mut self) -> Result<Object> {
        Ok(Object {
            name: "$".to_string(),
            load: 0,
            exec: 0,
            length: 0,
            attrs: ATTR_DIRECTORY,
            is_directory: true,
            extents: self.root_extents.clone(),
        })
    }

    fn list(&mut self, dir: &Object) -> Result<ListResult> {
        let raw = read_extents(&mut self.source, &dir.extents)?;
        match self.dir_type {
            DirType::Old | DirType::New => {
                let small = self.dir_type == DirType::Old;
                let r = dir_old::decode_dir(
                    &raw,
                    small,
                    self.map_type,
                    self.new_map.as_ref(),
                    self.sharing_unit,
                    self.sml_geometry.as_ref(),
                )?;
                Ok(ListResult {
                    objects: r.objects,
                    title: r.title,
                    is_broken: r.is_broken,
                    anomalies: r.anomalies,
                })
            }
            DirType::Big => {
                let new_map = self
                    .new_map
                    .as_ref()
                    .expect("big directories only occur on new-map discs");
                let r = dir_big::decode_big_dir(&raw, new_map, self.sharing_unit)?;
                Ok(ListResult {
                    objects: r.objects,
                    title: r.title,
                    is_broken: r.is_broken,
                    anomalies: r.anomalies,
                })
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{NewMapConfig, build_new_map_disc};

    #[test]
    fn refuses_new_map_disc_with_sequence_sides_flag_set() {
        let cfg = NewMapConfig::default();
        let mut image = build_new_map_disc(vec![], &cfg);
        // `detect()` reads the disc record directly from absolute offset
        // 0x04 when there's no boot block (this synthetic image has none) -
        // low_sector is byte 8 of the record, so 0x04+8 = 0x0C. This must
        // fail before `read_new_map` ever runs, so it doesn't need (and
        // deliberately skips) fixing up the zone's `ZoneCheck` byte.
        image.bytes[0x0C] |= 0x40; // DiscRecord_SequenceSides_Flag
        match FileCoreFs::open(image.cursor()) {
            Err(FcError::Unsupported(msg)) => assert!(msg.contains("sequential"), "{msg}"),
            Err(e) => panic!("expected Unsupported, got a different error: {e}"),
            Ok(_) => panic!("expected Unsupported, got Ok"),
        }
    }
}
