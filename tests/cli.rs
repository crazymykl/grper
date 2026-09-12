#![cfg(feature = "cli")]

//! Black-box tests for the `grper` CLI, driven through a real subprocess with
//! [`assert_cmd`].
//!
//! The archives are built here (synthetic files, never the proprietary
//! `duke3d.grp`) and written to throwaway temp dirs, so these tests are safe
//! to check in and to run in CI.

use assert_cmd::Command;
use grper::build_grp;
use predicates::prelude::*;
use std::env::consts::EXE_SUFFIX;
use std::path::{Path, PathBuf};

/// The number of files in the archive built by [`write_archive`].
const FILE_COUNT: u32 = 3;

/// A fixed set of files, using a couple of different cases so the
/// case-insensitive selection can be exercised.
const FILES: &[(&str, &[u8])] = &[
    ("DEFS.CON", b"defs-data"),
    ("HELLO.TXT", b"hello"),
    ("DUB.MAP", b"map-bytes"),
];

/// Write a GRP archive holding `files` into `dir` and return its path.
fn write_archive(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, build_grp(files)).expect("writes archive");
    path
}

/// Write a source file `name` with `data` into `dir` and return its path.
fn write_source(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, data).expect("writes source");
    path
}

/// A temp dir holding a valid archive and an output dir.
struct Harness {
    dir: tempfile::TempDir,
    archive: PathBuf,
    out: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_owned();
        let archive = write_archive(&root, "test.grp", FILES);
        Self {
            dir,
            archive,
            out: root.join("out"),
        }
    }
}

#[test]
fn extract_all_default_out_dir() {
    let h = Harness::new();
    // No `-o`: files land in the current directory.
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .current_dir(&h.dir)
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "extracted {FILE_COUNT} file(s)"
        )));
    assert_eq!(
        std::fs::read(h.dir.path().join("DEFS.CON")).unwrap(),
        b"defs-data"
    );
    assert_eq!(
        std::fs::read(h.dir.path().join("HELLO.TXT")).unwrap(),
        b"hello"
    );
    assert_eq!(
        std::fs::read(h.dir.path().join("DUB.MAP")).unwrap(),
        b"map-bytes"
    );
}

#[test]
fn extract_all_to_out_dir() {
    let h = Harness::new();
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .arg("-o")
        .arg(&h.out)
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "extracted {FILE_COUNT} file(s)"
        )));
    // The out dir was created and holds every file under its stored name.
    assert!(h.out.is_dir());
    assert_eq!(std::fs::read(h.out.join("DEFS.CON")).unwrap(), b"defs-data");
    assert_eq!(std::fs::read(h.out.join("HELLO.TXT")).unwrap(), b"hello");
    assert_eq!(std::fs::read(h.out.join("DUB.MAP")).unwrap(), b"map-bytes");
}

#[test]
fn extract_selected_entries_only() {
    let h = Harness::new();
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .arg("-o")
        .arg(&h.out)
        .arg("HELLO.TXT")
        .arg("dub.map")
        .assert()
        .success()
        .stdout(predicate::str::contains("extracted 2 file(s)"));
    assert_eq!(std::fs::read(h.out.join("HELLO.TXT")).unwrap(), b"hello");
    assert_eq!(std::fs::read(h.out.join("DUB.MAP")).unwrap(), b"map-bytes");
    // The unselected entry must not be written.
    assert!(!h.out.join("DEFS.CON").exists());
}

#[test]
fn case_insensitive_selection_warns_on_stderr() {
    let h = Harness::new();
    // `defs.con` differs in case from the stored `DEFS.CON`: it resolves and
    // warns, and the file is written under the stored spelling.
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .arg("-o")
        .arg(&h.out)
        .arg("defs.con")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "warning: \"defs.con\" is stored in the archive as \"DEFS.CON\"",
        ))
        .stdout(predicate::str::contains("extracted 1 file(s)"));
    assert_eq!(std::fs::read(h.out.join("DEFS.CON")).unwrap(), b"defs-data");
    // The file is named with the stored spelling (checked by directory
    // listing, which is reliable even on case-insensitive file systems).
    let names: Vec<String> = h
        .out
        .read_dir()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            entry.file_name().to_string_lossy().into_owned()
        })
        .collect();
    assert_eq!(names, vec!["DEFS.CON".to_owned()]);
}

#[test]
fn strict_mode_aborts_on_case_mismatch() {
    let h = Harness::new();
    // `defs.con` differs in case from the stored `DEFS.CON`: strict mode
    // reports the warning and aborts without extracting anything.
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .arg("-o")
        .arg(&h.out)
        .arg("--strict")
        .arg("defs.con")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "warning: \"defs.con\" is stored in the archive as \"DEFS.CON\"",
        ))
        .stderr(predicate::str::contains("--strict mode"));
    // Nothing was extracted and the out dir was not created.
    assert!(!h.out.exists());
}

#[test]
fn strict_mode_proceeds_when_the_case_matches() {
    let h = Harness::new();
    // The requested spelling matches the stored one, so no warning is raised
    // and strict mode proceeds normally.
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .arg("-o")
        .arg(&h.out)
        .arg("--strict")
        .arg("DEFS.CON")
        .assert()
        .success()
        .stdout(predicate::str::contains("extracted 1 file(s)"));
    assert_eq!(std::fs::read(h.out.join("DEFS.CON")).unwrap(), b"defs-data");
}

#[test]
fn dry_run_reports_without_writing() {
    let h = Harness::new();
    // `--dry-run` lists what would be extracted, but the out dir is never
    // created and no file is written.
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .arg("-o")
        .arg(&h.out)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("would extract 3 file(s)"));
    assert!(!h.out.exists());
}

#[test]
fn dry_run_with_strict_aborts_on_case_mismatch() {
    let h = Harness::new();
    // The case mismatch raises a warning; with `--strict` even a dry run
    // aborts, leaving nothing behind.
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .arg("-o")
        .arg(&h.out)
        .arg("--strict")
        .arg("--dry-run")
        .arg("defs.con")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--strict mode"));
    assert!(!h.out.exists());
}

#[test]
fn missing_name_fails_and_names_it() {
    let h = Harness::new();
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .arg("-o")
        .arg(&h.out)
        .arg("MISSING.CON")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no entry named \"MISSING.CON\""));
}

#[test]
fn non_grp_file_fails() {
    let h = Harness::new();
    let bad = h.dir.path().join("notgrp.grp");
    std::fs::write(&bad, b"this is not a grper archive").unwrap();
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&bad)
        .arg("-o")
        .arg(&h.out)
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a valid GRP archive"));
}

#[test]
fn short_data_region_is_rejected() {
    let h = Harness::new();
    // A single entry whose declared size overruns the data actually present:
    // the archive passes the signature and table checks, but its file is too
    // short to hold the declared data, so opening it fails.
    let short = h.dir.path().join("short.grp");
    std::fs::write(
        &short,
        overdeclare_size(&build_grp(&[("BIG.BIN", b"1234")])),
    )
    .unwrap();
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&short)
        .arg("-o")
        .arg(&h.out)
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a valid GRP archive"));
}

#[test]
fn missing_archive_fails() {
    let h = Harness::new();
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(h.dir.path().join("does-not-exist.grp"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read"));
}

#[test]
fn missing_positional_fails_with_usage() {
    // No subcommand at all: clap rejects it with its usage error and exit
    // code 2.
    Command::cargo_bin("grper")
        .expect("binary builds")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "Usage: grper{EXE_SUFFIX} <COMMAND>"
        )));
}

#[test]
fn extract_without_an_archive_fails_with_usage() {
    // The extract subcommand requires its path argument.
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage:"));
}

#[test]
fn create_builds_an_archive_that_extracts_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let s1 = write_source(root, "HELLO.TXT", b"hello");
    let s2 = write_source(root, "DUB.MAP", b"map-bytes");
    let archive = root.join("made.grp");
    // Create the archive from the source files.
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("create")
        .arg(&archive)
        .arg(&s1)
        .arg(&s2)
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));
    // It extracts back to the original files.
    let out = root.join("out");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&archive)
        .arg("-o")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("extracted 2 file(s)"));
    assert_eq!(std::fs::read(out.join("HELLO.TXT")).unwrap(), b"hello");
    assert_eq!(std::fs::read(out.join("DUB.MAP")).unwrap(), b"map-bytes");
}

#[test]
fn create_refuses_an_existing_archive() {
    let h = Harness::new();
    let source = write_source(h.dir.path(), "EXTRA.BIN", b"x");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("create")
        .arg(&h.archive)
        .arg(&source)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn create_dry_run_writes_nothing() {
    let h = Harness::new();
    let source = write_source(h.dir.path(), "EXTRA.BIN", b"x");
    let archive = h.dir.path().join("made.grp");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("create")
        .arg(&archive)
        .arg(&source)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("would create"));
    assert!(!archive.exists());
}

#[test]
fn update_replaces_and_appends() {
    let h = Harness::new();
    // Replace DEFS.CON in place and append a new entry.
    let replaced = write_source(h.dir.path(), "DEFS.CON", b"updated-data");
    let added = write_source(h.dir.path(), "NEW.BIN", b"new-bytes");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("update")
        .arg(&h.archive)
        .arg(&replaced)
        .arg(&added)
        .assert()
        .success()
        .stdout(predicate::str::contains("updated"));
    let out = h.dir.path().join("out");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .arg("-o")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("extracted 4 file(s)"));
    // The replaced entry has the new data, under its stored spelling; the
    // added entry is present; the untouched entry is unchanged.
    assert_eq!(
        std::fs::read(out.join("DEFS.CON")).unwrap(),
        b"updated-data"
    );
    assert_eq!(std::fs::read(out.join("NEW.BIN")).unwrap(), b"new-bytes");
    assert_eq!(std::fs::read(out.join("HELLO.TXT")).unwrap(), b"hello");
    assert_eq!(std::fs::read(out.join("DUB.MAP")).unwrap(), b"map-bytes");
}

#[test]
fn update_refuses_a_case_only_match() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // An archive storing "DEFS.CON".
    let archive = write_archive(root, "base.grp", &[("DEFS.CON", b"old-data")]);
    // A source whose base name differs only in case must be rejected.
    let lower = write_source(root, "defs.con", b"new-data");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("update")
        .arg(&archive)
        .arg(&lower)
        .assert()
        .failure()
        .stderr(predicate::str::contains("conflicts with"));
    // The archive is left untouched.
    let out = root.join("out");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&archive)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();
    assert_eq!(std::fs::read(out.join("DEFS.CON")).unwrap(), b"old-data");
}

#[test]
fn create_dry_run_with_no_sources_reports_empty() {
    // A dry-run create with no source files plans zero entries: it reports the
    // (empty) plan without writing an archive or a temp file.
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("made.grp");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("create")
        .arg(&archive)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("would create"))
        .stdout(predicate::str::contains("0 file(s)"));
    assert!(!archive.exists());
    // No temp file lingers in the archive's directory.
    assert!(!dir.path().join("made.grp.grper-tmp").exists());
}

#[test]
fn update_an_empty_archive_appends() {
    // An archive that holds no entries: the update reads nothing and appends
    // the staged file as its first entry.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let archive = write_archive(root, "empty.grp", &[]);
    let added = write_source(root, "NEW.BIN", b"new-bytes");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("update")
        .arg(&archive)
        .arg(&added)
        .assert()
        .success()
        .stdout(predicate::str::contains("1 added"));
    let out = root.join("out");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&archive)
        .arg("-o")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("extracted 1 file(s)"));
    assert_eq!(std::fs::read(out.join("NEW.BIN")).unwrap(), b"new-bytes");
}

#[test]
fn update_missing_archive_fails() {
    let h = Harness::new();
    let source = write_source(h.dir.path(), "EXTRA.BIN", b"x");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("update")
        .arg(h.dir.path().join("does-not-exist.grp"))
        .arg(&source)
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read"));
}

#[test]
fn update_dry_run_writes_nothing() {
    let h = Harness::new();
    let replaced = write_source(h.dir.path(), "DEFS.CON", b"updated-data");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("update")
        .arg(&h.archive)
        .arg(&replaced)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("would update"));
    // The archive is unchanged.
    let out = h.dir.path().join("out");
    Command::cargo_bin("grper")
        .expect("binary builds")
        .arg("extract")
        .arg(&h.archive)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();
    assert_eq!(std::fs::read(out.join("DEFS.CON")).unwrap(), b"defs-data");
}

/// Make the single entry in a one-entry archive claim more data than it has,
/// so opening it fails as a truncated archive.
fn overdeclare_size(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    // 16-byte header, then the entry's 12-byte (null-padded) name, then its 4-byte size.
    let size_offset = 16 + 12;
    out[size_offset..size_offset + 4].copy_from_slice(&100u32.to_le_bytes());
    out
}
