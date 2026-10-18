use rusqlite::params;

use crate::ErrorKind;
use crate::metrics::directory::Row;
use crate::metrics::fixture::{Fixture, MINUTE, SECOND, bits, fixture};
use crate::metrics::publish::read_group;
use crate::metrics::{Labels, Series, maintain};

/// Values a block cannot keep inline, so that packing gives them payloads.
fn noisy(index: usize) -> f64 {
    (index * 7919 % 1009) as f64 * 0.37
}

/// Two series of `cpu`, `a` and `b`, of 841 samples a quarter of a minute
/// apart, ending at the clock.
fn two(f: &Fixture) -> Series {
    let a = f.gauge("cpu", 841, 15 * SECOND, noisy).label("host", "a");
    let b = f.gauge("cpu", 841, 15 * SECOND, |index| noisy(index + 3)).label("host", "b");
    f.metrics().ingest(&[a, b.clone()]).unwrap();
    b
}

/// Changes one byte of a row's blob, as a disk that fails might.
fn flip(f: &Fixture, table: &'static str, column: &'static str, id: i64) {
    f.engine()
        .file
        .write(0, move |tx| {
            let read = format!("select {column} from {table} where rowid = ?1");
            let mut bytes: Vec<u8> = tx.query_row(&read, [id], |row| row.get(0)).unwrap();
            let middle = bytes.len() / 2;
            bytes[middle] ^= 0x10;
            tx.execute(&format!("update {table} set {column} = ?2 where rowid = ?1"), params![id, bytes]).unwrap();
            Ok(())
        })
        .unwrap();
}

/// The id of a payload of series `a`'s first group.
fn payload_of_a(f: &Fixture) -> i64 {
    let engine = f.engine();
    let mut zstd = engine.codecs.take();
    let sql = "select g.start_ts, g.end_ts, g.clock, g.directory, g.series_id from groups g
        join series s on s.id = g.series_id order by s.id, g.start_ts limit 1";
    engine
        .file
        .read(|connection| {
            let row = connection
                .query_row(sql, [], |row| {
                    Ok(Row {
                        start: row.get(0)?,
                        end: row.get(1)?,
                        clock: row.get(2)?,
                        directory: row.get(3)?,
                        series: row.get(4)?,
                    })
                })
                .unwrap();
            let group = read_group(connection, &mut zstd, &row)?;
            Ok(group.blocks.iter().find(|block| block.external()).unwrap().payload)
        })
        .unwrap()
}

fn host(name: &str) -> Labels {
    Labels::from([("host".to_owned(), name.to_owned())])
}

#[test]
fn a_series_whose_bytes_changed_is_suspended_by_the_read_that_met_them_and_its_neighbours_go_on() {
    let f = fixture();
    let stats = f.metrics();
    let b = two(&f);
    maintain(&f.store).unwrap();
    flip(&f, "payloads", "body", payload_of_a(&f));
    let failed = stats.query().name("cpu").read().unwrap_err();
    assert_eq!(failed.kind(), ErrorKind::Corrupt);
    assert!(failed.to_string().starts_with("metrics: cpu{host=\"a\"}: a block from "), "{failed}");
    let damaged = stats.damaged().unwrap();
    assert_eq!((damaged.len(), damaged[0].name.as_str(), &damaged[0].labels), (1, "cpu", &host("a")));
    assert!(damaged[0].error.starts_with("a block from ") && damaged[0].error.ends_with("corrupt"), "{damaged:?}");
    assert_eq!(damaged[0].to_string(), "cpu{host=\"a\"}");
    // it takes no sample; what does not reach the bytes that changed still reads
    let refused = stats.ingest(&[Series::gauge("cpu").label("host", "a").sample(f.now(), 1.0)]).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Suspended);
    let recent = stats.query().name("cpu").filter("host", "a").since(10 * MINUTE).read().unwrap();
    assert_eq!(recent[0].samples().len(), 41);
    assert_eq!(bits(&stats.query().name("cpu").filter("host", "b").read().unwrap()[0]), bits(&b));
    // the store remembers it after a restart, and a delete takes it whole
    let f = f.reopen();
    let stats = f.metrics();
    assert_eq!(stats.damaged().unwrap().len(), 1);
    for damage in stats.damaged().unwrap() {
        assert!(stats.delete(&damage.name, &damage.labels).unwrap());
    }
    assert!(stats.damaged().unwrap().is_empty());
    assert_eq!(stats.query().name("cpu").read().unwrap().len(), 1);
    stats.ingest(&[Series::gauge("cpu").label("host", "a").sample(f.now(), 1.0)]).unwrap();
}

#[test]
fn packing_suspends_a_series_whose_head_changed_and_packs_the_rest() {
    let f = fixture();
    let stats = f.metrics();
    let b = two(&f);
    flip(&f, "heads", "body", 1);
    let done = maintain(&f.store).unwrap();
    assert_eq!((done.suspended, done.packed), (1, 840));
    let damaged = stats.damaged().unwrap();
    assert_eq!((damaged.len(), &damaged[0].labels), (1, &host("a")));
    assert_eq!(bits(&stats.query().name("cpu").filter("host", "b").read().unwrap()[0]), bits(&b));
    assert_eq!(maintain(&f.store).unwrap().suspended, 0, "said once");
    assert!(stats.delete("cpu", [("host", "a")]).unwrap());
    assert!(stats.damaged().unwrap().is_empty());
}

#[test]
fn a_deleted_series_whose_group_no_longer_reads_leaves_that_groups_values_in_the_file() {
    let f = fixture();
    let stats = f.metrics();
    two(&f);
    maintain(&f.store).unwrap();
    let (payloads, group) = (f.rows("payloads"), f.value("select min(id) from groups where series_id = 1"));
    flip(&f, "groups", "directory", group);
    assert_eq!(stats.query().name("cpu").read().unwrap_err().kind(), ErrorKind::Corrupt);
    assert!(stats.delete("cpu", [("host", "a")]).unwrap());
    assert_eq!(f.value("select count(*) from groups where series_id = 1"), 0);
    assert_eq!(f.rows("payloads"), payloads, "b's, and those the group that no longer reads named");
    assert_eq!(stats.query().name("cpu").read().unwrap().len(), 1);
}

#[test]
fn a_series_whose_head_no_longer_reads_is_suspended_and_the_others_expire() {
    let f = fixture();
    let stats = f.metrics();
    two(&f);
    let a = f.value("select min(series_id) from heads");
    flip(&f, "heads", "body", a);
    f.pass(31 * crate::metrics::fixture::DAY);
    maintain(&f.store).unwrap();
    let damaged = stats.damaged().unwrap();
    assert_eq!((damaged.len(), &damaged[0].labels), (1, &host("a")), "{damaged:?}");
    assert_eq!(f.value(&format!("select count(*) from heads where series_id != {a}")), 0, "b expired");
}
