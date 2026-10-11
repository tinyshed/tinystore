use super::*;
use crate::metrics::codec::Point;
use crate::metrics::series::Kind;
use crate::metrics::summary::summarize;
use crate::metrics::values::{self, UNTRIED};

fn zstd() -> Zstd {
    Zstd::windowed(3, 13)
}

/// A block of `values` from `start`, a second apart, its body encoded.
fn block(start: i64, values: &[f64], payload: i64) -> Block {
    let points: Vec<Point> =
        values.iter().zip(0..).map(|(&value, index)| Point { at: start + index * 1000, value }).collect();
    let times = Times::of(&points);
    let (body, _) = values::encode(&mut zstd(), &points, &times, UNTRIED).unwrap();
    let body = body.unwrap_or_default();
    let summary = summarize(&points, Kind::Counter);
    let payload = if body.len() > INLINE { payload } else { 0 };
    let inline = if body.len() > INLINE { Vec::new() } else { body.clone() };
    Block { times, first: values[0], summary, body_len: body.len(), payload, body: inline }
}

fn group() -> Group {
    let blocks = vec![
        block(0, &[5.0; 240], 0),
        block(300_000, &(0..240).map(|n| f64::from(n * n)).collect::<Vec<_>>(), 17),
        block(600_000, &[1.0, 2.0, 2.0, 7.0], 0),
    ];
    Group { series: 9, start: 0, end: blocks[2].times.end, clock: 4, live: 0b111, blocks }
}

fn row(group: &Group, directory: Vec<u8>) -> Row {
    Row { series: group.series, start: group.start, end: group.end, clock: group.clock, directory }
}

#[test]
fn a_directory_reads_back_every_block_it_describes() {
    let group = group();
    let directory = group.encode(&mut zstd()).unwrap();
    let clock: Vec<Times> = group.blocks.iter().map(|block| block.times.clone()).collect();
    let read = Group::decode(&mut zstd(), &row(&group, directory), clock).unwrap();
    assert_eq!(read.live, 0b111);
    for (wrote, read) in group.blocks.iter().zip(&read.blocks) {
        assert_eq!(
            (wrote.first.to_bits(), wrote.body_len, wrote.payload),
            (read.first.to_bits(), read.body_len, read.payload)
        );
        assert_eq!(wrote.body, read.body);
        assert_eq!(wrote.summary.exact, read.summary.exact);
        assert_eq!(
            (wrote.summary.last, wrote.summary.resets, wrote.summary.valid),
            (read.summary.last, read.summary.resets, read.summary.valid)
        );
    }
    assert!(read.blocks[1].external() && !read.blocks[2].external());
}

#[test]
fn a_directory_reads_only_as_its_own_groups() {
    let group = group();
    let directory = group.encode(&mut zstd()).unwrap();
    let clock: Vec<Times> = group.blocks.iter().map(|block| block.times.clone()).collect();
    for other in [
        Row { series: 10, ..row(&group, directory.clone()) },
        Row { clock: 5, ..row(&group, directory.clone()) },
        Row { end: group.end + 1, ..row(&group, directory.clone()) },
    ] {
        assert!(Group::decode(&mut zstd(), &other, clock.clone()).is_err(), "{other:?}");
    }
    assert!(
        Group::decode(&mut zstd(), &row(&group, directory.clone()), clock[..2].to_vec()).is_err(),
        "a clock of two blocks"
    );
    for place in 0..directory.len() {
        let mut broken = directory.clone();
        broken[place] ^= 0x08;
        assert!(Group::decode(&mut zstd(), &row(&group, broken), clock.clone()).is_err(), "byte {place}");
    }
}

#[test]
fn a_payload_two_blocks_name_is_refused() {
    let mut group = group();
    group.blocks[0] = block(0, &(0..240).map(f64::from).map(f64::sqrt).collect::<Vec<_>>(), 17);
    let directory = group.encode(&mut zstd()).unwrap();
    let clock: Vec<Times> = group.blocks.iter().map(|block| block.times.clone()).collect();
    let error = Group::decode(&mut zstd(), &row(&group, directory), clock).unwrap_err();
    assert!(error.to_string().contains("a payload two blocks name"), "{error}");
}
