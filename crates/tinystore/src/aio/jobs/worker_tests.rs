//! A worker as a future: its handlers' answers, its stops, its steps and its
//! schedules, each against what the blocking worker does.

use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::task::{Context, Waker};
use std::thread;

use super::super::fixture::{Draft, HOUR, MINUTE, SECOND, draft, eventually, fixture};
use super::*;
use crate::ErrorKind;
use crate::jobs::{Job, State};

/// How each case's handler answers, by the chat its draft names.
fn answered(chat: i64, run: &dyn Fn(i64) -> Outcome) -> std::result::Result<Outcome, String> {
    match chat {
        1 => Ok(().into()),
        2 => Err("the provider is down".to_owned()),
        _ => Ok(run(chat)),
    }
}

// contract: JOB-07
#[tokio::test]
async fn run_due_settles_each_job_by_what_its_handler_answers_as_the_blocking_worker_does() {
    let f = fixture();
    let awaited = f.store.queue::<Draft>("awaited").attempts(5).open().unwrap();
    let blocking = f.store.blocking().queue::<Draft>("blocking").attempts(5).open().unwrap();
    for chat in 1..=6 {
        awaited.id(format!("case-{chat}")).add(&draft(chat)).await.unwrap();
        blocking.id(format!("case-{chat}")).add(&draft(chat)).unwrap();
    }
    let ran = awaited
        .run_due(|draft: Draft, run: Run| async move {
            if draft.chat == 6 {
                panic!("the handler's bug");
            }
            answered(draft.chat, &|chat| match chat {
                3 => run.retry(10 * MINUTE),
                4 => run.snooze(10 * MINUTE),
                _ => run.fail("the provider no longer knows the token"),
            })
        })
        .await
        .unwrap();
    let blocking_ran = blocking
        .run_due(|draft: Draft, run: &jobs::Run| {
            if draft.chat == 6 {
                panic!("the handler's bug");
            }
            answered(draft.chat, &|chat| match chat {
                3 => run.retry(10 * MINUTE),
                4 => run.snooze(10 * MINUTE),
                _ => run.fail("the provider no longer knows the token"),
            })
        })
        .unwrap();
    assert_eq!(ran, blocking_ran);
    for chat in 1..=6 {
        let id = format!("case-{chat}");
        let shown = |job: Option<Job<Draft>>| job.map(|job| (job.state, job.attempt, job.error));
        assert_eq!(shown(awaited.get(&id).await.unwrap()), shown(blocking.get(&id).unwrap()), "{id}");
    }
    let retried = awaited.get("case-3").await.unwrap().unwrap();
    assert_eq!(retried.at, f.store.now() + 10 * MINUTE);
    let panicked = awaited.get("case-6").await.unwrap().unwrap();
    assert!(panicked.error.unwrap().contains("the handler panicked: the handler's bug"));
    f.store.close().await.unwrap();
}

// contract: STORE-10
#[tokio::test]
async fn a_handler_runs_inside_the_workers_future_on_the_thread_that_polls_it() {
    let f = fixture();
    let pushes = f.store.queue::<Draft>("pushes").open().unwrap();
    pushes.id("p").add(&draft(1)).await.unwrap();
    let ran_on = Arc::new(Mutex::new(None));
    let noting = Arc::clone(&ran_on);
    let ran = pushes
        .run_due(move |_: Draft, _run: Run| {
            let noting = Arc::clone(&noting);
            async move {
                *noting.lock().unwrap() = Some(thread::current().id());
                Ok::<(), String>(())
            }
        })
        .await
        .unwrap();
    assert_eq!(ran, 1);
    assert_eq!(*ran_on.lock().unwrap(), Some(thread::current().id()), "the executor's own thread ran it");
    f.store.close().await.unwrap();
}

// contract: STORE-10
#[test]
fn a_workers_first_poll_waits_for_no_commit_its_loop_claims_on_a_thread_of_its_own() {
    let f = fixture();
    let pushes = f.store.queue::<Draft>("pushes").open().unwrap();
    pushes.blocking().id("p").add(&draft(1)).unwrap();
    let (release, held_until) = mpsc::channel::<()>();
    let (holding, held) = mpsc::channel();
    let engine = Arc::clone(&pushes.queue.jobs);
    let holder = thread::spawn(move || {
        let _writer = engine.file().hold_writer();
        holding.send(()).unwrap();
        let _ = held_until.recv_timeout(std::time::Duration::from_secs(1));
    });
    held.recv().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut worker = pin!(pushes.work(|_: Draft, _run: Run| async { Ok::<(), String>(()) }).until(stopped));
    assert!(worker.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending(), "the poll waited for nothing");
    release.send(()).unwrap();
    holder.join().unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    runtime.block_on(async {
        let done = pushes.queue.clone();
        let stopping = async {
            eventually("the job ran once the writer was let go", || done.get("p").unwrap().is_none()).await;
            stop.send(()).unwrap();
        };
        let (ran, ()) = tokio::join!(worker.as_mut(), stopping);
        ran.unwrap();
        f.store.close().await.unwrap();
    });
}

/// The leases the queue's file holds: the jobs claimed and not yet settled,
/// running or held for a handler still busy.
fn leases<V>(queue: &Queue<V>) -> i64 {
    let count =
        |c: &rusqlite::Connection| c.query_row("select count(*) from _tinystore_jobs_leases", [], |row| row.get(0));
    queue.queue.jobs.file().read(|c| count(c).map_err(|error| crate::sqlite::sql_error("the leases", error))).unwrap()
}

/// A handler that says which job it began, then waits for `release`.
fn waiting(
    began: tokio::sync::mpsc::UnboundedSender<String>,
    release: Arc<tokio::sync::Notify>,
) -> impl Fn(Draft, Run) -> std::pin::Pin<Box<dyn Future<Output = std::result::Result<(), String>> + Send>> {
    move |_: Draft, run: Run| {
        let (began, release) = (began.clone(), Arc::clone(&release));
        let id = run.id().unwrap_or_default().to_owned();
        Box::pin(async move {
            began.send(id).unwrap();
            release.notified().await;
            Ok(())
        })
    }
}

// contract: JOB-11
#[tokio::test]
async fn until_stops_the_worker_once_its_handler_finished_and_gives_back_the_job_it_had_not_begun() {
    let f = fixture();
    let emails = f.store.queue::<Draft>("emails").open().unwrap();
    for id in ["a", "b", "c"] {
        emails.id(id).add(&draft(1)).await.unwrap();
    }
    let (began, mut beginning) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let worker = tokio::spawn(emails.work(waiting(began, Arc::clone(&release))).until(stopped));
    assert_eq!(beginning.recv().await.as_deref(), Some("a"));
    eventually("the loop claimed the next job for its handler", || leases(&emails) == 2).await;

    stop.send(()).unwrap();
    release.notify_one();
    worker.await.unwrap().unwrap();
    assert_eq!(emails.get("a").await.unwrap(), None, "the handler under way finished, and its job settled");
    for id in ["b", "c"] {
        let job = emails.get(id).await.unwrap().unwrap();
        assert_eq!((job.state, job.attempt), (State::Waiting, 0), "{id} went back uncounted");
    }
    assert!(beginning.try_recv().is_err(), "no job began after the signal");
    f.store.close().await.unwrap();
}

#[tokio::test]
async fn a_dropped_worker_retries_the_job_its_handler_ran_and_gives_back_the_one_it_had_not_begun() {
    let f = fixture();
    let emails = f.store.queue::<Draft>("emails").open().unwrap();
    for id in ["a", "b"] {
        emails.id(id).add(&draft(1)).await.unwrap();
    }
    let (began, mut beginning) = tokio::sync::mpsc::unbounded_channel();
    let worker = tokio::spawn(emails.work(waiting(began, Arc::new(tokio::sync::Notify::new()))));
    assert_eq!(beginning.recv().await.as_deref(), Some("a"));
    eventually("the loop claimed b for its handler", || leases(&emails) == 2).await;

    worker.abort();
    let held = emails.queue.clone();
    assert!(worker.await.unwrap_err().is_cancelled());
    eventually("the loop wrote what the drop answered", || {
        held.get("a").unwrap().is_some_and(|job| job.state == State::Scheduled)
    })
    .await;
    let a = emails.get("a").await.unwrap().unwrap();
    assert_eq!(a.attempt, 1, "its attempt counted");
    assert_eq!(a.error.as_deref(), Some(DROPPED));
    let b = emails.get("b").await.unwrap().unwrap();
    assert_eq!((b.state, b.attempt), (State::Waiting, 0), "b went back uncounted");
    f.store.close().await.unwrap();
}

#[tokio::test]
async fn the_stores_close_stops_a_worker_once_its_handler_finished() {
    let f = fixture();
    let emails = f.store.queue::<Draft>("emails").open().unwrap();
    emails.id("a").add(&draft(1)).await.unwrap();
    let (began, mut beginning) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let worker = tokio::spawn(emails.work(waiting(began, Arc::clone(&release))));
    assert_eq!(beginning.recv().await.as_deref(), Some("a"));
    let closing = tokio::spawn(f.store.close());
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(!closing.is_finished(), "the close waits for the handler under way");
    release.notify_one();
    closing.await.unwrap().unwrap();
    worker.await.unwrap().unwrap();

    let again = crate::Store::open(f._dir.path(), crate::Options { background: false, ..Default::default() }).unwrap();
    let emails = again.queue::<Draft>("emails").open().unwrap();
    assert_eq!(emails.get("a").unwrap(), None, "the close wrote what the handler answered");
    again.close().unwrap();
}

#[tokio::test]
async fn a_worker_first_polled_after_its_store_closed_fails_closed() {
    let f = fixture();
    let emails = f.store.queue::<Draft>("emails").open().unwrap();
    let worker = emails.work(|_: Draft, _run: Run| async { Ok::<(), String>(()) });
    f.store.close().await.unwrap();
    let closed = worker.await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
}

#[tokio::test]
async fn a_signal_ready_before_the_first_poll_starts_nothing() {
    let f = fixture();
    let emails = f.store.queue::<Draft>("emails").open().unwrap();
    emails.id("a").add(&draft(1)).await.unwrap();
    let worker = emails.work(|_: Draft, _run: Run| async { Ok::<(), String>(()) }).until(std::future::ready(()));
    worker.await.unwrap();
    assert_eq!(emails.get("a").await.unwrap().unwrap().state, State::Waiting, "nothing was claimed");
    f.store.close().await.unwrap();
}

#[tokio::test]
async fn a_worker_runs_as_many_handlers_at_once_as_its_concurrency_inside_one_future() {
    let f = fixture();
    let emails = f.store.queue::<Draft>("emails").open().unwrap();
    for id in ["a", "b", "c"] {
        emails.id(id).add(&draft(1)).await.unwrap();
    }
    let together = Arc::new(tokio::sync::Barrier::new(3));
    let running = emails.concurrency(3).run_due(move |_: Draft, _run: Run| {
        let together = Arc::clone(&together);
        async move {
            together.wait().await; // only three handlers under way at once pass it
            Ok::<(), String>(())
        }
    });
    let ran = tokio::time::timeout(crate::testing::PATIENCE, running).await.expect("three handlers at once");
    assert_eq!(ran.unwrap(), 3);
    f.store.close().await.unwrap();
}

// contract: JOB-04
#[tokio::test]
async fn a_cancel_tells_a_running_handler_to_stop_and_its_answer_settles_nothing() {
    let f = fixture();
    let emails = f.store.queue::<Draft>("emails").open().unwrap();
    emails.id("a").add(&draft(1)).await.unwrap();
    let (began, mut beginning) = tokio::sync::mpsc::unbounded_channel::<()>();
    let worker = tokio::spawn(emails.work(move |_: Draft, run: Run| {
        let began = began.clone();
        async move {
            began.send(()).unwrap();
            while !run.stopped() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            Ok::<_, String>(run.retry(HOUR)) // settles nothing: a cancel took the job
        }
    }));
    beginning.recv().await.unwrap();
    assert!(emails.cancel("a").await.unwrap());
    let gone = emails.queue.clone();
    eventually("the job stays gone", || gone.get("a").unwrap().is_none()).await;
    emails.id("a").add(&draft(2)).await.unwrap();
    beginning.recv().await.unwrap();
    assert!(emails.cancel("a").await.unwrap(), "the worker went on with the next job");
    f.store.close().await.unwrap();
    worker.await.unwrap().unwrap();
}

/// A handler whose step counts its runs in `fetched`, and whose first attempt
/// fails after the step.
fn fetching(fetched: &Arc<AtomicUsize>) -> impl Fn(Draft, Run) -> Answering + Send + Sync + 'static {
    let fetched = Arc::clone(fetched);
    move |_: Draft, run: Run| {
        let fetched = Arc::clone(&fetched);
        Box::pin(async move {
            let fetch = || async { Ok::<_, crate::Error>(fetched.fetch_add(1, Ordering::SeqCst) + 41) };
            let rows: usize = run.step("fetch", fetch).await.map_err(|error| error.to_string())?;
            if run.attempt() == 1 {
                return Err("the upload after the fetch failed".to_owned());
            }
            assert_eq!(rows, 41, "the kept answer comes back");
            Ok(().into())
        })
    }
}

// contract: JOB-06
#[tokio::test]
async fn a_step_runs_once_across_its_jobs_attempts() {
    let f = fixture();
    let imports = f.store.queue::<Draft>("imports").backoff(SECOND, SECOND).open().unwrap();
    imports.id("a").add(&draft(1)).await.unwrap();
    let fetched = Arc::new(AtomicUsize::new(0));
    assert_eq!(imports.run_due(fetching(&fetched)).await.unwrap(), 1);
    assert_eq!(imports.get("a").await.unwrap().unwrap().attempt, 1);
    f.advance(2 * SECOND);
    assert_eq!(imports.run_due(fetching(&fetched)).await.unwrap(), 1);
    assert_eq!(imports.get("a").await.unwrap(), None, "done in its second attempt");
    assert_eq!(fetched.load(Ordering::SeqCst), 1, "the step ran once");
    f.store.close().await.unwrap();
}

#[tokio::test]
async fn a_schedule_runs_reads_and_cancels_as_the_blocking_one_does() {
    let f = fixture();
    let cleanup = f.store.schedule("cleanup").every(HOUR).open().unwrap();
    let next = cleanup.get().await.unwrap().unwrap();
    assert_eq!(Some(next.clone()), cleanup.blocking().get().unwrap());
    assert_eq!(next.state, State::Scheduled);
    assert_eq!(cleanup.run_due(|_run: Run| async { Ok::<(), String>(()) }).await.unwrap(), 0, "not yet");
    f.advance(HOUR);
    let ran = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&ran);
    let runs = cleanup
        .run_due(move |_run: Run| {
            counting.fetch_add(1, Ordering::SeqCst);
            async { Ok::<(), String>(()) }
        })
        .await
        .unwrap();
    assert_eq!((runs, ran.load(Ordering::SeqCst)), (1, 1));
    assert!(cleanup.get().await.unwrap().unwrap().at > next.at, "it repeats");
    assert!(cleanup.cancel().await.unwrap());
    assert_eq!(cleanup.get().await.unwrap(), None, "until the program opens it again");

    let worker = f.store.schedule("report").every(HOUR).work(|_run: Run| async { Ok::<(), String>(()) }).unwrap();
    let worker = tokio::spawn(worker);
    // the worker's first poll starts it before the close, which then stops it; first polled
    // after, it fails closed, as a_worker_first_polled_after_its_store_closed_fails_closed says
    tokio::task::yield_now().await;
    f.store.close().await.unwrap();
    worker.await.unwrap().unwrap();
}
