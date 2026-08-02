use std::io::{self, Read, Seek, SeekFrom};

/// A byte-addressable source of disc image data, read in bounded chunks.
///
/// Implemented via a blanket impl over `Read + Seek` so both a real `File`
/// (production use, never loaded whole into memory) and an in-memory
/// `Cursor<Vec<u8>>` (synthetic test images) work identically.
pub trait SectorSource {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn total_len(&mut self) -> io::Result<u64>;
}

impl<T: Read + Seek> SectorSource for T {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        let mut total = 0;
        while total < buf.len() {
            match self.read(&mut buf[total..])? {
                0 => break,
                n => total += n,
            }
        }
        if total < buf.len() {
            buf[total..].fill(0);
        }
        Ok(())
    }

    fn total_len(&mut self) -> io::Result<u64> {
        let cur = self.stream_position()?;
        let end = self.seek(SeekFrom::End(0))?;
        self.seek(SeekFrom::Start(cur))?;
        Ok(end)
    }
}
