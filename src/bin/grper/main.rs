#[cfg(test)]
mod tests;

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
