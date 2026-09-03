use std::io;

use thiserror::Error;

/// Errors returned when opening or reading a GRP archive.
#[derive(Debug, Error)]
pub enum Error {
    /// The first 12 bytes of the file are not the `"KenSilverman"` signature.
    #[error("not a GRP archive: signature {found:?}, expected \"KenSilverman\"")]
    BadSignature {
        /// The 12 signature bytes that were found.
        found: [u8; 12],
    },

    /// The file is shorter than the 16-byte header.
    #[error("file too small to be a GRP archive: {0} bytes, need at least 16")]
    TooSmall(usize),

    /// The file table declares more entries than the archive contains.
    #[error("archive truncated in file table at entry {index}")]
    TableTruncated {
        /// Index of the first entry that could not be read in full.
        index: usize,
    },

    /// The archive ends before the last declared file's data.
    #[error("archive ends at byte {actual} but its file table declares data until byte {declared}")]
    DataTruncated {
        /// Offset where the declared file data ends.
        declared: u64,
        /// Actual length of the archive file.
        actual: u64,
    },

    /// The entry index does not address an entry in the archive.
    #[error("entry index {index} out of range: archive has {len} entries")]
    IndexOutOfBounds {
        /// The requested index.
        index: usize,
        /// Number of entries in the archive.
        len: usize,
    },

    /// No entry with the requested name is present in the archive.
    #[error("no entry named {name:?} in archive")]
    EntryNotFound {
        /// The requested name.
        name: String,
    },

    /// An underlying I/O error from the wrapped reader.
    #[error(transparent)]
    Io(#[from] io::Error),
}
