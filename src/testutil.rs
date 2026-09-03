use std::io::{self, Cursor, Read, Seek, SeekFrom};

/// A `Cursor` whose I/O can be made to fail, for exercising error paths.
#[derive(Debug)]
pub struct FlakyReader {
    inner: Cursor<Vec<u8>>,
    fail_reads_from: u64,
    fail_reads_at: u64,
    fail_start_seeks: bool,
    fail_end_seeks: bool,
    fail_position_seeks: bool,
}

impl FlakyReader {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
            fail_reads_from: u64::MAX,
            fail_reads_at: u64::MAX,
            fail_start_seeks: false,
            fail_end_seeks: false,
            fail_position_seeks: false,
        }
    }

    /// Reads issued at or beyond `pos` fail.
    pub fn failing_reads_from(mut self, pos: u64) -> Self {
        self.fail_reads_from = pos;
        self
    }

    /// The read issued exactly at `pos` fails (any other read succeeds).
    pub fn failing_reads_at(mut self, pos: u64) -> Self {
        self.fail_reads_at = pos;
        self
    }

    /// All `SeekFrom::Start` seeks fail.
    pub fn failing_start_seeks(mut self) -> Self {
        self.fail_start_seeks = true;
        self
    }

    /// All `SeekFrom::End` seeks fail.
    pub fn failing_end_seeks(mut self) -> Self {
        self.fail_end_seeks = true;
        self
    }

    /// `stream_position` (a `SeekFrom::Current(0)` seek) fails.
    pub fn failing_position_seeks(mut self) -> Self {
        self.fail_position_seeks = true;
        self
    }
}

fn forced_error() -> io::Error {
    io::Error::other("forced I/O error")
}

impl Read for FlakyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.inner.position() >= self.fail_reads_from
            || self.inner.position() == self.fail_reads_at
        {
            return Err(forced_error());
        }
        self.inner.read(buf)
    }
}

impl Seek for FlakyReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if self.fail_start_seeks && matches!(pos, SeekFrom::Start(_)) {
            return Err(forced_error());
        }
        if self.fail_end_seeks && matches!(pos, SeekFrom::End(_)) {
            return Err(forced_error());
        }
        if self.fail_position_seeks && pos == SeekFrom::Current(0) {
            return Err(forced_error());
        }
        self.inner.seek(pos)
    }
}
