use super::*;
use crate::{FlakyReader, FlakyWriter, build_grp};
use std::{
    assert_matches,
    io::{BufReader, Cursor},
};

/// Parse `bytes` and return each file as a `(name, data)` pair, in order.
fn files_of(bytes: Vec<u8>) -> Vec<(String, Vec<u8>)> {
    let mut archive = Archive::new(Cursor::new(bytes)).expect("valid archive");
    let names: Vec<String> = archive
        .entries()
        .iter()
        .map(|e| e.name().to_owned())
        .collect();
    names
        .iter()
        .map(|name| {
            let mut data = Vec::new();
            archive
                .entry_by_name(name)
                .expect("entry opens")
                .read_to_end(&mut data)
                .expect("reads file");
            (name.clone(), data)
        })
        .collect()
}

/// Data for a single-entry archive starts right after the 16-byte header
/// plus the one 16-byte table entry.
const DATA_START: u64 = 32;

#[test]
fn new_should_parse_signature_and_table() {
    let archive = Archive::new(Cursor::new(build_grp(&[
        ("A.TXT", b"123"),
        ("B.CON", b"4567"),
    ])))
    .unwrap();
    assert_eq!(archive.len(), 2);
}

#[test]
fn new_should_reject_bad_signature() {
    let mut buf = build_grp(&[("A.TXT", b"1")]);
    buf[0] = b'k';
    let err = Archive::new(Cursor::new(buf)).unwrap_err();
    assert_matches!(err, Error::BadSignature { .. });
}

#[test]
fn new_should_reject_files_shorter_than_header() {
    let err = Archive::new(Cursor::new(vec![0u8; 5])).unwrap_err();
    assert_matches!(err, Error::TooSmall(len) if len == 5);
}

#[test]
fn new_should_reject_truncated_table() {
    // The count claims 3 entries but only 2 are present.
    let mut buf = build_grp(&[("A.TXT", b"1"), ("B.TXT", b"2")]);
    buf[12..16].copy_from_slice(&3u32.to_le_bytes());
    let err = Archive::new(Cursor::new(buf)).unwrap_err();
    assert_matches!(err, Error::TableTruncated { index } if index == 2);
}

#[test]
fn new_should_reject_short_data_region() {
    let mut buf = build_grp(&[("A.TXT", b"12345")]);
    buf.truncate(buf.len() - 2);
    let err = Archive::new(Cursor::new(buf)).unwrap_err();
    assert_matches!(err, Error::DataTruncated { .. });
}

#[test]
fn new_should_ignore_trailing_bytes() {
    let mut buf = build_grp(&[("A.TXT", b"123")]);
    buf.extend_from_slice(b"trailing junk");
    let archive = Archive::new(Cursor::new(buf)).unwrap();
    assert_eq!(archive.len(), 1);
}

#[test]
fn new_should_parse_name_without_null_terminator() {
    // A 12-character name fills the field and has no room for a NUL.
    let mut buf = Vec::new();
    buf.extend_from_slice(b"KenSilverman");
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(b"ABCDEFGHIJKL");
    buf.extend_from_slice(&3u32.to_le_bytes());
    buf.extend_from_slice(b"xyz");
    let archive = Archive::new(Cursor::new(buf)).unwrap();
    assert_eq!(
        archive.get_entry("ABCDEFGHIJKL").map(|entry| entry.size()),
        Some(3)
    );
}

#[test]
fn new_should_parse_empty_archive() {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"KenSilverman");
    buf.extend_from_slice(&0u32.to_le_bytes());
    let archive = Archive::new(Cursor::new(buf)).unwrap();
    assert!(archive.is_empty());
}

#[test]
fn new_should_work_through_buf_reader() {
    let archive =
        Archive::new(BufReader::new(Cursor::new(build_grp(&[("A.TXT", b"123")])))).unwrap();
    assert_eq!(archive.len(), 1);
}

#[test]
fn entries_should_report_names_sizes_and_offsets() {
    // Data starts after the 16-byte header plus two 16-byte entries: 48.
    let archive = Archive::new(Cursor::new(build_grp(&[
        ("A.TXT", b"123"),
        ("B.CON", b"abcd"),
    ])))
    .unwrap();
    assert_eq!(archive.entries()[0].name(), "A.TXT");
    assert_eq!(archive.entries()[0].size(), 3);
    assert_eq!(archive.entries()[0].offset(), 48);
    assert_eq!(archive.entries()[1].offset(), 51);
}

#[test]
fn get_entry_should_find_entry_by_name() {
    let archive =
        Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1"), ("B.CON", b"2")]))).unwrap();
    assert!(archive.get_entry("B.CON").is_some());
}

#[test]
fn get_entry_should_return_none_for_missing_name() {
    let archive = Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1")]))).unwrap();
    assert!(archive.get_entry("NOPE.TXT").is_none());
}

#[test]
fn entry_should_reject_out_of_range_index() {
    let mut archive = Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1")]))).unwrap();
    let err = archive.entry(1).unwrap_err();
    assert_matches!(err, Error::IndexOutOfBounds { index: 1, len: 1 });
}

#[test]
fn new_should_propagate_io_errors_from_header_read() {
    let data = build_grp(&[("A.TXT", b"123")]);
    let err = Archive::new(FlakyReader::new(data).failing_reads_from(0)).unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn new_should_propagate_io_errors_from_length_probe() {
    // Both the header read and the length probe fail; the probe's error
    // must propagate as an I/O error.
    let err = Archive::new(FlakyReader::new(vec![0u8; 5]).failing_position_seeks()).unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn new_should_propagate_io_errors_from_table_read() {
    let data = build_grp(&[("A.TXT", b"123")]);
    let err =
        Archive::new(FlakyReader::new(data).failing_reads_from(HEADER_LEN as u64)).unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn entry_should_open_entry_at_index() {
    let data = build_grp(&[("A.TXT", b"123"), ("B.CON", b"45")]);
    let mut archive = Archive::new(Cursor::new(data)).unwrap();
    let file = archive.entry(1).unwrap();
    assert_eq!(file.name(), "B.CON");
    assert_eq!(file.size(), 2);
}

#[test]
fn entry_by_name_should_open_entry_that_exists() {
    let data = build_grp(&[("A.TXT", b"123"), ("B.CON", b"45")]);
    let mut archive = Archive::new(Cursor::new(data)).unwrap();
    let file = archive.entry_by_name("B.CON").unwrap();
    assert_eq!(file.name(), "B.CON");
}

#[test]
fn entry_by_name_should_reject_missing_name() {
    let mut archive = Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1")]))).unwrap();
    let err = archive.entry_by_name("MISS.TXT").unwrap_err();
    assert_matches!(err, Error::EntryNotFound { .. });
}

#[test]
fn is_empty_should_distinguish_empty_and_non_empty_archives() {
    let empty = Archive::new(Cursor::new(build_grp(&[]))).unwrap();
    assert!(empty.is_empty());
    let full = Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1")]))).unwrap();
    assert!(!full.is_empty());
}

#[test]
fn new_should_reject_bad_signature_through_buf_reader() {
    let mut data = build_grp(&[("A.TXT", b"1")]);
    data[0] = b'k';
    let err = Archive::new(BufReader::new(Cursor::new(data))).unwrap_err();
    assert_matches!(err, Error::BadSignature { .. });
}

#[test]
fn new_should_reject_files_shorter_than_header_through_buf_reader() {
    let err = Archive::new(BufReader::new(Cursor::new(vec![0u8; 5]))).unwrap_err();
    assert_matches!(err, Error::TooSmall(len) if len == 5);
}

#[test]
fn new_should_reject_truncated_table_through_buf_reader() {
    let mut data = build_grp(&[("A.TXT", b"1"), ("B.TXT", b"2")]);
    data[12..16].copy_from_slice(&3u32.to_le_bytes());
    let err = Archive::new(BufReader::new(Cursor::new(data))).unwrap_err();
    assert_matches!(err, Error::TableTruncated { .. });
}

#[test]
fn new_should_reject_short_data_region_through_buf_reader() {
    let mut data = build_grp(&[("A.TXT", b"12345")]);
    data.truncate(data.len() - 2);
    let err = Archive::new(BufReader::new(Cursor::new(data))).unwrap_err();
    assert_matches!(err, Error::DataTruncated { .. });
}

#[test]
fn new_should_propagate_io_errors_from_buf_reader() {
    // A buffered reader can surface non-Eof I/O errors from its source;
    // they must map to Error::Io, not a format error.
    let data = build_grp(&[("A.TXT", b"123")]);
    let err =
        Archive::new(BufReader::new(FlakyReader::new(data).failing_reads_from(0))).unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn new_should_reject_files_shorter_than_header_on_flaky_reader() {
    let err = Archive::new(FlakyReader::new(vec![0u8; 5])).unwrap_err();
    assert_matches!(err, Error::TooSmall(len) if len == 5);
}

#[test]
fn new_should_reject_bad_signature_on_flaky_reader() {
    let mut data = build_grp(&[("A.TXT", b"1")]);
    data[0] = b'k';
    let err = Archive::new(FlakyReader::new(data)).unwrap_err();
    assert_matches!(err, Error::BadSignature { .. });
}

#[test]
fn new_should_reject_truncated_table_on_flaky_reader() {
    let mut data = build_grp(&[("A.TXT", b"1"), ("B.TXT", b"2")]);
    data[12..16].copy_from_slice(&3u32.to_le_bytes());
    let err = Archive::new(FlakyReader::new(data)).unwrap_err();
    assert_matches!(err, Error::TableTruncated { .. });
}

#[test]
fn new_should_reject_short_data_region_on_flaky_reader() {
    let mut data = build_grp(&[("A.TXT", b"12345")]);
    data.truncate(data.len() - 2);
    let err = Archive::new(FlakyReader::new(data)).unwrap_err();
    assert_matches!(err, Error::DataTruncated { .. });
}

#[test]
fn extract_should_copy_entry_data() {
    let mut archive = Archive::new(Cursor::new(build_grp(&[
        ("A.TXT", b"12345"),
        ("B.CON", b""),
    ])))
    .unwrap();
    let mut out = Vec::new();
    let written = archive.extract(0, &mut out).unwrap();
    assert_eq!(written, 5);
    assert_eq!(out, b"12345");
}

#[test]
fn extract_should_reject_out_of_range_index() {
    let mut archive = Archive::new(Cursor::new(build_grp(&[("A.TXT", b"1")]))).unwrap();
    let err = archive.extract(1, &mut Vec::new()).unwrap_err();
    assert_matches!(err, Error::IndexOutOfBounds { index: 1, len: 1 });
}

#[test]
fn extract_should_propagate_read_errors() {
    let data = build_grp(&[("A.TXT", b"12345")]);
    let mut archive = Archive::new(FlakyReader::new(data).failing_reads_from(32))
        .expect("open succeeds; the failure is in the extraction read");
    let err = archive.extract(0, &mut Vec::new()).unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn writer_new_should_create_an_empty_archive() {
    let bytes = Writer::new(Cursor::new(Vec::new()))
        .finish()
        .unwrap()
        .into_inner();
    assert_eq!(bytes, build_grp(&[]));
}

#[test]
fn writer_new_should_write_files_in_order() {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    writer.add_file("A.TXT", b"12345").unwrap();
    writer.add_file("B.CON", b"45").unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(bytes, build_grp(&[("A.TXT", b"12345"), ("B.CON", b"45")]));
}

#[test]
fn writer_new_should_write_empty_file_data() {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    writer.add_file("A.TXT", b"").unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(bytes, build_grp(&[("A.TXT", b"")]));
}

#[test]
fn writer_new_should_write_a_full_length_name_without_null() {
    // A 12-byte name fills the field and has no room for a NUL terminator.
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    writer.add_file("ABCDEFGHIJKL", b"xyz").unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(
        files_of(bytes),
        vec![("ABCDEFGHIJKL".to_owned(), b"xyz".to_vec())]
    );
}

#[test]
fn writer_add_file_should_reject_names_that_are_too_long() {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let err = writer.add_file("ABCDEFGHIJKLM", b"1").unwrap_err();
    assert_matches!(err, Error::NameTooLong { .. });
}

#[test]
fn writer_add_file_should_reject_names_containing_a_null() {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let err = writer.add_file("A\0.TXT", b"1").unwrap_err();
    assert_matches!(err, Error::NameContainsNull { .. });
}

#[test]
fn writer_finish_should_handle_an_empty_archive() {
    // Finishing with no staged files writes just the header; the table
    // and data loops are entered zero times.
    let writer = Writer::new(FlakyWriter::new(Vec::new()));
    assert!(writer.finish().is_ok());
}

#[test]
fn writer_finish_should_succeed_when_position_seeks_fail() {
    // `finish` only issues a Start seek, so a position-seek failure is
    // harmless.
    let mut writer = Writer::new(FlakyWriter::new(Vec::new()).failing_position_seeks());
    writer.add_file("A.TXT", b"1").unwrap();
    assert!(writer.finish().is_ok());
}

#[test]
fn writer_finish_should_succeed_when_end_seeks_fail() {
    // `finish` only issues a Start seek, so a failing End seek is
    // harmless.
    let mut writer = Writer::new(FlakyWriter::new(Vec::new()).failing_end_seeks());
    writer.add_file("A.TXT", b"1").unwrap();
    assert!(writer.finish().is_ok());
}

#[test]
fn writer_finish_should_propagate_header_write_errors() {
    let mut writer = Writer::new(FlakyWriter::new(Vec::new()).failing_writes_at(0));
    writer.add_file("A.TXT", b"1").unwrap();
    let err = writer.finish().unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn writer_finish_should_propagate_table_write_errors() {
    // The 16-byte header writes cleanly; the first table record, issued at
    // position 16, fails.
    let mut writer = Writer::new(FlakyWriter::new(Vec::new()).failing_writes_at(HEADER_LEN as u64));
    writer.add_file("A.TXT", b"1").unwrap();
    let err = writer.finish().unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn writer_finish_should_propagate_data_write_errors() {
    // Header and table write cleanly; data for a one-entry archive begins
    // at 16 (header) + 16 (entry) = 32.
    let mut writer = Writer::new(FlakyWriter::new(Vec::new()).failing_writes_at(DATA_START));
    writer.add_file("A.TXT", b"1").unwrap();
    let err = writer.finish().unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn writer_finish_should_propagate_flush_errors() {
    let mut writer = Writer::new(FlakyWriter::new(Vec::new()).failing_flush());
    writer.add_file("A.TXT", b"1").unwrap();
    let err = writer.finish().unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn writer_finish_should_propagate_start_seek_errors() {
    let mut writer = Writer::new(FlakyWriter::new(Vec::new()).failing_start_seeks());
    writer.add_file("A.TXT", b"1").unwrap();
    let err = writer.finish().unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn writer_open_should_load_an_existing_archive() {
    let existing = build_grp(&[("A.TXT", b"123"), ("B.CON", b"45")]);
    let writer = Writer::open(Cursor::new(existing.clone())).unwrap();
    // Opening stages the existing files; finishing without adding any
    // rewrites them byte for byte.
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(bytes, existing);
}

#[test]
fn writer_open_should_handle_an_empty_archive() {
    let existing = build_grp(&[]);
    let writer = Writer::open(Cursor::new(existing.clone())).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(bytes, existing);
}

#[test]
fn writer_open_should_handle_an_empty_archive_on_flaky_writer() {
    // An empty archive is valid, so opening it stages zero files and the
    // file-copying loop is entered zero times. `failing_start_seeks` is
    // set so the opener's End probe takes the non-matching seek path.
    let writer = Writer::open(FlakyWriter::new(build_grp(&[])).failing_start_seeks());
    assert!(writer.is_ok());
}

#[test]
fn writer_open_should_stage_existing_files_on_flaky_writer() {
    // A non-empty archive opened via a flaky target (no faults forced)
    // runs the file-copying loop that stages the existing files.
    let existing = build_grp(&[("A.TXT", b"123"), ("B.CON", b"45")]);
    assert!(Writer::open(FlakyWriter::new(existing)).is_ok());
}

#[test]
fn writer_open_should_append_after_existing_files() {
    let existing = build_grp(&[("A.TXT", b"123"), ("B.CON", b"45")]);
    let mut writer = Writer::open(Cursor::new(existing)).unwrap();
    writer.add_file("C.BIN", &[9, 8, 7]).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(
        files_of(bytes),
        vec![
            ("A.TXT".to_owned(), b"123".to_vec()),
            ("B.CON".to_owned(), b"45".to_vec()),
            ("C.BIN".to_owned(), vec![9, 8, 7]),
        ]
    );
}

#[test]
fn writer_open_should_reject_a_missing_signature() {
    // At least 16 bytes so the header is read in full and the signature
    // is checked, but the wrong first twelve bytes.
    let err = Writer::open(Cursor::new(b"this is not a grp archive".to_vec())).unwrap_err();
    assert_matches!(err, Error::BadSignature { .. });
}

#[test]
fn writer_open_should_reject_a_file_shorter_than_header() {
    let err = Writer::open(Cursor::new(vec![0u8; 5])).unwrap_err();
    assert_matches!(err, Error::TooSmall(5));
}

#[test]
fn writer_open_should_propagate_header_read_errors() {
    let existing = build_grp(&[("A.TXT", b"1")]);
    let err = Writer::open(FlakyWriter::new(existing).failing_reads_at(0)).unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn writer_open_should_propagate_table_read_errors() {
    // The header reads cleanly; the table read issued at position 16 fails.
    let existing = build_grp(&[("A.TXT", b"1")]);
    let err =
        Writer::open(FlakyWriter::new(existing).failing_reads_at(HEADER_LEN as u64)).unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn writer_open_should_propagate_end_seek_errors() {
    // The header and table parse; the End(0) probe that `Archive::new`
    // issues to find the data region fails.
    let existing = build_grp(&[("A.TXT", b"1")]);
    let err = Writer::open(FlakyWriter::new(existing).failing_end_seeks()).unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn writer_open_should_propagate_position_seek_errors() {
    // A very short buffer fails the header read with EOF, which then
    // probes the position; that probe fails and propagates as I/O.
    let err = Writer::open(FlakyWriter::new(vec![0u8; 5]).failing_position_seeks()).unwrap_err();
    assert_matches!(err, Error::Io(_));
}

#[test]
fn writer_open_should_propagate_extract_read_errors() {
    // The archive opens, but reading the data of its first file (which
    // starts at 32) fails.
    let existing = build_grp(&[("A.TXT", b"12345")]);
    let err = Writer::open(FlakyWriter::new(existing).failing_reads_at(DATA_START)).unwrap_err();
    assert_matches!(err, Error::Io(_));
}
