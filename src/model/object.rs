use crate::model::filetype::LoadExec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub disc_addr: u64,
    pub len: u64,
}

/// Standard RISC OS object attribute bits (guide §3.3), as they appear in a
/// FileCore directory entry's attributes byte. DFS has only one of these
/// (`ATTR_LOCKED`) - see `format::dfs::catalogue::DfsEntry::locked`. Old
/// (S/M/L) directories encode these in the top bit of each name-field
/// character rather than a separate byte (see `dir_old.rs::decode_name`),
/// but the resulting bit values here are the same.
pub const ATTR_OWNER_READ: u32 = 1 << 0;
pub const ATTR_OWNER_WRITE: u32 = 1 << 1;
pub const ATTR_LOCKED: u32 = 1 << 2;
pub const ATTR_DIRECTORY: u32 = 1 << 3;
pub const ATTR_PUBLIC_READ: u32 = 1 << 4;
pub const ATTR_PUBLIC_WRITE: u32 = 1 << 5;

/// A fully-resolved FileCore object (file or directory): a directory
/// entry's fields plus its disc extents, already resolved against the
/// allocation map (old-map contiguous run, or new-map fragment list).
#[derive(Debug, Clone)]
pub struct Object {
    pub name: String,
    pub load: u32,
    pub exec: u32,
    pub length: u64,
    pub attrs: u32,
    pub is_directory: bool,
    pub extents: Vec<Extent>,
    /// The object's own new-map SIN (guide §3.2), when known: `Some` for
    /// every entry on a new-map disc, `None` on old-map/DFS. Only used for
    /// directory objects, and only to validate the parent-reference field
    /// when a directory is later listed - a child directory's tail
    /// `NewDirParent` must equal its *containing* directory's SIN, so the
    /// walker passes the containing directory's `sin` down when it recurses.
    pub sin: Option<u32>,
}

impl Object {
    pub fn load_exec(&self) -> LoadExec {
        crate::model::filetype::decode(self.load, self.exec)
    }

    pub fn is_locked(&self) -> bool {
        self.attrs & ATTR_LOCKED != 0
    }

    pub fn total_extent_len(&self) -> u64 {
        self.extents.iter().map(|e| e.len).sum()
    }
}

/// Drops the first `skip` bytes from a concatenated extent list (used when
/// applying a SIN's sharing offset, guide §3.2).
pub fn trim_extents_from(extents: Vec<Extent>, skip: u64) -> Vec<Extent> {
    let mut remaining = skip;
    let mut out = Vec::new();
    for e in extents {
        if remaining >= e.len {
            remaining -= e.len;
            continue;
        }
        out.push(Extent {
            disc_addr: e.disc_addr + remaining,
            len: e.len - remaining,
        });
        remaining = 0;
    }
    out
}

/// Truncates a concatenated extent list to at most `max_len` bytes (a
/// resolved fragment can be larger than the directory entry's exact byte
/// length, rounded up to allocation units).
pub fn truncate_extents(extents: Vec<Extent>, max_len: u64) -> Vec<Extent> {
    let mut remaining = max_len;
    let mut out = Vec::new();
    for e in extents {
        if remaining == 0 {
            break;
        }
        if e.len <= remaining {
            remaining -= e.len;
            out.push(e);
        } else {
            out.push(Extent {
                disc_addr: e.disc_addr,
                len: remaining,
            });
            remaining = 0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_across_multiple_extents() {
        let extents = vec![
            Extent {
                disc_addr: 0,
                len: 10,
            },
            Extent {
                disc_addr: 100,
                len: 10,
            },
        ];
        let trimmed = trim_extents_from(extents, 15);
        assert_eq!(
            trimmed,
            vec![Extent {
                disc_addr: 105,
                len: 5
            }]
        );
    }

    #[test]
    fn truncates_to_exact_length() {
        let extents = vec![
            Extent {
                disc_addr: 0,
                len: 10,
            },
            Extent {
                disc_addr: 100,
                len: 10,
            },
        ];
        let truncated = truncate_extents(extents, 15);
        assert_eq!(
            truncated,
            vec![
                Extent {
                    disc_addr: 0,
                    len: 10
                },
                Extent {
                    disc_addr: 100,
                    len: 5
                },
            ]
        );
    }
}
