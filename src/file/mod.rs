#[cfg(test)]
mod tests;

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};

/// A proxy for one file's data inside an [`Archive`].
///
/// The proxy serves reads and seeks lazily from the archive's underlying
/// reader: no file data is copied into memory, and sequential reads never
/// seek. `seek` is likewise lazy (as with `std::io::BufReader`) — it only
/// updates the proxy's position and the next read repositions the reader, so
/// it touches the reader not at all and only fails for a position before the
/// entry's start.
///
/// A proxy is valid for as long as it borrows the archive. Reading past the
/// end of the file returns `Ok(0)`; if the archive is shorter than its file
/// table declares, data runs out early in the same way.
pub struct File<'a, R> {
    reader: &'a mut R,
    name: String,
    start: u64,
    size: u64,
    pos: u64,
    // Whether `reader` is known to be positioned at `start + pos`.
    // Cleared by `new` (the archive reader's position is undefined after
    // parsing) and by `seek`, so the next read repositions; sequential
    // reads reuse the reader's position and never seek.
    reader_positioned: bool,
}

impl<R> File<'_, R> {
    /// The 8.3-style name of the file.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared size of the file in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The current position within the file, in bytes.
    pub fn position(&self) -> u64 {
        self.pos
    }
}

impl<'a, R: Read + Seek> File<'a, R> {
    pub(crate) fn new(reader: &'a mut R, name: String, start: u64, size: u64) -> Self {
        Self {
            reader,
            name,
            start,
            size,
            pos: 0,
            reader_positioned: false,
        }
    }
}

impl<R: Read + Seek> Read for File<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        if !self.reader_positioned {
            self.reader.seek(SeekFrom::Start(self.start + self.pos))?;
            self.reader_positioned = true;
        }
        let to_read = (self.size - self.pos).min(buf.len() as u64) as usize;
        let n = self.reader.read(&mut buf[..to_read])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for File<'_, R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target: i128 = match pos {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::End(offset) => self.size as i128 + offset as i128,
            SeekFrom::Current(offset) => self.pos as i128 + offset as i128,
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek position before start of entry",
            ));
        }
        let target = target as u64;
        self.pos = target;
        self.reader_positioned = false;
        Ok(target)
    }
}

impl<R> fmt::Debug for File<'_, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("File")
            .field("name", &self.name)
            .field("size", &self.size)
            .field("pos", &self.pos)
            .finish()
    }
}
