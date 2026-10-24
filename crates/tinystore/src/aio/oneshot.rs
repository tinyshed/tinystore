//! A call's answer, handed from the thread that has it to the future that
//! awaits it: a one-shot that wakes the task polling it, on any executor.
//!
//! | the sender                       | the future                              |
//! |----------------------------------|-----------------------------------------|
//! | [`Answer::send`]                 | is ready with that answer               |
//! | [`Answer::panicked`]             | resumes the panic where it is awaited   |
//! | dropped unsent, a job never run  | is ready with `Closed`, naming the call |
//!
//! A future dropped first lets go of what is sent after it, and
//! [`Answer::is_awaited`] tells the sender so.

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use crate::sync::lock;
use crate::{Error, Result};

/// A sender and the future it answers. `what` names the call in the `Closed`
/// a sender dropped unsent answers: `kv bucket sessions: a get`.
pub(crate) fn pending<T>(what: impl Into<String>) -> (Answer<T>, Pending<T>) {
    let slot = Arc::new(Mutex::new(State::Waiting(None)));
    (Answer { slot: Some(Arc::clone(&slot)), what: what.into() }, Pending { slot })
}

type Slot<T> = Arc<Mutex<State<T>>>;

enum State<T> {
    /// No answer yet, and the waker of the task that polled last.
    Waiting(Option<Waker>),
    Answered(Result<T>),
    Panicked(Box<dyn Any + Send>),
    /// The future took its answer.
    Taken,
    /// The future was dropped: nobody takes what comes.
    Gone,
}

/// The sending half: what has a call's answer gives it here, once.
pub(crate) struct Answer<T> {
    /// None once sent.
    slot: Option<Slot<T>>,
    what: String,
}

impl<T> Answer<T> {
    pub(crate) fn send(mut self, answer: Result<T>) {
        self.fill(State::Answered(answer));
    }

    /// Hands a panic of the call's code to the future, which resumes it.
    pub(crate) fn panicked(mut self, payload: Box<dyn Any + Send>) {
        self.fill(State::Panicked(payload));
    }

    /// Whether the future still waits: a read whose future is gone need not
    /// run at all.
    pub(crate) fn is_awaited(&self) -> bool {
        self.slot.as_ref().is_some_and(|slot| !matches!(*lock(slot), State::Gone))
    }

    fn fill(&mut self, answer: State<T>) {
        let Some(slot) = self.slot.take() else { return };
        let mut state = lock(&slot);
        let waker = match std::mem::replace(&mut *state, answer) {
            State::Waiting(waker) => waker,
            // what a future dropped first would have taken is dropped here, outside the lock
            gone => {
                let unread = std::mem::replace(&mut *state, gone);
                drop(state);
                return drop(unread);
            }
        };
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Drop for Answer<T> {
    /// A job dropped unrun, by a pool that stopped as its store closed,
    /// answers its future rather than leave it waiting for ever.
    fn drop(&mut self) {
        if self.slot.is_some() {
            let closed = Error::closed(format!("{}: the store closed before it answered", self.what));
            self.fill(State::Answered(Err(closed)));
        }
    }
}

impl<T> fmt::Debug for Answer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Answer({})", self.what)
    }
}

/// The future of a call's answer.
pub(crate) struct Pending<T> {
    slot: Slot<T>,
}

impl<T> Future for Pending<T> {
    type Output = Result<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<T>> {
        let mut state = lock(&self.slot);
        match std::mem::replace(&mut *state, State::Taken) {
            State::Answered(answer) => Poll::Ready(answer),
            State::Panicked(payload) => {
                drop(state);
                std::panic::resume_unwind(payload)
            }
            State::Waiting(waker) => {
                let waker = match waker {
                    Some(waker) if waker.will_wake(context.waker()) => waker,
                    _ => context.waker().clone(),
                };
                *state = State::Waiting(Some(waker));
                Poll::Pending
            }
            State::Taken | State::Gone => {
                Poll::Ready(Err(Error::internal("a call's answer was awaited after it was taken")))
            }
        }
    }
}

impl<T> Drop for Pending<T> {
    fn drop(&mut self) {
        let unread = std::mem::replace(&mut *lock(&self.slot), State::Gone);
        drop(unread);
    }
}

impl<T> fmt::Debug for Pending<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Pending")
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::time::Duration;

    use super::*;
    use crate::ErrorKind;

    #[tokio::test]
    async fn an_answer_sent_from_another_thread_wakes_the_future() {
        let (answer, answered) = pending::<u32>("a test's call");
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            answer.send(Ok(7));
        });
        assert_eq!(answered.await.unwrap(), 7);
        sender.join().unwrap();
    }

    #[tokio::test]
    async fn a_sender_dropped_unsent_answers_closed_naming_the_call() {
        let (answer, answered) = pending::<u32>("kv bucket sessions: a get");
        drop(answer);
        let closed = answered.await.unwrap_err();
        assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
        assert!(closed.to_string().contains("kv bucket sessions: a get"), "{closed}");
    }

    #[tokio::test]
    async fn a_panic_sent_resumes_where_the_answer_is_awaited() {
        let (answer, answered) = pending::<u32>("a test's call");
        answer.panicked(Box::new("the call's code panicked"));
        let resumed = tokio::spawn(answered).await.unwrap_err();
        assert!(resumed.is_panic());
        let payload = resumed.into_panic();
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"the call's code panicked"));
    }

    #[test]
    fn what_is_sent_once_the_future_is_gone_is_let_go() {
        let (answer, answered) = pending::<Arc<()>>("a test's call");
        drop(answered);
        let sent = Arc::new(());
        answer.send(Ok(Arc::clone(&sent)));
        assert_eq!(Arc::strong_count(&sent), 1, "nobody holds what no future takes");
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            let (answer, answered) = pending::<u32>("a test's call");
            drop(answered);
            answer.panicked(Box::new("nobody resumes it"));
        }));
        assert!(unwound.is_ok(), "a panic sent to a future that is gone is let go");
    }
}
