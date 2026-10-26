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
/// The bytes a connection's writer takes from its pipe at once.
const WRITE_ROOM: usize = 256 << 10;

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
    pipe.client_closed();
    drain(&pipe, &output).await;
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
    /// The writer task took what the pipe owed, or left: a reader that
    /// stopped while the pipe was full looks again.
    drained: Notify,
    /// The writer task left, its client gone: a reader that waits for it to
    /// take what the pipe owes would wait for ever, so it reads no more.
    gone: AtomicBool,
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
        let (drained, gone) = (Notify::new(), AtomicBool::new(false));
        Output { writing, ready: Notify::new(), drained, gone, handing: AtomicU8::new(IDLE) }
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
/// While the pipe is full it reads nothing, until the writer task took what
/// the pipe owed: a client that sends and never reads holds the server's
/// memory at the pipe's bound, its sends waiting in its own socket.
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
        if pipe.finished() || (going_away && pipe.streams() == 0) || output.gone.load(Ordering::Acquire) {
            return;
        }
        let full = pipe.full();
        tokio::select! {
            read = reader.read(&mut buffer), if !full => match read {
                Ok(0) | Err(_) => return,
                Ok(read) => output.hand(pipe, &buffer[..read]),
            },
            () = output.drained.notified(), if full => {}
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
/// a private child's parent and a Node socket's `end()` do, still gets what it
/// asked for; the streams only its cancel would end were ended as it closed.
/// A client gone takes nothing more, and the calls that wait for it to read
/// never start.
async fn drain<W>(pipe: &Pipe, output: &Output<W>) {
    let deadline = Instant::now() + DRAIN;
    while pipe.streams() > 0 && !pipe.finished() && !output.gone.load(Ordering::Acquire) && Instant::now() < deadline {
        tokio::time::sleep(DRAIN_TICK).await;
    }
}

/// Writes what the pipe owes each time it wakes, after what the reader's
/// write left, until the connection is done.
async fn write_frames<W: AsyncWrite + Unpin>(pipe: Arc<Pipe>, output: Arc<Output<W>>, done: Arc<AtomicBool>) {
    // what the pipe owes is copied here, a room at a time, rather than into
    // bytes of its own: a file's mebibyte is not a fresh mebibyte each time
    let mut room = vec![0; WRITE_ROOM];
    'woken: loop {
        output.ready.notified().await;
        // read before the frames are taken: every frame owed before the end
        // was said is taken by this pass, and none is left behind it
        let last = done.load(Ordering::Acquire) || pipe.finished();
        let mut writing = output.writing.lock().await;
        let left = std::mem::take(&mut writing.left);
        let mut wrote = std::mem::take(&mut writing.unflushed) || !left.is_empty();
        if !left.is_empty() && writing.writer.write_all(&left).await.is_err() {
            break;
        }
        loop {
            // a room filled may have left more, which no wake announces
            let taken = pipe.recv_into(Duration::ZERO, &mut room);
            output.drained.notify_one();
            if taken > 0 && writing.writer.write_all(&room[..taken]).await.is_err() {
                break 'woken;
            }
            wrote |= taken > 0;
            if taken < room.len() {
                break;
            }
        }
        // stdout holds what it is given until a newline, which frames lack
        if wrote && writing.writer.flush().await.is_err() {
            break;
        }
        drop(writing);
        if last {
            break;
        }
    }
    let mut writing = output.writing.lock().await;
    let _ = writing.writer.flush().await;
    let _ = writing.writer.shutdown().await;
    output.gone.store(true, Ordering::Release);
    output.drained.notify_one();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tinystore::pipe::Connect;
    use tinystore::{Options, Store};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::watch;

    /// A frame as docs/wire.md spells it, on stream 0: its header, then its body.
    fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut bytes = u32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
        bytes.extend_from_slice(&[kind, 0, 0, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(body);
        bytes
    }

    /// A PING's PONG counts against no credit: a client that sends them and
    /// reads nothing finds its sends wait once the pipe is full, and every
    /// PONG comes once it reads.
    // contract: WIRE-03
    #[test]
    fn a_client_that_sends_and_never_reads_is_read_no_further_than_the_pipes_bound() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        runtime().block_on(async {
            let (mut from_server, to_server, serving) = welcomed(&store).await;
            let (sending, sent, total) = pinging(to_server);
            let taken = sent_until_held(&sent).await;
            assert!(taken < total, "the server read all {total} bytes of a client that reads nothing");
            assert!(taken <= (8 << 20) + (1 << 20), "the server read {taken} bytes");

            let mut received = 0;
            let mut room = vec![0; 64 << 10];
            while received < total {
                let read = from_server.read(&mut room).await.unwrap();
                assert!(read > 0, "{received} of {total} bytes of PONGs, and the connection ended");
                received += read;
            }
            assert_eq!(received, total, "a PONG for every PING");
            let to_server = sending.await.unwrap();
            drop((to_server, from_server));
            serving.await.unwrap();
        });
        store.close().unwrap();
    }

    /// A client held at the pipe's bound that leaves: its connection ends,
    /// though the server reads nothing of it while it waits to write.
    #[test]
    fn a_client_held_at_the_pipes_bound_that_leaves_ends_its_connection() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        runtime().block_on(async {
            let (from_server, to_server, serving) = welcomed(&store).await;
            let (sending, sent, total) = pinging(to_server);
            assert!(sent_until_held(&sent).await < total, "the server held the client");
            sending.abort();
            let _ = sending.await;
            drop(from_server);
            let ended = tokio::time::timeout(Duration::from_secs(10), serving).await;
            assert!(ended.is_ok(), "the connection outlived its client");
        });
        store.close().unwrap();
    }

    /// A client whose calls wait for it to read, that leaves: its connection
    /// ends at once, rather than waiting for streams nobody will take.
    #[test]
    fn a_client_that_leaves_while_its_calls_wait_for_it_to_read_ends_its_connection_at_once() {
        const OPEN: u16 = 0x0101;
        const GET: u16 = 0x0102;
        const SET: u16 = 0x0104;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        runtime().block_on(async {
            let (mut from_server, mut to_server, serving) = welcomed(&store).await;
            // kv.bucket.open {name: "files"}
            let open = [0x81, 0x01, 0xa5, b'f', b'i', b'l', b'e', b's'];
            to_server.write_all(&request(1, OPEN, &open)).await.unwrap();
            let opened = response(&mut from_server).await;
            assert_eq!((opened.len(), &opened[..2]), (3, &[0x81, 0x01][..]), "a handle of one byte");
            let handle = opened[2];
            // kv.set {handle, key: "k", value: a mebibyte}
            let mut set = vec![0x83, 0x01, handle, 0x03, 0xa1, b'k', 0x04, 0xc6];
            set.extend_from_slice(&(1u32 << 20).to_be_bytes());
            set.resize(set.len() + (1 << 20), 7);
            to_server.write_all(&request(2, SET, &set)).await.unwrap();
            response(&mut from_server).await;
            // kv.get {handle, key: "k"}, 64 in one write, their answers never read
            let get = [0x82, 0x01, handle, 0x03, 0xa1, b'k'];
            let gets: Vec<u8> = (1000..1064).flat_map(|stream| request(stream, GET, &get)).collect();
            to_server.write_all(&gets).await.unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
            drop((to_server, from_server));
            let ended = tokio::time::timeout(Duration::from_secs(5), serving).await;
            assert!(ended.is_ok(), "the connection waited for answers its client could not take");
        });
        store.close().unwrap();
    }

    /// A REQUEST of `method` on `stream`, the client's only frame there.
    /// A client that closes its side while it listens on a channel: the
    /// subscription, which only its cancel would end, ends with a last frame
    /// it still reads, and the connection ends at once rather than after the
    /// drain's ten seconds.
    #[test]
    fn a_client_that_closes_its_side_while_it_listens_ends_its_connection_at_once() {
        const SUBSCRIBE: u16 = 0x0a02;
        const END: u8 = 1;
        const ERROR: u8 = 2;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        runtime().block_on(async {
            let (mut from_server, mut to_server, serving) = welcomed(&store).await;
            // channels.subscribe {name: "chat", under: [], recursive: false}
            let subscribe = [0x83, 0x01, 0xa4, b'c', b'h', b'a', b't', 0x02, 0x90, 0x03, 0xc2];
            to_server.write_all(&request(1, SUBSCRIBE, &subscribe)).await.unwrap();
            let mut head = [0u8; 12];
            from_server.read_exact(&mut head).await.unwrap();
            assert_eq!(head[5] & END, 0, "listening, its stream open");
            let mut answer = vec![0; usize::try_from(u32::from_le_bytes(head[..4].try_into().unwrap())).unwrap()];
            from_server.read_exact(&mut answer).await.unwrap();
            to_server.shutdown().await.unwrap();
            let began = std::time::Instant::now();
            let ended = tokio::time::timeout(Duration::from_secs(5), serving).await;
            assert!(ended.is_ok() && began.elapsed() < Duration::from_secs(5), "{:?}", began.elapsed());
            let mut last = Vec::new();
            from_server.read_to_end(&mut last).await.unwrap();
            assert!(last.len() >= 12 && last[5] == END | ERROR, "the subscription's last frame: {last:?}");
            assert!(String::from_utf8_lossy(&last).contains("the client closed its side"));
        });
        store.close().unwrap();
    }

    fn request(stream: u32, method: u16, body: &[u8]) -> Vec<u8> {
        const REQUEST: u8 = 3;
        const END: u8 = 1;
        let mut bytes = u32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
        bytes.extend_from_slice(&[REQUEST, END]);
        bytes.extend_from_slice(&method.to_le_bytes());
        bytes.extend_from_slice(&stream.to_le_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    /// The body of the next frame, a RESPONSE that did not fail.
    async fn response(from_server: &mut Reads) -> Vec<u8> {
        const RESPONSE: u8 = 4;
        const END: u8 = 1;
        let mut head = [0u8; 12];
        from_server.read_exact(&mut head).await.unwrap();
        assert_eq!((head[4], head[5]), (RESPONSE, END), "a RESPONSE that did not fail");
        let mut body = vec![0; usize::try_from(u32::from_le_bytes(head[..4].try_into().unwrap())).unwrap()];
        from_server.read_exact(&mut body).await.unwrap();
        body
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
    }

    type Reads = tokio::io::ReadHalf<tokio::io::DuplexStream>;
    type Writes = tokio::io::WriteHalf<tokio::io::DuplexStream>;

    /// A client of `store` served over bytes in memory, its HELLO welcomed.
    async fn welcomed(store: &Store) -> (Reads, Writes, tokio::task::JoinHandle<()>) {
        const HELLO: u8 = 1;
        const WELCOME: u8 = 2;
        let (client, server) = tokio::io::duplex(64 << 10);
        let (server_reads, server_writes) = tokio::io::split(server);
        let serving = {
            let store = store.clone();
            tokio::spawn(async move {
                let (_closing, closed) = watch::channel(false);
                super::serve(server_reads, server_writes, &store, Connect::default(), closed).await;
            })
        };
        let (mut from_server, mut to_server) = tokio::io::split(client);
        to_server.write_all(&frame(HELLO, &[0x81, 0x01, 0x01])).await.unwrap();
        let mut head = [0u8; 12];
        from_server.read_exact(&mut head).await.unwrap();
        assert_eq!(head[4], WELCOME);
        let mut welcome = vec![0; usize::try_from(u32::from_le_bytes(head[..4].try_into().unwrap())).unwrap()];
        from_server.read_exact(&mut welcome).await.unwrap();
        (from_server, to_server, serving)
    }

    /// Sends PINGs, 16 MB of them, counting the bytes the server took.
    fn pinging(mut to_server: Writes) -> (tokio::task::JoinHandle<Writes>, Arc<AtomicUsize>, usize) {
        const PING: u8 = 8;
        let pings: Vec<u8> = (0..800_000).flat_map(|_| frame(PING, &[7; 8])).collect();
        let total = pings.len();
        let sent = Arc::new(AtomicUsize::new(0));
        let counting = Arc::clone(&sent);
        let sending = tokio::spawn(async move {
            for chunk in pings.chunks(64 << 10) {
                to_server.write_all(chunk).await.unwrap();
                counting.fetch_add(chunk.len(), Ordering::SeqCst);
            }
            to_server
        });
        (sending, sent, total)
    }

    /// The bytes sent once they stop growing for half a second.
    async fn sent_until_held(sent: &AtomicUsize) -> usize {
        let mut taken = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let now = sent.load(Ordering::SeqCst);
            if now == taken {
                return taken;
            }
            taken = now;
        }
    }
}
