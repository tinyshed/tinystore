use super::*;
use crate::ErrorKind;

fn zstd() -> Zstd {
    Zstd::windowed(3, 13)
}

fn points(count: usize, from: i64) -> Vec<Point> {
    (0..count).map(|index| Point { at: from + index as i64 * 1000, value: (index % 13) as f64 * 0.5 }).collect()
}

fn read_all(series: i64, bytes: &[u8], extent: Extent) -> Vec<Point> {
    let chunks = parse(series, extent, bytes).unwrap();
    let mut out = Vec::new();
    decode(&mut zstd(), &chunks, i64::MIN, i64::MAX, &mut out).unwrap();
    out
}

fn same(a: &[Point], b: &[Point]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.at == b.at && a.value.to_bits() == b.value.to_bits())
}

#[test]
fn a_head_reads_back_in_chunks_of_240() {
    let written = points(777, 1_000);
    let bytes = encode(&mut zstd(), 7, &[], &written).unwrap();
    let extent = Extent::of(&written);
    let chunks = parse(7, extent, &bytes).unwrap();
    assert_eq!(chunks.iter().map(|chunk| chunk.head.count).collect::<Vec<_>>(), [240, 240, 240, 57]);
    assert!(same(&read_all(7, &bytes, extent), &written));
    assert!(encode(&mut zstd(), 7, &[], &[]).unwrap().is_empty());
}

#[test]
fn untouched_head_chunks_keep_their_bytes() {
    let written = points(500, 0);
    let bytes = encode(&mut zstd(), 7, &[], &written).unwrap();
    let chunks = parse(7, Extent::of(&written), &bytes).unwrap();
    let later = points(10, 500_000);
    let kept = kept_before(&chunks, later[0].at);
    assert_eq!(kept, 2, "the two full chunks end before the write");
    let mut rest = Vec::new();
    decode(&mut zstd(), &chunks[kept..], i64::MIN, i64::MAX, &mut rest).unwrap();
    let again = encode(&mut zstd(), 7, &chunks[..kept], &merge(&rest, &later)).unwrap();
    let whole: Vec<Point> = written.iter().chain(&later).copied().collect();
    assert!(same(&read_all(7, &again, Extent::of(&whole)), &whole));
    // a version and a count of two bytes, then the kept chunks as they were
    let copied = 3..3 + chunks[0].stored.len() + chunks[1].stored.len();
    assert_eq!(again[copied.clone()], bytes[copied], "the kept chunks are copied");
    // a write behind a full chunk's end decodes it again
    assert_eq!(kept_before(&chunks, 100_000), 0);
}

#[test]
fn a_head_reads_only_as_its_own_series_and_extent() {
    let written = points(300, 0);
    let bytes = encode(&mut zstd(), 7, &[], &written).unwrap();
    let extent = Extent::of(&written);
    assert_eq!(parse(8, extent, &bytes).unwrap_err().kind(), ErrorKind::Corrupt, "another series");
    for other in
        [Extent { count: 299, ..extent }, Extent { start: 1, ..extent }, Extent { end: extent.end + 1, ..extent }]
    {
        assert_eq!(parse(7, other, &bytes).unwrap_err().kind(), ErrorKind::Corrupt, "{other:?}");
    }
    for place in 0..bytes.len() {
        let mut broken = bytes.clone();
        broken[place] ^= 0x01;
        assert!(parse(7, extent, &broken).is_err(), "byte {place}");
    }
}

#[test]
fn a_read_decodes_only_the_chunks_its_range_touches() {
    let written = points(600, 0);
    let bytes = encode(&mut zstd(), 1, &[], &written).unwrap();
    let chunks = parse(1, Extent::of(&written), &bytes).unwrap();
    // chunks hold [0, 239 000], [240 000, 479 000], [480 000, 599 000]
    assert_eq!(samples_within(&chunks, 250_000, 260_000), 240);
    assert_eq!(samples_within(&chunks, 200_000, 260_000), 480);
    let mut out = Vec::new();
    decode(&mut zstd(), &chunks, 479_500, 480_001, &mut out).unwrap();
    assert_eq!((out.len(), out[0].at), (120, 480_000));
}

#[test]
fn a_merge_keeps_the_incoming_value_of_a_time_given_twice() {
    let at = |at: i64, value: f64| Point { at, value };
    let merged = merge(&[at(10, 1.0), at(11, 1.0), at(13, 1.0)], &[at(11, 2.0), at(12, 2.0), at(14, 2.0)]);
    let read: Vec<(i64, f64)> = merged.iter().map(|point| (point.at, point.value)).collect();
    assert_eq!(read, [(10, 1.0), (11, 2.0), (12, 2.0), (13, 1.0), (14, 2.0)]);
}

#[test]
fn a_head_past_its_bound_is_a_limit() {
    let error = encode(&mut zstd(), 1, &[], &points(MOST_SAMPLES + 1, 0)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
}
