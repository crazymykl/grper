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

```
Usage: grper <PATH> [FILE...] [--out-dir <OUT_DIR>] [--strict]
```

Extracts the files of a GRP archive to disk.

- `<PATH>` — path to the `.grp` archive.
- `[FILE...]` — optional names of entries to extract. When omitted, every
  entry is extracted. Names are matched **case-insensitively**, as on the DOS
  filesystems the format was designed for. Files are always written under the
  archive's spelling of their names; a request whose case differs from the
  stored name warns on stderr:

  ```console
  $ grper archive.grp defs.con -o out
  warning: "defs.con" is stored in the archive as "DEFS.CON"
  ```

- `-o`, `--out-dir <DIR>` — directory to extract into, created if missing
  (default: the current directory).
- `--strict` — abort with an error instead of proceeding when any warning is
  raised. Nothing is extracted and the out dir is not created.
- `--dry-run` — report what would be extracted without creating the out dir
  or writing any file. Selection, warnings, and `--strict` are still applied,
  so a `--strict --dry-run` with a case mismatch aborts the same way.

Examples:

```sh
# Extract everything to the current directory.
grper duke3d.grp

# Extract a couple of files to a specific directory.
grper duke3d.grp -o out DEFS.CON sounds.lump
```

Exit status is `0` on success, `1` on a runtime error (missing archive,
invalid archive, ...), and `2` on an argument error.

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

Errors are returned as `grper::Error`, a `thiserror` enum distinguishing a bad
signature, a file too small for the header, a truncated table or data region,
an out-of-range index, a missing name, and underlying I/O errors.

### Features

| Feature    | Default | Effect                                             |
| ---------- | :-----: | -------------------------------------------------- |
| `cli`      | yes     | Builds the `grper` extraction binary.              |
| `testutil` | no      | Exposes `FlakyReader`, a test double for injecting I/O errors. |

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

For coverage, `cargo llvm-cov --branch` reports 100% on lines, functions,
regions, and branches, including the CLI.
