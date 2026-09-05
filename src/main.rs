#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use grper::{Archive, Entry};

/// Extract the files of a Build engine GRP archive.
///
/// By default every file in the archive is extracted. Pass one or more file
/// names to extract only those; names are matched case-insensitively, as on
/// the DOS filesystems the format was designed for.
///
/// With `--strict`, a request whose spelling differs from the archive's
/// spelling aborts the extraction instead of warning and proceeding.
#[derive(Parser)]
#[command(version, about)]
struct Args {
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
}

/// Forwards to [`cli_main`] and exits with a status code.
fn main() {
    std::process::exit(cli_main(&std::env::args().collect::<Vec<String>>()));
}

/// Parse `args`, run the extraction, print the result to stderr on
/// failure, and return the process exit code.
fn cli_main(args: &[String]) -> i32 {
    match Args::try_parse_from(args) {
        Ok(args) => match run(&args.path, &args.out_dir, &args.files, args.strict) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("grper: {err:?}");
                1
            }
        },
        Err(err) => {
            // `try_parse_from` does not print or exit; do both here so the
            // error goes to stderr like a normal clap failure.
            let _ = err.print();
            err.exit_code()
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
fn run(path: &Path, out_dir: &Path, only: &[String], strict: bool) -> Result<()> {
    let data = std::fs::read(path).with_context(|| format!("failed to read {path:?}"))?;
    run_with(grper::FlakyReader::new(data), path, out_dir, only, strict)
}

/// Extract a GRP archive from `reader` into `out_dir`, where `label` is the
/// display name of the archive used in error messages.
///
/// By default every entry is extracted; `only` names entries to extract
/// (case-insensitively, warning on stderr when the case differs from the
/// archive's spelling). Files are written under the archive's spelling of
/// their names, and an entry whose data runs short of its declared size is
/// reported as a truncated archive. When `strict` is set, any warning aborts
/// the extraction before `out_dir` is created or any file is written.
fn run_with<R: Read + Seek>(
    reader: R,
    label: &Path,
    out_dir: &Path,
    only: &[String],
    strict: bool,
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

    if !out_dir.is_dir() {
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
        "extracted {} file(s) from {label:?} to {out_dir:?}",
        selection.indices.len()
    );
    Ok(())
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
        run(&grp, dir.path(), &[], false).expect("extracts all");
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
        run(&grp, &out, &["defs.con".to_owned()], false).expect("extracts selection");
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
        run(&grp, &out, &[], false).expect("no entries to extract");
        assert!(out.is_dir());
        assert!(out.read_dir().expect("reads out").count() == 0);
    }

    #[test]
    fn run_should_error_when_a_requested_name_is_missing() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "miss.grp", build_grp(&[("A.TXT", b"1")]));
        let err = run(&grp, dir.path(), &["MISS.CON".to_owned()], false).unwrap_err();
        assert!(err.to_string().contains("MISS.CON"));
    }

    #[test]
    fn run_should_error_when_the_archive_is_not_valid_grp() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "bad.grp", b"definitely not a grp file".to_vec());
        let err = run(&grp, dir.path(), &[], false).unwrap_err();
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
        let err = run(&grp, blocked.join("in").as_path(), &[], false).unwrap_err();
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
        let err = run(&grp, &out, &[], false).unwrap_err();
        assert!(err.to_string().contains("failed to create"));
    }

    #[test]
    fn run_should_refuse_an_unsafe_entry_name() {
        let dir = tempfile::tempdir().expect("scratch dir");
        // A name with a separator is not a single ordinary component.
        let grp = grp_path(dir.path(), "deep.grp", build_grp(&[("A/B.TXT", b"1")]));
        let err = run(&grp, dir.path(), &["A/B.TXT".to_owned()], false).unwrap_err();
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
        let err = run(&grp, &out, &["defs.con".to_owned()], true).unwrap_err();
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
        run(&grp, &out, &["DEFS.CON".to_owned()], true).expect("extracts in strict mode");
        assert_eq!(
            std::fs::read(out.join("DEFS.CON")).expect("reads DEFS.CON"),
            b"abc"
        );
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

    #[test]
    fn cli_main_should_return_zero_on_successful_extraction() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let grp = grp_path(dir.path(), "ok.grp", build_grp(&[("A.TXT", b"1")]));
        let out = dir.path().join("out");
        let args = vec![
            "grper".to_owned(),
            grp.to_string_lossy().into_owned(),
            "-o".to_owned(),
            out.to_string_lossy().into_owned(),
        ];
        assert_eq!(cli_main(&args), 0);
        assert_eq!(std::fs::read(out.join("A.TXT")).expect("reads A.TXT"), b"1");
    }

    #[test]
    fn cli_main_should_return_one_on_a_runtime_error() {
        let dir = tempfile::tempdir().expect("scratch dir");
        // Valid arg list, but the archive does not exist.
        let args = vec![
            "grper".to_owned(),
            dir.path()
                .join("missing.grp")
                .to_string_lossy()
                .into_owned(),
        ];
        assert_eq!(cli_main(&args), 1);
    }

    #[test]
    fn cli_main_should_return_clap_error_code_on_bad_arguments() {
        // No archive path: clap's usage-error exit code.
        let args = vec!["grper".to_owned()];
        assert_eq!(cli_main(&args), 2);
    }

    /// The error from a `pick` call that is expected to fail.
    fn pick_err(names: &[&str], only: &[String]) -> anyhow::Error {
        match pick(names, only) {
            Ok(_) => panic!("pick was expected to fail"),
            Err(err) => err,
        }
    }

    #[test]
    fn selection_debug_should_show_indices_and_warnings() {
        let selection = Selection {
            indices: vec![0],
            warnings: vec!["note".to_owned()],
        };
        let rendered = format!("{selection:?}");
        assert!(rendered.contains("indices"));
        assert!(rendered.contains("warnings"));
        assert!(rendered.contains("note"));
    }
}
