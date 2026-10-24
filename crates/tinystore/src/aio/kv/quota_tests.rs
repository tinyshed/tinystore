use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};

use super::*;
use crate::kv::fixture::{HOUR, eventually, fixture};

fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

#[tokio::test]
async fn each_call_answers_as_its_blocking_twin_on_the_same_clock() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let opened = |name: &str| f.store.quota(name).window("session", 3, 5 * HOUR).window("weekly", 5, 168 * HOUR);
    let messages = Quota::from(opened("messages").open().unwrap());
    let blocking = opened("messages-blocking").open().unwrap();
    let built = store.quota("built").window("session", 3, 5 * HOUR).window("weekly", 5, 168 * HOUR).open().unwrap();

    for user in [42, 43] {
        assert_eq!(messages.allow(user).await.unwrap(), blocking.allow(user).unwrap());
        assert_eq!(messages.allow_n(user, 2).await.unwrap(), blocking.allow_n(user, 2).unwrap());
        assert_eq!(messages.allow(user).await.unwrap(), blocking.allow(user).unwrap(), "refused: nothing counted");
        assert_eq!(messages.peek(user).await.unwrap(), blocking.peek(user).unwrap());
    }
    messages.refund(42).await.unwrap();
    blocking.refund(42).unwrap();
    messages.refund_n(43, 2).await.unwrap();
    blocking.refund_n(43, 2).unwrap();
    assert_eq!(messages.peek(42).await.unwrap(), blocking.peek(42).unwrap());
    assert_eq!(messages.peek(43).await.unwrap(), blocking.peek(43).unwrap());
    f.clock.advance(6 * HOUR);
    assert_eq!(messages.under("eu").allow(42).await.unwrap(), blocking.under("eu").allow(42).unwrap());
    assert_eq!(messages.allow(42).await.unwrap(), blocking.allow(42).unwrap(), "a new session window");
    messages.reset(42).await.unwrap();
    blocking.reset(42).unwrap();
    assert_eq!(messages.peek(42).await.unwrap(), blocking.peek(42).unwrap());
    assert_eq!(built.allow(42).await.unwrap().left, 2, "a quota the aio store's builder opened");
    let past = messages.allow_n(42, 4).await.unwrap_err();
    assert_eq!(past.to_string(), messages.blocking().allow_n(42, 4).unwrap_err().to_string());
    f.store.close().unwrap();
}

// contract: DUR-11, STORE-10
#[test]
fn a_use_polled_once_queues_without_waiting_and_counts_though_its_future_is_dropped() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let messages = store.quota("messages").window("daily", 100, 24 * HOUR).open().unwrap();
    {
        let _writer = f.hold_writer();
        let mut used = pin!(messages.allow(42));
        assert!(poll_once(used.as_mut()).is_pending(), "queued at the writer without waiting for it");
    }
    eventually("the dropped use counted", || messages.blocking().peek(42).unwrap().left == 99);
    f.store.close().unwrap();
}
