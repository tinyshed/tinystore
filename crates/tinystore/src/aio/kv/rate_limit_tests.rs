use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};

use super::*;
use crate::kv::fixture::{SECOND, eventually, fixture};

fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

#[tokio::test]
async fn each_call_answers_as_its_blocking_twin_on_the_same_clock() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let api = store.rate_limit("api").rate(3, 3 * SECOND).burst(3).open().unwrap();
    let blocking = f.store.rate_limit("api-blocking").rate(3, 3 * SECOND).burst(3).open().unwrap();
    for user in ["42", "43"] {
        assert_eq!(api.allow(user).await.unwrap(), blocking.allow(user).unwrap());
        assert_eq!(api.allow_n(user, 2).await.unwrap(), blocking.allow_n(user, 2).unwrap());
        assert_eq!(api.allow(user).await.unwrap(), blocking.allow(user).unwrap(), "refused, with its retry");
        assert_eq!(api.peek(user).await.unwrap(), blocking.peek(user).unwrap());
    }
    f.clock.advance(SECOND);
    assert_eq!(api.allow("42").await.unwrap(), blocking.allow("42").unwrap());
    api.reset("43").await.unwrap();
    blocking.reset("43").unwrap();
    assert_eq!(api.under("eu").peek("43").await.unwrap(), blocking.under("eu").peek("43").unwrap());
    assert_eq!(api.peek("43").await.unwrap(), blocking.peek("43").unwrap());
    let past = api.allow_n("42", 4).await.unwrap_err();
    assert_eq!(past.kind(), blocking.allow_n("42", 4).unwrap_err().kind());
    f.store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_request_is_decided_in_memory_in_its_first_poll() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let api = store.rate_limit("api").rate(100, SECOND).open().unwrap();
    api.blocking().allow("42").unwrap();
    assert!(api.blocking().peek("43").unwrap().ok, "a read leaves its reader idle");
    let _writer = f.hold_writer();
    for user in ["42", "44"] {
        let mut allowed = pin!(api.allow(user));
        assert!(matches!(poll_once(allowed.as_mut()), Poll::Ready(Ok(ref answer)) if answer.ok), "{user}");
    }
    let mut peeked = pin!(api.peek("42"));
    assert!(matches!(poll_once(peeked.as_mut()), Poll::Ready(Ok(_))));
    let mut reset = pin!(api.reset("42"));
    assert!(matches!(poll_once(reset.as_mut()), Poll::Ready(Ok(()))));
    drop(_writer);
    f.store.close().unwrap();
}

// contract: DUR-11
#[test]
fn an_allow_whose_future_is_dropped_on_the_pool_counts_all_the_same() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let api = store.rate_limit("api").rate(2, 60 * SECOND).open().unwrap();
    api.blocking().buffer.bound(1);
    api.blocking().allow("41").unwrap();
    assert_eq!(api.blocking().peek("42").unwrap().left, 1, "a read leaves its reader idle");
    {
        let _writer = f.hold_writer();
        let mut allowed = pin!(api.allow("42"));
        assert!(poll_once(allowed.as_mut()).is_pending(), "memory holds its bound: the allow went to the pool");
        // dropped while the flush before the allow waits for the writer, let go after it
    }
    eventually("the dropped allow counted", || api.blocking().peek("42").unwrap().left == 0);
    f.store.close().unwrap();
}

#[tokio::test]
async fn a_rate_limit_crosses_to_the_blocking_api_and_back_as_the_same_limit() {
    let f = fixture();
    let blocking = f.store.rate_limit("api").rate(2, SECOND).open().unwrap();
    let api = RateLimit::from(blocking.clone());
    assert_eq!(api.allow("42").await.unwrap().left, 1);
    assert_eq!(blocking.allow("42").unwrap().left, 0, "one limit counts both");
    assert!(!api.blocking().allow("42").unwrap().ok);
    f.store.close().unwrap();
}
