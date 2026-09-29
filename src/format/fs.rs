//! The Phase-2 seam: a filesystem-agnostic interface between `extract`
//! orchestration and a specific format backend. Only `filecore` implements
//! this today; a future DFS backend would be a second implementation, not
//! a change to `extract`.

use crate::error::Result;
use crate::model::object::Object;

/// Result of listing a directory: the entries plus enough diagnostic
/// information for `extract::walker` to apply the `--on-broken-directory`
/// policy, generic across filesystem backends (a future DFS backend has
/// its own notion of catalog corruption, but the same shape applies).
#[derive(Debug, Default)]
pub struct ListResult {
    pub objects: Vec<Object>,
    pub title: String,
    pub is_broken: bool,
    pub anomalies: Vec<String>,
    /// Non-fatal quirks (unsorted entries, wrong parent SIN, zero-length file
    /// with a real fragment). Reported by `info`/`verify`/`extract` as
    /// warnings; never block a best-effort extraction.
    pub warnings: Vec<String>,
}

pub trait FileSystem {
    fn root(&mut self) -> Result<Object>;

    /// Lists a directory's entries, resolved into `Object`s (files already
    /// carry their disc extents; directories carry the extents of their own
    /// directory structure, readable again via `list`).
    fn list(&mut self, dir: &Object) -> Result<ListResult>;

    /// Like [`list`](Self::list), but with the SIN of the directory that
    /// contains `dir`. FileCore new-map directories store their *parent's*
    /// SIN in the tail `NewDirParent` field (the root points back to its own
    /// SIN), so a backend that can validate that reference needs the value
    /// the walker already knows when it recurses. Backends without such a
    /// field ignore it (the default just forwards to `list`).
    fn list_with_parent(
        &mut self,
        dir: &Object,
        _expected_parent_sin: Option<u32>,
    ) -> Result<ListResult> {
        self.list(dir)
    }

    /// Streams one file object's data extents through `sink` in order, in
    /// bounded chunks. `sink` receives each chunk's disc address alongside
    /// its bytes so a caller can cross-reference bad-sector ranges (e.g.
    /// from a ddrescue mapfile) without this trait needing to know about
    /// that concern itself.
    fn read_object(
        &mut self,
        obj: &Object,
        sink: &mut dyn FnMut(u64, &[u8]) -> Result<()>,
    ) -> Result<()>;
}
