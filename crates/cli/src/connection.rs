//! One client over a byte stream: what it sends goes to a pipe on the store,
//! and one writer takes every frame the pipe owes it, so that frames leave in
//! the order they were made. A socket, a named pipe and stdio all go here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tinystore::Store;
use tinystore::pipe::{Connect, Pipe};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, watch};
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
    let ready = Arc::new(Notify::new());
    let woken = Arc::clone(&ready);
    let connect = Connect { wake: Some(Arc::new(move || woken.notify_one())), ..connect };
    let pipe = match Pipe::connect(store, connect) {
        Ok(pipe) => Arc::new(pipe),
        Err(error) => return tracing::warn!(target: "tinystore", %error, "a connection did not open"),
    };
    let done = Arc::new(AtomicBool::new(false));
    let writing = tokio::spawn(write_frames(Arc::clone(&pipe), writer, Arc::clone(&ready), Arc::clone(&done)));
    read_frames(reader, &pipe, closing).await;
    drain(&pipe).await;
    done.store(true, Ordering::Release);
    ready.notify_one();
    let _ = writing.await;
}

/// Hands the client's bytes to the pipe until it leaves or breaks a rule; a
/// server closing tells it `GOAWAY` and reads on while its streams finish.
async fn read_frames<R: AsyncRead + Unpin>(mut reader: R, pipe: &Pipe, mut closing: watch::Receiver<bool>) {
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
                Ok(read) => pipe.push(&buffer[..read]),
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

/// Writes what the pipe owes each time it wakes, until the connection is done.
async fn write_frames<W: AsyncWrite + Unpin>(
    pipe: Arc<Pipe>,
    mut writer: W,
    ready: Arc<Notify>,
    done: Arc<AtomicBool>,
) {
    loop {
        ready.notified().await;
        let owed = pipe.recv(Duration::ZERO);
        // stdout holds what it is given until a newline, which frames lack
        if !owed.is_empty() && (writer.write_all(&owed).await.is_err() || writer.flush().await.is_err()) {
            break;
        }
        if done.load(Ordering::Acquire) || pipe.finished() {
            break;
        }
    }
    let _ = writer.flush().await;
    let _ = writer.shutdown().await;
}
