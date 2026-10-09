//! Watches: a job as it is, then again each time it changes, until it ends.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::State;
use super::fixture::{HOUR, fixture};
use crate::{ErrorKind, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Video {
    id: i64,
}

#[test]
fn a_watch_follows_its_job_to_its_end() {
    let f = fixture();
    let videos = f.store.queue::<Video>("videos").open().unwrap();
    videos.id("v1").add(&Video { id: 1 }).unwrap();
    let watched = videos.watch("v1");
    let (seen_tx, seen_rx) = mpsc::channel();
    let watching = thread::spawn(move || {
        for job in watched {
            seen_tx.send(job.unwrap().state).unwrap();
        }
    });
    assert_eq!(seen_rx.recv().unwrap(), State::Waiting, "the job as it is first");

    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let worker = videos
        .work(move |_: Video, run| -> Result<()> {
            run.set_progress(&0.5)?;
            release_rx.lock().unwrap().recv().unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(seen_rx.recv().unwrap(), State::Running);
    release_tx.send(()).unwrap();
    watching.join().unwrap();
    let rest: Vec<State> = seen_rx.try_iter().collect();
    assert_eq!(rest.last(), Some(&State::Done), "it ends with its job: {rest:?}");
    worker.stop();
}

#[test]
fn a_watch_of_a_cancelled_job_ends_cancelled() {
    let f = fixture();
    let videos = f.store.queue::<Video>("videos").open().unwrap();
    videos.id("v1").delay(HOUR).add(&Video { id: 1 }).unwrap();
    let mut watched = videos.watch("v1");
    assert_eq!(watched.next().unwrap().unwrap().state, State::Scheduled);
    assert!(videos.cancel("v1").unwrap());
    assert_eq!(watched.next().unwrap().unwrap().state, State::Cancelled);
    assert!(watched.next().is_none());
}

#[test]
fn a_watch_of_an_id_with_no_job_yields_nothing() {
    let f = fixture();
    let videos = f.store.queue::<Video>("videos").open().unwrap();
    assert!(videos.watch("never").next().is_none());
}

#[test]
fn a_watch_sees_its_job_come_due_when_the_clock_moves() {
    let f = fixture();
    let videos = f.store.queue::<Video>("videos").open().unwrap();
    videos.id("v1").delay(HOUR).add(&Video { id: 1 }).unwrap();
    let mut watched = videos.watch("v1");
    assert_eq!(watched.next().unwrap().unwrap().state, State::Scheduled);
    let (seen_tx, seen_rx) = mpsc::channel();
    let watching = thread::spawn(move || seen_tx.send(watched.next().unwrap().unwrap().state).unwrap());
    thread::sleep(Duration::from_millis(50)); // the watcher waits, rather than reads after the move
    f.advance(HOUR);
    assert_eq!(seen_rx.recv_timeout(Duration::from_secs(5)).unwrap(), State::Waiting);
    watching.join().unwrap();
}

#[test]
fn a_watch_hears_its_store_close() {
    let f = fixture();
    let videos = f.store.queue::<Video>("videos").open().unwrap();
    videos.id("v1").delay(HOUR).add(&Video { id: 1 }).unwrap();
    let mut watched = videos.watch("v1");
    watched.next().unwrap().unwrap();
    let closing = thread::spawn({
        let store = f.store.clone();
        move || {
            thread::sleep(Duration::from_millis(20));
            store.close().unwrap();
        }
    });
    let error = watched.next().unwrap().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Closed, "{error}");
    assert!(watched.next().is_none());
    closing.join().unwrap();
}
