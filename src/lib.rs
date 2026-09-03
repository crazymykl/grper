//! Read access to Build engine **GRP** archive files.
//!
//! GRP is the uncompressed archive format used by Ken Silverman's Build
//! engine (Duke Nukem 3D, Redneck Rampage, Shadow Warrior, ...). A GRP file
//! is laid out as:
//!
//! | Data type  | Name      | Description                                |
//! | ---------- | --------- | ------------------------------------------ |
//! | `char[12]` | signature | `"KenSilverman"` (not NUL-terminated)      |
//! | `u32le`    | fileCount | Number of files                            |
//! | ...        | entries   | `fileCount` × (12-byte 8.3 name + `u32le` size) |
//!
//! File data starts immediately after the last entry; a file's offset is the
//! sum of the sizes of all files before it. Names are 8.3 style and
//! NUL-terminated only when they fit in the 12-byte field.
//!
//! # Example
//!
//! ```
//! use std::io::{Cursor, Read, Seek};
//! use grper::Archive;
//!
//! // A minimal in-memory archive containing one file, "HELLO.TXT".
//! let mut grp = Vec::new();
//! grp.extend_from_slice(b"KenSilverman");
//! grp.extend_from_slice(&1u32.to_le_bytes());
//! let mut name = [0u8; 12];
//! name[..9].copy_from_slice(b"HELLO.TXT");
//! grp.extend_from_slice(&name);
//! grp.extend_from_slice(&5u32.to_le_bytes());
//! grp.extend_from_slice(b"world");
//!
//! let mut archive = Archive::new(Cursor::new(grp)).expect("valid archive");
//! assert_eq!(archive.len(), 1);
//! assert_eq!(archive.get_entry("HELLO.TXT").expect("entry exists").size(), 5);
//!
//! let mut file = archive.entry_by_name("HELLO.TXT").expect("entry exists");
//! let mut contents = Vec::new();
//! file.read_to_end(&mut contents).expect("read succeeds");
//! assert_eq!(contents, b"world");
//! ```
//!
//! See [`Archive`] for the full API.

#![deny(missing_docs)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod archive;
mod error;
mod file;

#[cfg(test)]
mod testutil;

pub use archive::{Archive, Entry};
pub use error::Error;
pub use file::File;
