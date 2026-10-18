use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::thread;

use super::*;
use crate::testing::{PATIENCE, holds_within};
use crate::{ErrorKind, Options};

fn opened() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    (dir, store)
}

/// What a handler kept of what it received.
type Kept<T> = Arc<Mutex<Vec<Message<T>>>>;

/// A handler that keeps what it receives, and what it keeps.
fn kept<T: Send + 'static>() -> (impl FnMut(Message<T>) + Send + 'static, Kept<T>) {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let keeping = Arc::clone(&messages);
    (move |message| keeping.lock().unwrap().push(message), messages)
}

fn values<T: Clone>(messages: &Kept<T>) -> Vec<T> {
    messages.lock().unwrap().iter().map(|message| message.value.clone()).collect()
}

#[test]
fn a_subscriber_receives_what_is_published_from_now_on_in_order() {
    let (_dir, store) = opened();
    let chat = store.channel::<String>("chat").open().unwrap();
    chat.publish(&"before anyone listened".to_owned()).unwrap();
    let (handler, received) = kept();
    let sub = chat.subscribe(handler).unwrap();
    for text in ["a", "b", "c"] {
        chat.publish(&text.to_owned()).unwrap();
    }
    holds_within(PATIENCE, "three messages", || received.lock().unwrap().len() == 3);
    assert_eq!(values(&received), ["a", "b", "c"]);
    sub.stop();
    chat.publish(&"after the stop".to_owned()).unwrap();
    thread::sleep(Duration::from_millis(20));
    assert_eq!(received.lock().unwrap().len(), 3);

    let mut each = chat.each().unwrap();
    chat.publish(&"to the loop".to_owned()).unwrap();
    assert_eq!(each.next().unwrap().unwrap().value, "to the loop");
}

#[test]
fn a_subscription_takes_its_exact_branch_and_recursive_the_branches_below_too() {
    let (_dir, store) = opened();
    let orders = store.channel::<u32>("orders").open().unwrap();
    let (exact, eu) = kept();
    let (below, eu_and_below) = kept();
    let (root, root_only) = kept();
    let subs = [
        orders.under("eu").subscribe(exact).unwrap(),
        orders.under("eu").recursive().subscribe(below).unwrap(),
        orders.subscribe(root).unwrap(),
    ];
    orders.publish(&1).unwrap();
    orders.under("eu").publish(&2).unwrap();
    orders.under("eu").under("shop-7").publish(&3).unwrap();
    orders.under("us").publish(&4).unwrap();
    holds_within(PATIENCE, "the branch and below", || eu_and_below.lock().unwrap().len() == 2);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(values(&eu), [2]);
    assert_eq!(values(&eu_and_below), [2, 3]);
    assert_eq!(values(&root_only), [1], "the root without recursive hears the root alone");
    let branch = &eu_and_below.lock().unwrap()[1].branch;
    assert_eq!(branch, &["eu", "shop-7"]);
    for sub in subs {
        sub.stop();
    }
}

#[test]
fn a_subscriber_that_falls_behind_misses_messages_counted_and_the_publisher_never_waits() {
    let (_dir, store) = opened();
    let ticks = store.channel::<usize>("ticks").open().unwrap();
    let missed = Arc::new(Mutex::new(Vec::new()));
    let counting = Arc::clone(&missed);
    let mut each = ticks.on_missed(move |missed: Missed| counting.lock().unwrap().push(missed.count)).each().unwrap();
    let started = Instant::now();
    for tick in 0..log::MOST_MESSAGES + 5 {
        ticks.publish(&tick).unwrap();
    }
    assert!(started.elapsed() < PATIENCE, "publishing waited for nobody");
    assert_eq!(each.next().unwrap().unwrap().value, 5, "the five oldest went past the log's bound");
    assert_eq!(*missed.lock().unwrap(), [5], "said once, before the next message");
}

#[cfg(all(feature = "kv", feature = "sql"))]
#[test]
fn a_message_of_a_transaction_is_handed_over_after_its_commit_and_never_after_a_rollback() {
    let (_dir, store) = opened();
    let events = store.channel::<String>("events").open().unwrap();
    let orders = store.bucket::<String>("orders").open().unwrap();
    let (handler, received) = kept();
    let sub = events.recursive().subscribe(handler).unwrap();

    let rolled_back = store.tx(|tx| -> Result<()> {
        tx.with(&orders).set("1", &"order".to_owned())?;
        tx.with(&events).under("1").publish(&"created 1".to_owned())?;
        Err(Error::invalid("the order is refused"))
    });
    assert!(rolled_back.is_err());
    let seen = Arc::clone(&received);
    store
        .tx(|tx| -> Result<()> {
            tx.with(&events).under("2").publish(&"created 2".to_owned())?;
            tx.with(&orders).set("2", &"order".to_owned())?;
            thread::sleep(Duration::from_millis(20));
            assert!(seen.lock().unwrap().is_empty(), "nothing before the commit");
            Ok(())
        })
        .unwrap();
    store.tx(|tx| tx.with(&events).publish(&"a transaction of no file".to_owned())).unwrap();

    let app = store
        .database("app")
        .migrations([("0001_notes.sql", "create table notes (id integer primary key)")])
        .open()
        .unwrap();
    let in_db = app.tx(|tx| -> Result<()> {
        tx.exec(crate::sql!("insert into notes (id) values (?)", 1))?;
        tx.with(&events).under("note").publish(&"noted".to_owned())
    });
    in_db.unwrap();
    holds_within(PATIENCE, "three handed over", || received.lock().unwrap().len() == 3);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(values(&received), ["created 2", "a transaction of no file", "noted"]);
    sub.stop();
}

#[test]
fn stop_from_inside_its_handler_answers_at_once_and_from_outside_waits_for_the_running_call() {
    let (_dir, store) = opened();
    let chat = store.channel::<u32>("chat").open().unwrap();
    let stopping: Arc<Mutex<Option<Subscription>>> = Arc::new(Mutex::new(None));
    let inside = Arc::clone(&stopping);
    let (handler, received) = kept::<u32>();
    let mut handler = handler;
    let sub = chat
        .subscribe(move |message: Message<u32>| {
            let last = message.value == 2;
            handler(message);
            if last && let Some(sub) = inside.lock().unwrap().as_ref() {
                sub.stop();
            }
        })
        .unwrap();
    *stopping.lock().unwrap() = Some(sub);
    for value in 1..=4 {
        chat.publish(&value).unwrap();
    }
    holds_within(PATIENCE, "two", || received.lock().unwrap().len() >= 2);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(values(&received), [1, 2], "no call after the stop");

    let (entered, running) = mpsc::channel();
    let (finish, finishing) = mpsc::channel::<()>();
    let finishing = Mutex::new(finishing);
    let slow = chat
        .subscribe(move |_| {
            entered.send(()).unwrap();
            finishing.lock().unwrap().recv_timeout(PATIENCE).unwrap();
        })
        .unwrap();
    chat.publish(&5).unwrap();
    running.recv_timeout(PATIENCE).unwrap();
    let stopper = thread::spawn(move || {
        slow.stop();
        Instant::now()
    });
    thread::sleep(Duration::from_millis(30));
    let released = Instant::now();
    finish.send(()).unwrap();
    assert!(stopper.join().unwrap() >= released, "stop waited for the running call");
}

#[test]
fn a_handler_that_panics_or_a_message_of_another_type_is_told_and_the_next_follows() {
    let (_dir, store) = opened();
    let numbers = store.channel::<u32>("numbers").open().unwrap();
    let texts = store.channel::<String>("numbers").open().unwrap();
    let failures = Arc::new(Mutex::new(Vec::new()));
    let failing = Arc::clone(&failures);
    let (mut handler, received) = kept::<u32>();
    let sub = numbers
        .on_error(move |error: Error| failing.lock().unwrap().push(error.kind()))
        .subscribe(move |message: Message<u32>| {
            assert_ne!(message.value, 2, "the handler panics on two");
            handler(message);
        })
        .unwrap();
    numbers.publish(&1).unwrap();
    numbers.publish(&2).unwrap();
    texts.publish(&"not a number".to_owned()).unwrap();
    numbers.publish(&3).unwrap();
    holds_within(PATIENCE, "the two numbers", || received.lock().unwrap().len() == 2);
    assert_eq!(values(&received), [1, 3]);
    assert_eq!(*failures.lock().unwrap(), [ErrorKind::Internal, ErrorKind::Corrupt]);
    sub.stop();
}

#[test]
fn closing_the_store_ends_a_loop_and_refuses_a_publish() {
    let (_dir, store) = opened();
    let chat = store.channel::<u32>("chat").open().unwrap();
    let mut each = chat.each().unwrap();
    let looping = thread::spawn(move || each.next().is_none());
    thread::sleep(Duration::from_millis(20));
    store.close().unwrap();
    assert!(looping.join().unwrap(), "the loop ends");
    assert_eq!(chat.publish(&1).unwrap_err().kind(), ErrorKind::Closed);

    let (_dir, store) = opened();
    let chat = store.channel::<u32>("chat").open().unwrap();
    let each = chat.each().unwrap();
    let stopper = each.stopper();
    let waiting = thread::spawn(move || each.into_iter().count());
    thread::sleep(Duration::from_millis(20));
    stopper.stop();
    assert_eq!(waiting.join().unwrap(), 0, "stopped from another thread");
}

#[test]
fn names_branches_and_messages_are_checked() {
    let (_dir, store) = opened();
    assert_eq!(store.channel::<u32>("Chat").open().unwrap_err().kind(), ErrorKind::Invalid);
    let chat = store.channel::<String>("chat").open().unwrap();
    let long = "x".repeat(MOST_MESSAGE);
    let error = chat.publish(&long).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    let deep = chat.under("o".repeat(MOST_BRANCH + 1));
    assert_eq!(deep.publish(&"x".to_owned()).unwrap_err().kind(), ErrorKind::Invalid);
    let error = chat.under("room-7").publish(&long).unwrap_err();
    assert!(error.to_string().starts_with("channel chat: under \"room-7\""), "{error}");
}

#[test]
fn a_store_short_of_memory_drops_the_oldest_unread_messages_rather_than_refuse_a_publish() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options { background: false, memory: Some(100 << 10), ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    let photos = store.channel::<String>("photos").open().unwrap();
    let missed = Arc::new(Mutex::new(0));
    let counting = Arc::clone(&missed);
    let mut slow = photos.on_missed(move |missed: Missed| *counting.lock().unwrap() += missed.count).each().unwrap();
    let ten_kib = "p".repeat(10 << 10);
    for _ in 0..30 {
        photos.publish(&ten_kib).unwrap();
    }
    assert!(store.memory().used() <= 100 << 10, "the store's budget held");
    slow.next().unwrap().unwrap();
    let missed = *missed.lock().unwrap();
    assert!((20..30).contains(&missed), "the oldest went to make room: {missed} missed");
}

#[test]
fn no_call_of_the_running_batch_starts_once_stop_was_called() {
    let (_dir, store) = opened();
    let chat = store.channel::<u32>("chat").open().unwrap();
    let (entered, in_handler) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let calls = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&calls);
    let sub = Arc::new(
        chat.subscribe(move |_: Message<u32>| {
            counting.fetch_add(1, Ordering::SeqCst);
            let _ = entered.send(());
            let _ = released.lock().unwrap().recv();
        })
        .unwrap(),
    );
    for value in 0..5 {
        chat.publish(&value).unwrap();
    }
    in_handler.recv_timeout(PATIENCE).unwrap();
    let stopping = Arc::clone(&sub);
    let stopped = thread::spawn(move || stopping.stop());
    thread::sleep(Duration::from_millis(50));
    drop(release);
    stopped.join().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the batch's other four calls never start");
}

#[test]
fn a_stopped_loop_answers_none_though_its_last_batch_held_more() {
    let (_dir, store) = opened();
    let chat = store.channel::<u32>("chat").open().unwrap();
    let mut each = chat.each().unwrap();
    for value in 0..3 {
        chat.publish(&value).unwrap();
    }
    assert_eq!(each.next().unwrap().unwrap().value, 0);
    each.stopper().stop();
    assert!(each.next().is_none());
}
