use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker, ready};

use futures_core::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::queue::Queue;
use crate::Result;
use crate::jobs::{Job, Report, Seen, State, Subscription};
use crate::sync::lock;

impl<V> Queue<V>
where
    V: Serialize + DeserializeOwned + Clone + Send + 'static,
{
    /// The job under `id` as it is, then again each time its state, place,
    /// attempt, time, progress or error changes, as a [`Stream`]; it ends
    /// with the job: done, failed or cancelled, the last it yields. An id
    /// with no job yields nothing.
    ///
    /// Each commit of the queue wakes the watch, which reads its job again on
    /// the store's pool, one read at a time, and once more after a read when
    /// a change came meanwhile. Unlike the blocking watch it does not look
    /// again every minute: a job whose time comes is seen when a worker takes
    /// it. Dropped, it stops watching, and a read under way stops.
    pub fn watch(&self, id: impl AsRef<str>) -> Watch<V> {
        let woken = Arc::new(Woken { state: Mutex::new((true, None)) });
        Watch {
            queue: self.clone(),
            key: id.as_ref().to_owned(),
            woken,
            sub: None,
            seen: Some(Seen::default()),
            reading: None,
            over: false,
        }
    }
}

/// A job watched: a [`Stream`] of the job as it is, then again each time it
/// changes, until it ends.
pub struct Watch<V> {
    queue: Queue<V>,
    key: String,
    woken: Arc<Woken>,
    /// The watch's place among the queue's watchers, taken at its first poll.
    sub: Option<Arc<Subscription>>,
    /// What it reported last; none while a read holds it.
    seen: Option<Seen<V>>,
    reading: Option<Reading<V>>,
    over: bool,
}

/// A read of the watched job on the store's pool, which hands back what the
/// watch saw with what it read.
type Reading<V> = Pin<Box<dyn Future<Output = Result<(Seen<V>, Result<Report<V>>)>> + Send>>;

/// Whether a change came since the watch last read, and the waker of the
/// task that polls it.
struct Woken {
    state: Mutex<(bool, Option<Waker>)>,
}

impl Woken {
    fn raise(&self) {
        let waker = {
            let mut state = lock(&self.state);
            state.0 = true;
            state.1.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Lowers the flag and says it was raised; else keeps `waker` to wake.
    fn lower_or_wait(&self, waker: &Waker) -> bool {
        let mut state = lock(&self.state);
        if std::mem::replace(&mut state.0, false) {
            return true;
        }
        state.1 = Some(waker.clone());
        false
    }
}

/// No field is pinned: the read under way is boxed.
impl<V> Unpin for Watch<V> {}

impl<V> Stream for Watch<V>
where
    V: Serialize + DeserializeOwned + Clone + Send + 'static,
{
    type Item = Result<Job<V>>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let watch = self.get_mut();
        loop {
            if watch.over {
                return Poll::Ready(None);
            }
            if let Some(reading) = &mut watch.reading {
                let read = ready!(reading.as_mut().poll(context));
                watch.reading = None;
                if let Some(report) = watch.reported(read) {
                    return Poll::Ready(report);
                }
            }
            if !watch.woken.lower_or_wait(context.waker()) {
                return Poll::Pending;
            }
            watch.reading = Some(watch.read());
        }
    }
}

impl<V> Watch<V>
where
    V: Serialize + DeserializeOwned + Clone + Send + 'static,
{
    /// Reads the job again on the store's pool, the watch subscribed first so
    /// that no change after the read goes unheard.
    fn read(&mut self) -> Reading<V> {
        let sub = match &self.sub {
            Some(sub) => Arc::clone(sub),
            None => {
                let woken = Arc::clone(&self.woken);
                let sub =
                    Arc::new(Subscription::new(&self.queue.queue.state, &self.key, Arc::new(move || woken.raise())));
                Arc::clone(self.sub.insert(sub))
            }
        };
        let (queue, key, mut seen) = (self.queue.queue.clone(), self.key.clone(), self.seen.take().unwrap_or_default());
        let what = format!("{queue:?}: id {key:?}: a watch");
        Box::pin(crate::aio::run::read(&self.queue.pool, what, move || {
            let report = seen.read(&queue, &key, &sub);
            Ok((seen, report))
        }))
    }

    /// What a read gives the stream: a job, its end or an error, or nothing
    /// yet when the job shows the same.
    fn reported(&mut self, read: Result<(Seen<V>, Result<Report<V>>)>) -> Option<Option<Result<Job<V>>>> {
        let report = read.and_then(|(seen, report)| {
            self.seen = Some(seen);
            report
        });
        match report {
            Ok(Report::Same) => None,
            Ok(Report::Changed(job)) => {
                self.over = matches!(job.state, State::Done | State::Failed | State::Cancelled);
                Some(Some(Ok(job)))
            }
            Ok(Report::Over) => {
                self.over = true;
                Some(None)
            }
            Err(error) => {
                self.over = true;
                Some(Some(Err(error)))
            }
        }
    }
}

impl<V> fmt::Debug for Watch<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Watch({:?}: id {:?})", self.queue, self.key)
    }
}
