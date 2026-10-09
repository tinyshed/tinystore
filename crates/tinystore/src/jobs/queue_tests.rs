//! Adding, setting, updating, cancelling and finding jobs.

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::fixture::{DAY, HOUR, MINUTE, SECOND, fixture};
use super::{Filter, Queue, State};
use crate::{ErrorKind, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Note {
    text: String,
}

fn note(text: &str) -> Note {
    Note { text: text.to_owned() }
}

/// Runs what is due and answers the texts of the jobs that ran, in order.
fn ran(queue: &Queue<Note>) -> Vec<String> {
    let seen = Mutex::new(Vec::new());
    queue
        .concurrency(1)
        .run_due(|note: Note, _run| -> Result<()> {
            seen.lock().unwrap().push(note.text);
            Ok(())
        })
        .unwrap();
    seen.into_inner().unwrap()
}

#[test]
fn a_job_runs_at_its_time_and_not_before() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    assert!(notes.delay(HOUR).add(&note("later")).unwrap());
    assert!(notes.add(&note("now")).unwrap());
    assert_eq!(ran(&notes), ["now"]);
    f.clock.advance(HOUR - SECOND);
    assert!(ran(&notes).is_empty());
    f.clock.advance(SECOND);
    assert_eq!(ran(&notes), ["later"]);
    assert!(ran(&notes).is_empty(), "a done job leaves the queue");
}

#[test]
fn an_added_job_survives_a_reopen() {
    let mut f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    notes.add(&note("kept")).unwrap();
    f.reopen();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    assert_eq!(ran(&notes), ["kept"]);
}

#[test]
fn jobs_run_in_the_order_of_their_time_equal_times_in_the_order_added() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    notes.delay(2 * SECOND).add(&note("third")).unwrap();
    notes.delay(SECOND).add(&note("first")).unwrap();
    notes.delay(SECOND).add(&note("second")).unwrap();
    f.clock.advance(2 * SECOND);
    assert_eq!(ran(&notes), ["first", "second", "third"]);
}

#[test]
fn add_adds_only_to_an_id_that_is_free() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").attempts(1).open().unwrap();
    assert!(notes.id("a").delay(HOUR).add(&note("first")).unwrap());
    assert!(!notes.id("a").add(&note("second")).unwrap(), "a scheduled job keeps its id");
    assert_eq!(notes.get("a").unwrap().unwrap().value, note("first"), "and its value");
    f.clock.advance(HOUR);
    assert_eq!(ran(&notes), ["first"]);
    assert!(notes.id("a").add(&note("again")).unwrap(), "a done job leaves its id free");

    let failing = notes.concurrency(1).run_due(|_: Note, _run| -> Result<(), String> { Err("down".into()) });
    assert_eq!(failing.unwrap(), 1);
    assert_eq!(notes.get("a").unwrap().unwrap().state, State::Failed);
    assert!(!notes.id("a").add(&note("after the failure")).unwrap(), "a failed job is kept to be looked at");
}

#[test]
fn add_of_an_id_whose_job_runs_adds_nothing() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    notes.id("a").add(&note("running")).unwrap();
    let added = Mutex::new(None);
    notes
        .run_due(|_: Note, _run| -> Result<()> {
            *added.lock().unwrap() = Some(notes.id("a").add(&note("while it runs"))?);
            Ok(())
        })
        .unwrap();
    assert_eq!(*added.lock().unwrap(), Some(false));
    assert!(ran(&notes).is_empty());
}

#[test]
fn dedupe_keeps_a_done_id_taken_until_its_span_passes() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").dedupe(HOUR).open().unwrap();
    pushes.id("m1:u1").add(&note("push")).unwrap();
    assert_eq!(ran(&pushes), ["push"]);
    assert!(!pushes.id("m1:u1").add(&note("push")).unwrap(), "an id runs once in the hour");
    let done = pushes.get("m1:u1").unwrap().unwrap();
    assert_eq!((done.state, done.value), (State::Done, note("push")));
    assert!(done.last_run.is_some());

    f.clock.advance(HOUR);
    assert_eq!(super::maintain(&f.store).unwrap().done, 1);
    assert!(pushes.id("m1:u1").add(&note("push")).unwrap());
}

#[test]
fn set_makes_the_ids_job_this_value_at_this_time_whatever_it_was() {
    let f = fixture();
    let checks = f.store.queue::<Note>("checks").attempts(1).open().unwrap();
    checks.id("backup").delay(25 * HOUR).set(&note("ping 1")).unwrap();
    f.clock.advance(24 * HOUR);
    checks.id("backup").delay(25 * HOUR).set(&note("ping 2")).unwrap();
    f.clock.advance(24 * HOUR);
    assert!(ran(&checks).is_empty(), "each ping moved the alarm on");
    f.clock.advance(HOUR);
    assert_eq!(ran(&checks), ["ping 2"]);

    checks.set("failing", &note("one")).unwrap();
    checks.run_due(|_: Note, _run| -> Result<(), &str> { Err("down") }).unwrap();
    assert_eq!(checks.get("failing").unwrap().unwrap().state, State::Failed);
    checks.set("failing", &note("two")).unwrap();
    let again = checks.get("failing").unwrap().unwrap();
    assert_eq!(
        (again.state, again.attempt, again.value),
        (State::Waiting, 0, note("two")),
        "a failed job starts again"
    );
}

#[test]
fn set_of_a_running_job_runs_it_once_more_with_its_value() {
    let f = fixture();
    let indexing = f.store.queue::<Note>("indexing").open().unwrap();
    indexing.set("note:7", &note("draft")).unwrap();
    let seen = Mutex::new(Vec::new());
    let index = |note: Note, _run: &super::Run| -> Result<()> {
        if note.text == "draft" {
            indexing.set("note:7", &note_of("edited"))?;
            indexing.set("note:7", &note_of("edited twice"))?;
        }
        seen.lock().unwrap().push(note.text);
        Ok(())
    };
    indexing.run_due(index).unwrap();
    assert_eq!(*seen.lock().unwrap(), ["draft", "edited twice"], "two sets while it ran ask one run more");
}

fn note_of(text: &str) -> Note {
    note(text)
}

#[test]
fn update_changes_only_a_job_that_has_not_started() {
    let f = fixture();
    let later = f.store.queue::<Note>("send-later").open().unwrap();
    assert!(!later.update("missing", &note("x")).unwrap(), "there was no job");
    later.id("m1").delay(HOUR).add(&note("hello")).unwrap();
    assert!(later.id("m1").delay(2 * HOUR).update(&note("hello, edited")).unwrap());
    let job = later.get("m1").unwrap().unwrap();
    assert_eq!((job.value.text.as_str(), job.at), ("hello, edited", f.now() + 2 * HOUR));
    assert!(later.update("m1", &note("text alone")).unwrap(), "an update without a time keeps it");
    assert_eq!(later.get("m1").unwrap().unwrap().at, f.now() + 2 * HOUR);

    f.clock.advance(2 * HOUR);
    let too_late = Mutex::new(None);
    later
        .run_due(|_: Note, _run| -> Result<()> {
            *too_late.lock().unwrap() = Some(later.update("m1", &note("while it is sent"))?);
            Ok(())
        })
        .unwrap();
    assert_eq!(*too_late.lock().unwrap(), Some(false));
    assert!(!later.update("m1", &note("after it was sent")).unwrap());
}

#[test]
fn cancel_says_whether_there_was_a_job_and_tells_a_running_handler_to_stop() {
    let f = fixture();
    let later = f.store.queue::<Note>("send-later").attempts(1).open().unwrap();
    assert!(!later.cancel("missing").unwrap());
    later.id("m1").delay(HOUR).add(&note("scheduled")).unwrap();
    assert!(later.cancel("m1").unwrap());
    assert!(later.get("m1").unwrap().is_none());

    later.id("m2").add(&note("running")).unwrap();
    let stopped = Mutex::new(false);
    later
        .run_due(|_: Note, run| -> Result<()> {
            assert!(later.cancel("m2")?);
            *stopped.lock().unwrap() = run.stopped();
            Err(crate::Error::invalid("what a cancelled handler returns settles nothing"))
        })
        .unwrap();
    assert!(*stopped.lock().unwrap());
    assert!(later.get("m2").unwrap().is_none(), "the cancel took it, not a retry");

    later.id("m3").add(&note("failing")).unwrap();
    later.run_due(|_: Note, _run| -> Result<(), &str> { Err("down") }).unwrap();
    assert!(later.cancel("m3").unwrap(), "a failed job is cancelled too");
    assert!(later.get("m3").unwrap().is_none());
}

#[test]
fn get_says_where_a_job_is_and_how_many_run_before_it() {
    let f = fixture();
    let videos = f.store.queue::<Note>("videos").attempts(1).open().unwrap();
    for at in 0..3 {
        videos.add(&note(&format!("before {at}"))).unwrap();
    }
    videos.id("mine").add(&note("mine")).unwrap();
    videos.id("later").delay(HOUR).add(&note("later")).unwrap();
    let mine = videos.get("mine").unwrap().unwrap();
    assert_eq!((mine.state, mine.ahead), (State::Waiting, 3));
    assert_eq!(videos.get("later").unwrap().unwrap().state, State::Scheduled);

    let running = Mutex::new(None);
    videos
        .concurrency(1)
        .run_due(|note: Note, run| -> Result<()> {
            if note.text == "mine" {
                run.set_progress(&0.4)?;
                *running.lock().unwrap() = videos.get("mine")?;
                return Err(crate::Error::invalid("transcoding failed"));
            }
            Ok(())
        })
        .unwrap();
    let running = running.into_inner().unwrap().unwrap();
    assert_eq!((running.state, running.attempt, running.progress), (State::Running, 1, Some(serde_json::json!(0.4))));
    let failed = videos.get("mine").unwrap().unwrap();
    assert_eq!(failed.state, State::Failed);
    assert!(failed.error.unwrap().contains("transcoding failed"));
    assert!(failed.last_run.is_some());
}

#[test]
fn list_reads_the_ids_under_a_prefix_a_page_at_a_time() {
    let f = fixture();
    let later = f.store.queue::<Note>("send-later").open().unwrap();
    for id in ["chat:42:1", "chat:42:2", "chat:42:3", "chat:420:1", "chat:7:1"] {
        later.id(id).delay(HOUR).add(&note(id)).unwrap();
    }
    let first = later.list(&Filter::prefix("chat:42:").limit(2)).unwrap();
    let ids: Vec<_> = first.jobs.iter().map(|job| job.id.clone().unwrap()).collect();
    assert_eq!(ids, ["chat:42:1", "chat:42:2"]);
    let rest = later.list(&Filter::prefix("chat:42:").limit(2).after(first.next.unwrap())).unwrap();
    assert_eq!(rest.jobs.len(), 1);
    assert_eq!(rest.next, None);
    let all: Vec<_> = later.all(Filter::prefix("chat:4")).map(|job| job.unwrap().id.unwrap()).collect();
    assert_eq!(all.len(), 4, "a prefix is text: chat:4 meets chat 420 too");
}

#[test]
fn the_failed_jobs_list_the_last_failed_first() {
    let f = fixture();
    let emails = f.store.queue::<Note>("emails").attempts(1).open().unwrap();
    for text in ["a", "b", "c"] {
        emails.add(&note(text)).unwrap();
        emails.run_due(|_: Note, _run| -> Result<(), &str> { Err("bounced") }).unwrap();
        f.clock.advance(SECOND);
    }
    let failed = emails.list(&Filter::default().state(State::Failed)).unwrap();
    let texts: Vec<_> = failed.jobs.iter().map(|job| job.value.text.as_str()).collect();
    assert_eq!(texts, ["c", "b", "a"]);
}

#[test]
fn a_value_comes_back_as_the_json_it_went_in() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    let long = note(&"x".repeat(2000));
    notes.id("long").add(&long).unwrap();
    assert_eq!(notes.get("long").unwrap().unwrap().value, long, "a value past 512 bytes lives in a row of its own");
    assert_eq!(ran(&notes), [long.text]);

    let raw = f.store.queue::<serde_json::Value>("notes").open().unwrap();
    raw.id("plain").add(&serde_json::json!({ "text": "from another language" })).unwrap();
    assert_eq!(notes.get("plain").unwrap().unwrap().value, note("from another language"));
}

#[test]
fn a_value_that_no_longer_reads_fails_its_job_not_its_queue() {
    let f = fixture();
    let raw = f.store.queue::<serde_json::Value>("notes").open().unwrap();
    raw.id("old").add(&serde_json::json!({ "body": "an older shape" })).unwrap();
    raw.add(&serde_json::json!({ "text": "fine" })).unwrap();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    assert_eq!(ran(&notes), ["fine"]);
    let failed = raw.get("old").unwrap().unwrap();
    assert_eq!(failed.state, State::Failed);
    assert!(failed.error.unwrap().contains("no longer reads"));
}

#[test]
fn a_queue_past_max_waiting_refuses_the_next_job() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").max_waiting(2).open().unwrap();
    notes.add(&note("1")).unwrap();
    notes.add(&note("2")).unwrap();
    let error = notes.add(&note("3")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    assert!(error.to_string().starts_with("jobs queue notes"), "{error}");
}

#[test]
fn a_call_that_cannot_be_kept_is_invalid_and_says_why() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    let cases = [
        notes.id("").add(&note("x")),
        notes.at(f.now()).delay(HOUR).add(&note("x")),
        notes.delay(SECOND).every(MINUTE).add(&note("x")),
        notes.id("a").cron("0 9 * * *", "").add(&note("x")),
        notes.id("a").cron("0 9 * * *", "Mars/Olympus").add(&note("x")),
    ];
    for case in cases {
        assert_eq!(case.unwrap_err().kind(), ErrorKind::Invalid);
    }
    let error = notes.id("a").cron("0 9 * * *", "").add(&note("x")).unwrap_err();
    assert_eq!(
        error.to_string(),
        "jobs queue notes: id \"a\": cron \"0 9 * * *\": a cron needs a time zone, \"UTC\" among them: invalid"
    );
}

#[test]
fn a_name_keeps_its_kind_and_one_process_its_options() {
    let f = fixture();
    f.store.queue::<Note>("notes").open().unwrap();
    let error = f.store.queue::<Note>("notes").attempts(3).open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    let error = f.store.schedule("notes").every(HOUR).open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    let error = f.store.queue::<Note>("Notes!").open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
}

#[test]
fn a_failed_job_is_kept_then_removed() {
    let f = fixture();
    let emails = f.store.queue::<Note>("emails").attempts(1).keep(DAY).open().unwrap();
    emails.id("welcome").add(&note("hi")).unwrap();
    emails.run_due(|_: Note, _run| -> Result<(), &str> { Err("bounced") }).unwrap();
    assert_eq!(super::maintain(&f.store).unwrap().failed, 0);
    f.clock.advance(DAY);
    assert_eq!(super::maintain(&f.store).unwrap().failed, 1);
    assert!(emails.get("welcome").unwrap().is_none());
}

#[test]
fn an_id_a_done_job_left_behind_names_nothing_and_maintenance_drops_it() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    notes.id("a").add(&note("a")).unwrap();
    assert_eq!(ran(&notes), ["a"]);
    assert!(notes.get("a").unwrap().is_none());
    assert_eq!(super::maintain(&f.store).unwrap().ids, 1);
    assert_eq!(super::maintain(&f.store).unwrap().ids, 0);
}

#[test]
fn a_repeating_job_neither_overlaps_nor_piles_up() {
    let f = fixture();
    let probes = f.store.queue::<Note>("probes").open().unwrap();
    probes.id("probe:7").every(HOUR).set(&note("probe")).unwrap();
    let first = probes.get("probe:7").unwrap().unwrap();
    assert!(first.repeat.unwrap().starts_with("@every 1h +"));
    assert!(first.at > f.now() && first.at <= f.now() + HOUR, "its first run within the next interval");

    f.clock.advance(5 * HOUR);
    assert_eq!(ran(&probes), ["probe"], "a night down runs it once, not once a missed time");
    let next = probes.get("probe:7").unwrap().unwrap();
    assert!(next.at > f.now() && next.at <= f.now() + HOUR);
    assert_eq!((next.state, next.attempt), (State::Scheduled, 0));
    assert!(probes.cancel("probe:7").unwrap(), "only an id stops it");
}

#[test]
fn ids_repeating_together_run_each_at_a_phase_of_its_own() {
    let f = fixture();
    let probes = f.store.queue::<Note>("probes").open().unwrap();
    let mut times = Vec::new();
    for site in 0..20 {
        let id = format!("probe:{site}");
        probes.id(&id).every(Duration::from_secs(30)).set(&note(&id)).unwrap();
        times.push(probes.get(&id).unwrap().unwrap().at);
    }
    times.sort();
    times.dedup();
    assert!(times.len() > 10, "{} instants for 20 ids", times.len());
}

#[test]
fn a_cron_runs_on_its_zones_wall_clock() {
    let f = fixture();
    let digests = f.store.queue::<Note>("digests").open().unwrap();
    digests.id("user:42").cron("0 20 * * *", "Europe/Berlin").set(&note("digest")).unwrap();
    let digest = digests.get("user:42").unwrap().unwrap();
    // 2026-10-09 20:00 in Berlin, two hours ahead of UTC in October
    assert_eq!(crate::unix_millis(digest.at), 1_791_568_800_000);
    assert_eq!(digest.repeat.as_deref(), Some("0 20 * * * Europe/Berlin"));
}

#[test]
fn a_schedule_keeps_the_codes_repeat_each_time_it_opens() {
    let mut f = fixture();
    let cleanup = f.store.schedule("cleanup").cron("10 3 * * *", "UTC").open().unwrap();
    let first = cleanup.get().unwrap().unwrap();
    assert_eq!(first.repeat.as_deref(), Some("10 3 * * * UTC"));

    f.reopen();
    let cleanup = f.store.schedule("cleanup").cron("10 3 * * *", "UTC").open().unwrap();
    assert_eq!(cleanup.get().unwrap().unwrap().at, first.at, "the same repeat keeps its time");

    f.reopen();
    let cleanup = f.store.schedule("cleanup").cron("30 4 * * *", "UTC").open().unwrap();
    let changed = cleanup.get().unwrap().unwrap();
    assert_eq!(changed.repeat.as_deref(), Some("30 4 * * * UTC"), "the code's repeat replaces the kept one");
    assert_ne!(changed.at, first.at);

    f.clock.advance(DAY);
    let runs = Mutex::new(Vec::new());
    let ran = cleanup.run_due(|run| -> Result<()> {
        runs.lock().unwrap().push(run.at());
        Ok(())
    });
    assert_eq!(ran.unwrap(), 1);
    assert_eq!(*runs.lock().unwrap(), [changed.at], "a run knows when it was due");
}

#[test]
fn a_time_outside_the_years_a_job_keeps_is_refused() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    let year_10000 = std::time::UNIX_EPOCH + Duration::from_secs(253_402_300_800);
    assert_eq!(notes.at(year_10000).add(&note("x")).unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn a_progress_past_4_kib_is_refused() {
    let f = fixture();
    let videos = f.store.queue::<Note>("videos").open().unwrap();
    videos.add(&note("v")).unwrap();
    let refused = Mutex::new(None);
    videos
        .run_due(|_: Note, run| -> Result<()> {
            *refused.lock().unwrap() = run.set_progress(&"x".repeat(5000)).err().map(|error| error.kind());
            Ok(())
        })
        .unwrap();
    assert_eq!(*refused.lock().unwrap(), Some(ErrorKind::Limit));
}
