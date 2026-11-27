//! A read cut into sessions by a pause: in the order of the events' times
//! across streams, segments and heads, each session counted whole, cut by
//! the range said, and past its bounds refused.

use std::time::{Duration, SystemTime};

use serde_json::json;

use super::super::fixture::{DAY, Fixture, fixture, released, timed_in_turns};
use super::super::maintain::maintain;
use super::*;
use crate::ErrorKind;
use crate::records::{Events, NewEvent};

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(60 * 60);

/// An event of `userId` at `at`, named `name`.
fn used(user: i64, name: &str, at: SystemTime) -> NewEvent {
    NewEvent::new(name).at(at).fields(&json!({ "userId": user }))
}

/// Each session as its first event's minute after `start`, its user, its count
/// and whether it is partial.
fn shown(sessions: &[Session], start: SystemTime) -> Vec<(u64, i64, u64, bool)> {
    let minute = |at: SystemTime| at.duration_since(start).unwrap().as_secs() / 60;
    let user = |session: &Session| session.by["userId"].as_i64().unwrap_or(-1);
    sessions.iter().map(|session| (minute(session.from), user(session), session.count, session.partial)).collect()
}

/// Checks a cut: each session's events in the order of their times, each
/// within `gap` of the one before, and a pause past `gap` before the next
/// session of the same values; every event of the read in one session. A
/// session's events are checked where it keeps them all.
fn well_cut(sessions: &[Session], gap: Duration, events: u64) {
    assert_eq!(sessions.iter().map(|session| session.count).sum::<u64>(), events, "every event once");
    let mut last: HashMap<String, SystemTime> = HashMap::new();
    for session in sessions.iter() {
        let times: Vec<SystemTime> = session.events.iter().map(|event| event.at).collect();
        if times.len() as u64 == session.count {
            let within = |pair: &[SystemTime]| pair[0] <= pair[1] && pair[1].duration_since(pair[0]).unwrap() <= gap;
            assert!(times.windows(2).all(within), "{session:?}");
            assert_eq!((times.first(), times.last()), (Some(&session.from), Some(&session.to)), "{session:?}");
        }
        let by = serde_json::to_string(&session.by).unwrap();
        if let Some(before) = last.insert(by, session.to) {
            assert!(session.from.duration_since(before).unwrap() > gap, "a pause past the gap: {session:?}");
        }
    }
}

// contract: REC-23
#[test]
fn two_visits_ninety_minutes_apart_are_two_sessions_and_first_sessions_keeps_the_first() {
    let fixture = fixture();
    let activity = fixture.events("activity");
    let start = fixture.now() - 4 * HOUR;
    let visits = [(1, 0), (1, 10), (2, 15), (1, 25), (1, 115), (1, 120), (2, 200)];
    let made = visits.map(|(user, minute)| used(user, "open", start + minute * MINUTE));
    activity.append_all(made).unwrap();
    let cut = Sessions::by(["userId"]).gap(30 * MINUTE);
    let sessions = activity.list().from(start).sessions(&cut).unwrap();
    assert_eq!(shown(&sessions, start), [(0, 1, 3, true), (15, 2, 1, true), (115, 1, 2, false), (200, 2, 1, false)]);
    well_cut(&sessions, 30 * MINUTE, 7);
    // each person's first visit alone
    let first = activity.list().from(start).sessions(&cut.clone().first_sessions(1)).unwrap();
    assert_eq!(shown(&first, start), [(0, 1, 3, true), (15, 2, 1, true)]);
    // max ends a visit that long after its first event, pause or none
    let longest = activity.list().from(start).sessions(&cut.clone().max(20 * MINUTE)).unwrap();
    assert_eq!(
        shown(&longest, start).iter().map(|(at, user, count, _)| (*at, *user, *count)).collect::<Vec<_>>(),
        [(0, 1, 2), (15, 2, 1), (25, 1, 1), (115, 1, 2), (200, 2, 1)]
    );
}

// contract: REC-23
#[test]
fn a_session_counts_every_event_keeps_its_first_and_is_partial_where_the_range_cuts_it() {
    let fixture = fixture();
    let activity = fixture.events("activity");
    let start = fixture.now() - 2 * HOUR;
    activity.append_all((0..6).map(|n| used(7, &format!("step-{n}"), start + n * MINUTE))).unwrap();
    activity.append_all([used(8, "late", fixture.now() - 5 * MINUTE)]).unwrap();
    let cut = Sessions::by(["userId"]).gap(10 * MINUTE).max_events(2);
    let sessions = activity.list().from(start - HOUR).sessions(&cut).unwrap();
    let kept: Vec<&str> = sessions[0].events.iter().map(|event| event.name.as_str()).collect();
    assert_eq!((sessions[0].count, kept), (6, vec!["step-0", "step-1"]));
    assert_eq!((sessions[0].partial, sessions[1].partial), (false, true), "the last may go on past now");
    // a range that begins inside a session cuts it
    let cut_in = activity.list().from(start + 2 * MINUTE).to(start + HOUR).sessions(&cut).unwrap();
    assert_eq!(shown(&cut_in, start), [(2, 7, 4, true)]);
    let bare = activity.list().from(start - HOUR).sessions(&Sessions::by(["userId"]).gap(10 * MINUTE)).unwrap();
    assert!(bare.iter().all(|session| session.events.is_empty()));
}

// contract: REC-23
#[test]
fn sessions_are_cut_in_the_order_of_times_whatever_the_order_appended_streams_and_packing() {
    let fixture = fixture();
    let (web, api) = (fixture.events("web"), fixture.events("api"));
    let start = fixture.now() - 3 * HOUR;
    // appended late, behind events already packed: a session still follows the times
    web.append_all([used(1, "a", start), used(1, "c", start + 20 * MINUTE)]).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    api.append_all([used(1, "b", start + 10 * MINUTE), used(1, "d", start + 45 * MINUTE)]).unwrap();
    // two events of one time come as they arrived
    api.append_all([used(1, "e", start + 50 * MINUTE), used(1, "f", start + 50 * MINUTE)]).unwrap();
    let every = fixture.store.all_events().unwrap();
    let cut = Sessions::by(["userId"]).gap(30 * MINUTE).max_events(10);
    let sessions = every.list().from(start).sessions(&cut).unwrap();
    let names: Vec<&str> = sessions[0].events.iter().map(|event| event.name.as_str()).collect();
    assert_eq!((sessions.len(), names), (1, vec!["a", "b", "c", "d", "e", "f"]));
    // by the stream as well: web's two events, and api's apart by a pause past the gap
    let by_stream = every.list().from(start).sessions(&Sessions::by(["userId", "stream"]).gap(30 * MINUTE)).unwrap();
    let streams: Vec<(&Value, u64)> = by_stream.iter().map(|session| (&session.by["stream"], session.count)).collect();
    assert_eq!(streams, [(&json!("web"), 2), (&json!("api"), 1), (&json!("api"), 3)]);
}

// contract: REC-23
#[test]
fn sessions_past_their_bound_are_limit_and_a_cut_out_of_bounds_is_invalid() {
    let fixture = fixture();
    let activity = fixture.events("activity");
    let at = fixture.now() - HOUR;
    let people = (0..10_001).map(|user| used(user, "open", at + Duration::from_millis(user.unsigned_abs())));
    activity.append_all(people).unwrap();
    let cut = Sessions::by(["userId"]).gap(MINUTE);
    let refused = activity.list().since(DAY).sessions(&cut).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Limit, "{refused}");
    assert!(refused.to_string().contains("past the 10000 sessions one read holds"), "{refused}");
    assert_eq!(refused.fact("limit"), Some("sessions"));
    let fewer = activity.list().since(DAY).filter("userId", &(0..100).collect::<Vec<_>>()).sessions(&cut).unwrap();
    assert_eq!(fewer.len(), 100);
    for (wrong, why) in [
        (Sessions::by(["userId"]), "a gap"),
        (Sessions::by(Vec::<String>::new()).gap(MINUTE), "take by"),
        (cut.clone().first_sessions(0), "first 0"),
        (cut.clone().max_events(1001), "1001 events"),
        (cut.clone().max(Duration::ZERO), "max"),
    ] {
        let refused = activity.list().since(DAY).sessions(&wrong).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
        assert!(refused.to_string().contains(why), "{refused}");
    }
    let newest = activity.list().since(DAY).newest().sessions(&cut).unwrap_err();
    assert_eq!(newest.kind(), ErrorKind::Invalid, "{newest}");
}

// contract: REC-23, NOT-12
#[test]
fn what_v0_1_0_packed_is_cut_into_sessions_in_the_order_it_reads() {
    let fixture = released();
    let every = fixture.store.all_events().unwrap();
    let events = every.list().count().unwrap();
    let gap = 30 * MINUTE;
    let sessions = every.list().sessions(&Sessions::by(["stream"]).gap(gap).max_events(1000)).unwrap();
    well_cut(&sessions, gap, events);
    // what a page reads, one after another
    let read: Vec<(String, String)> =
        every.list().all().map(|event| event.unwrap()).map(|e| (e.stream, e.name)).collect();
    let mut cut: Vec<(String, String)> = Vec::new();
    for session in &sessions {
        cut.extend(session.events.iter().map(|event| (event.stream.clone(), event.name.clone())));
    }
    let by_stream = |pairs: &[(String, String)], stream: &str| {
        pairs.iter().filter(|(of, _)| of == stream).map(|(_, name)| name.clone()).collect::<Vec<_>>()
    };
    for stream in ["api", "web"] {
        assert_eq!(by_stream(&cut, stream), by_stream(&read, stream), "{stream}");
    }
}

/// 1,000,000 events of 8,000 people over a day, appended and packed: each
/// person's 125 events 5 visits of 25, a minute apart, 4 hours between them.
/// Each person's first visit is 8,000 sessions, within the 10,000 a read
/// holds; every visit, 40,000, is past it.
fn visits(fixture: &Fixture) -> Events {
    let activity = fixture.events("activity");
    let start = fixture.now() - DAY;
    for batch in 0..100 {
        let made = (0..10_000).map(|n| {
            let event = batch * 10_000 + n;
            let user = event % 8_000;
            let (visit, step) = ((event / 8_000) / 25, (event / 8_000) % 25);
            let at = start + Duration::from_secs((visit * 4 * 3600 + step * 60 + user % 3600) as u64);
            NewEvent::new("used").at(at).fields(&json!({ "userId": user }))
        });
        activity.append_all(made).unwrap();
    }
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    activity
}

#[test]
#[ignore = "a timing, run by hand"]
fn a_million_events_of_eight_thousand_people_cut_into_sessions_beside_their_count() {
    let fixture = fixture();
    let activity = visits(&fixture);
    let since = DAY + 2 * HOUR;
    let count = || activity.list().since(since).count().unwrap();
    let cut = Sessions::by(["userId"]).gap(30 * MINUTE).first_sessions(1);
    let first = || activity.list().since(since).sessions(&cut).unwrap().len() as u64;
    let kept = Sessions::by(["userId"]).gap(30 * MINUTE).first_sessions(1).max_events(5);
    let with_events = || activity.list().since(since).sessions(&kept).unwrap().len() as u64;
    println!("1000000 events of 8000 people, 5 visits each, the median of 7 passes in turns");
    timed_in_turns(
        7,
        &[
            ("a count of the range", &count),
            ("each person's first visit", &first),
            ("each person's first visit with its first 5 events", &with_events),
        ],
    );
}
