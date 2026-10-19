use std::time::Duration;

use super::super::fixture::{DAY, fixture};
use crate::ErrorKind;

#[test]
fn a_stream_nobody_gave_a_keep_keeps_fourteen_days() {
    let fixture = fixture();
    fixture.events("api");
    assert_eq!(fixture.records().stream("api").unwrap().keeps(), 14 * DAY);
}

// contract: REC-09
#[test]
fn a_stream_keeps_what_an_open_gave_it_across_runs_and_an_open_that_gives_none_changes_nothing() {
    let fixture = fixture();
    fixture.store.events("audit").keep(365 * DAY).open().unwrap();
    fixture.events("audit");
    assert_eq!(fixture.records().stream("audit").unwrap().keeps(), 365 * DAY);

    let fixture = fixture.reopen();
    fixture.events("audit");
    assert_eq!(fixture.records().stream("audit").unwrap().keeps(), 365 * DAY);
    // a new run may say another: the code changed
    fixture.store.events("audit").keep(30 * DAY).open().unwrap();
    assert_eq!(fixture.reopen().records().stream("audit").unwrap().keeps(), 30 * DAY);
}

// contract: REC-09
#[test]
fn another_keep_while_the_store_is_open_is_invalid_and_names_both() {
    let fixture = fixture();
    fixture.store.events("api").keep(30 * DAY).open().unwrap();
    fixture.store.events("api").keep(30 * DAY).open().unwrap();
    let refused = fixture.store.events("api").keep(365 * DAY).open().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert_eq!(refused.to_string(), "events api: keep 365d, where the stream is open with keep 30d: invalid");
    assert_eq!(fixture.records().stream("api").unwrap().keeps(), 30 * DAY);
}

#[test]
fn a_stream_has_a_name_as_a_bucket_has_and_keeps_for_more_than_no_time() {
    let fixture = fixture();
    for name in ["", "Api", "a b", "-a", &"a".repeat(65)] {
        assert_eq!(fixture.store.events(name).open().unwrap_err().kind(), ErrorKind::Invalid, "{name:?}");
    }
    assert_eq!(fixture.store.events("api").keep(Duration::ZERO).open().unwrap_err().kind(), ErrorKind::Invalid);
    let under_a_millisecond = fixture.store.events("api").keep(Duration::from_micros(500)).open().unwrap_err();
    assert_eq!(under_a_millisecond.kind(), ErrorKind::Invalid, "it would be kept as none");
    fixture.store.events("a-1_b").open().unwrap();
    assert_eq!(fixture.rows("streams"), 1);
}
