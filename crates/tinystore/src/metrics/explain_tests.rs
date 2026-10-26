use std::time::Duration;

use super::*;
use crate::metrics::aggregate::Asking;
use crate::metrics::fixture::{HOUR, MINUTE, SECOND, fixture};
use crate::metrics::{Op, Series, maintain};

// contract: MET-06, MET-07
#[test]
fn a_plan_says_what_the_call_spends_and_whole_blocks_decode_nothing() {
    let f = fixture();
    let now = f.now();
    // eight hours of three hosts, packed an hour a block
    let hosts = ["a", "b", "c"].map(|host| f.gauge("cpu", 1921, 15 * SECOND, |index| index as f64).label("host", host));
    f.metrics().ingest(&hosts).unwrap();
    assert_eq!(maintain(&f.store).unwrap().blocks, 24);
    let hours = || f.metrics().query().name("cpu").from(now - 8 * HOUR).to(now);
    let hourly = hours().every(HOUR).explain(Op::Avg).unwrap();
    assert_eq!((hourly.series, hourly.decoded, &hourly.exceeds), (3, 0, &None), "every hour from its summary");
    let fine = hours().every(7 * MINUTE).explain(Op::Avg).unwrap();
    assert_eq!(fine.decoded, 3 * 1920, "a bucket finer than a block decodes it");
    let read = hours().explain(None).unwrap();
    assert_eq!(read.decoded, 3 * 1920);
    assert!(hourly.bytes < read.bytes, "a block answered from its summary leaves its payload in the file");
    // an hour that the range cuts decodes, the others still answer whole
    let cut = hours().from(now - 8 * HOUR + MINUTE).every(HOUR).explain(Op::Avg).unwrap();
    assert_eq!(cut.decoded, 3 * 240);
    // a first step begins at the last sample of the hour before the range, which its directory holds
    let stepped = hours().from(now - 7 * HOUR).every(HOUR).explain(Op::Delta).unwrap();
    assert_eq!(stepped.decoded, 0);
    let moved = hours().from(now - 7 * HOUR).every(HOUR).filter("host", "a").aggregate(Op::Delta).unwrap().groups;
    assert_eq!(moved[0].buckets.iter().map(|hour| hour.value.exact()).collect::<Vec<_>>(), [Some(240.0); 7]);
    // what each plan says is what its call spends
    let hour = Shape::of(Op::Avg, Asking { every: Some(HOUR), ..Asking::default() }).unwrap();
    let minutes = Shape::of(Op::Avg, Asking { every: Some(7 * MINUTE), ..Asking::default() }).unwrap();
    let asked = hours().asked().unwrap();
    for (plan, want) in [(&hourly, Want::Buckets(&hour)), (&fine, Want::Buckets(&minutes)), (&read, Want::Samples)] {
        let mut spent = Budget::default();
        f.engine().take(&asked, want, &mut spent).unwrap();
        let said = (plan.series as usize, plan.bytes as usize, plan.decoded as usize);
        assert_eq!(said, (spent.series, spent.bytes, spent.decoded), "{want:?}");
    }
    // and the aggregates it planned answer
    assert_eq!(hours().every(HOUR).aggregate(Op::Avg).unwrap().groups[0].buckets.len(), 8);
}

// contract: MET-07
#[test]
fn a_plan_names_the_bound_a_call_would_stop_at() {
    let f = fixture();
    let now = f.now();
    let series: Vec<Series> = (0..1001).map(|n| Series::gauge("wide").label("n", n).sample(now, 1.0)).collect();
    f.metrics().ingest(&series).unwrap();
    let plan = f.metrics().query().name("wide").explain(None).unwrap();
    assert_eq!(plan.exceeds, Some(Exceeds { limit: "series", wanted: 1001, bound: 1000 }));
    assert_eq!(plan.series, 1000, "it stops counting where the call stops");
    let error = f.metrics().query().name("wide").read().unwrap_err();
    assert_eq!(error.fact("limit"), Some("series"), "as the call says it");
    let invalid = f.metrics().query().name("wide").filter("n", "7").explain(Op::Increase).unwrap_err();
    assert_eq!(invalid.kind(), ErrorKind::Invalid, "an `op` a gauge has not, as the call refuses it");
    let nothing = f.metrics().query().name("nothing").since(HOUR).explain(Op::Count).unwrap();
    assert_eq!((nothing.series, nothing.bytes, nothing.decoded, nothing.exceeds), (0, 0, 0, None));
}

// contract: MET-07
#[test]
fn a_plan_counts_every_sample_a_call_would_decode_past_their_bound() {
    let f = fixture();
    let now = f.now();
    // 66 series of 16,000 samples a second apart, in their heads: 1,056,000 samples
    for n in 0..66_u32 {
        for (from, count) in [(16_000_u32, 10_000_u32), (6_000, 6_000)] {
            let mut series = Series::gauge("deep").label("n", n);
            series
                .extend((0..count).map(|index| (now - Duration::from_secs(u64::from(from - index)), f64::from(index))));
            f.metrics().ingest(&[series]).unwrap();
        }
    }
    let plan = f.metrics().query().name("deep").explain(None).unwrap();
    assert_eq!(plan.exceeds, Some(Exceeds { limit: "samples decoded", wanted: 1_056_000, bound: 1_048_576 }));
    assert_eq!((plan.series, plan.decoded), (66, 1_056_000));
    let error = f.metrics().query().name("deep").read().unwrap_err();
    assert_eq!((error.kind(), error.fact("limit")), (ErrorKind::Limit, Some("samples decoded")));
    let fits = f.metrics().query().name("deep").filter("n", "0").since(HOUR).explain(Op::Max).unwrap();
    assert_eq!(fits.exceeds, None);
}

// contract: MET-07
#[test]
fn a_plan_counts_the_samples_or_buckets_a_call_would_answer() {
    let f = fixture();
    // 100,001 samples a second apart, packed a minute a block but for the newest
    let first = f.now() - 100_000 * SECOND;
    for part in 0..=10_u32 {
        let mut series = Series::gauge("fine");
        let indices = part * 10_000..(part * 10_000 + 10_000).min(100_001);
        series.extend(indices.map(|index| (first + index * SECOND, f64::from(index))));
        f.metrics().ingest(&[series]).unwrap();
        maintain(&f.store).unwrap();
    }
    let answer = Some(Exceeds { limit: "answer", wanted: 100_001, bound: 100_000 });
    let all = || f.metrics().query().name("fine");
    let read = all().explain(None).unwrap();
    assert_eq!((read.decoded, &read.exceeds), (100_001, &answer));
    assert_eq!(all().read().unwrap_err().fact("limit"), Some("answer"), "as the call stops");
    let seconds = all().every(SECOND).explain(Op::Avg).unwrap();
    assert_eq!(seconds.exceeds, answer);
    assert_eq!(all().every(SECOND).aggregate(Op::Avg).unwrap_err().fact("limit"), Some("answer"));
    // a range the newest samples cut, and buckets of two seconds, fit
    let cut = f.metrics().query().name("fine").from(first + 30 * SECOND).explain(None).unwrap();
    assert_eq!(cut.exceeds, None);
    assert_eq!(
        f.metrics().query().name("fine").from(first + 30 * SECOND).read().unwrap().series[0].samples().len(),
        99_971
    );
    let near = f.metrics().query().name("fine").from(first + 30 * SECOND).to(f.now() - 30 * SECOND);
    assert_eq!(near.clone().explain(None).unwrap().exceeds, None);
    assert_eq!(all().every(2 * SECOND).explain(Op::Avg).unwrap().exceeds, None);
    assert_eq!(all().every(2 * SECOND).aggregate(Op::Avg).unwrap().groups[0].buckets.len(), 50_001);
}
