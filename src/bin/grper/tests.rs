use super::*;
use grper::{FlakyReader, FlakyWriter, build_grp};

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
    let mut archive = Archive::new(FlakyReader::new(
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
        FlakyReader::new(data).zero_reads_at(DATA_START),
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
        FlakyReader::new(data).failing_reads_from(DATA_START),
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
        FlakyReader::new(data).failing_reads_at(DATA_START),
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
        FlakyReader::new(data).failing_start_seeks(),
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
        FlakyReader::new(data).failing_end_seeks(),
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
        FlakyReader::new(data).failing_position_seeks(),
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
        FlakyReader::new(b"definitely not a grp file".to_vec()),
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
        FlakyReader::new(data).failing_reads_from(DATA_START),
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
        FlakyReader::new(data),
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
    let writer = FlakyWriter::new(Vec::new()).failing_flush();
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
    let mut archive = Archive::new(FlakyReader::new(build_grp(&[
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
    let mut archive = Archive::new(FlakyReader::new(build_grp(&[]))).expect("valid archive");
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
