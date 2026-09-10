#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]

use std::collections::HashSet;
use std::fs::File;
use std::io::{Cursor, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use grper::{Archive, Entry, Writer};

/// Read, create, and update Build engine GRP archives.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Extract the files of a GRP archive.
    Extract(Extract),
    /// Create a new GRP archive from files on disk.
    Create(Create),
    /// Replace or add files in an existing GRP archive.
    Update(Update),
}

/// Extract the files of a Build engine GRP archive.
///
/// By default every file in the archive is extracted. Pass one or more file
/// names to extract only those; names are matched case-insensitively, as on
/// the DOS filesystems the format was designed for.
///
/// With `--strict`, a request whose spelling differs from the archive's
/// spelling aborts the extraction instead of warning and proceeding.
#[derive(Args)]
struct Extract {
    /// Path to the .grp archive to extract.
    path: PathBuf,

    /// Extract only these archive entries, matched by name
    /// (case-insensitively, as on the DOS filesystems the format was
    /// designed for).
    ///
    /// All files are extracted when no names are given.
    #[arg(value_name = "FILE")]
    files: Vec<String>,

    /// Directory to extract the files into (created if it does not exist).
    #[arg(short = 'o', long, default_value = ".")]
    out_dir: PathBuf,

    /// Abort without extracting if any warnings are raised.
    #[arg(long)]
    strict: bool,

    /// Print the entries that would be extracted without creating or writing
    /// anything.
    #[arg(long)]
    dry_run: bool,
}

/// Create a new GRP archive from files on disk.
///
/// Each file's base name becomes the entry name. Entry names that differ
/// only in case are rejected, as the format's 8.3 names do not distinguish
/// case.
#[derive(Args)]
struct Create {
    /// Path of the .grp archive to create (must not already exist).
    archive: PathBuf,

    /// Files to add; each file's base name becomes its entry name.
    #[arg(value_name = "FILE")]
    files: Vec<PathBuf>,

    /// Print the entries that would be created without writing anything.
    #[arg(long)]
    dry_run: bool,
}

/// Replace or add files in an existing GRP archive.
///
/// A file whose base name exactly matches a stored entry replaces that
/// entry's data (keeping the stored spelling and position); any other file is
/// appended. A base name that differs only in case from a stored entry is an
/// error, as the format's 8.3 names do not distinguish case.
#[derive(Args)]
struct Update {
    /// Path of the .grp archive to update (must be a valid GRP archive).
    archive: PathBuf,

    /// Files to add or replace; each file's base name is matched against the
    /// archive's entries.
    #[arg(value_name = "FILE")]
    files: Vec<PathBuf>,

    /// Print the changes that would be made without writing anything.
    #[arg(long)]
    dry_run: bool,
}

/// Forwards to [`cli_main`] and exits with a status code.
fn main() {
    std::process::exit(cli_main(&std::env::args().collect::<Vec<String>>()));
}

/// Parse `args`, run the requested verb, print the result to stderr on
/// failure, and return the process exit code.
fn cli_main(args: &[String]) -> i32 {
    match Cli::try_parse_from(args) {
        Ok(Cli { command }) => match command {
            Command::Extract(Extract {
                path,
                files,
                out_dir,
                strict,
                dry_run,
            }) => exit_code(run(&path, &out_dir, &files, strict, dry_run)),
            Command::Create(Create {
                archive,
                files,
                dry_run,
            }) => exit_code(create(&archive, &files, dry_run)),
            Command::Update(Update {
                archive,
                files,
                dry_run,
            }) => exit_code(update(&archive, &files, dry_run)),
        },
        Err(err) => {
            // `try_parse_from` does not print or exit; do both here so the
            // error goes to stderr like a normal clap failure.
            let _ = err.print();
            err.exit_code()
        }
    }
}

/// Map a verb's result to a process exit code, printing errors to stderr.
fn exit_code(result: Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("grper: {err:?}");
            1
        }
    }
}

/// The entries selected for extraction, and any warnings raised while
/// resolving their names.
#[derive(Debug)]
struct Selection {
    /// Indices into the archive's file table, in request order.
    indices: Vec<usize>,

    /// Notes about names that matched only after ignoring case.
    warnings: Vec<String>,
}

/// The entries to extract: all of them, or the ones named in `only`.
///
/// Names are matched case-insensitively: GRP is a DOS-era format, and FAT
/// looked up 8.3 names without regard to case. A name that differs in case
/// from the archive's spelling still resolves, but a warning naming both
/// spellings is returned with the selection. Repeated names in `only` are
/// de-duplicated while preserving first-seen order.
fn pick(names: &[&str], only: &[String]) -> Result<Selection> {
    if only.is_empty() {
        return Ok(Selection {
            indices: (0..names.len()).collect(),
            warnings: Vec::new(),
        });
    }
    let mut indices: Vec<usize> = Vec::with_capacity(only.len());
    let mut warnings: Vec<String> = Vec::new();
    for name in only {
        let index = names
            .iter()
            .position(|stored| stored.eq_ignore_ascii_case(name))
            .with_context(|| format!("no entry named {name:?} in archive"))?;
        let stored = names[index];
        if stored != name {
            warnings.push(format!("{name:?} is stored in the archive as {stored:?}"));
        }
        if !indices.contains(&index) {
            indices.push(index);
        }
    }
    Ok(Selection { indices, warnings })
}

/// Load `path` and extract it (see [`run_with`]).
fn run(path: &Path, out_dir: &Path, only: &[String], strict: bool, dry_run: bool) -> Result<()> {
    let data = std::fs::read(path).with_context(|| format!("failed to read {path:?}"))?;
    run_with(Cursor::new(data), path, out_dir, only, strict, dry_run)
}

/// Extract a GRP archive from `reader` into `out_dir`, where `label` is the
/// display name of the archive used in error messages.
///
/// By default every entry is extracted; `only` names entries to extract
/// (case-insensitively, warning on stderr when the case differs from the
/// archive's spelling). Files are written under the archive's spelling of
/// their names, and an entry whose data runs short of its declared size is
/// reported as a truncated archive. When `strict` is set, any warning aborts
/// the extraction before `out_dir` is created or any file is written. With
/// `dry_run` the archive is opened and the selection resolved (including the
/// warnings and the strict check) but nothing is created or written; instead
/// the entries that would be extracted are printed.
fn run_with<R: Read + Seek>(
    reader: R,
    label: &Path,
    out_dir: &Path,
    only: &[String],
    strict: bool,
    dry_run: bool,
) -> Result<()> {
    let mut archive =
        Archive::new(reader).with_context(|| format!("{label:?} is not a valid GRP archive"))?;

    let names: Vec<&str> = archive.entries().iter().map(Entry::name).collect();
    let selection = pick(&names, only)?;
    for warning in &selection.warnings {
        eprintln!("warning: {warning}");
    }
    // In strict mode a warning is fatal: abort before creating `out_dir` or
    // writing any file, so a mismatched request leaves nothing behind.
    if strict && !selection.warnings.is_empty() {
        bail!(
            "{} warning(s) raised; aborting in --strict mode",
            selection.warnings.len()
        );
    }

    if !dry_run && !out_dir.is_dir() {
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("failed to create {out_dir:?}"))?;
    }

    for &index in &selection.indices {
        let meta = archive.entries()[index].clone();
        let name = meta.name();
        let name_path = Path::new(name);
        // Entry names come from the archive; a hand-crafted one can contain
        // anything. GRP names are flat 8.3 filenames, so require exactly one
        // ordinary component (no separators, no `.`/`..`, not absolute) to
        // keep writes inside `out_dir`.
        let components: Vec<Component<'_>> = name_path.components().collect();
        match components.as_slice() {
            // Safe: a single flat name such as `DEFS.CON`.
            [Component::Normal(_)] => {}
            _ => bail!("refusing to extract entry with unsafe path {name:?} into {out_dir:?}"),
        }

        if dry_run {
            println!("{name:<12} {} bytes", meta.size());
            continue;
        }

        let out = out_dir.join(name);
        let mut out_file =
            File::create(&out).with_context(|| format!("failed to create {out:?}"))?;
        let written = archive
            .extract(index, &mut out_file)
            .with_context(|| format!("failed to extract {name:?} from {label:?}"))?;
        if written != meta.size() {
            bail!(
                "{label:?} is truncated: {name:?} declares {} bytes but only {} are present",
                meta.size(),
                written
            );
        }
        println!("{name:<12} {} bytes", written);
    }
    println!(
        "{} {} file(s) from {label:?} to {out_dir:?}",
        if dry_run {
            "would extract"
        } else {
            "extracted"
        },
        selection.indices.len()
    );
    Ok(())
}

/// What happened to an entry when planning a create or update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// An existing entry whose data was left as-is (update only).
    Unchanged,
    /// An existing entry whose data was replaced (update only).
    Replaced,
    /// A newly added entry.
    Added,
}

/// Create a new GRP archive at `archive` from the source `files`.
///
/// `archive` must not already exist; use `update` to modify an archive. Each
/// source file's base name becomes its entry name.
fn create(archive: &Path, files: &[PathBuf], dry_run: bool) -> Result<()> {
    if archive.exists() {
        bail!("{archive:?} already exists; use grper update");
    }
    let staged = stage_sources(files)?;
    let plan = plan_update(Vec::new(), staged)?;
    if dry_run {
        print_plan(&plan, false);
        println!("would create {archive:?} with {} file(s)", plan.len());
        return Ok(());
    }
    commit_plan(&plan, archive)?;
    println!("created {archive:?} with {} file(s)", plan.len());
    Ok(())
}

/// Load `archive` and replace or add files in it (see [`update_with`]).
fn update(archive: &Path, files: &[PathBuf], dry_run: bool) -> Result<()> {
    let data = std::fs::read(archive).with_context(|| format!("failed to read {archive:?}"))?;
    update_with(Cursor::new(data), archive, files, dry_run)
}

/// Replace or add files in the GRP archive read from `reader`, where `archive`
/// is the display name of the archive used in messages and the path the
/// result is written back to.
///
/// A source file whose base name exactly matches a stored entry replaces that
/// entry's data (keeping the stored spelling and position); any other file is
/// appended. A base name differing only in case from a stored entry is an
/// error. With `dry_run` the plan is printed but nothing is written.
fn update_with<R: Read + Seek>(
    reader: R,
    archive: &Path,
    files: &[PathBuf],
    dry_run: bool,
) -> Result<()> {
    let mut archive_read =
        Archive::new(reader).with_context(|| format!("{archive:?} is not a valid GRP archive"))?;
    let existing = read_all_entries(&mut archive_read)?;
    let staged = stage_sources(files)?;
    let plan = plan_update(existing, staged)?;
    if dry_run {
        print_plan(&plan, true);
    } else {
        commit_plan(&plan, archive)?;
    }
    let replaced = plan
        .iter()
        .filter(|entry| entry.2 == Status::Replaced)
        .count();
    let added = plan.iter().filter(|entry| entry.2 == Status::Added).count();
    println!(
        "{} {archive:?}: {replaced} replaced, {added} added",
        if dry_run { "would update" } else { "updated" }
    );
    Ok(())
}

/// Read each source `files` path and return `(base name, data)` pairs, in
/// order, with identical base names de-duplicated (first wins).
fn stage_sources(files: &[PathBuf]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut staged = Vec::with_capacity(files.len());
    for path in files {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("cannot use {path:?} as a source file"))?;
        let data = std::fs::read(path).with_context(|| format!("failed to read {path:?}"))?;
        staged.push((name.to_owned(), data));
    }
    let mut seen = HashSet::new();
    staged.retain(|(name, _)| seen.insert(name.clone()));
    Ok(staged)
}

/// Build the resulting archive's entries from `existing` and `incoming`.
///
/// `existing` are the archive's current `(name, data)` pairs, in table order;
/// `incoming` are the staged source files, de-duplicated by exact name. An
/// incoming file whose name exactly matches an existing entry replaces that
/// entry's data in place; any other incoming file is appended. An incoming
/// name that differs only in case from any existing or already-added entry is
/// an error, since the format's 8.3 names do not distinguish case.
fn plan_update(
    existing: Vec<(String, Vec<u8>)>,
    incoming: Vec<(String, Vec<u8>)>,
) -> Result<Vec<(String, Vec<u8>, Status)>> {
    let mut plan: Vec<(String, Vec<u8>, Status)> = existing
        .into_iter()
        .map(|(name, data)| (name, data, Status::Unchanged))
        .collect();
    for (name, data) in incoming {
        match plan.iter().position(|(stored, _, _)| stored == &name) {
            Some(index) => {
                plan[index].1 = data;
                plan[index].2 = Status::Replaced;
            }
            None => {
                if let Some((conflict, _, _)) = plan
                    .iter()
                    .find(|(stored, _, _)| stored.eq_ignore_ascii_case(&name))
                {
                    bail!(
                        "{name:?} conflicts with {conflict:?}; \
                         names differing only in case are not allowed"
                    );
                }
                plan.push((name, data, Status::Added));
            }
        }
    }
    Ok(plan)
}

/// Read every entry of `archive` into memory as `(name, data)` pairs, in
/// table order.
fn read_all_entries<R: Read + Seek>(archive: &mut Archive<R>) -> Result<Vec<(String, Vec<u8>)>> {
    let metas: Vec<(String, u64)> = archive
        .entries()
        .iter()
        .map(|entry| (entry.name().to_owned(), entry.size()))
        .collect();
    let mut files = Vec::with_capacity(metas.len());
    for (index, (name, size)) in metas.iter().enumerate() {
        let mut data = Vec::with_capacity(*size as usize);
        archive
            .extract(index, &mut data)
            .with_context(|| format!("failed to read entry {name:?}"))?;
        files.push((name.clone(), data));
    }
    Ok(files)
}

/// Write the GRP archive holding `plan` into `writer` and return the target.
///
/// The target is generic so a test can hand in a faulted writer to exercise
/// the build's error paths (a name rejection from `add_file`, or an I/O error
/// from `finish`); production hands in an in-memory `Cursor`.
fn build_archive<W: Read + Seek + Write>(
    writer: W,
    plan: &[(String, Vec<u8>, Status)],
) -> Result<W> {
    let mut writer = Writer::new(writer);
    for (name, data, _) in plan {
        writer.add_file(name, data)?;
    }
    Ok(writer.finish()?)
}

/// Build `plan` into a temporary file in the archive's directory, then rename
/// it over `archive`.
///
/// The archive is built in memory first, so a failed build leaves neither the
/// archive nor a temp file behind; the single `fs::write` then moves the bytes
/// into place.
fn commit_plan(plan: &[(String, Vec<u8>, Status)], archive: &Path) -> Result<()> {
    let temp = temp_path(archive);
    let bytes = build_archive(Cursor::new(Vec::new()), plan)?.into_inner();
    std::fs::write(&temp, bytes).with_context(|| format!("failed to create {temp:?}"))?;
    move_into_place(&temp, archive)
}

/// Rename `temp` over `archive`, removing `temp` if the rename fails, so a
/// failed commit leaves the archive untouched and no temp file behind.
fn move_into_place(temp: &Path, archive: &Path) -> Result<()> {
    match std::fs::rename(temp, archive) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(temp);
            Err(error).with_context(|| format!("failed to replace {archive:?}"))
        }
    }
}

/// The temporary file path used to build an archive before renaming it into
/// place: the archive path with a `.grper-tmp` suffix, in the same directory.
fn temp_path(archive: &Path) -> PathBuf {
    let mut name = archive.as_os_str().to_owned();
    name.push(".grper-tmp");
    PathBuf::from(name)
}

/// Print one line per planned entry; `show_status` appends the entry's status
/// (used for updates, where entries are replaced, added, or unchanged).
fn print_plan(plan: &[(String, Vec<u8>, Status)], show_status: bool) {
    for (name, data, status) in plan {
        let tag = if show_status {
            match status {
                Status::Unchanged => " unchanged",
                Status::Replaced => " replaced",
                Status::Added => " added",
            }
        } else {
            ""
        };
        println!("{name:<12} {} bytes{tag}", data.len());
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Build an in-memory GRP archive from `(name, data)` pairs.
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

    fn grp_path(dir: &Path, name: &str, bytes: Vec<u8>) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("writes test grp");
        path
    }

    /// Write a source file `name` with `data` into `dir` and return its path.
    fn source_path(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, data).expect("writes test source");
        path
    }

    /// The entry names and data of a GRP archive on disk, in table order.
    fn read_archive(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut archive = Archive::new(grper::FlakyReader::new(
            std::fs::read(path).expect("reads archive"),
        ))
        .expect("valid archive");
        read_all_entries(&mut archive).expect("reads all entries")
    }

    // Data for a single-entry archive starts right after the 16-byte header
    // plus the one 16-byte table entry.
    const DATA_START: u64 = 32;

    #[test]
    fn run_should_extract_all_entries_with_their_stored_names() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(
            dir.path(),
            "all.grp",
            build_grp(&[("A.TXT", b"123"), ("B.CON", b"45")]),
        );
        // `dir` already exists, so the create-dir path is skipped.
        run(&grp, dir.path(), &[], false, false).expect("extracts all");
        assert_eq!(
            std::fs::read(dir.path().join("A.TXT")).expect("reads A.TXT"),
            b"123"
        );
        assert_eq!(
            std::fs::read(dir.path().join("B.CON")).expect("reads B.CON"),
            b"45"
        );
    }

    #[test]
    fn run_should_extract_only_selected_entries_and_warn_on_case() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(
            dir.path(),
            "sel.grp",
            build_grp(&[("DEFS.CON", b"abc"), ("DUB.MAP", b"")]),
        );
        // `out` does not exist yet, so it must be created.
        let out = dir.path().join("out");
        run(&grp, &out, &["defs.con".to_owned()], false, false).expect("extracts selection");
        assert_eq!(
            std::fs::read(out.join("DEFS.CON")).expect("reads DEFS.CON"),
            b"abc"
        );
        // Only the selected entry was written, under its stored spelling
        // (checked by directory listing, which is reliable even on
        // case-insensitive file systems).
        let names: Vec<String> = out
            .read_dir()
            .expect("reads out")
            .map(|entry| {
                let entry = entry.expect("reads dir entry");
                entry.file_name().to_string_lossy().into_owned()
            })
            .collect();
        assert_eq!(names, vec!["DEFS.CON".to_owned()]);
    }

    #[test]
    fn run_should_extract_zero_entries_from_an_empty_archive() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "empty.grp", build_grp(&[]));
        let out = dir.path().join("out");
        run(&grp, &out, &[], false, false).expect("no entries to extract");
        assert!(out.is_dir());
        assert!(out.read_dir().expect("reads out").count() == 0);
    }

    #[test]
    fn run_should_error_when_a_requested_name_is_missing() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "miss.grp", build_grp(&[("A.TXT", b"1")]));
        let err = run(&grp, dir.path(), &["MISS.CON".to_owned()], false, false).unwrap_err();
        assert!(err.to_string().contains("MISS.CON"));
    }

    #[test]
    fn run_should_error_when_the_archive_is_not_valid_grp() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "bad.grp", b"definitely not a grp file".to_vec());
        let err = run(&grp, dir.path(), &[], false, false).unwrap_err();
        assert!(err.to_string().contains("not a valid GRP archive"));
    }

    #[test]
    fn run_should_error_when_the_archive_path_is_missing() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let err = run(
            dir.path().join("missing.grp").as_path(),
            dir.path(),
            &[],
            false,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to read"));
    }

    #[test]
    fn run_should_error_when_the_out_dir_cannot_be_created() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "ok.grp", build_grp(&[("A.TXT", b"1")]));
        // A file where the out dir wants to be: creation must fail.
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, []).expect("creates blocker");
        let err = run(&grp, blocked.join("in").as_path(), &[], false, false).unwrap_err();
        assert!(err.to_string().contains("failed to create"));
    }

    #[test]
    fn run_should_error_when_output_file_cannot_be_created() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "ok.grp", build_grp(&[("A.TXT", b"1")]));
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).expect("creates out");
        // A directory where the output file wants to be: create must fail.
        std::fs::create_dir(out.join("A.TXT")).expect("creates blocker dir");
        let err = run(&grp, &out, &[], false, false).unwrap_err();
        assert!(err.to_string().contains("failed to create"));
    }

    #[test]
    fn run_should_refuse_an_unsafe_entry_name() {
        let dir = tempfile::tempdir().expect("scratch dir");
        // A name with a separator is not a single ordinary component.
        let grp = grp_path(dir.path(), "deep.grp", build_grp(&[("A/B.TXT", b"1")]));
        let err = run(&grp, dir.path(), &["A/B.TXT".to_owned()], false, false).unwrap_err();
        assert!(err.to_string().contains("unsafe path"));
    }

    #[test]
    fn run_should_error_when_entry_data_is_truncated() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = grp_path(dir.path(), "short.grp", build_grp(&[("A.TXT", b"12345")]));
        let data = std::fs::read(&path).expect("reads grp");
        // The file is complete, but the first data read returns zero as if
        // the file ended right after the table: the written byte count falls
        // short of the declared size.
        let err = run_with(
            grper::FlakyReader::new(data).zero_reads_at(DATA_START),
            Path::new("short.grp"),
            dir.path(),
            &[],
            false,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("truncated"));
    }

    #[test]
    fn run_should_propagate_extraction_read_errors() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = grp_path(dir.path(), "flaky.grp", build_grp(&[("A.TXT", b"12345")]));
        let data = std::fs::read(&path).expect("reads grp");
        // The table parses fine; make the data read fail.
        let err = run_with(
            grper::FlakyReader::new(data).failing_reads_from(DATA_START),
            Path::new("flaky.grp"),
            dir.path(),
            &[],
            false,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to extract"));
    }

    #[test]
    fn run_should_propagate_a_single_failing_read() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = grp_path(dir.path(), "once.grp", build_grp(&[("A.TXT", b"12345")]));
        let data = std::fs::read(&path).expect("reads grp");
        // Only the exact read at the data start fails.
        let err = run_with(
            grper::FlakyReader::new(data).failing_reads_at(DATA_START),
            Path::new("once.grp"),
            dir.path(),
            &[],
            false,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to extract"));
    }

    #[test]
    fn run_should_propagate_repositioning_seek_errors() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = grp_path(dir.path(), "seek.grp", build_grp(&[("A.TXT", b"12345")]));
        let data = std::fs::read(&path).expect("reads grp");
        // Opening only seeks from the end, so it succeeds; the first data
        // read repositions with a Start seek and fails.
        let err = run_with(
            grper::FlakyReader::new(data).failing_start_seeks(),
            Path::new("seek.grp"),
            dir.path(),
            &[],
            false,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to extract"));
    }

    #[test]
    fn run_should_fail_to_open_when_end_seeks_fail() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = grp_path(dir.path(), "end.grp", build_grp(&[("A.TXT", b"12345")]));
        let data = std::fs::read(&path).expect("reads grp");
        // Opening probes the archive length with an End seek.
        let err = run_with(
            grper::FlakyReader::new(data).failing_end_seeks(),
            Path::new("end.grp"),
            dir.path(),
            &[],
            false,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a valid GRP archive"));
    }

    #[test]
    fn run_should_succeed_when_position_seeks_fail() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = grp_path(dir.path(), "pos.grp", build_grp(&[("A.TXT", b"123")]));
        let data = std::fs::read(&path).expect("reads grp");
        // The CLI never probes the reader's position (no `stream_position`),
        // so position-seek failures are harmless.
        let out = dir.path().join("out");
        run_with(
            grper::FlakyReader::new(data).failing_position_seeks(),
            Path::new("pos.grp"),
            &out,
            &[],
            false,
            false,
        )
        .expect("extracts despite failing position seeks");
        assert_eq!(
            std::fs::read(out.join("A.TXT")).expect("reads A.TXT"),
            b"123"
        );
    }

    #[test]
    fn run_should_abort_in_strict_mode_when_a_warning_is_raised() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "sel.grp", build_grp(&[("DEFS.CON", b"abc")]));
        // `defs.con` differs in case, which raises a warning; in strict mode
        // that must abort the extraction.
        let out = dir.path().join("out");
        let err = run(&grp, &out, &["defs.con".to_owned()], true, false).unwrap_err();
        assert!(err.to_string().contains("--strict mode"));
        // Aborted before `out_dir` was created, so nothing was written.
        assert!(!out.exists());
    }

    #[test]
    fn run_should_proceed_in_strict_mode_without_warnings() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "sel.grp", build_grp(&[("DEFS.CON", b"abc")]));
        // The requested case matches the stored spelling, so no warning is
        // raised and strict mode proceeds normally.
        let out = dir.path().join("out");
        run(&grp, &out, &["DEFS.CON".to_owned()], true, false).expect("extracts in strict mode");
        assert_eq!(
            std::fs::read(out.join("DEFS.CON")).expect("reads DEFS.CON"),
            b"abc"
        );
    }

    #[test]
    fn create_should_build_an_archive_from_source_files() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let s1 = source_path(dir.path(), "A.TXT", b"12345");
        let s2 = source_path(dir.path(), "B.CON", b"45");
        let archive = dir.path().join("new.grp");
        create(&archive, &[s1, s2], false).expect("creates archive");
        let entries = read_archive(&archive);
        assert_eq!(
            entries,
            vec![
                ("A.TXT".to_owned(), b"12345".to_vec()),
                ("B.CON".to_owned(), b"45".to_vec()),
            ]
        );
    }

    #[test]
    fn create_should_build_an_empty_archive_with_no_sources() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let archive = dir.path().join("empty.grp");
        create(&archive, &[], false).expect("creates empty archive");
        assert!(read_archive(&archive).is_empty());
    }

    #[test]
    fn create_should_error_when_the_archive_already_exists() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let source = source_path(dir.path(), "A.TXT", b"1");
        let archive = grp_path(dir.path(), "busy.grp", build_grp(&[("B.CON", b"2")]));
        let err = create(&archive, &[source], false).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn create_should_deduplicate_repeated_sources() {
        let dir = tempfile::tempdir().expect("scratch dir");
        // The same source twice: only one entry, first-seen data wins.
        let source = source_path(dir.path(), "A.TXT", b"12345");
        let archive = dir.path().join("dup.grp");
        create(&archive, &[source.clone(), source], false).expect("creates archive");
        let entries = read_archive(&archive);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "A.TXT");
    }

    #[test]
    fn create_should_error_when_a_source_is_missing() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let archive = dir.path().join("new.grp");
        let missing = dir.path().join("nope.txt");
        let err = create(&archive, &[missing], false).unwrap_err();
        assert!(err.to_string().contains("failed to read"));
    }

    #[test]
    fn create_should_error_when_a_source_has_no_base_name() {
        // A path with no file component cannot supply an entry name.
        let archive = Path::new("/unused/grp");
        let err = create(archive, &[PathBuf::from("..")], false).unwrap_err();
        assert!(err.to_string().contains("cannot use"));
    }

    // Non-UTF-8 base names only arise on Unix, so this test is Unix-only;
    // coverage is measured on Linux, where the arm it exercises is covered.
    #[cfg(unix)]
    #[test]
    fn create_should_error_when_a_source_name_is_not_utf8() {
        // A base name that is not valid UTF-8 cannot become an entry name.
        use std::os::unix::ffi::OsStringExt;
        let weird = std::ffi::OsString::from_vec(vec![b'A', 0xFF, b'.', b'T', b'X', b'T']);
        let archive = Path::new("/unused/grp");
        let err = create(archive, &[PathBuf::from(weird)], false).unwrap_err();
        assert!(err.to_string().contains("cannot use"));
    }

    #[test]
    fn create_should_error_when_an_entry_name_is_too_long() {
        let dir = tempfile::tempdir().expect("scratch dir");
        // A 13-character base name exceeds the format's 12-byte limit.
        let source = source_path(dir.path(), "ABCDEFGHIJKLM", b"1");
        let archive = dir.path().join("new.grp");
        let err = create(&archive, &[source], false).unwrap_err();
        assert!(err.to_string().contains("too long"));
        // The failed build must not leave the archive (or a temp file) behind.
        assert!(!archive.exists());
        assert!(!temp_path(&archive).exists());
    }

    #[test]
    fn create_should_error_when_source_names_conflict_in_case() {
        let dir = tempfile::tempdir().expect("scratch dir");
        // Two sources differing only in case conflict: the format's 8.3 names
        // do not distinguish case. (On a case-insensitive file system the two
        // paths are the same file, but their base names still differ in case.)
        let upper = source_path(dir.path(), "A.TXT", b"1");
        let lower = source_path(dir.path(), "a.txt", b"2");
        let archive = dir.path().join("new.grp");
        let err = create(&archive, &[upper, lower], false).unwrap_err();
        assert!(err.to_string().contains("conflicts with"));
        // No archive or temp file is left behind.
        assert!(!archive.exists());
        assert!(!temp_path(&archive).exists());
    }

    #[test]
    fn create_should_print_and_write_nothing_in_dry_run() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let source = source_path(dir.path(), "A.TXT", b"123");
        let archive = dir.path().join("new.grp");
        create(&archive, &[source], true).expect("dry run succeeds");
        assert!(!archive.exists());
        assert!(!temp_path(&archive).exists());
    }

    #[test]
    fn update_should_replace_matching_and_append_new_entries() {
        let dir = tempfile::tempdir().expect("scratch dir");
        // Base archive: A.TXT and B.CON.
        let archive = grp_path(
            dir.path(),
            "base.grp",
            build_grp(&[("A.TXT", b"old-a"), ("B.CON", b"keep-b")]),
        );
        // Replace A.TXT (exact match) and add C.MAP; B.CON is untouched.
        let a = source_path(dir.path(), "A.TXT", b"new-a");
        let c = source_path(dir.path(), "C.MAP", b"map");
        update(&archive, &[a, c], false).expect("updates archive");
        let entries = read_archive(&archive);
        assert_eq!(
            entries,
            vec![
                ("A.TXT".to_owned(), b"new-a".to_vec()),
                ("B.CON".to_owned(), b"keep-b".to_vec()),
                ("C.MAP".to_owned(), b"map".to_vec()),
            ]
        );
    }

    #[test]
    fn update_should_error_on_a_case_only_match() {
        let dir = tempfile::tempdir().expect("scratch dir");
        // The archive stores "A.TXT".
        let archive = grp_path(dir.path(), "base.grp", build_grp(&[("A.TXT", b"old-a")]));
        // A source named "a.txt" differs only in case: it must be rejected
        // rather than silently creating a case-duplicate. (On case-
        // insensitive file systems this reuses the "A.TXT" file, which is
        // fine; only its base name matters here.)
        let lower = source_path(dir.path(), "a.txt", b"new-a");
        let err = update(&archive, &[lower], false).unwrap_err();
        assert!(err.to_string().contains("conflicts with"));
        // The archive is left untouched.
        let entries = read_archive(&archive);
        assert_eq!(entries, vec![("A.TXT".to_owned(), b"old-a".to_vec())]);
    }

    #[test]
    fn update_should_error_when_the_archive_is_missing() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let source = source_path(dir.path(), "A.TXT", b"1");
        let archive = dir.path().join("missing.grp");
        let err = update(&archive, &[source], false).unwrap_err();
        assert!(err.to_string().contains("failed to read"));
    }

    #[test]
    fn update_should_error_when_the_archive_is_not_grp() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let source = source_path(dir.path(), "A.TXT", b"1");
        let archive = grp_path(dir.path(), "bad.grp", b"not a grp file".to_vec());
        let err = update(&archive, &[source], false).unwrap_err();
        assert!(err.to_string().contains("not a valid GRP archive"));
    }

    #[test]
    fn update_should_add_to_an_empty_archive() {
        // Updating an archive that holds no entries reads nothing and appends
        // the staged file as its first entry.
        let dir = tempfile::tempdir().expect("scratch dir");
        let archive = grp_path(dir.path(), "empty.grp", build_grp(&[]));
        let a = source_path(dir.path(), "A.TXT", b"1");
        update(&archive, &[a], false).expect("updates archive");
        assert_eq!(
            read_archive(&archive),
            vec![("A.TXT".to_owned(), b"1".to_vec())]
        );
    }

    #[test]
    fn update_should_print_and_write_nothing_in_dry_run() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let archive = grp_path(
            dir.path(),
            "base.grp",
            build_grp(&[("A.TXT", b"old-a"), ("B.CON", b"keep-b")]),
        );
        let a = source_path(dir.path(), "A.TXT", b"new-a");
        let c = source_path(dir.path(), "C.MAP", b"map");
        update(&archive, &[a, c], true).expect("dry run succeeds");
        // The archive is unchanged and no temp file is left behind.
        let entries = read_archive(&archive);
        assert_eq!(
            entries,
            vec![
                ("A.TXT".to_owned(), b"old-a".to_vec()),
                ("B.CON".to_owned(), b"keep-b".to_vec()),
            ]
        );
        assert!(!temp_path(&archive).exists());
    }

    #[test]
    fn update_with_should_reject_an_invalid_archive() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let archive = dir.path().join("bad.grp");
        let err = update_with(
            grper::FlakyReader::new(b"definitely not a grp file".to_vec()),
            &archive,
            &[],
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a valid GRP archive"));
    }

    #[test]
    fn update_with_should_propagate_entry_read_errors() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let archive = dir.path().join("bad.grp");
        // The table parses, but reading the data of the first file (which
        // starts at 32) fails.
        let data = build_grp(&[("A.TXT", b"12345")]);
        let err = update_with(
            grper::FlakyReader::new(data).failing_reads_from(DATA_START),
            &archive,
            &[],
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to read entry"));
    }

    #[test]
    fn update_with_should_propagate_source_errors() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let archive = dir.path().join("base.grp");
        let data = build_grp(&[("A.TXT", b"123")]);
        let err = update_with(
            grper::FlakyReader::new(data),
            &archive,
            &[dir.path().join("missing.txt")],
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to read"));
    }

    #[test]
    fn update_should_error_when_an_entry_name_is_too_long() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let archive = grp_path(dir.path(), "base.grp", build_grp(&[("A.TXT", b"123")]));
        // A 13-character base name exceeds the format's 12-byte limit; the
        // build fails before anything is written.
        let source = source_path(dir.path(), "ABCDEFGHIJKLM", b"1");
        let err = update(&archive, &[source], false).unwrap_err();
        assert!(err.to_string().contains("too long"));
        // The archive is untouched and no temp file is left behind.
        let entries = read_archive(&archive);
        assert_eq!(entries, vec![("A.TXT".to_owned(), b"123".to_vec())]);
        assert!(!temp_path(&archive).exists());
    }

    #[test]
    fn commit_should_fail_when_the_temp_path_is_a_directory() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let source = source_path(dir.path(), "A.TXT", b"1");
        let archive = dir.path().join("new.grp");
        // A directory where the temp file wants to be: the write must fail.
        std::fs::create_dir(temp_path(&archive)).expect("creates blocker dir");
        let err = create(&archive, &[source], false).unwrap_err();
        assert!(err.to_string().contains("failed to create"));
        assert!(!archive.exists());
    }

    #[test]
    fn build_archive_should_propagate_finish_errors() {
        // A faulted target (a failing flush) makes `finish` error, and the
        // error propagates out of the build.
        let plan = vec![("A.TXT".to_owned(), b"1".to_vec(), Status::Added)];
        let writer = grper::FlakyWriter::new(Vec::new()).failing_flush();
        let err = build_archive(writer, &plan).unwrap_err();
        assert!(err.to_string().contains("forced I/O error"));
    }

    #[test]
    fn move_into_place_should_fail_and_remove_temp_when_the_target_is_a_directory() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let temp = dir.path().join("x.grp.grper-tmp");
        std::fs::write(&temp, []).expect("creates temp");
        // Renaming a file over a directory fails.
        let err = move_into_place(&temp, dir.path()).unwrap_err();
        assert!(err.to_string().contains("failed to replace"));
        assert!(!temp.exists());
    }

    #[test]
    fn stage_sources_should_reject_an_unreadable_source() {
        let dir = tempfile::tempdir().expect("scratch dir");
        // A directory is not a readable source file.
        let subdir = dir.path().join("adir");
        std::fs::create_dir(&subdir).expect("creates dir");
        let err = stage_sources(&[subdir]).unwrap_err();
        assert!(err.to_string().contains("failed to read"));
    }

    #[test]
    fn plan_update_should_replace_an_exact_match_in_place() {
        let existing = vec![
            ("A.TXT".to_owned(), b"old-a".to_vec()),
            ("B.CON".to_owned(), b"keep-b".to_vec()),
        ];
        let incoming = vec![("A.TXT".to_owned(), b"new-a".to_vec())];
        let plan = plan_update(existing, incoming).unwrap();
        assert_eq!(
            plan,
            vec![
                ("A.TXT".to_owned(), b"new-a".to_vec(), Status::Replaced),
                ("B.CON".to_owned(), b"keep-b".to_vec(), Status::Unchanged),
            ]
        );
    }

    #[test]
    fn plan_update_should_append_new_entries_in_request_order() {
        let existing = vec![("A.TXT".to_owned(), b"old-a".to_vec())];
        let incoming = vec![
            ("C.MAP".to_owned(), b"c".to_vec()),
            ("B.CON".to_owned(), b"b".to_vec()),
        ];
        let plan = plan_update(existing, incoming).unwrap();
        assert_eq!(
            plan,
            vec![
                ("A.TXT".to_owned(), b"old-a".to_vec(), Status::Unchanged),
                ("C.MAP".to_owned(), b"c".to_vec(), Status::Added),
                ("B.CON".to_owned(), b"b".to_vec(), Status::Added),
            ]
        );
    }

    #[test]
    fn plan_update_should_mark_all_entries_added_when_creating() {
        let incoming = vec![
            ("A.TXT".to_owned(), b"a".to_vec()),
            ("B.CON".to_owned(), b"b".to_vec()),
        ];
        let plan = plan_update(Vec::new(), incoming).unwrap();
        assert!(plan.iter().all(|(_, _, status)| *status == Status::Added));
    }

    #[test]
    fn plan_update_should_error_when_incoming_conflicts_with_existing_case() {
        let existing = vec![("A.TXT".to_owned(), b"old".to_vec())];
        let incoming = vec![("a.txt".to_owned(), b"new".to_vec())];
        let err = plan_update(existing, incoming).unwrap_err();
        assert!(err.to_string().contains("conflicts with"));
    }

    #[test]
    fn plan_update_should_error_when_incoming_entries_conflict_in_case() {
        // Two new entries differing only in case collide with each other.
        let incoming = vec![
            ("A.TXT".to_owned(), b"a".to_vec()),
            ("a.txt".to_owned(), b"b".to_vec()),
        ];
        let err = plan_update(Vec::new(), incoming).unwrap_err();
        assert!(err.to_string().contains("conflicts with"));
    }

    #[test]
    fn read_all_entries_should_return_names_and_data_in_order() {
        let mut archive = Archive::new(grper::FlakyReader::new(build_grp(&[
            ("A.TXT", b"123"),
            ("B.CON", b"45"),
        ])))
        .unwrap();
        assert_eq!(
            read_all_entries(&mut archive).unwrap(),
            vec![
                ("A.TXT".to_owned(), b"123".to_vec()),
                ("B.CON".to_owned(), b"45".to_vec()),
            ]
        );
    }

    #[test]
    fn read_all_entries_should_return_empty_for_an_empty_archive() {
        // An archive with no entries yields no staged data; the per-entry
        // read loop runs zero times.
        let mut archive =
            Archive::new(grper::FlakyReader::new(build_grp(&[]))).expect("valid archive");
        assert!(
            read_all_entries(&mut archive)
                .expect("reads all entries")
                .is_empty()
        );
    }

    #[test]
    fn create_should_report_an_empty_plan_in_dry_run() {
        // A dry-run create with no sources plans zero entries; printing that
        // empty plan leaves no archive or temp file behind.
        let dir = tempfile::tempdir().expect("scratch dir");
        let archive = dir.path().join("empty.grp");
        create(&archive, &[], true).expect("dry run succeeds");
        assert!(!archive.exists());
        assert!(!temp_path(&archive).exists());
    }

    #[test]
    fn temp_path_should_append_a_suffix_in_the_same_directory() {
        assert_eq!(
            temp_path(Path::new("dir/archive.grp")),
            PathBuf::from("dir/archive.grp.grper-tmp")
        );
    }

    #[test]
    fn cli_main_should_return_zero_on_successful_extraction() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "ok.grp", build_grp(&[("A.TXT", b"1")]));
        let out = dir.path().join("out");
        let args = vec![
            "grper".to_owned(),
            "extract".to_owned(),
            grp.to_string_lossy().into_owned(),
            "-o".to_owned(),
            out.to_string_lossy().into_owned(),
        ];
        assert_eq!(cli_main(&args), 0);
        assert_eq!(std::fs::read(out.join("A.TXT")).expect("reads A.TXT"), b"1");
    }

    #[test]
    fn cli_main_should_return_zero_on_successful_create() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let source = source_path(dir.path(), "A.TXT", b"1");
        let archive = dir.path().join("made.grp");
        let args = vec![
            "grper".to_owned(),
            "create".to_owned(),
            archive.to_string_lossy().into_owned(),
            source.to_string_lossy().into_owned(),
        ];
        assert_eq!(cli_main(&args), 0);
        assert_eq!(
            read_archive(&archive),
            vec![("A.TXT".to_owned(), b"1".to_vec())]
        );
    }

    #[test]
    fn cli_main_should_return_zero_on_successful_update() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let archive = grp_path(dir.path(), "base.grp", build_grp(&[("A.TXT", b"old")]));
        let source = source_path(dir.path(), "A.TXT", b"new");
        let args = vec![
            "grper".to_owned(),
            "update".to_owned(),
            archive.to_string_lossy().into_owned(),
            source.to_string_lossy().into_owned(),
        ];
        assert_eq!(cli_main(&args), 0);
        assert_eq!(
            read_archive(&archive),
            vec![("A.TXT".to_owned(), b"new".to_vec())]
        );
    }

    #[test]
    fn cli_main_should_return_one_on_a_runtime_error() {
        let dir = tempfile::tempdir().expect("scratch dir");
        // Valid arguments, but the archive does not exist.
        let args = vec![
            "grper".to_owned(),
            "extract".to_owned(),
            dir.path()
                .join("missing.grp")
                .to_string_lossy()
                .into_owned(),
        ];
        assert_eq!(cli_main(&args), 1);
    }

    #[test]
    fn cli_main_should_return_clap_error_code_on_bad_arguments() {
        // No subcommand: clap's usage-error exit code.
        let args = vec!["grper".to_owned()];
        assert_eq!(cli_main(&args), 2);
    }

    /// The error from a `pick` call that is expected to fail.
    fn pick_err(names: &[&str], only: &[String]) -> anyhow::Error {
        match pick(names, only) {
            Ok(_) => panic!("pick should have failed"),
            Err(err) => err,
        }
    }

    #[test]
    fn pick_should_return_all_indices_when_no_names_given() {
        let names = ["A.TXT", "B.CON", "C.MAP"];
        let selection = pick(&names, &[]).unwrap();
        assert_eq!(selection.indices, vec![0, 1, 2]);
        assert!(selection.warnings.is_empty());
    }

    #[test]
    fn pick_should_return_requested_indices_in_request_order() {
        let names = ["A.TXT", "B.CON", "C.MAP"];
        let only = vec!["C.MAP".to_owned(), "A.TXT".to_owned()];
        assert_eq!(pick(&names, &only).unwrap().indices, vec![2, 0]);
    }

    #[test]
    fn pick_should_deduplicate_repeated_names() {
        let names = ["A.TXT", "B.CON"];
        let only = vec!["A.TXT".to_owned(), "A.TXT".to_owned()];
        assert_eq!(pick(&names, &only).unwrap().indices, vec![0]);
    }

    #[test]
    fn pick_should_error_when_a_name_is_missing() {
        let names = ["A.TXT"];
        let only = vec!["MISS.CON".to_owned()];
        let err = pick_err(&names, &only);
        assert!(err.to_string().contains("MISS.CON"));
    }

    #[test]
    fn pick_should_match_names_case_insensitively_and_warn() {
        let names = ["DEFS.CON", "DUB.MAP"];
        let only = vec!["defs.con".to_owned()];
        let selection = pick(&names, &only).unwrap();
        assert_eq!(selection.indices, vec![0]);
        assert_eq!(selection.warnings.len(), 1);
        assert!(selection.warnings[0].contains("defs.con"));
        assert!(selection.warnings[0].contains("DEFS.CON"));
    }

    #[test]
    fn pick_should_not_warn_when_the_case_matches() {
        let names = ["DEFS.CON"];
        let only = vec!["DEFS.CON".to_owned()];
        let selection = pick(&names, &only).unwrap();
        assert_eq!(selection.indices, vec![0]);
        assert!(selection.warnings.is_empty());
    }

    #[test]
    fn pick_should_pick_first_of_case_variants_and_warn() {
        // Duplicates differing only in case resolve to the first entry, with
        // a warning naming the archive's spelling.
        let names = ["A.TXT", "a.txt"];
        let only = vec!["a.txt".to_owned()];
        let selection = pick(&names, &only).unwrap();
        assert_eq!(selection.indices, vec![0]);
        assert_eq!(selection.warnings.len(), 1);
    }

    #[test]
    fn pick_should_deduplicate_names_that_differ_only_in_case() {
        let names = ["A.TXT"];
        let only = vec!["A.TXT".to_owned(), "a.txt".to_owned()];
        let selection = pick(&names, &only).unwrap();
        assert_eq!(selection.indices, vec![0]);
        assert_eq!(selection.warnings.len(), 1);
    }

    #[test]
    fn pick_should_not_case_fold_non_ascii_names() {
        // 8.3 names are ASCII in practice; only ASCII letters fold.
        let names = ["ÜBER.CON"];
        let only = vec!["über.con".to_owned()];
        let err = pick_err(&names, &only);
        assert!(err.to_string().contains("über.con"));
    }
}
