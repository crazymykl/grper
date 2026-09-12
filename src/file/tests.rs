use super::*;
use crate::{Archive, Error, FlakyReader, build_grp};
use std::assert_matches;
use std::io::Cursor;

fn archive_with(files: &[(&str, &[u8])]) -> Archive<Cursor<Vec<u8>>> {
    Archive::new(Cursor::new(build_grp(files))).expect("valid archive")
}

fn flaky_archive(reader: FlakyReader) -> Archive<FlakyReader> {
    Archive::new(reader).expect("valid archive")
}

#[test]
fn read_should_return_all_entry_bytes() {
    let mut archive = archive_with(&[("A.TXT", b"hello"), ("B.TXT", b"world")]);
    let mut file = archive.entry(0).unwrap();
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"hello");
}

#[test]
fn read_should_stop_at_entry_boundary() {
    let mut archive = archive_with(&[("A.TXT", b"12"), ("B.TXT", b"345")]);
    let mut file = archive.entry(0).unwrap();
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    assert_eq!(buf.len(), 2);
}

#[test]
fn read_should_return_zero_at_entry_end() {
    let mut archive = archive_with(&[("A.TXT", b"ab")]);
    let mut file = archive.entry(0).unwrap();
    let mut buf = [0u8; 8];
    assert_eq!(file.read(&mut buf).unwrap(), 2);
    assert_eq!(file.read(&mut buf).unwrap(), 0);
}

#[test]
fn read_should_return_zero_for_empty_entry() {
    let mut archive = archive_with(&[("A.TXT", b"")]);
    let mut file = archive.entry(0).unwrap();
    assert_eq!(file.read(&mut [0u8; 1]).unwrap(), 0);
}

#[test]
fn seek_should_reposition_reads() {
    let mut archive = archive_with(&[("A.TXT", b"0123456789")]);
    let mut file = archive.entry(0).unwrap();
    file.seek(SeekFrom::Start(4)).unwrap();
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"4567");
}

#[test]
fn seek_should_support_end_and_current() {
    let mut archive = archive_with(&[("A.TXT", b"0123456789")]);
    let mut file = archive.entry(0).unwrap();
    assert_eq!(file.seek(SeekFrom::End(-3)).unwrap(), 7);
    let mut buf = [0u8; 3];
    file.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"789");
    assert_eq!(file.stream_position().unwrap(), 10);
}

#[test]
fn seek_should_reject_positions_before_start() {
    let mut archive = archive_with(&[("A.TXT", b"12345")]);
    let mut file = archive.entry(0).unwrap();
    assert!(file.seek(SeekFrom::End(-6)).is_err());
    assert!(file.seek(SeekFrom::Current(-1)).is_err());
}

#[test]
fn seek_should_allow_positions_past_end() {
    let mut archive = archive_with(&[("A.TXT", b"123")]);
    let mut file = archive.entry(0).unwrap();
    assert_eq!(file.seek(SeekFrom::Start(100)).unwrap(), 100);
    assert_eq!(file.read(&mut [0u8; 1]).unwrap(), 0);
}

#[test]
fn read_should_be_repeatable_after_rewind() {
    let mut archive = archive_with(&[("A.TXT", b"abc")]);
    let mut file = archive.entry(0).unwrap();
    let mut first = Vec::new();
    file.read_to_end(&mut first).unwrap();
    file.rewind().unwrap();
    let mut second = Vec::new();
    file.read_to_end(&mut second).unwrap();
    assert_eq!(first, second);
}

#[test]
fn name_and_size_should_match_entry_metadata() {
    let mut archive = archive_with(&[("A.TXT", b"12345")]);
    let file = archive.entry(0).unwrap();
    assert_eq!(file.name(), "A.TXT");
    assert_eq!(file.size(), 5);
    assert_eq!(file.position(), 0);
}

#[test]
fn debug_should_show_name_size_and_position() {
    let mut archive = archive_with(&[("A.TXT", b"12345")]);
    let mut file = archive.entry(0).unwrap();
    file.seek(SeekFrom::Start(2)).unwrap();
    assert_eq!(
        format!("{file:?}"),
        "File { name: \"A.TXT\", size: 5, pos: 2 }"
    );
}

#[test]
fn read_should_propagate_seek_errors_when_repositioning() {
    let data = build_grp(&[("A.TXT", b"12345")]);
    // Failing Start-seeks: the open succeeds (it only seeks from the end),
    // but the first read must reposition the reader and fails.
    let mut archive = flaky_archive(FlakyReader::new(data).failing_start_seeks());
    let mut file = archive.entry(0).unwrap();
    let err = file.read(&mut [0u8; 10]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Other);
}

#[test]
fn read_should_propagate_read_errors() {
    let data = build_grp(&[("A.TXT", b"12345")]);
    // The open's length probe seeks from the end (position 48); the first
    // read seeks back to the data start (32) and is the only read that
    // fails.
    let mut archive = flaky_archive(FlakyReader::new(data).failing_reads_at(32));
    let mut file = archive.entry(0).unwrap();
    let err = file.read(&mut [0u8; 10]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Other);
}

#[test]
fn read_should_return_zero_when_buffer_is_empty() {
    let mut archive = archive_with(&[("A.TXT", b"12345")]);
    let mut file = archive.entry(0).unwrap();
    // Positioned at 0, before the end, with an empty buffer.
    assert_eq!(file.read(&mut []).unwrap(), 0);
}

#[test]
fn read_should_serve_sequential_reads_without_keeping_seeking() {
    let mut archive = archive_with(&[("A.TXT", b"12345")]);
    let mut file = archive.entry(0).unwrap();
    let mut buf = [0u8; 2];
    let mut got = Vec::new();
    loop {
        let n = file.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, b"12345");
}

#[test]
fn opening_should_propagate_seek_errors_when_reader_supports_end_seeks() {
    let data = build_grp(&[("A.TXT", b"12345")]);
    let err = flaky_archive_err(FlakyReader::new(data).failing_end_seeks());
    assert_matches!(err, Error::Io(_));
}

#[test]
fn read_should_work_when_end_seeks_fail() {
    // End seeks fail, but reads only reposition with Start seeks, so the
    // proxy serves the entry normally.
    let mut reader = FlakyReader::new(build_grp(&[("A.TXT", b"12345")])).failing_end_seeks();
    let mut file = File::new(&mut reader, "A.TXT".to_owned(), 32, 5);
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"12345");
}

#[test]
fn read_should_serve_sequential_and_empty_reads_on_flaky_reader() {
    // A non-failing FlakyReader: looped small reads exercise the
    // already-positioned path, and an empty buffer must return zero.
    let mut archive = flaky_archive(FlakyReader::new(build_grp(&[("A.TXT", b"12345")])));
    let mut file = archive.entry(0).unwrap();
    let mut buf = [0u8; 2];
    let mut got = Vec::new();
    loop {
        let n = file.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, b"12345");
    file.rewind().unwrap();
    assert_eq!(file.read(&mut []).unwrap(), 0);
}

#[test]
fn open_should_succeed_when_position_seeks_fail() {
    // Opening only uses End seeks, so a position-failing reader still
    // opens fine — and its Seek impl sees non-Current(0) positions.
    let data = build_grp(&[("A.TXT", b"12345")]);
    let mut archive = flaky_archive(FlakyReader::new(data).failing_position_seeks());
    let mut file = archive.entry(0).unwrap();
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"12345");
}

fn flaky_archive_err(reader: FlakyReader) -> Error {
    Archive::new(reader).unwrap_err()
}
