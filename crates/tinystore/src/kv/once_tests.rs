use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use crate::ErrorKind;
use crate::kv::fixture::*;

/// A function that counts its runs and answers with the count.
fn counting(runs: &AtomicUsize) -> impl Fn() -> Result<String> + '_ {
    move || Ok(format!("receipt {}", runs.fetch_add(1, Ordering::SeqCst) + 1))
}

#[test]
fn a_run_keeps_its_answer_and_runs_a_key_once() {
    let f = fixture();
    let charges = f.store.once::<String>("charges").open().unwrap();
    let runs = AtomicUsize::new(0);
    let charge = counting(&runs);
    assert_eq!(charges.run("req-7", &charge).unwrap(), "receipt 1");
    assert_eq!(charges.run("req-7", &charge).unwrap(), "receipt 1", "the kept answer; nothing ran");
    assert_eq!(charges.get("req-7").unwrap().as_deref(), Some("receipt 1"));

    f.clock.advance(DAY - Duration::from_millis(1));
    assert_eq!(charges.run("req-7", &charge).unwrap(), "receipt 1");
    f.clock.advance(Duration::from_millis(1));
    assert_eq!(charges.run("req-7", &charge).unwrap(), "receipt 2", "kept a day");

    assert!(charges.delete("req-7").unwrap());
    assert_eq!(charges.run("req-7", &charge).unwrap(), "receipt 3");
    assert_eq!(charges.under("tenant-7").run("req-7", &charge).unwrap(), "receipt 4", "a branch keeps its own");
    assert_eq!(charges.under("tenant-7").run("req-7", &charge).unwrap(), "receipt 4");

    let hourly = f.store.once::<String>("hourly").keep(HOUR).open().unwrap();
    assert_eq!(hourly.run("req-7", &charge).unwrap(), "receipt 5");
    f.clock.advance(HOUR);
    assert_eq!(hourly.run("req-7", &charge).unwrap(), "receipt 6");
    assert_eq!(f.store.bucket::<String>("charges").open().unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn an_error_keeps_nothing_and_a_panic_lets_its_key_go() {
    let f = fixture();
    let imports = f.store.once::<i64>("imports").open().unwrap();
    let declined = imports.run("file-1", || Err(Error::invalid("the provider said no"))).unwrap_err();
    assert_eq!(declined.to_string(), "the provider said no: invalid", "the function's error, as it was");
    assert_eq!(imports.get("file-1").unwrap(), None);

    let panicked = catch_unwind(AssertUnwindSafe(|| imports.run("file-1", || -> Result<i64> { panic!("a bug") })));
    assert!(panicked.is_err());
    assert_eq!(imports.run("file-1", || Ok::<_, Error>(7)).unwrap(), 7, "the run after an error and a panic");
}

/// A function that tells the test it started, then waits for its word.
fn slow(started: mpsc::Sender<()>, release: mpsc::Receiver<()>, answer: Result<i64>) -> impl FnOnce() -> Result<i64> {
    move || {
        started.send(()).unwrap();
        release.recv().unwrap();
        answer
    }
}

#[test]
fn a_run_waits_for_the_run_of_its_key_and_takes_its_answer() {
    let f = fixture();
    let imports = f.store.once::<i64>("imports").open().unwrap();
    let (started, starting) = mpsc::channel();
    let (release, released) = mpsc::channel();
    thread::scope(|scope| {
        let first = scope.spawn(|| imports.run("file-1", slow(started, released, Ok(7))));
        starting.recv().unwrap();
        let second = scope.spawn(|| imports.run("file-1", || -> Result<i64> { panic!("ran beside its key's run") }));
        assert_eq!(imports.run("file-2", || Ok::<_, Error>(9)).unwrap(), 9, "a key apart runs at once");
        thread::sleep(Duration::from_millis(20));
        assert!(!second.is_finished(), "the second run waits");
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap().unwrap(), 7);
        assert_eq!(second.join().unwrap().unwrap(), 7, "and returns the first one's answer");
    });
}

#[test]
fn a_run_after_one_that_failed_runs_its_own_function() {
    let f = fixture();
    let imports = f.store.once::<i64>("imports").open().unwrap();
    let (started, starting) = mpsc::channel();
    let (release, released) = mpsc::channel();
    thread::scope(|scope| {
        let gone = Err(Error::not_found("the file is gone"));
        let first = scope.spawn(|| imports.run("file-3", slow(started, released, gone)));
        starting.recv().unwrap();
        let second = scope.spawn(|| imports.run("file-3", || Ok::<_, Error>(8)));
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap().unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(second.join().unwrap().unwrap(), 8);
    });
}

#[test]
fn a_run_takes_the_callers_own_error_type() {
    #[derive(Debug)]
    enum Payment {
        Declined,
        Store(Error),
    }
    impl From<Error> for Payment {
        fn from(error: Error) -> Self {
            Payment::Store(error)
        }
    }
    let f = fixture();
    let charges = f.store.once::<String>("charges").open().unwrap();
    let declined = charges.run("req-1", || Err(Payment::Declined));
    assert!(matches!(declined, Err(Payment::Declined)));
    let refused = charges.under("").run("req-1", || Ok::<_, Payment>("never".to_owned()));
    assert!(matches!(refused, Err(Payment::Store(error)) if error.kind() == ErrorKind::Invalid));
}

#[test]
fn a_run_of_a_key_inside_its_own_run_fails_rather_than_waits() {
    let f = fixture();
    let imports = f.store.once::<i64>("imports").open().unwrap();
    let inner = imports.run("file-1", || imports.run("file-1", || Ok::<_, Error>(1)));
    let error = inner.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert_eq!(imports.run("file-1", || Ok::<_, Error>(2)).unwrap(), 2, "the outer run let its key go");
}
