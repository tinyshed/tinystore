//! Gathering what arrives under one id into one job.

use std::sync::Mutex;
use std::time::Duration;

use super::fixture::{HOUR, MINUTE, SECOND, fixture};
use super::{Filter, Gather, Gathered, Queue, State};
use crate::{ErrorKind, Result};

/// Runs what is due and answers each run's list, in order.
fn ran(queue: &Queue<Vec<i64>>) -> Vec<Vec<i64>> {
    let seen = Mutex::new(Vec::new());
    queue
        .run_due(|items: Vec<i64>, _run| -> Result<()> {
            seen.lock().unwrap().push(items);
            Ok(())
        })
        .unwrap();
    seen.into_inner().unwrap()
}

fn five_minutes() -> Gather {
    Gather::settle(5 * MINUTE)
}

// contract: JOB-22, JOB-23
#[test]
fn two_gathers_a_moment_apart_are_one_job_of_both_items_due_five_minutes_after_the_second() {
    let f = fixture();
    let batch = f.store.queue::<Vec<i64>>("notify-batch").open().unwrap();
    let first = batch.gather("grades:7", &vec![1], five_minutes().max(HOUR)).unwrap();
    assert_eq!(first, Gathered { added: true, count: 1, at: f.now() + 5 * MINUTE });
    f.advance(Duration::from_millis(1200));
    let second = batch.gather("grades:7", &vec![2], five_minutes().max(HOUR)).unwrap();
    assert_eq!(second, Gathered { added: false, count: 2, at: f.now() + 5 * MINUTE });
    let job = batch.get("grades:7").unwrap().unwrap();
    assert_eq!((job.state, job.value, job.at), (State::Scheduled, vec![1, 2], second.at), "get shows the moved time");

    f.advance(5 * MINUTE - SECOND);
    assert!(ran(&batch).is_empty(), "the first gather's settle moved with the second");
    f.advance(SECOND);
    assert_eq!(ran(&batch), [vec![1, 2]]);
    assert!(batch.get("grades:7").unwrap().is_none(), "the job ran once and left");
}

// contract: JOB-23
#[test]
fn a_gathering_runs_at_its_max_after_the_first_however_many_follow() {
    let f = fixture();
    let batch = f.store.queue::<Vec<i64>>("notify-batch").open().unwrap();
    let started = f.now();
    for item in 0..15 {
        let gathered = batch.gather("grades:7", &vec![item], five_minutes().max(HOUR)).unwrap();
        assert_eq!(gathered.at, (f.now() + 5 * MINUTE).min(started + HOUR), "gather {item}");
        f.advance(4 * MINUTE - SECOND);
        assert!(ran(&batch).is_empty(), "gather {item}: ran before its max");
        f.advance(SECOND);
    }
    assert_eq!(f.now(), started + HOUR);
    assert_eq!(ran(&batch), [(0..15).collect::<Vec<_>>()], "every item in arrival order, none lost");
    assert!(batch.gather("grades:7", &vec![99], five_minutes()).unwrap().added, "the next gather starts anew");
}

// contract: JOB-22
#[test]
fn items_gathered_while_the_job_runs_are_the_next_runs_alone_due_at_their_settle() {
    let f = fixture();
    let batch = f.store.queue::<Vec<i64>>("notify-batch").open().unwrap();
    batch.gather("grades:7", &vec![1, 2], five_minutes()).unwrap();
    f.advance(5 * MINUTE);
    let during = Mutex::new(Vec::new());
    let seen = Mutex::new(Vec::new());
    batch
        .run_due(|items: Vec<i64>, _run| -> Result<()> {
            if items == [1, 2] {
                let mut during = during.lock().unwrap();
                during.push(batch.gather("grades:7", &vec![3], five_minutes())?);
                during.push(batch.gather("grades:7", &vec![4], five_minutes())?);
            }
            seen.lock().unwrap().push(items);
            Ok(())
        })
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), [vec![1, 2]], "the run read its own list, and the next is not yet due");
    let during = during.into_inner().unwrap();
    let due = f.now() + 5 * MINUTE;
    assert_eq!(during, [Gathered { added: true, count: 1, at: due }, Gathered { added: false, count: 2, at: due }]);
    let next = batch.get("grades:7").unwrap().unwrap();
    assert_eq!((next.state, next.value, next.at), (State::Scheduled, vec![3, 4], due));
    f.advance(5 * MINUTE);
    assert_eq!(ran(&batch), [vec![3, 4]]);
}

#[test]
fn a_running_jobs_max_starts_with_the_first_gather_since_its_claim() {
    let f = fixture();
    let batch = f.store.queue::<Vec<i64>>("notify-batch").open().unwrap();
    batch.gather("grades:7", &vec![1], five_minutes().max(10 * MINUTE)).unwrap();
    f.advance(10 * MINUTE);
    let gathered = Mutex::new(None);
    batch
        .run_due(|_: Vec<i64>, _run| -> Result<()> {
            *gathered.lock().unwrap() = Some(batch.gather("grades:7", &vec![2], five_minutes().max(10 * MINUTE))?);
            Ok(())
        })
        .unwrap();
    let started = f.now();
    assert_eq!(gathered.into_inner().unwrap().unwrap().at, started + 5 * MINUTE);
    f.advance(4 * MINUTE);
    let again = batch.gather("grades:7", &vec![3], five_minutes()).unwrap();
    assert_eq!(again.at, started + 9 * MINUTE, "the old gathering's max ended with its run");
    f.advance(4 * MINUTE);
    assert_eq!(batch.gather("grades:7", &vec![4], five_minutes()).unwrap().at, started + 10 * MINUTE);
}

#[test]
fn a_value_that_is_no_list_is_refused_naming_the_queue_and_the_id() {
    let f = fixture();
    let raw = f.store.queue::<serde_json::Value>("notify-batch").open().unwrap();
    let refused = raw.gather("grades:7", &serde_json::json!({ "course": 1 }), five_minutes()).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert!(refused.to_string().contains(r#"notify-batch: id "grades:7""#), "{refused}");
    raw.id("grades:7").add(&serde_json::json!({ "course": 1 })).unwrap();
    let refused = raw.gather("grades:7", &serde_json::json!([2]), five_minutes()).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    assert!(refused.to_string().contains("the job's value that is not a JSON list"), "{refused}");
    assert_eq!(raw.get("grades:7").unwrap().unwrap().value, serde_json::json!({ "course": 1 }));
}

#[test]
fn a_gather_past_a_jobs_bound_is_a_limit_and_changes_nothing() {
    let f = fixture();
    let batch = f.store.queue::<Vec<String>>("notify-batch").open().unwrap();
    let half = vec!["x".repeat(600 * 1024)];
    batch.gather("big", &half, five_minutes()).unwrap();
    let refused = batch.gather("big", &half, five_minutes()).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Limit, "{refused}");
    assert_eq!(batch.get("big").unwrap().unwrap().value.len(), 1);
}

#[test]
fn a_gather_needs_an_id_and_takes_its_time_from_its_settle() {
    let f = fixture();
    let batch = f.store.queue::<Vec<i64>>("notify-batch").open().unwrap();
    let refused = [
        batch.delay(HOUR).gather(&vec![1], five_minutes()),
        batch.id("a").delay(HOUR).gather(&vec![1], five_minutes()),
        batch.id("a").every(HOUR).gather(&vec![1], five_minutes()),
    ];
    for refused in refused {
        assert_eq!(refused.unwrap_err().kind(), ErrorKind::Invalid);
    }
    batch.id("polls").every(HOUR).add(&vec![1]).unwrap();
    assert_eq!(batch.gather("polls", &vec![2], five_minutes()).unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn a_done_job_its_dedupe_holds_gathers_nothing_and_a_failed_one_gives_up_its_id() {
    let f = fixture();
    let batch = f.store.queue::<Vec<i64>>("notify-batch").attempts(1).dedupe(HOUR).open().unwrap();
    batch.gather("done", &vec![1], five_minutes()).unwrap();
    batch.gather("failing", &vec![1], five_minutes()).unwrap();
    f.advance(5 * MINUTE);
    batch
        .run_due(|_: Vec<i64>, run| -> Result<(), &str> {
            if run.id() == Some("failing") { Err("down") } else { Ok(()) }
        })
        .unwrap();
    let nothing = batch.gather("done", &vec![2], five_minutes()).unwrap();
    assert_eq!((nothing.added, nothing.count), (false, 0));
    assert_eq!(batch.get("done").unwrap().unwrap().state, State::Done);
    let again = batch.gather("failing", &vec![2], five_minutes()).unwrap();
    assert_eq!((again.added, again.count), (true, 1));
    assert_eq!(batch.get("failing").unwrap().unwrap().value, vec![2]);
}

#[test]
fn a_gather_in_a_transaction_is_there_only_once_it_commits() {
    let f = fixture();
    let batch = f.store.queue::<Vec<i64>>("notify-batch").open().unwrap();
    let rolled_back = f.store.tx(|tx| -> Result<()> {
        tx.with(&batch).gather("grades:7", &vec![1], five_minutes())?;
        Err(crate::Error::invalid("changed my mind"))
    });
    assert!(rolled_back.is_err());
    assert!(batch.get("grades:7").unwrap().is_none());
    let gathered = f.store.tx(|tx| tx.with(&batch).gather("grades:7", &vec![2], five_minutes())).unwrap();
    assert!(gathered.added);
    assert_eq!(batch.get("grades:7").unwrap().unwrap().value, vec![2]);
}

/// An item of a list: its number alone, or past the 512 bytes a job keeps in
/// its row once a few are together, so that the list is spilled.
fn item(number: u32, spilled: bool) -> String {
    if spilled { format!("{number}:{}", "x".repeat(300)) } else { number.to_string() }
}

/// The numbers of a list's items.
fn numbers(items: &[String]) -> Vec<u32> {
    items.iter().map(|item| item.split(':').next().unwrap().parse().unwrap()).collect()
}

/// Gathers 1 and 2, then runs them with `handler`, which gathers 3 while the
/// run is under way the first time it sees 1 and 2; answers each run's numbers.
fn run_with_a_gather_during(
    batch: &Queue<Vec<String>>,
    spilled: bool,
    handler: impl Fn(&[String]) -> std::result::Result<(), String> + Sync,
) -> Vec<Vec<u32>> {
    let seen = Mutex::new(Vec::new());
    batch
        .run_due(|items: Vec<String>, _run| -> std::result::Result<(), String> {
            if numbers(&items) == [1, 2] && seen.lock().unwrap().is_empty() {
                batch.gather("grades:7", &vec![item(3, spilled)], five_minutes()).map_err(|error| error.to_string())?;
            }
            seen.lock().unwrap().push(numbers(&items));
            handler(&items)
        })
        .unwrap();
    seen.into_inner().unwrap()
}

fn gathered_two(f: &super::fixture::Fixture, batch: &Queue<Vec<String>>, spilled: bool) {
    batch.gather("grades:7", &vec![item(1, spilled), item(2, spilled)], five_minutes()).unwrap();
    f.advance(5 * MINUTE);
}

// contract: JOB-22
#[test]
fn a_run_that_ends_well_leaves_only_what_was_gathered_while_it_ran() {
    for spilled in [false, true] {
        let f = fixture();
        let batch = f.store.queue::<Vec<String>>("notify-batch").open().unwrap();
        gathered_two(&f, &batch, spilled);
        assert_eq!(run_with_a_gather_during(&batch, spilled, |_| Ok(())), [vec![1, 2]], "spilled {spilled}");
        let next = batch.get("grades:7").unwrap().unwrap();
        assert_eq!((numbers(&next.value), next.at), (vec![3], f.now() + 5 * MINUTE), "spilled {spilled}");
        f.advance(5 * MINUTE);
        assert_eq!(run_with_a_gather_during(&batch, spilled, |_| Ok(())), [vec![3]], "spilled {spilled}");
    }
}

// contract: JOB-22
#[test]
fn a_run_that_fails_after_a_gather_is_retried_with_its_items_and_the_gathered_ones_once_each() {
    for spilled in [false, true] {
        let f = fixture();
        let batch = f.store.queue::<Vec<String>>("notify-batch").open().unwrap();
        gathered_two(&f, &batch, spilled);
        let first = run_with_a_gather_during(&batch, spilled, |_| Err("push service 503".into()));
        assert_eq!(first, [vec![1, 2]], "spilled {spilled}");
        let retry = batch.get("grades:7").unwrap().unwrap();
        assert_eq!((retry.attempt, retry.at), (1, f.now() + 5 * MINUTE), "the later of its backoff and its settle");
        f.advance(5 * MINUTE);
        assert_eq!(run_with_a_gather_during(&batch, spilled, |_| Ok(())), [vec![1, 2, 3]], "spilled {spilled}");
        assert!(batch.get("grades:7").unwrap().is_none(), "none twice: spilled {spilled}");
    }
}

// contract: JOB-22
#[test]
fn a_run_that_fails_for_good_after_a_gather_keeps_its_items_and_the_gathered_ones_still_run() {
    for spilled in [false, true] {
        let f = fixture();
        let batch = f.store.queue::<Vec<String>>("notify-batch").attempts(1).open().unwrap();
        gathered_two(&f, &batch, spilled);
        assert_eq!(run_with_a_gather_during(&batch, spilled, |_| Err("push service 503".into())), [vec![1, 2]]);
        let failed = batch.list(&Filter::default().state(State::Failed)).unwrap().jobs;
        let failed: Vec<_> = failed.iter().map(|job| (numbers(&job.value), job.error.clone())).collect();
        assert_eq!(failed, [(vec![1, 2], Some("push service 503".to_owned()))], "spilled {spilled}");
        let next = batch.get("grades:7").unwrap().unwrap();
        let shown = (next.state, numbers(&next.value), next.at, next.error.as_deref());
        assert_eq!(shown, (State::Scheduled, vec![3], f.now() + 5 * MINUTE, Some("push service 503")));
        f.advance(5 * MINUTE);
        assert_eq!(run_with_a_gather_during(&batch, spilled, |_| Err("down again".into())), [vec![3]]);
        let failed = batch.list(&Filter::default().state(State::Failed)).unwrap().jobs;
        assert_eq!(failed.iter().map(|job| numbers(&job.value)).collect::<Vec<_>>(), [vec![3]], "the id's one failure");
    }
}

// contract: JOB-22
#[test]
fn a_claimed_list_read_after_a_gather_committed_is_the_items_the_claim_took() {
    for spilled in [false, true] {
        let f = fixture();
        let batch = f.store.queue::<Vec<String>>("notify-batch").open().unwrap();
        gathered_two(&f, &batch, spilled);
        let gather = || {
            batch.gather("grades:7", &vec![item(3, spilled)], five_minutes()).unwrap();
        };
        let claimed = batch.claim_with(gather).unwrap().expect("1 and 2 are due");
        assert_eq!(numbers(&claimed.value), [1, 2], "spilled {spilled}");
        claimed.settle(()).unwrap();
        let next = batch.get("grades:7").unwrap().unwrap();
        assert_eq!(numbers(&next.value), [3], "spilled {spilled}");
        f.advance(5 * MINUTE);
        assert_eq!(batch.claim().unwrap().map(|claimed| numbers(&claimed.value)), Some(vec![3]), "spilled {spilled}");
    }
}

// contract: JOB-22
#[test]
fn a_retry_whose_list_grew_while_it_ran_runs_its_steps_again_on_the_whole_list() {
    let f = fixture();
    let batch = f.store.queue::<Vec<i64>>("notify-batch").open().unwrap();
    batch.gather("grades:7", &vec![1, 2], five_minutes()).unwrap();
    f.advance(5 * MINUTE);
    let sent = Mutex::new(Vec::new());
    let notify = |items: Vec<i64>, run: &super::Run| -> Result<()> {
        let first = sent.lock().unwrap().is_empty();
        run.step("notify", || -> Result<()> {
            sent.lock().unwrap().push(items.clone());
            Ok(())
        })?;
        if first {
            batch.gather("grades:7", &vec![3], five_minutes())?;
            return Err(crate::Error::invalid("the receipt did not save"));
        }
        Ok(())
    };
    batch.run_due(notify).unwrap();
    f.advance(5 * MINUTE);
    batch.run_due(notify).unwrap();
    assert_eq!(*sent.lock().unwrap(), [vec![1, 2], vec![1, 2, 3]], "3 is sent, by the step run again");
    assert!(batch.get("grades:7").unwrap().is_none());
}

// contract: JOB-07, JOB-22
#[test]
fn a_gather_into_a_job_waiting_for_its_retry_keeps_its_backoff() {
    let f = fixture();
    let batch = f.store.queue::<Vec<i64>>("notify-batch").backoff(HOUR, HOUR).open().unwrap();
    batch.gather("grades:7", &vec![1], five_minutes()).unwrap();
    f.advance(5 * MINUTE);
    batch.run_due(|_: Vec<i64>, _run| -> Result<(), &str> { Err("push service 503") }).unwrap();
    let retry = batch.get("grades:7").unwrap().unwrap();
    assert!(retry.at > f.now() + 50 * MINUTE, "a backoff of an hour");
    let gathered = batch.gather("grades:7", &vec![2], five_minutes()).unwrap();
    assert_eq!(gathered.at, retry.at, "the later of its retry and the settle");
    let job = batch.get("grades:7").unwrap().unwrap();
    assert_eq!((job.attempt, job.at, job.value), (1, retry.at, vec![1, 2]));
    f.advance(5 * MINUTE);
    assert!(ran(&batch).is_empty(), "no attempt spent at the settle");
}

/// Gathers 1 and 2, claims them, gathers 3 while the claim holds them, and
/// lets the claim's lease end unsettled, as a worker that died does.
fn a_run_that_died_after_a_gather(f: &super::fixture::Fixture) -> Queue<Vec<i64>> {
    let batch = f.store.queue::<Vec<i64>>("notify-batch").timeout(MINUTE).open().unwrap();
    batch.gather("grades:7", &vec![1, 2], five_minutes()).unwrap();
    f.advance(5 * MINUTE);
    let claimed = batch.claim().unwrap().expect("1 and 2 are due");
    assert_eq!(claimed.value, [1, 2]);
    batch.gather("grades:7", &vec![3], five_minutes()).unwrap();
    f.advance(2 * MINUTE);
    batch
}

/// Runs what is due until nothing is, the clock moved on an hour each time.
fn every_run(f: &super::fixture::Fixture, batch: &Queue<Vec<i64>>) -> Vec<Vec<i64>> {
    let mut runs = Vec::new();
    for _ in 0..3 {
        f.advance(HOUR);
        runs.extend(ran(batch));
    }
    runs
}

// contract: JOB-22
#[test]
fn a_run_after_one_that_died_with_a_gather_runs_the_whole_list_once() {
    let f = fixture();
    let batch = a_run_that_died_after_a_gather(&f);
    assert_eq!(every_run(&f, &batch), [vec![1, 2, 3]], "3 runs once, not again on its own");
    assert!(batch.get("grades:7").unwrap().is_none());
}

// contract: JOB-22
#[test]
fn a_gather_after_a_run_that_died_with_a_gather_runs_the_whole_list_once() {
    let f = fixture();
    let batch = a_run_that_died_after_a_gather(&f);
    batch.gather("grades:7", &vec![4], five_minutes()).unwrap();
    assert_eq!(every_run(&f, &batch), [vec![1, 2, 3, 4]]);
    assert!(batch.get("grades:7").unwrap().is_none());
}

// contract: JOB-22
#[test]
fn an_update_or_an_upsert_after_a_run_that_died_with_a_gather_runs_its_value_once_whole() {
    for upsert in [false, true] {
        let f = fixture();
        let batch = a_run_that_died_after_a_gather(&f);
        if upsert {
            assert!(batch.upsert("grades:7", &vec![9, 8, 7]).unwrap());
        } else {
            assert!(batch.update("grades:7", &vec![9, 8, 7]).unwrap());
        }
        assert_eq!(every_run(&f, &batch), [vec![9, 8, 7]], "upsert {upsert}");
        assert!(batch.get("grades:7").unwrap().is_none(), "upsert {upsert}");
    }
}
