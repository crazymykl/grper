//! Test doubles for exercising I/O error paths.
//!
//! This module is only compiled when it is needed: in unit tests, or when the
//! `testutil` feature is enabled. The binary's own tests enable the feature (via
//! a dev-dependency) so they can fault the archive's I/O on demand.

use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

/// An in-memory `Cursor<Vec<u8>>` whose I/O can be made to fail, for
/// exercising error paths.
///
/// Reads and seeks can be forced to fail at specific positions or for
/// specific seek flavors, and a single read can be made to report
/// end-of-file without failing (`zero_reads_at`), making truncated-archive
/// paths testable.
///
/// The reader is expected to start at offset 0, which holds for a fresh
/// `Cursor::new`.
#[derive(Debug)]
pub struct FlakyReader {
    inner: Cursor<Vec<u8>>,
    fail_reads_from: u64,
    fail_reads_at: u64,
    /// The read issued exactly at this position returns zero bytes, as if the
    /// file ended there, without failing.
    zero_reads_at: u64,
    fail_start_seeks: bool,
    fail_end_seeks: bool,
    fail_position_seeks: bool,
}

impl FlakyReader {
    /// Wrap `data` with no forced failures.
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
            fail_reads_from: u64::MAX,
            fail_reads_at: u64::MAX,
            zero_reads_at: u64::MAX,
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

    /// The read issued exactly at `pos` returns zero bytes, as if the file
    /// ended there, without failing (any other read succeeds).
    pub fn zero_reads_at(mut self, pos: u64) -> Self {
        self.zero_reads_at = pos;
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
        if self.inner.position() == self.zero_reads_at {
            return Ok(0);
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

/// An in-memory `Cursor<Vec<u8>>` whose I/O can be made to fail, for
/// exercising the write-side error paths of the archive writer.
///
/// Pass `Vec::new()` for a fresh archive target, or an existing archive's
/// bytes to stand in for a file being appended to. Writes, seeks, and reads
/// can be forced to fail at specific positions or for specific seek flavors,
/// so both the writing and the archive-opening paths a writer exercises can
/// be made to error.
#[derive(Debug)]
pub struct FlakyWriter {
    inner: Cursor<Vec<u8>>,
    fail_writes_at: u64,
    fail_reads_at: u64,
    fail_start_seeks: bool,
    fail_end_seeks: bool,
    fail_position_seeks: bool,
    fail_flush: bool,
}

impl FlakyWriter {
    /// Wrap `data` (usually `Vec::new()`) with no forced failures, positioned
    /// at its start.
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
            fail_writes_at: u64::MAX,
            fail_reads_at: u64::MAX,
            fail_start_seeks: false,
            fail_end_seeks: false,
            fail_position_seeks: false,
            fail_flush: false,
        }
    }

    /// The write issued exactly at `pos` fails (any other write succeeds).
    pub fn failing_writes_at(mut self, pos: u64) -> Self {
        self.fail_writes_at = pos;
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

    /// `flush` fails.
    pub fn failing_flush(mut self) -> Self {
        self.fail_flush = true;
        self
    }
}

impl Write for FlakyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.inner.position() == self.fail_writes_at {
            return Err(forced_error());
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            return Err(forced_error());
        }
        self.inner.flush()
    }
}

impl Read for FlakyWriter {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.inner.position() == self.fail_reads_at {
            return Err(forced_error());
        }
        self.inner.read(buf)
    }
}

impl Seek for FlakyWriter {
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
