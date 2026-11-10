//! Where a private child's frames leave: a blocking writer on its stdout's
//! handle, each write handed to tokio's blocking pool and written whole
//! before the next starts, through `stdout::write_whole` rather than the
//! `write_all` that `tokio::fs::File` writes with.

use std::io::{self, Write};
use std::mem;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};

use tinystore::stdout;
use tokio::io::AsyncWrite;
use tokio::task::JoinHandle;

/// The most of what it is given a write takes, copied for the blocking pool,
/// as `tokio::fs::File` took.
const HANDED: usize = 2 << 20;

/// A blocking writer written from tokio: a write takes what it is given at
/// once and writes it on the blocking pool, and the next write, a flush or a
/// shutdown waits for it. So a write polled without waiting, as the reader's
/// is, hands its bytes over or finds a write under way, and never blocks.
pub(crate) struct FramesOut<W> {
    writer: Arc<Mutex<W>>,
    /// The bytes the last write took, kept so that the next reuses them.
    spare: Vec<u8>,
    writing: Option<JoinHandle<(Vec<u8>, io::Result<()>)>>,
    /// Why writing stopped: every later call fails as that write did.
    failed: Option<io::Error>,
}

impl<W: Write + Send + 'static> FramesOut<W> {
    pub(crate) fn new(writer: W) -> Self {
        FramesOut { writer: Arc::new(Mutex::new(writer)), spare: Vec::new(), writing: None, failed: None }
    }

    /// Waits for the write under way; ready once none runs, or with the
    /// failure of the write that stopped the writing.
    fn poll_written(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Some(writing) = &mut self.writing {
            let done = ready!(Pin::new(writing).poll(context));
            self.writing = None;
            match done {
                Ok((spare, written)) => {
                    self.spare = spare;
                    self.failed = written.err();
                }
                Err(lost) => self.failed = Some(io::Error::other(lost)),
            }
        }
        Poll::Ready(self.failed.as_ref().map_or(Ok(()), |failed| Err(again(failed))))
    }

    /// Writes as much of `bytes` as one write takes on the blocking pool.
    fn hand(&mut self, bytes: &[u8]) -> usize {
        let taken = bytes.len().min(HANDED);
        let mut handed = mem::take(&mut self.spare);
        handed.clear();
        handed.extend_from_slice(&bytes[..taken]);
        let writer = Arc::clone(&self.writer);
        self.writing = Some(tokio::task::spawn_blocking(move || {
            let written = stdout::write_whole(&mut *writer.lock().unwrap_or_else(PoisonError::into_inner), &handed);
            (handed, written)
        }));
        taken
    }
}

impl<W: Write + Send + 'static> AsyncWrite for FramesOut<W> {
    fn poll_write(self: Pin<&mut Self>, context: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        ready!(this.poll_written(context))?;
        Poll::Ready(Ok(this.hand(bytes)))
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().poll_written(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().poll_written(context)
    }
}

/// The failure that stopped the writing, as each later call gives it.
fn again(failed: &io::Error) -> io::Error {
    match failed.raw_os_error() {
        Some(code) => io::Error::from_raw_os_error(code),
        None => io::Error::new(failed.kind(), failed.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    use tokio::io::{AsyncWrite, AsyncWriteExt};

    use super::FramesOut;

    /// A pipe that takes nothing on three writes of every four, and at most
    /// 1000 bytes on the fourth.
    struct Hesitant {
        taken: Arc<Mutex<Vec<u8>>>,
        writes: usize,
        refused: Arc<AtomicUsize>,
    }

    impl Write for Hesitant {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if !self.writes.is_multiple_of(4) {
                self.refused.fetch_add(1, Ordering::SeqCst);
                return Ok(0);
            }
            let taken = bytes.len().min(1000);
            self.taken.lock().unwrap().extend_from_slice(&bytes[..taken]);
            Ok(taken)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Frames of 10, 3000 and 20,000 bytes, each its own byte over.
    fn frames() -> Vec<Vec<u8>> {
        [10, 3000, 20_000].iter().zip(1u8..).map(|(&length, byte)| vec![byte; length]).collect()
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().build().unwrap()
    }

    #[test]
    fn a_write_that_takes_nothing_is_tried_again_and_the_frames_arrive_whole_and_in_order() {
        let (taken, refused) = (Arc::default(), Arc::default());
        let pipe = Hesitant { taken: Arc::clone(&taken), writes: 0, refused: Arc::clone(&refused) };
        let mut out = FramesOut::new(pipe);
        runtime().block_on(async {
            for frame in frames() {
                out.write_all(&frame).await.unwrap();
                out.flush().await.unwrap();
            }
        });
        assert_eq!(*taken.lock().unwrap(), frames().concat(), "every frame whole, in order");
        // 24 writes of 1000 bytes or fewer took them, each after three that took none
        assert_eq!(refused.load(Ordering::SeqCst), 72);
    }

    /// A pipe whose first write fails, as one whose reader is gone does.
    struct Gone;

    impl Write for Gone {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "the reader is gone"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_write_that_fails_gives_its_error_to_every_later_call() {
        let mut out = FramesOut::new(Gone);
        runtime().block_on(async {
            out.write_all(b"a frame").await.unwrap();
            let flushed = out.flush().await.unwrap_err();
            assert_eq!((flushed.kind(), flushed.to_string()), (io::ErrorKind::BrokenPipe, "the reader is gone".into()));
            let written = out.write(b"another").await.unwrap_err();
            assert_eq!((written.kind(), written.to_string()), (io::ErrorKind::BrokenPipe, "the reader is gone".into()));
            assert_eq!(out.shutdown().await.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        });
    }

    /// A pipe whose writes wait until the test lets each go.
    struct Held {
        go: mpsc::Receiver<()>,
        taken: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for Held {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.go.recv().unwrap();
            self.taken.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The reader's write, polled without waiting: it hands its bytes over at
    /// once, and finds the next write and the flush waiting on the first,
    /// rather than blocking on a pipe nobody reads.
    #[test]
    fn a_write_polled_without_waiting_hands_its_bytes_over_and_never_blocks() {
        let (go, waiting) = mpsc::channel();
        let taken = Arc::default();
        let mut out = FramesOut::new(Held { go: waiting, taken: Arc::clone(&taken) });
        let mut context = Context::from_waker(Waker::noop());
        runtime().block_on(async {
            let handed = Pin::new(&mut out).poll_write(&mut context, b"first");
            assert!(matches!(handed, Poll::Ready(Ok(5))), "{handed:?}");
            assert!(Pin::new(&mut out).poll_write(&mut context, b"second").is_pending());
            assert!(Pin::new(&mut out).poll_flush(&mut context).is_pending());
            go.send(()).unwrap();
            out.flush().await.unwrap();
        });
        assert_eq!(*taken.lock().unwrap(), b"first");
    }
}
