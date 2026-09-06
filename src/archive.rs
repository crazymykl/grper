use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};

use crate::error::Error;
use crate::file::File;

const SIGNATURE: &[u8; 12] = b"KenSilverman";
const HEADER_LEN: usize = 16;
const ENTRY_LEN: usize = 16;

/// A file stored inside an [`Archive`]: its 8.3-style name, its size, and the
/// offset of its data within the archive file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    name: String,
    size: u64,
    offset: u64,
}

impl Entry {
    /// The 8.3-style name of the file.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The size of the file's data in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The offset of the file's data within the archive, in bytes.
    pub fn offset(&self) -> u64 {
        self.offset
    }
}

/// A GRP archive: a 16-byte header, a file table, and the files' data.
///
/// Opening an archive validates the signature and the file table, and checks
/// that the archive is long enough to contain every declared file. Trailing
/// bytes beyond the declared data are ignored.
///
/// File data is served lazily through [`File`] proxies: nothing is copied
/// into memory until it is read.
///
/// # Examples
///
/// ```
/// use std::io::{Cursor, Read, Seek};
/// use grper::Archive;
///
/// // A minimal in-memory archive containing one file, "HELLO.TXT".
/// let mut grp = Vec::new();
/// grp.extend_from_slice(b"KenSilverman");
/// grp.extend_from_slice(&1u32.to_le_bytes());
/// let mut name = [0u8; 12];
/// name[..9].copy_from_slice(b"HELLO.TXT");
/// grp.extend_from_slice(&name);
/// grp.extend_from_slice(&5u32.to_le_bytes());
/// grp.extend_from_slice(b"world");
///
/// let mut archive = Archive::new(Cursor::new(grp)).expect("valid archive");
/// assert_eq!(archive.len(), 1);
///
/// let mut file = archive.entry_by_name("HELLO.TXT").expect("entry exists");
/// let mut contents = Vec::new();
/// file.read_to_end(&mut contents).expect("read succeeds");
/// assert_eq!(contents, b"world");
/// ```
#[derive(Debug)]
pub struct Archive<R: Read + Seek> {
    reader: R,
    entries: Vec<Entry>,
}

impl<R: Read + Seek> Archive<R> {
    /// Open an archive from any `Read + Seek` source (file, cursor, ...).
    pub fn new(mut reader: R) -> Result<Self, Error> {
        let mut header = [0u8; HEADER_LEN];
        if let Err(err) = reader.read_exact(&mut header) {
            if err.kind() == ErrorKind::UnexpectedEof {
                let len = reader.stream_position()? as usize;
                return Err(Error::TooSmall(len));
            }
            return Err(Error::Io(err));
        }

        // `try_into` cannot fail: the slice lengths are fixed by the format.
        let found: [u8; 12] = header[..12].try_into().unwrap();
        if &found != SIGNATURE {
            return Err(Error::BadSignature { found });
        }
        let count = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;

        // The file table: `count` records of a 12-byte name + 4-byte size.
        let mut table = Vec::with_capacity(count);
        for index in 0..count {
            let mut record = [0u8; ENTRY_LEN];
            if let Err(err) = reader.read_exact(&mut record) {
                if err.kind() == ErrorKind::UnexpectedEof {
                    return Err(Error::TableTruncated { index });
                }
                return Err(Error::Io(err));
            }
            let size = u32::from_le_bytes(record[12..16].try_into().unwrap()) as u64;
            table.push((parse_name(&record[..12]), size));
        }

        // File data starts right after the last table entry, so a file's
        // offset is the data start plus the sizes of all files before it.
        let data_start = (count + 1) as u64 * ENTRY_LEN as u64;
        let data_end = data_start + table.iter().map(|&(_, size)| size).sum::<u64>();
        let offsets: Vec<u64> = table
            .iter()
            .scan(data_start, |offset, &(_, size)| {
                let start = *offset;
                *offset += size;
                Some(start)
            })
            .collect();
        let entries = table
            .into_iter()
            .zip(offsets)
            .map(|((name, size), offset)| Entry { name, size, offset })
            .collect();

        // Trailing data beyond the declared files is ignored, but an archive
        // that ends early is corrupt.
        let end = reader.seek(SeekFrom::End(0))?;
        if end < data_end {
            return Err(Error::DataTruncated {
                declared: data_end,
                actual: end,
            });
        }

        Ok(Self { reader, entries })
    }

    /// The number of files in the archive.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the archive contains no files.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The file table, in archive order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Look up an entry's metadata by name, or `None` if absent.
    ///
    /// If (as in unmodified archives) the same name appears more than once,
    /// the first entry is returned.
    pub fn get_entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// Open the file at `index` for lazy reading.
    ///
    /// The returned [`File`] borrows the archive, so it cannot be used while
    /// the archive is otherwise borrowed.
    pub fn entry(&mut self, index: usize) -> Result<File<'_, R>, Error> {
        let (name, start, size) = {
            let entry = self.entries.get(index).ok_or(Error::IndexOutOfBounds {
                index,
                len: self.entries.len(),
            })?;
            (entry.name.clone(), entry.offset, entry.size)
        };
        Ok(File::new(&mut self.reader, name, start, size))
    }

    /// Open the file with the given name for lazy reading.
    pub fn entry_by_name(&mut self, name: &str) -> Result<File<'_, R>, Error> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.name == name)
            .ok_or_else(|| Error::EntryNotFound {
                name: name.to_owned(),
            })?;
        self.entry(index)
    }

    /// Copy the file at `index` into `writer`.
    ///
    /// Unlike [`entry`](Self::entry), this copies the whole file and does not
    /// borrow it, which is what bulk extraction (and the bundled CLI) needs.
    /// Errors from opening the entry or copying its data propagate; the
    /// caller can compare the number of bytes written against
    /// [`Entry::size`] to detect a truncated archive.
    pub fn extract<W: Write>(&mut self, index: usize, writer: &mut W) -> Result<u64, Error> {
        let (name, start, size) = {
            let entry = self.entries.get(index).ok_or(Error::IndexOutOfBounds {
                index,
                len: self.entries.len(),
            })?;
            (entry.name.clone(), entry.offset, entry.size)
        };
        let mut file = File::new(&mut self.reader, name, start, size);
        io::copy(&mut file, writer).map_err(Error::from)
    }
}

fn parse_name(raw: &[u8]) -> String {
    let end = raw.iter().position(|&byte| byte == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::FlakyReader;
    use std::{
        assert_matches,
        io::{BufReader, Cursor},
    };

    fn build_grp(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"KenSilverman");
        buf.extend_from_slice(&(files.len() as u32).to_le_bytes());
        for (name, data) in files {
            let mut field = [0u8; 12];
            let len = name.len().min(12);
            field[..len].copy_from_slice(&name.as_bytes()[..len]);
            buf.extend_from_slice(&field);
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        }
        for (_, data) in files {
            buf.extend_from_slice(data);
        }
        buf
    }

    #[test]
    fn new_should_parse_signature_and_table() {
        let archive = Archive::new(Cursor::new(build_grp(&[
            ("A.TXT", b"123"),
            ("B.CON", b"4567"),
        ])))
        .unwrap();
        assert_eq!(archive.len(), 2);
    }

    #[test]
    fn new_should_reject_bad_signature() {
        let mut buf = build_grp(&[("A.TXT", b"1")]);
        buf[0] = b'k';
        let err = Archive::new(Cursor::new(buf)).unwrap_err();
        assert_matches!(err, Error::BadSignature { .. });
    }

    #[test]
    fn new_should_reject_files_shorter_than_header() {
        let err = Archive::new(Cursor::new(vec![0u8; 5])).unwrap_err();
        assert_matches!(err, Error::TooSmall(len) if len == 5);
    }

    #[test]
    fn new_should_reject_truncated_table() {
        // The count claims 3 entries but only 2 are present.
        let mut buf = build_grp(&[("A.TXT", b"1"), ("B.TXT", b"2")]);
        buf[12..16].copy_from_slice(&3u32.to_le_bytes());
        let err = Archive::new(Cursor::new(buf)).unwrap_err();
        assert_matches!(err, Error::TableTruncated { index } if index == 2);
    }

    #[test]
    fn new_should_reject_short_data_region() {
        let mut buf = build_grp(&[("A.TXT", b"12345")]);
        buf.truncate(buf.len() - 2);
        let err = Archive::new(Cursor::new(buf)).unwrap_err();
        assert_matches!(err, Error::DataTruncated { .. });
    }

    #[test]
    fn new_should_ignore_trailing_bytes() {
        let mut buf = build_grp(&[("A.TXT", b"123")]);
        buf.extend_from_slice(b"trailing junk");
        let archive = Archive::new(Cursor::new(buf)).unwrap();
        assert_eq!(archive.len(), 1);
    }

    #[test]
    fn new_should_parse_name_without_null_terminator() {
        // A 12-character name fills the field and has no room for a NUL.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"KenSilverman");
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(b"ABCDEFGHIJKL");
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(b"xyz");
        let archive = Archive::new(Cursor::new(buf)).unwrap();
        assert_eq!(
            archive.get_entry("ABCDEFGHIJKL").map(|entry| entry.size()),
            Some(3)
        );
    }

    #[test]
    fn new_should_parse_empty_archive() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"KenSilverman");
        buf.extend_from_slice(&0u32.to_le_bytes());
        let archive = Archive::new(Cursor::new(buf)).unwrap();
        assert!(archive.is_empty());
    }

    #[test]
    fn new_should_work_through_buf_reader() {
        let archive =
            Archive::new(BufReader::new(Cursor::new(build_grp(&[("A.TXT", b"123")])))).unwrap();
        assert_eq!(archive.len(), 1);
    }

    #[test]
    fn entries_should_report_names_sizes_and_offsets() {
        // Data starts after the 16-byte header plus two 16-byte entries: 48.
        let archive = Archive::new(Cursor::new(build_grp(&[
            ("A.TXT", b"123"),
            ("B.CON", b"abcd"),
        ])))
        .unwrap();
        assert_eq!(archive.entries()[0].name(), "A.TXT");
        assert_eq!(archive.entries()[0].size(), 3);
        assert_eq!(archive.entries()[0].offset(), 48);
        assert_eq!(archive.entries()[1].offset(), 51);
    }

    #[test]
    fn get_entry_should_find_entry_by_name() {
        let archive =
            Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1"), ("B.CON", b"2")]))).unwrap();
        assert!(archive.get_entry("B.CON").is_some());
    }

    #[test]
    fn get_entry_should_return_none_for_missing_name() {
        let archive = Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1")]))).unwrap();
        assert!(archive.get_entry("NOPE.TXT").is_none());
    }

    #[test]
    fn entry_should_reject_out_of_range_index() {
        let mut archive = Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1")]))).unwrap();
        let err = archive.entry(1).unwrap_err();
        assert_matches!(err, Error::IndexOutOfBounds { index: 1, len: 1 });
    }

    #[test]
    fn new_should_propagate_io_errors_from_header_read() {
        let data = build_grp(&[("A.TXT", b"123")]);
        let err = Archive::new(FlakyReader::new(data).failing_reads_from(0)).unwrap_err();
        assert_matches!(err, Error::Io(_));
    }

    #[test]
    fn new_should_propagate_io_errors_from_length_probe() {
        // Both the header read and the length probe fail; the probe's error
        // must propagate as an I/O error.
        let err =
            Archive::new(FlakyReader::new(vec![0u8; 5]).failing_position_seeks()).unwrap_err();
        assert_matches!(err, Error::Io(_));
    }

    #[test]
    fn new_should_propagate_io_errors_from_table_read() {
        let data = build_grp(&[("A.TXT", b"123")]);
        let err =
            Archive::new(FlakyReader::new(data).failing_reads_from(HEADER_LEN as u64)).unwrap_err();
        assert_matches!(err, Error::Io(_));
    }

    #[test]
    fn entry_should_open_entry_at_index() {
        let data = build_grp(&[("A.TXT", b"123"), ("B.CON", b"45")]);
        let mut archive = Archive::new(Cursor::new(data)).unwrap();
        let file = archive.entry(1).unwrap();
        assert_eq!(file.name(), "B.CON");
        assert_eq!(file.size(), 2);
    }

    #[test]
    fn entry_by_name_should_open_entry_that_exists() {
        let data = build_grp(&[("A.TXT", b"123"), ("B.CON", b"45")]);
        let mut archive = Archive::new(Cursor::new(data)).unwrap();
        let file = archive.entry_by_name("B.CON").unwrap();
        assert_eq!(file.name(), "B.CON");
    }

    #[test]
    fn entry_by_name_should_reject_missing_name() {
        let mut archive = Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1")]))).unwrap();
        let err = archive.entry_by_name("MISS.TXT").unwrap_err();
        assert_matches!(err, Error::EntryNotFound { .. });
    }

    #[test]
    fn is_empty_should_distinguish_empty_and_non_empty_archives() {
        let empty = Archive::new(Cursor::new(build_grp(&[]))).unwrap();
        assert!(empty.is_empty());
        let full = Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1")]))).unwrap();
        assert!(!full.is_empty());
    }

    #[test]
    fn new_should_reject_bad_signature_through_buf_reader() {
        let mut data = build_grp(&[("A.TXT", b"1")]);
        data[0] = b'k';
        let err = Archive::new(BufReader::new(Cursor::new(data))).unwrap_err();
        assert_matches!(err, Error::BadSignature { .. });
    }

    #[test]
    fn new_should_reject_files_shorter_than_header_through_buf_reader() {
        let err = Archive::new(BufReader::new(Cursor::new(vec![0u8; 5]))).unwrap_err();
        assert_matches!(err, Error::TooSmall(len) if len == 5);
    }

    #[test]
    fn new_should_reject_truncated_table_through_buf_reader() {
        let mut data = build_grp(&[("A.TXT", b"1"), ("B.TXT", b"2")]);
        data[12..16].copy_from_slice(&3u32.to_le_bytes());
        let err = Archive::new(BufReader::new(Cursor::new(data))).unwrap_err();
        assert_matches!(err, Error::TableTruncated { .. });
    }

    #[test]
    fn new_should_reject_short_data_region_through_buf_reader() {
        let mut data = build_grp(&[("A.TXT", b"12345")]);
        data.truncate(data.len() - 2);
        let err = Archive::new(BufReader::new(Cursor::new(data))).unwrap_err();
        assert_matches!(err, Error::DataTruncated { .. });
    }

    #[test]
    fn new_should_propagate_io_errors_from_buf_reader() {
        // A buffered reader can surface non-Eof I/O errors from its source;
        // they must map to Error::Io, not a format error.
        let data = build_grp(&[("A.TXT", b"123")]);
        let err =
            Archive::new(BufReader::new(FlakyReader::new(data).failing_reads_from(0))).unwrap_err();
        assert_matches!(err, Error::Io(_));
    }

    #[test]
    fn new_should_reject_files_shorter_than_header_on_flaky_reader() {
        let err = Archive::new(FlakyReader::new(vec![0u8; 5])).unwrap_err();
        assert_matches!(err, Error::TooSmall(len) if len == 5);
    }

    #[test]
    fn new_should_reject_bad_signature_on_flaky_reader() {
        let mut data = build_grp(&[("A.TXT", b"1")]);
        data[0] = b'k';
        let err = Archive::new(FlakyReader::new(data)).unwrap_err();
        assert_matches!(err, Error::BadSignature { .. });
    }

    #[test]
    fn new_should_reject_truncated_table_on_flaky_reader() {
        let mut data = build_grp(&[("A.TXT", b"1"), ("B.TXT", b"2")]);
        data[12..16].copy_from_slice(&3u32.to_le_bytes());
        let err = Archive::new(FlakyReader::new(data)).unwrap_err();
        assert_matches!(err, Error::TableTruncated { .. });
    }

    #[test]
    fn new_should_reject_short_data_region_on_flaky_reader() {
        let mut data = build_grp(&[("A.TXT", b"12345")]);
        data.truncate(data.len() - 2);
        let err = Archive::new(FlakyReader::new(data)).unwrap_err();
        assert_matches!(err, Error::DataTruncated { .. });
    }

    #[test]
    fn extract_should_copy_entry_data() {
        let mut archive = Archive::new(Cursor::new(build_grp(&[
            ("A.TXT", b"12345"),
            ("B.CON", b""),
        ])))
        .unwrap();
        let mut out = Vec::new();
        let written = archive.extract(0, &mut out).unwrap();
        assert_eq!(written, 5);
        assert_eq!(out, b"12345");
    }

    #[test]
    fn extract_should_reject_out_of_range_index() {
        let mut archive = Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1")]))).unwrap();
        let err = archive.extract(1, &mut Vec::new()).unwrap_err();
        assert_matches!(err, Error::IndexOutOfBounds { index: 1, len: 1 });
    }

    #[test]
    fn extract_should_propagate_read_errors() {
        let data = build_grp(&[("A.TXT", b"12345")]);
        let mut archive = Archive::new(FlakyReader::new(data).failing_reads_from(32))
            .expect("open succeeds; the failure is in the extraction read");
        let err = archive.extract(0, &mut Vec::new()).unwrap_err();
        assert_matches!(err, Error::Io(_));
    }
}
