#[cfg(test)]
mod tests;

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

/// The maximum length of a file name, in bytes: the 12-byte name field.
const MAX_NAME_LEN: usize = 12;

/// Staged file data: its name and its bytes.
#[derive(Debug)]
struct FileData {
    name: String,
    data: Vec<u8>,
}

/// Validate a file name against the format's limits.
fn validate_name(name: &str) -> Result<(), Error> {
    if name.len() > MAX_NAME_LEN {
        return Err(Error::NameTooLong {
            name: name.to_owned(),
            max: MAX_NAME_LEN,
        });
    }
    if name.bytes().any(|b| b == 0) {
        return Err(Error::NameContainsNull {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// A GRP archive being written: staged file data plus the target it will be
/// written to.
///
/// Build an archive with [`Writer::new`] (a fresh archive) or [`Writer::open`]
/// (an existing archive whose files are carried through), stage files with
/// [`Writer::add_file`], then call [`Writer::finish`] to write the header,
/// file table, and data, and take back the target.
///
/// Names must be at most 12 bytes and must not contain a NUL byte. Appending
/// to an archive means opening it, staging files, and finishing: the existing
/// files are rewritten, in order, ahead of the newly staged ones.
///
/// # Examples
///
/// ```
/// use std::io::{Cursor, Read, Seek, Write};
/// use grper::{Archive, Writer};
///
/// // Create an archive with two files.
/// let mut w = Cursor::new(Vec::new());
/// let mut writer = Writer::new(&mut w);
/// writer.add_file("HELLO.TXT", b"world")?;
/// writer.add_file("DEF.CON", b"def")?;
/// writer.finish()?;
/// let bytes = w.into_inner();
///
/// // A writer can open the archive it produced and append to it.
/// let mut w = Cursor::new(bytes);
/// let mut writer = Writer::open(&mut w)?;
/// writer.add_file("EXTRA.BIN", &[1, 2, 3])?;
/// writer.finish()?;
///
/// let archive = Archive::new(Cursor::new(w.into_inner()))?;
/// assert_eq!(archive.len(), 3);
/// # Ok::<(), grper::Error>(())
/// ```
#[derive(Debug)]
pub struct Writer<W> {
    target: W,
    files: Vec<FileData>,
}

impl<W: Read + Seek + Write> Writer<W> {
    /// Start a fresh archive to write into `target`.
    ///
    /// The target's existing content is overwritten when [`finish`](Self::finish)
    /// is called, so point this at an empty target.
    pub fn new(target: W) -> Self {
        Self {
            target,
            files: Vec::new(),
        }
    }

    /// Open an existing archive for appending: `target` must hold a valid GRP
    /// archive, whose files are carried through and rewritten by
    /// [`finish`](Self::finish) ahead of any newly staged files.
    pub fn open(mut target: W) -> Result<Self, Error> {
        // Read the existing files into memory while the target is still
        // borrowed by `archive`, then drop `archive` so the target can be
        // moved into the writer.
        let files = {
            let mut archive = Archive::new(&mut target)?;
            (0..archive.len())
                .map(|index| {
                    let entry = archive.entries()[index].clone();
                    let mut data = Vec::with_capacity(entry.size() as usize);
                    archive.extract(index, &mut data)?;
                    Ok(FileData {
                        name: entry.name().to_owned(),
                        data,
                    })
                })
                .collect::<Result<_, Error>>()?
        };
        Ok(Self { target, files })
    }

    /// Stage a file for the archive under `name`.
    pub fn add_file(&mut self, name: &str, data: &[u8]) -> Result<(), Error> {
        validate_name(name)?;
        self.files.push(FileData {
            name: name.to_owned(),
            data: data.to_vec(),
        });
        Ok(())
    }

    /// Write the header, file table, and data to the target and return it.
    ///
    /// The archive is rewritten from the beginning, so the target's position
    /// is reset to the start first (after [`open`](Self::open) the reader has
    /// been left past the old data).
    pub fn finish(mut self) -> Result<W, Error> {
        self.target.seek(SeekFrom::Start(0)).map_err(Error::from)?;
        let mut header = [0u8; HEADER_LEN];
        header[..SIGNATURE.len()].copy_from_slice(SIGNATURE);
        header[SIGNATURE.len()..].copy_from_slice(&(self.files.len() as u32).to_le_bytes());
        self.target.write_all(&header).map_err(Error::from)?;

        for file in &self.files {
            let mut record = [0u8; ENTRY_LEN];
            record[..file.name.len()].copy_from_slice(file.name.as_bytes());
            record[12..16].copy_from_slice(&(file.data.len() as u32).to_le_bytes());
            self.target.write_all(&record).map_err(Error::from)?;
        }

        // The data starts right after the table just written, so no further
        // positioning is needed: the staged order matches the written order.
        for file in &self.files {
            self.target.write_all(&file.data).map_err(Error::from)?;
        }
        self.target.flush().map_err(Error::from)?;
        Ok(self.target)
    }
}
