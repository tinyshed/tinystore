//! One client over a byte stream: what it sends goes to a pipe on the store,
//! and the frames the pipe owes it leave in the order they were made. A
//! socket, a named pipe and stdio all go here.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use tinystore::Store;
use tinystore::pipe::{Connect, Pipe};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, Notify, watch};
use tokio::time::Instant;

/// How long a closing server, or a client gone, waits for the streams running.
const DRAIN: Duration = Duration::from_secs(10);
/// How long a client may take to say HELLO before its connection ends.
const HELLO_TIME: Duration = Duration::from_secs(5);
/// How often draining looks whether the streams are done.
const DRAIN_TICK: Duration = Duration::from_millis(10);

/// Serves one client until it leaves, breaks the protocol, or the server
/// closes; then the streams running finish and what they answer leaves.
pub(crate) async fn serve<R, W>(reader: R, writer: W, store: &Store, connect: Connect, closing: watch::Receiver<bool>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let output = Arc::new(Output::new(writer));
    let woken = Arc::clone(&output);
    let connect = Connect { wake: Some(Arc::new(move || woken.wake())), ..connect };
    let pipe = match Pipe::connect(store, connect) {
        Ok(pipe) => Arc::new(pipe),
        Err(error) => return tracing::warn!(target: "tinystore", %error, "a connection did not open"),
    };
    let done = Arc::new(AtomicBool::new(false));
    let writing = tokio::spawn(write_frames(Arc::clone(&pipe), Arc::clone(&output), Arc::clone(&done)));
    read_frames(reader, &pipe, &output, closing).await;
    drain(&pipe).await;
    done.store(true, Ordering::Release);
    output.ready.notify_one();
    let _ = writing.await;
}

/// The reader hands the pipe no bytes.
const IDLE: u8 = 0;
/// The reader hands the pipe the bytes it read: a wake meanwhile is its own.
const HANDING: u8 = 1;
/// The reader hands the pipe its bytes, and frames became ready meanwhile.
const HANDING_WOKEN: u8 = 2;

/// Where the frames the pipe owes leave: the connection's writer, which the
/// reader writes to at once when it can, and a task that writes the rest.
///
/// A point read is answered while the reader hands the pipe its request,
/// and the reader writes its answer there and then: handed to the writer
/// task, each answer cost the task's wake. The reader only tries, so that a
/// client slow to read stops nothing it sends; whatever the writer does not
/// take at once is the writer task's.
struct Output<W> {
    writing: Mutex<Writing<W>>,
    ready: Notify,
    handing: AtomicU8,
}

struct Writing<W> {
    writer: W,
    /// What the reader's write left of the frames it took: it leaves before
    /// anything after it.
    left: Vec<u8>,
    /// The reader wrote frames it could not flush at once.
    unflushed: bool,
}

impl<W: AsyncWrite + Unpin> Output<W> {
    fn new(writer: W) -> Self {
        let writing = Mutex::new(Writing { writer, left: Vec::new(), unflushed: false });
        Output { writing, ready: Notify::new(), handing: AtomicU8::new(IDLE) }
    }

    /// The pipe's wake, from whichever thread made the frames: one that
    /// comes while the reader hands the pipe bytes is the reader's, which
    /// writes what they answered; any other wakes the writer task.
    fn wake(&self) {
        let taken = self.handing.compare_exchange(HANDING, HANDING_WOKEN, Ordering::AcqRel, Ordering::Acquire);
        if taken.is_err() {
            self.ready.notify_one();
        }
    }

    /// Hands the pipe what the reader read, then writes at once what that
    /// made ready.
    fn hand(&self, pipe: &Pipe, bytes: &[u8]) {
        self.handing.store(HANDING, Ordering::Release);
        pipe.push(bytes);
        if self.handing.swap(IDLE, Ordering::AcqRel) == HANDING_WOKEN {
            self.write_now(pipe);
        }
    }

    /// Writes what the pipe owes without waiting: what waits behind a write
    /// under way, and what the writer does not take at once, the writer task
    /// writes.
    fn write_now(&self, pipe: &Pipe) {
        let Ok(mut writing) = self.writing.try_lock() else {
            return self.ready.notify_one();
        };
        if !writing.left.is_empty() || writing.unflushed {
            return self.ready.notify_one();
        }
        let owed = pipe.recv(Duration::ZERO);
        let mut context = Context::from_waker(Waker::noop());
        let mut written = 0;
        while written < owed.len() {
            match Pin::new(&mut writing.writer).poll_write(&mut context, &owed[written..]) {
                Poll::Ready(Ok(taken)) if taken > 0 => written += taken,
                _ => break,
            }
        }
        // stdout holds what it is given until a newline, which frames lack
        let flushed = matches!(Pin::new(&mut writing.writer).poll_flush(&mut context), Poll::Ready(Ok(())));
        if written < owed.len() || !flushed {
            writing.left.extend_from_slice(&owed[written..]);
            writing.unflushed = true;
            self.ready.notify_one();
        }
    }
}

/// Hands the client's bytes to the pipe until it leaves or breaks a rule; a
/// server closing tells it `GOAWAY` and reads on while its streams finish.
async fn read_frames<R, W>(mut reader: R, pipe: &Pipe, output: &Output<W>, mut closing: watch::Receiver<bool>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0; 64 << 10];
    let hello_by = Instant::now() + HELLO_TIME;
    let mut going_away = *closing.borrow();
    if going_away {
        pipe.go_away();
    }
    loop {
        if pipe.finished() || (going_away && pipe.streams() == 0) {
            return;
        }
        tokio::select! {
            read = reader.read(&mut buffer) => match read {
                Ok(0) | Err(_) => return,
                Ok(read) => output.hand(pipe, &buffer[..read]),
            },
            changed = closing.changed(), if !going_away => {
                going_away = true;
                if changed.is_ok() {
                    pipe.go_away();
                }
            }
            () = tokio::time::sleep(DRAIN_TICK), if going_away => {}
            () = tokio::time::sleep_until(hello_by), if !pipe.welcomed() => return,
        }
    }
}

/// Waits for the streams running, so that a client that closed its side, as
/// a private child's parent does, still gets what it asked for.
async fn drain(pipe: &Pipe) {
    let deadline = Instant::now() + DRAIN;
    while pipe.streams() > 0 && !pipe.finished() && Instant::now() < deadline {
        tokio::time::sleep(DRAIN_TICK).await;
    }
}

/// Writes what the pipe owes each time it wakes, after what the reader's
/// write left, until the connection is done.
async fn write_frames<W: AsyncWrite + Unpin>(pipe: Arc<Pipe>, output: Arc<Output<W>>, done: Arc<AtomicBool>) {
    loop {
        output.ready.notified().await;
        // read before the frames are taken: every frame owed before the end
        // was said is taken by this pass, and none is left behind it
        let last = done.load(Ordering::Acquire) || pipe.finished();
        let mut writing = output.writing.lock().await;
        let mut owed = std::mem::take(&mut writing.left);
        owed.extend_from_slice(&pipe.recv(Duration::ZERO));
        if !owed.is_empty() || writing.unflushed {
            writing.unflushed = false;
            // stdout holds what it is given until a newline, which frames lack
            if writing.writer.write_all(&owed).await.is_err() || writing.writer.flush().await.is_err() {
                break;
            }
        }
        drop(writing);
        if last {
            break;
        }
    }
    let mut writing = output.writing.lock().await;
    let _ = writing.writer.flush().await;
    let _ = writing.writer.shutdown().await;
}
