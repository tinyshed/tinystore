use std::time::Duration;

use crate::pipe::fixture::{Client, answered};
use crate::pipe::{Connect, Pipe};
use crate::wire::codec::Message;
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{
    Empty, Handle, Hello, JobsAnswer, JobsCall, JobsChanged, JobsHeld, JobsId, JobsJob, JobsKept, JobsList, JobsOp,
    JobsPage, JobsQueueOpen, JobsScheduleOpen, JobsStep, JobsTx, JobsTxResults, JobsWork, method,
};
use crate::{Options, Store};

fn queue(client: &mut Client, name: &str) -> u64 {
    let open = JobsQueueOpen { name: name.to_owned(), ..JobsQueueOpen::default() };
    client.call::<Handle>(method::JOBS_QUEUE_OPEN, &open).unwrap().handle
}

fn job(handle: u64, id: &str, value: &str) -> JobsCall {
    JobsCall { handle, id: Some(id.to_owned()), value: value.to_owned(), ..JobsCall::default() }
}

fn add(client: &mut Client, call: &JobsCall) -> bool {
    client.call::<JobsChanged>(method::JOBS_ADD, call).unwrap().changed
}

fn get(client: &mut Client, handle: u64, id: &str) -> JobsJob {
    client.call(method::JOBS_GET, &JobsId { handle, id: id.to_owned() }).unwrap()
}

/// Starts a worker on a queue, its stream open both ways once the `RESPONSE`
/// came.
fn work(client: &mut Client, handle: u64, concurrency: Option<u64>) -> u32 {
    start(client, &JobsWork { handle, concurrency, ..JobsWork::default() })
}

fn start(client: &mut Client, work: &JobsWork) -> u32 {
    let stream = client.open_stream(method::JOBS_WORK, work);
    let started = client.next_on(stream);
    assert_eq!((started.kind, started.flags), (Kind::Response, 0), "the stream stays open");
    stream
}

fn held(client: &mut Client, stream: u32) -> JobsHeld {
    let frame = client.next_on(stream);
    assert_eq!((frame.kind, frame.flags), (Kind::Data, 0), "a held job, the stream open");
    JobsHeld::decode(&frame.body).unwrap()
}

fn answer(client: &mut Client, stream: u32, answer: &JobsAnswer) {
    client.write(Frame::new(Kind::Data, stream, answer.encode()));
}

fn how(run: u64, how: &str) -> JobsAnswer {
    JobsAnswer { run, how: how.to_owned(), ..JobsAnswer::default() }
}

/// Ends the client's side of a worker, and returns the server's last frame.
fn end(client: &mut Client, stream: u32) -> Frame {
    client.write(Frame::new(Kind::Data, stream, Vec::new()).ending());
    client.next_on(stream)
}

/// Watches a job, the stream open once the `RESPONSE` came.
fn watch(client: &mut Client, handle: u64, id: &str) -> u32 {
    let stream = client.start(method::JOBS_WATCH, &JobsId { handle, id: id.to_owned() });
    let started = client.next_on(stream);
    assert_eq!((started.kind, started.flags), (Kind::Response, 0), "the watch stays open");
    stream
}

fn seen(client: &mut Client, stream: u32) -> JobsJob {
    let frame = client.next_on(stream);
    assert_eq!((frame.kind, frame.flags), (Kind::Data, 0), "the job as it is now, the watch open");
    JobsJob::decode(&frame.body).unwrap()
}

#[test]
fn a_client_adds_finds_and_cancels_jobs_by_their_ids() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "emails");
    let welcome = JobsCall { delay: Some(60_000), ..job(handle, "welcome:42", r#"{"to":"ada@example.com"}"#) };

    assert!(add(&mut client, &welcome));
    assert!(!add(&mut client, &welcome), "the id is taken");
    let found = get(&mut client, handle, "welcome:42");
    assert_eq!((found.found, found.state.as_str()), (true, "scheduled"));
    assert_eq!(found.value.as_deref(), Some(r#"{"to":"ada@example.com"}"#), "the JSON as it came");

    let grace = job(handle, "welcome:42", r#"{"to":"grace@example.com"}"#);
    assert!(client.call::<JobsChanged>(method::JOBS_UPDATE, &grace).unwrap().changed);
    let updated = get(&mut client, handle, "welcome:42");
    assert_eq!((updated.state.as_str(), updated.value), ("scheduled", grace.value.clone().into()), "its time kept");

    let page: JobsPage = client
        .call(method::JOBS_LIST, &JobsList { handle, prefix: Some("welcome:".into()), ..JobsList::default() })
        .unwrap();
    assert_eq!(page.jobs.iter().map(|job| job.id.as_deref()).collect::<Vec<_>>(), [Some("welcome:42")]);

    let id = JobsId { handle, id: "welcome:42".to_owned() };
    assert!(client.call::<JobsChanged>(method::JOBS_CANCEL, &id).unwrap().changed);
    assert!(!get(&mut client, handle, "welcome:42").found);
    assert!(!client.call::<JobsChanged>(method::JOBS_UPDATE, &grace).unwrap().changed, "no job to change");
}

#[test]
fn a_value_that_is_not_json_is_refused_before_it_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "emails");
    let failed = client.call::<JobsChanged>(method::JOBS_ADD, &job(handle, "a", "{")).unwrap_err();
    assert_eq!(failed.code, "invalid");
    assert!(failed.message.contains("not JSON"), "{}", failed.message);
    assert!(!get(&mut client, handle, "a").found);
}

#[test]
fn a_client_works_a_queue_one_job_at_a_time_for_each_handler() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "emails");
    for n in 1..=3 {
        add(&mut client, &job(handle, &format!("e{n}"), &n.to_string()));
    }
    let stream = work(&mut client, handle, Some(1));

    let first = held(&mut client, stream);
    assert_eq!((first.id.as_deref(), first.value.as_deref(), first.attempt), (Some("e1"), Some("1"), 1));
    assert!(client.silent_on(stream, Duration::from_millis(100)), "one handler holds one job at a time");
    assert_eq!(get(&mut client, handle, "e1").state, "running");
    answer(&mut client, stream, &how(first.run, "done"));
    for n in 2..=3 {
        let next = held(&mut client, stream);
        assert_eq!(next.id, Some(format!("e{n}")), "in the order of their time");
        answer(&mut client, stream, &how(next.run, "done"));
    }

    answer(&mut client, stream, &how(0, "stop"));
    let last = client.next_on(stream);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END), "the worker wrote what it was told, and ended");
    for n in 1..=3 {
        assert!(!get(&mut client, handle, &format!("e{n}")).found, "e{n} is done, and no dedupe keeps it");
    }
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
}

#[test]
fn a_worker_until_idle_runs_what_is_due_and_then_its_stream_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "emails");
    add(&mut client, &job(handle, "now", "null"));
    add(&mut client, &JobsCall { delay: Some(3_600_000), ..job(handle, "later", "null") });
    let due = JobsWork { handle, until_idle: true, ..JobsWork::default() };

    let stream = start(&mut client, &due);
    let first = held(&mut client, stream);
    assert_eq!(first.id.as_deref(), Some("now"));
    answer(&mut client, stream, &how(first.run, "done"));
    let last = client.next_on(stream);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END), "nothing more is due");
    assert_eq!(get(&mut client, handle, "later").state, "scheduled", "a later job waits for its time");

    let idle = start(&mut client, &due);
    assert_eq!(client.next_on(idle).flags, frame::END, "a queue with nothing due ends its worker at once");
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
}

#[test]
fn a_stopped_worker_takes_no_job_more_and_gives_back_those_it_never_sent() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "emails");
    for n in 1..=3 {
        add(&mut client, &job(handle, &format!("e{n}"), "null"));
    }
    let stream = work(&mut client, handle, Some(1));
    let first = held(&mut client, stream);

    answer(&mut client, stream, &how(0, "stop"));
    assert!(client.silent_on(stream, Duration::from_millis(200)), "no job after the stop, the one in hand unanswered");
    answer(&mut client, stream, &how(first.run, "done"));
    let last = client.next_on(stream);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END), "the stream ends once the job in hand is answered");

    assert!(!get(&mut client, handle, "e1").found, "the job in hand was answered");
    let second = get(&mut client, handle, "e2");
    assert_eq!((second.state.as_str(), second.attempt), ("waiting", 0), "claimed ahead, never sent, given back");
}

#[test]
fn a_job_the_client_holds_when_its_worker_ends_fails_that_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "emails");
    add(&mut client, &job(handle, "e1", "null"));
    let stream = work(&mut client, handle, None);
    held(&mut client, stream);

    assert_eq!(end(&mut client, stream).flags, frame::END);
    let failed = get(&mut client, handle, "e1");
    assert_eq!((failed.state.as_str(), failed.attempt), ("scheduled", 1), "it retries after its backoff");
    assert!(failed.error.unwrap_or_default().contains("ended with the job in hand"));
}

#[test]
fn a_client_that_leaves_with_a_job_in_hand_fails_that_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "emails");
    add(&mut client, &job(handle, "e1", "null"));
    let stream = work(&mut client, handle, None);
    held(&mut client, stream);
    drop(client);

    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "emails");
    let failed = get(&mut client, handle, "e1");
    assert_eq!((failed.state.as_str(), failed.attempt), ("scheduled", 1));
}

#[test]
fn a_cancel_of_a_job_the_client_holds_tells_its_handler_and_settles_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "emails");
    add(&mut client, &job(handle, "e1", "null"));
    add(&mut client, &job(handle, "e2", "null"));
    let stream = work(&mut client, handle, Some(1));
    let first = held(&mut client, stream);

    let id = JobsId { handle, id: "e1".to_owned() };
    assert!(client.call::<JobsChanged>(method::JOBS_CANCEL, &id).unwrap().changed);
    let notice = held(&mut client, stream);
    assert_eq!((notice.run, notice.cancelled), (first.run, true));
    let second = held(&mut client, stream);
    assert_eq!(second.id.as_deref(), Some("e2"), "the cancelled job's handler is free for the next");

    answer(&mut client, stream, &how(first.run, "fail"));
    answer(&mut client, stream, &how(second.run, "done"));
    assert_eq!(end(&mut client, stream).flags, frame::END);
    assert!(!get(&mut client, handle, "e1").found, "the answer after the cancel settled nothing");
    assert!(!get(&mut client, handle, "e2").found);
}

#[test]
fn a_step_one_attempt_kept_is_found_by_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "orders");
    add(&mut client, &job(handle, "o1", "null"));
    let stream = work(&mut client, handle, None);
    let first = held(&mut client, stream);

    let step = JobsStep { run: first.run, name: "charge".to_owned(), answer: None };
    assert!(!client.call::<JobsKept>(method::JOBS_STEP, &step).unwrap().found);
    let charged = JobsStep { answer: Some(r#"{"charge":"ch_1"}"#.to_owned()), ..step.clone() };
    client.call::<Empty>(method::JOBS_KEEP, &charged).unwrap();
    let retry =
        JobsAnswer { delay: Some(0), error: Some("the mail server is down".to_owned()), ..how(first.run, "retry") };
    answer(&mut client, stream, &retry);

    let second = held(&mut client, stream);
    assert_eq!((second.id.as_deref(), second.attempt), (Some("o1"), 2));
    let kept: JobsKept = client.call(method::JOBS_STEP, &JobsStep { run: second.run, ..step.clone() }).unwrap();
    assert_eq!(kept.answer.as_deref(), Some(r#"{"charge":"ch_1"}"#));
    let stale = client.call::<JobsKept>(method::JOBS_STEP, &step).unwrap_err();
    assert_eq!(stale.code, "conflict", "the first run was answered: {}", stale.message);

    answer(&mut client, stream, &how(second.run, "done"));
    assert_eq!(end(&mut client, stream).flags, frame::END);
}

#[test]
fn what_a_handler_reports_shows_while_its_job_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "videos");
    add(&mut client, &job(handle, "v1", "null"));
    let stream = work(&mut client, handle, None);
    let run = held(&mut client, stream).run;

    answer(&mut client, stream, &JobsAnswer { progress: Some(r#"{"frames":120}"#.to_owned()), ..how(run, "progress") });
    let running = get(&mut client, handle, "v1");
    assert_eq!((running.state.as_str(), running.progress.as_deref()), ("running", Some(r#"{"frames":120}"#)));

    let past = format!("\"{}\"", "x".repeat(5000));
    answer(&mut client, stream, &JobsAnswer { progress: Some(past), ..how(run, "progress") });
    let failed = client.next_on(stream);
    assert_eq!(
        (failed.kind, failed.flags),
        (Kind::Data, frame::END | frame::ERROR),
        "a progress past 4 KiB fails the stream"
    );
    assert_eq!(get(&mut client, handle, "v1").attempt, 1, "the job in hand failed its attempt");
}

#[test]
fn the_server_sends_a_worker_no_more_than_the_credit_its_client_granted() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::connect(dir.path());
    let hello = Hello { protocol: 2, stream_credit: Some(1024), ..Hello::default() };
    assert_eq!(client.greet(hello).unwrap().max_body, 1024, "a body fits the credit it is sent under");
    let handle = queue(&mut client, "emails");
    for n in 0..4 {
        add(&mut client, &job(handle, &format!("e{n}"), &format!("\"{}\"", "x".repeat(300))));
    }
    let stream = work(&mut client, handle, Some(4));

    let mut sent = Vec::new();
    while !client.silent_on(stream, Duration::from_millis(200)) {
        sent.push(client.next_on(stream).body.len());
    }
    assert!(sent.len() < 4 && sent.iter().sum::<usize>() <= 1024, "within the credit: {sent:?}");
    client.write(Frame::new(Kind::Credit, stream, 1024u32.to_le_bytes().to_vec()));
    let more = held(&mut client, stream);
    assert!(more.id.is_some(), "a grant lets the next job go");
}

#[test]
fn a_closing_server_hands_its_workers_no_job_more() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let mut client = Client::over(Pipe::connect(&store, Connect::default()).unwrap());
    client.hello(2).unwrap();
    let handle = queue(&mut client, "emails");
    add(&mut client, &job(handle, "e1", "null"));
    add(&mut client, &job(handle, "e2", "null"));
    let stream = work(&mut client, handle, Some(1));
    let first = held(&mut client, stream);

    client.pipe.go_away();
    answer(&mut client, stream, &how(first.run, "done"));
    let last = client.next_on(stream);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END), "no job after the GOAWAY, and the stream ends");
    drop(client);

    let jobs = store.queue::<serde_json::Value>("emails").open().unwrap();
    assert!(jobs.get("e1").unwrap().is_none(), "the job in hand was answered");
    let second = jobs.get("e2").unwrap().unwrap();
    assert_eq!((second.state, second.attempt), (crate::jobs::State::Waiting, 0));
    store.close().unwrap();
}

#[test]
fn a_watch_follows_its_job_through_a_client_worker_to_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "videos");
    add(&mut client, &job(handle, "v1", "null"));
    let watched = watch(&mut client, handle, "v1");
    assert_eq!(seen(&mut client, watched).state, "waiting", "the job as it is first");

    let stream = work(&mut client, handle, None);
    let run = held(&mut client, stream).run;
    assert_eq!(seen(&mut client, watched).state, "running");
    answer(&mut client, stream, &JobsAnswer { progress: Some("0.5".to_owned()), ..how(run, "progress") });
    assert_eq!(seen(&mut client, watched).progress.as_deref(), Some("0.5"));
    answer(&mut client, stream, &how(run, "done"));
    assert_eq!(seen(&mut client, watched).state, "done");
    let last = client.next_on(watched);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END), "the watch ends with its job");

    assert_eq!(end(&mut client, stream).flags, frame::END);
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
}

#[test]
fn a_watch_ends_cancelled_with_its_job_and_at_once_with_no_job() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "videos");
    add(&mut client, &JobsCall { delay: Some(3_600_000), ..job(handle, "v1", "null") });
    let watched = watch(&mut client, handle, "v1");
    assert_eq!(seen(&mut client, watched).state, "scheduled");

    let id = JobsId { handle, id: "v1".to_owned() };
    assert!(client.call::<JobsChanged>(method::JOBS_CANCEL, &id).unwrap().changed);
    assert_eq!(seen(&mut client, watched).state, "cancelled");
    assert_eq!(client.next_on(watched).flags, frame::END);

    let nothing = watch(&mut client, handle, "never");
    let last = client.next_on(nothing);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END), "an id with no job ends its watch at once");
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
}

#[test]
fn a_watch_the_client_cancels_or_its_server_leaves_ends_with_why() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let mut client = Client::over(Pipe::connect(&store, Connect::default()).unwrap());
    client.hello(2).unwrap();
    let handle = queue(&mut client, "videos");
    add(&mut client, &JobsCall { delay: Some(3_600_000), ..job(handle, "v1", "null") });

    let cancelled = watch(&mut client, handle, "v1");
    seen(&mut client, cancelled);
    client.write(Frame::new(Kind::Cancel, cancelled, Vec::new()));
    let last = client.next_on(cancelled);
    assert_eq!(answered::<Empty>(&last).unwrap_err().code, "cancelled");

    let left = watch(&mut client, handle, "v1");
    seen(&mut client, left);
    client.pipe.go_away();
    let last = client.next_on(left);
    assert_eq!(answered::<Empty>(&last).unwrap_err().code, "unavailable", "watch again on another connection");
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
    drop(client);
    store.close().unwrap();
}

#[test]
fn a_schedule_opened_over_the_wire_keeps_its_one_job() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let open = JobsScheduleOpen {
        name: "nightly".to_owned(),
        cron: Some("10 3 * * *".to_owned()),
        time_zone: Some("Europe/Berlin".to_owned()),
        ..JobsScheduleOpen::default()
    };
    let handle = client.call::<Handle>(method::JOBS_SCHEDULE_OPEN, &open).unwrap().handle;
    let nightly = get(&mut client, handle, "nightly");
    assert_eq!((nightly.state.as_str(), nightly.repeat.as_deref()), ("scheduled", Some("10 3 * * * Europe/Berlin")));

    let refused = client.call::<JobsChanged>(method::JOBS_ADD, &job(handle, "extra", "null")).unwrap_err();
    assert_eq!(refused.code, "invalid", "{}", refused.message);
    let zoneless = JobsScheduleOpen { time_zone: None, ..open };
    let failed = client.call::<Handle>(method::JOBS_SCHEDULE_OPEN, &zoneless).unwrap_err();
    assert!(failed.message.contains("needs a time zone"), "{}", failed.message);
}

#[test]
fn a_transaction_of_jobs_writes_all_of_its_jobs_or_none() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let pushes = queue(&mut client, "pushes");
    let adds = |ids: &[&str]| -> Vec<JobsOp> {
        let op =
            |id: &&str| JobsOp { method: u64::from(method::JOBS_ADD), call: Some(job(pushes, id, "{}")), id: None };
        ids.iter().map(op).collect()
    };

    let mut writes = adds(&["ann", "bob"]);
    writes.push(JobsOp {
        method: u64::from(method::JOBS_SET),
        call: Some(JobsCall { handle: pushes, ..job(pushes, "", "{}") }),
        id: None,
    });
    let refused = client.call::<JobsTxResults>(method::JOBS_TX, &JobsTx { writes }).unwrap_err();
    assert_eq!(refused.code, "invalid", "{}", refused.message);
    assert_eq!(refused.what.unwrap().get("write").map(String::as_str), Some("2"), "the write that failed is named");
    assert!(!get(&mut client, pushes, "ann").found, "and none of them was written");

    let mut writes = adds(&["ann", "bob", "ann"]);
    writes.push(JobsOp {
        method: u64::from(method::JOBS_CANCEL),
        call: None,
        id: Some(JobsId { handle: pushes, id: "bob".to_owned() }),
    });
    let done: JobsTxResults = client.call(method::JOBS_TX, &JobsTx { writes }).unwrap();
    let changed: Vec<bool> = done.outcomes.iter().map(|outcome| outcome.changed).collect();
    assert_eq!(changed, [true, true, false, true], "an id taken inside the transaction adds nothing");
    assert!(get(&mut client, pushes, "ann").found && !get(&mut client, pushes, "bob").found);
    assert_eq!(client.pipe.streams(), 0);
}

#[test]
fn writes_of_jobs_in_flight_hold_no_thread_of_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let mut client = Client::over(Pipe::connect(&store, Connect::default()).unwrap());
    client.hello(2).unwrap();
    let mail = queue(&mut client, "mail");
    let held = store.queue::<serde_json::Value>("mail").open().unwrap();
    let (began, begun) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();

    std::thread::scope(|scope| {
        // a transaction of jobs.db holds its writer, so that no add can commit
        let (store, held) = (&store, &held);
        let holding = scope.spawn(move || {
            store.tx(|tx| -> crate::Result<()> {
                tx.with(held).id("held").add(&serde_json::json!({}))?;
                began.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            })
        });
        begun.recv().unwrap();
        let adds: Vec<u32> =
            (0..40).map(|n| client.start(method::JOBS_ADD, &job(mail, &format!("job-{n}"), "{}"))).collect();
        // more adds wait than the session has threads, and a read is still answered
        let page: JobsPage = client.call(method::JOBS_LIST, &JobsList { handle: mail, ..JobsList::default() }).unwrap();
        assert!(page.jobs.is_empty(), "nothing is committed yet");
        release.send(()).unwrap();
        holding.join().unwrap().unwrap();
        for stream in adds {
            assert!(answered::<JobsChanged>(&client.next_on(stream)).unwrap().changed);
        }
    });
    assert_eq!(client.pipe.streams(), 0);
    drop(client);
    store.close().unwrap();
}

#[test]
fn a_client_that_read_its_answer_finds_its_stream_ended() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = queue(&mut client, "emails");
    // a call of jobs is answered off the thread that reads the client's frames
    for _ in 0..20_000 {
        assert!(!get(&mut client, handle, "none").found);
        assert_eq!(client.pipe.streams(), 0, "the answer was read, and its stream is counted still");
    }
}
