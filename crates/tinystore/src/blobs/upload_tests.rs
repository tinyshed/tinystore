use super::*;
use crate::blobs::fixture::{bytes, fixture};

#[test]
fn an_upload_appears_whole_at_its_commit_and_never_before() {
    let f = fixture();
    let reports = f.files("reports");
    let body = bytes(3 << 20);
    let mut upload = reports.key("2026-10/report.csv").content_type("text/csv").upload().unwrap();
    upload.write_all(&body[..1 << 20]).unwrap();
    assert_eq!(reports.head("2026-10/report.csv").unwrap(), None);
    assert_eq!(f.uploads().len(), 1);
    upload.write_all(&body[1 << 20..]).unwrap();
    let info = upload.commit().unwrap();
    assert_eq!((info.size, info.content_type.as_str()), (body.len() as u64, "text/csv"));
    assert_eq!(reports.head("2026-10/report.csv").unwrap(), Some(info));
    assert_eq!(reports.get("2026-10/report.csv").unwrap().unwrap().read_all().unwrap(), body);
    assert!(f.uploads().is_empty());
    assert_eq!(f.objects().len(), 1);
}

#[test]
fn a_small_upload_stays_inline() {
    let f = fixture();
    let mut upload = f.files("docs").upload("a").unwrap();
    upload.write_all(&bytes(INLINE)).unwrap();
    upload.commit().unwrap();
    assert!(f.objects().is_empty());
    assert_eq!(f.rows("bodies"), 1);
}

#[test]
fn an_upload_aborted_or_dropped_leaves_nothing() {
    let f = fixture();
    let docs = f.files("docs");
    let mut aborted = docs.upload("a").unwrap();
    aborted.write_all(&bytes(1 << 20)).unwrap();
    aborted.abort();
    {
        let mut dropped = docs.upload("b").unwrap();
        dropped.write_all(&bytes(1 << 20)).unwrap();
    }
    assert!(f.uploads().is_empty());
    assert!(f.objects().is_empty());
    assert_eq!((docs.head("a").unwrap(), docs.head("b").unwrap()), (None, None));
    assert_eq!(f.rows("contents"), 0);
}

#[test]
fn a_body_that_disagrees_with_its_size_is_refused_and_leaves_nothing() {
    let f = fixture();
    let docs = f.files("docs");
    for (declared, body) in [(10, bytes(9)), (10, bytes(11)), (INLINE as u64 * 2, bytes(INLINE * 2 - 1))] {
        let error = docs.key("a").size(declared).put(&body).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{declared} for {}: {error}", body.len());
    }
    assert_eq!(docs.head("a").unwrap(), None);
    assert!(f.uploads().is_empty());
    assert!(f.objects().is_empty());
}

#[test]
fn a_file_past_its_bound_is_refused_and_leaves_nothing() {
    let f = fixture();
    let docs = f.store.files("docs").max_file_size(1 << 20).open().unwrap();
    let kept = docs.put("a", bytes(1 << 20)).unwrap();
    let error = docs.put("a", bytes((1 << 20) + 1)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    let mut upload = docs.upload("a").unwrap();
    upload.write_all(&bytes(1 << 20)).unwrap();
    assert!(upload.write_all(b"one more").is_err());
    let error = upload.commit().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Closed, "an upload a failed write ended: {error}");
    let error = docs.key("b").size(2 << 20).upload().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "a declared size is checked before the first byte: {error}");
    assert_eq!(docs.head("a").unwrap(), Some(kept), "nothing stored is removed to make room");
    assert!(f.uploads().is_empty());
    assert_eq!(f.objects().len(), 1);
}

#[test]
fn a_file_that_would_leave_less_free_disk_than_the_store_keeps_is_refused_and_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let options = crate::Options { keep_free: u64::MAX / 2, background: false, ..crate::Options::default() };
    let store = crate::Store::open(dir.path(), options).unwrap();
    let docs = store.files("docs").open().unwrap();
    docs.put("small", bytes(INLINE)).unwrap();
    let error = docs.put("large", bytes(INLINE + 1)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    let mut upload = docs.upload("streamed").unwrap();
    upload.write_all(&bytes(INLINE)).unwrap();
    assert!(upload.write_all(b"one more").is_err(), "the first byte past inline asks the disk");
    assert_eq!((docs.head("large").unwrap(), docs.head("streamed").unwrap()), (None, None));
    assert_eq!(std::fs::read_dir(dir.path().join("blobs/uploads")).unwrap().count(), 0);
    let objects = dir.path().join("blobs/objects");
    let stored = std::fs::read_dir(objects).unwrap().flat_map(|fan| std::fs::read_dir(fan.unwrap().path()).unwrap());
    assert_eq!(stored.count(), 0, "no byte reached objects/");
}
