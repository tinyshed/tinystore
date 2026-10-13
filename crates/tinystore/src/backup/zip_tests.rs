use std::io::{Cursor, Read};

use super::*;

/// A zip of `files` written with ZIP64's fields from `wide_from` bytes.
fn zipped(files: &[(&str, Vec<u8>)], wide_from: u64) -> Vec<u8> {
    let mut writer = Writer::new(Vec::new());
    writer.wide_from = wide_from;
    for (name, bytes) in files {
        writer.add(name, bytes.len() as u64, bytes.as_slice(), |_| {}).unwrap();
    }
    writer.finish().unwrap().0
}

fn read_back(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut cursor = Cursor::new(zip);
    let entries = entries(&mut cursor).unwrap();
    let mut files = Vec::new();
    for entry in &entries {
        let mut bytes = Vec::new();
        open(&mut cursor, entry).unwrap().read_to_end(&mut bytes).unwrap();
        files.push((entry.name.clone(), bytes));
    }
    files
}

fn files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("empty", Vec::new()),
        ("kv.db", b"SQLite format 3\0".repeat(9)),
        ("sql/app.db", (0..200_000).map(|at| at as u8).collect()),
    ]
}

#[test]
fn a_zip_reads_back_every_file_it_holds() {
    let files = files();
    let back = read_back(&zipped(&files, MAX32));
    let want: Vec<(String, Vec<u8>)> = files.into_iter().map(|(name, bytes)| (name.to_owned(), bytes)).collect();
    assert_eq!(back, want);
}

#[test]
fn zip64s_fields_read_back_where_a_size_or_an_offset_is_past_its_field() {
    let files = files();
    let zip = zipped(&files, 100);
    let mut cursor = Cursor::new(zip.as_slice());
    let offsets: Vec<u64> = entries(&mut cursor).unwrap().iter().map(|entry| entry.offset).collect();
    assert!(offsets[2] > 100, "the last file lies past the field, {offsets:?}");
    assert!(zip.windows(4).any(|bytes| bytes == END64.to_le_bytes()), "a ZIP64 end");
    assert_eq!(read_back(&zip).len(), 3);
    assert_eq!(read_back(&zip)[2].1, files[2].1);
}

#[test]
fn a_changed_byte_fails_its_crc_and_a_zip_cut_short_reads_as_none() {
    let mut zip = zipped(&files(), MAX32);
    let at = zip.windows(16).position(|bytes| bytes == b"SQLite format 3\0").unwrap() + 3;
    zip[at] ^= 1;
    let mut cursor = Cursor::new(zip.as_slice());
    let entries = entries(&mut cursor).unwrap();
    let error = open(&mut cursor, &entries[1]).unwrap().read_to_end(&mut Vec::new()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
    let cut = &zip[..zip.len() - 10];
    assert!(super::entries(&mut Cursor::new(cut)).is_err());
    assert!(super::entries(&mut Cursor::new(b"not a zip".as_slice())).is_err());
}

#[test]
fn a_file_that_changes_as_it_is_read_is_refused() {
    let mut writer = Writer::new(Vec::new());
    let error = writer.add("grew", 3, b"four".as_slice(), |_| {}).unwrap_err();
    assert!(error.to_string().contains("changed as it was read"), "{error}");
}
