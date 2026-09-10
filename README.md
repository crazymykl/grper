# grper

Reader for Ken Silverman's **GRP** archive files, the uncompressed archive
format used by Build engine games (Duke Nukem 3D, Redneck Rampage, Shadow
Warrior, ...).

`grper` exposes the files stored in a `.grp` archive:

- as **`Read + Seek` proxies** when used as a library, and
- by **extracting them to disk** via a bundled CLI.

Licensed under GPL-3.0-or-later.

## CLI

Build or install the `grper` binary (feature `cli`, on by default):

```sh
cargo install grper
```

The CLI has three subcommands: `extract`, `create`, and `update`.

### `extract`

```console
$ grper extract <PATH> [FILE...] [-o <DIR>] [--strict] [--dry-run]
```

Extracts the files of a GRP archive to disk.

- `<PATH>` — path to the `.grp` archive.
- `[FILE...]` — optional names of entries to extract. When omitted, every
  entry is extracted. Names are matched **case-insensitively**, as on the DOS
  filesystems the format was designed for. Files are always written under the
  archive's spelling of their names; a request whose case differs from the
  stored name warns on stderr:

  ```console
  $ grper extract archive.grp -o out defs.con
  warning: "defs.con" is stored in the archive as "DEFS.CON"
  ```

- `-o`, `--out-dir <DIR>` — directory to extract into, created if missing
  (default: the current directory).
- `--strict` — abort with an error instead of proceeding when any warning is
  raised. Nothing is extracted and the out dir is not created.
- `--dry-run` — report what would be extracted without creating the out dir
  or writing any file. Selection, warnings, and `--strict` are still applied,
  so a `--strict --dry-run` with a case mismatch aborts the same way.

### `create`

```console
$ grper create <ARCHIVE> <FILE>... [--dry-run]
```

Builds a new GRP archive at `<ARCHIVE>`, which must not already exist (use
`update` to modify an existing archive). Each source file's **base name**
becomes its entry name.

### `update`

```console
$ grper update <ARCHIVE> <FILE>... [--dry-run]
```

Replaces or adds files in an existing GRP archive. A source whose base name
**exactly** matches a stored entry replaces that entry's data in place
(keeping the stored spelling and position); any other source is appended. A
base name that differs from a stored entry **only in case** is a hard error, as
the format's 8.3 names do not distinguish case.

`create` and `update` both take `--dry-run`, which prints the planned entries
(with a `replaced`/`added` tag for `update`) without writing the archive or a
temporary file.

Examples:

```sh
# Extract everything to the current directory.
grper extract duke3d.grp

# Extract a couple of files to a specific directory.
grper extract duke3d.grp -o out DEFS.CON sounds.lump

# Build a fresh archive from some files on disk.
grper create new.grp defs.con hello.txt

# Replace DEFS.CON in place and append an extra file.
grper update new.grp defs.con extra.bin
```

Exit status is `0` on success, `1` on a runtime error (missing archive,
invalid archive, a name that is a case-only duplicate, ...), and `2` on an
argument error.

## Library

```toml
[dependencies]
grper = { version = "0.1", default-features = false }
```

The core is [`Archive`](https://docs.rs/grper/latest/grper/struct.Archive.html),
which opens any `Read + Seek` source and validates it. File data is served
lazily: nothing is copied into memory until it is read, and sequential reads
never seek.

```rust
use std::io::{Cursor, Read, Seek, SeekFrom};
use grper::Archive;

let bytes = std::fs::read("archive.grp")?;
let mut archive = Archive::new(Cursor::new(bytes))?;

// Metadata for every stored file.
for entry in archive.entries() {
    println!("{}  {} bytes", entry.name(), entry.size());
}

// Open one file by name (or by index) and read it.
let mut file = archive.entry_by_name("DEFS.CON")?;
let mut contents = Vec::new();
file.read_to_end(&mut contents)?;

// `File` implements `Seek` too.
file.seek(SeekFrom::Start(0))?;
file.read_to_end(&mut contents)?;
```

`Archive` also provides `extract(index, writer)`, which copies a whole entry
into any `Write` target without borrowing the archive — that is what the
bundled CLI uses.

### Writing

`grper` can also build and extend archives through
[`Writer`](https://docs.rs/grper/latest/grper/struct.Writer.html). Create a
fresh archive with `Writer::new`, or open one for appending with
`Writer::open` (which carries the existing files through). Stage files with
`add_file`, then call `finish` to write the header, file table, and data and
take back the target:

```rust
use std::io::{Cursor, Read, Seek, Write};
use grper::{Archive, Writer};

// Create an archive with two files.
let mut out = Cursor::new(Vec::new());
{
    let mut w = Writer::new(&mut out)?;
    w.add_file("DEFS.CON", b"def")?;
    w.add_file("HELLO.TXT", b"world")?;
    w.finish()?;
}

// Append to it: the existing files are carried through and rewritten ahead
// of the new one.
{
    let mut w = Writer::open(&mut out)?;
    w.add_file("EXTRA.BIN", &[1, 2, 3])?;
    w.finish()?;
}

let archive = Archive::new(Cursor::new(out.into_inner()))?;
assert_eq!(archive.len(), 3);
```

File names must be at most 12 bytes and must not contain a NUL byte; anything
else is rejected with a `grper::Error`.

Errors are returned as `grper::Error`, a `thiserror` enum distinguishing a bad
signature, a file too small for the header, a truncated table or data region,
an out-of-range index, a missing name, and underlying I/O errors.

### Features

| Feature    | Default | Effect                                             |
| ---------- | :-----: | -------------------------------------------------- |
| `cli`      | yes     | Builds the `grper` extraction binary.              |
| `testutil` | no      | Exposes the `FlakyReader` and `FlakyWriter` test doubles for injecting I/O errors. |

To use the library only, drop the CLI:

```toml
grper = { version = "0.1", default-features = false }
```

## The GRP format

A GRP file is a 16-byte header, a file table, and the files' data:

| Data type  | Name      | Description                              |
| ---------- | --------- | ---------------------------------------- |
| `char[12]` | signature | `"KenSilverman"` (not NUL-terminated)    |
| `u32le`    | fileCount | number of files                          |
| ...        | entries   | `fileCount` × (12-byte 8.3 name + `u32le` size) |

File data starts immediately after the last table entry; a file's offset is
the sum of the sizes of all files before it. Names are 8.3 style.

## Testing

Run the full suite (library, CLI in-process tests, and CLI subprocess tests):

```sh
cargo test
```

For coverage, `cargo llvm-cov --branch` is used, including the CLI. Line and
function coverage is held to 100%; region and branch coverage are aimed for
but not enforced, since they can be skewed by coverage-merge artifacts.
