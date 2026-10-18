//! `tinystore serve`: the store opened under its `LOCK`, then served until a
//! client says `server.stop`, the server idles out, or Ctrl+C. Closing tells
//! every client `GOAWAY`, lets the streams running finish, removes `SERVE`,
//! and only then lets go of the directory.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tinystore::pipe::{Connect, Greeted, Stop};
use tinystore::{Clock, ErrorKind, Options, Store, TestClock};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;

use crate::args::{self, Transport};
use crate::banner::{self, Shown};
use crate::lines::{self, Lines, Terminal};
use crate::{connection, local, remote};

/// The exit of a server that found its directory held, by a server that
/// started first or one still letting go: its client reads `SERVE` again.
const HELD: u8 = 3;

/// How long a closing server waits for its clients to leave.
const LEAVING: Duration = Duration::from_secs(10);
/// How long a remote client may take over its TLS handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long a server waits for its first client before idling counts: as long
/// as a client waits for the sidecar it started.
const FIRST_CLIENT: Duration = Duration::from_secs(15);
/// How long a server waits before it takes clients again after one failed
/// to connect, so that a lack of handles does not spin it.
const AGAIN: Duration = Duration::from_millis(10);

/// The node the flags and the node's config make the store, its token read
/// from its file.
fn node_of(node: &args::Node) -> Result<tinystore::replication::Node, String> {
    let settings = node.settings();
    Ok(tinystore::replication::Node {
        from: node.from.clone(),
        serve: node.listen.clone(),
        files: settings.files.clone(),
        token: settings.token()?,
        synchronous: settings.synchronous,
        synchronous_with: settings.synchronous_with.clone(),
        tls: settings.node_tls(),
        catalog: settings.catalog,
        advertise: settings.advertise.clone(),
        failover: settings.failover.clone(),
        witness: false,
        insecure: settings.insecure,
    })
}

pub(crate) fn run(args: &[String]) -> ExitCode {
    let started = Instant::now();
    let serve = match args::serve(args) {
        Ok(serve) => serve,
        Err(refused) => {
            eprintln!("tinystore serve: {refused}");
            return ExitCode::from(2);
        }
    };
    let shown = Shown::of(&serve);
    // a remote server's tokens and certificate are read first, so that a
    // mistake in them leaves no files behind
    let remote = match serve.remote.map(remote::Ready::of).transpose() {
        Ok(remote) => remote,
        Err(refused) => {
            eprintln!("tinystore serve: {refused}");
            return ExitCode::FAILURE;
        }
    };
    let console = match Console::open(serve.log.as_deref(), serve.quiet) {
        Ok(console) => console,
        Err(error) => {
            eprintln!("tinystore serve: its log: {error}");
            return ExitCode::FAILURE;
        }
    };
    let clock = serve.clock.map(|start| Arc::new(TestClock::new(start)));
    let node = match serve.node.as_ref().map(node_of).transpose() {
        Ok(node) => node,
        Err(refused) => {
            console.fail(&refused);
            return ExitCode::FAILURE;
        }
    };
    let options = Options {
        memory: serve.memory,
        clock: clock.clone().map(|clock| clock as Arc<dyn Clock>),
        durability: serve.durability,
        keep_free: serve.keep_free.unwrap_or(Options::default().keep_free),
        encryption_key_file: serve.encryption_key_file.clone(),
        node,
        ..Options::default()
    };
    let store = match open(&serve.dir, options) {
        Ok(store) => store,
        Err((code, error)) => {
            console.fail(&error);
            return code;
        }
    };
    let served = tokio::runtime::Builder::new_multi_thread().enable_all().build().and_then(|runtime| {
        runtime.block_on(async {
            match serve.transport {
                Transport::Stdio => stdio(&store, clock).await,
                Transport::Local { sidecar } => {
                    let listening = Listening { sidecar, idle: serve.idle, console, started, shown };
                    listen(&store, listening, remote).await
                }
            }
        })
    });
    let closing = Instant::now();
    let closed = store.close();
    let failed = match (served, closed) {
        (Ok(()), Ok(())) => {
            tracing::info!(target: "tinystore", took = %format!("{:.1}s", closing.elapsed().as_secs_f64()), "stopped");
            return ExitCode::SUCCESS;
        }
        (Err(error), _) => error.to_string(),
        (_, Err(error)) => format!("the store did not close cleanly: {error}"),
    };
    console.fail(&failed);
    ExitCode::FAILURE
}

/// Opens the store, or says why not: a directory another holds exits with 3.
fn open(dir: &Path, options: Options) -> Result<Store, (ExitCode, String)> {
    Store::open(dir, options).map_err(|error| {
        let code = if error.kind() == ErrorKind::InUse { ExitCode::from(HELD) } else { ExitCode::FAILURE };
        (code, error.to_string())
    })
}

/// A private child: one client, the parent, on stdin and stdout. The end of
/// stdin asks the server to leave, once what was asked of it is answered.
async fn stdio(store: &Store, clock: Option<Arc<TestClock>>) -> io::Result<()> {
    let (closing, closed) = watch::channel(false);
    let connect = Connect { stop: Some(stop_by(closing)), clock, ..Connect::default() };
    connection::serve(tokio::io::stdin(), tokio::io::stdout(), store, connect, closed).await;
    Ok(())
}

/// How a local server runs: whether its clients started it, how long it
/// stays with none, and where it speaks.
struct Listening {
    shown: Shown,
    sidecar: bool,
    idle: Option<Duration>,
    console: Console,
    started: Instant,
}

/// A server for as many clients as come, until it is stopped, idles out or
/// hears Ctrl+C: on a socket or a named pipe that `SERVE` names, and on TCP
/// when `--listen` says.
async fn listen(store: &Store, listening: Listening, remote: Option<remote::Ready>) -> io::Result<()> {
    let Listening { shown, sidecar, idle, console, started } = listening;
    let server = local::server_dir(store.dir())?;
    local::unpublish(&server)?;
    let (listener, endpoint) = Listener::bind(store.dir(), &server)?;
    let mut endpoints = vec![endpoint];
    let remote = match remote {
        Some(ready) => {
            let tcp = remote::bind(&ready.address).await?;
            endpoints.push(ready.address.endpoint(tcp.local_addr()?.port()));
            Some((tcp, ready))
        }
        None => None,
    };
    let serve = local::Serve::new(endpoints, sidecar)?;
    let (closing, closed) = watch::channel(false);
    let stop = stop_by(closing.clone());
    let connect = Connect { prove: Some(serve.prove()), stop: Some(Arc::clone(&stop)), ..Connect::default() };
    local::publish(&server, &serve)?;
    console.serving(&shown, store.dir(), &serve.endpoints, sidecar, started.elapsed());
    let (clients, counted) = watch::channel(0usize);
    let clients = Arc::new(clients);
    let accepted = listener.accept_all(store, &connect, &closed, &clients);
    let accepted_remotely = async {
        let Some((tcp, ready)) = remote else {
            return std::future::pending::<io::Result<()>>().await;
        };
        let connect = Connect { stop: Some(stop), admit: Some(ready.admit), ..Connect::default() };
        accept_remote(tcp, ready.tls, store, &connect, &closed, &clients).await
    };
    let why = tokio::select! {
        accepted = accepted => return accepted,
        accepted = accepted_remotely => return accepted,
        () = stopped(closed.clone()) => "a client asked",
        () = interrupted() => "interrupted",
        () = idled(counted.clone(), idle) => "idle",
    };
    let connected = *counted.borrow();
    tracing::info!(target: "tinystore", why, clients = connected, "stopping");
    let _ = closing.send(true);
    left(counted).await;
    local::unpublish(&server)?;
    listener.remove();
    Ok(())
}

/// Takes remote clients, each over TLS when the address said so, admitted
/// by its token once its HELLO comes.
async fn accept_remote(
    listener: TcpListener,
    tls: Option<TlsAcceptor>,
    store: &Store,
    connect: &Connect,
    closed: &watch::Receiver<bool>,
    clients: &Arc<watch::Sender<usize>>,
) -> io::Result<()> {
    loop {
        // A client that failed to connect, or a process out of descriptors for
        // a moment, ends no server: the local endpoint retries the same way.
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                tracing::debug!(target: "tinystore", %error, "a remote client did not connect");
                tokio::time::sleep(AGAIN).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let visit = served(clients);
        let (store, connect, closed) = (store.clone(), visit.connect(connect), closed.clone());
        let tls = tls.clone();
        tokio::spawn(async move {
            match tls {
                None => {
                    let (reader, writer) = stream.into_split();
                    connection::serve(reader, writer, &store, connect, closed).await;
                }
                Some(tls) => match tokio::time::timeout(HANDSHAKE, tls.accept(stream)).await {
                    Ok(Ok(stream)) => {
                        let (reader, writer) = tokio::io::split(stream);
                        connection::serve(reader, writer, &store, connect, closed).await;
                    }
                    Ok(Err(error)) => tracing::debug!(target: "tinystore", %error, "a TLS handshake failed"),
                    Err(_) => tracing::debug!(target: "tinystore", "a TLS handshake took too long"),
                },
            }
            drop(visit);
        });
    }
}

fn stop_by(closing: watch::Sender<bool>) -> Stop {
    let closing = Mutex::new(closing);
    Arc::new(move || {
        let _ = closing.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send(true);
    })
}

async fn stopped(mut closed: watch::Receiver<bool>) {
    let _ = closed.wait_for(|closing| *closing).await;
}

pub(crate) async fn interrupted() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminated) => tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminated.recv() => {}
            },
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Returns once the server has had no client for `idle`, and never without
/// one. Before its first client it waits as long as a client waits for a
/// sidecar it started, so that one told to idle briefly is not gone first.
async fn idled(mut clients: watch::Receiver<usize>, idle: Option<Duration>) {
    let Some(idle) = idle else {
        return std::future::pending().await;
    };
    let mut waiting = idle.max(FIRST_CLIENT);
    loop {
        if clients.wait_for(|clients| *clients == 0).await.is_err() {
            return std::future::pending().await;
        }
        tokio::select! {
            () = tokio::time::sleep(waiting) => return,
            _ = clients.changed() => waiting = idle,
        }
    }
}

/// Waits for every client to leave, each told `GOAWAY` and its streams let
/// finish, for a while at most.
async fn left(mut clients: watch::Receiver<usize>) {
    let _ = tokio::time::timeout(LEAVING, clients.wait_for(|clients| *clients == 0)).await;
}

/// A client while it is served: counted in, and said once its HELLO named
/// it, and again when it leaves.
struct Visit {
    clients: Arc<watch::Sender<usize>>,
    client: Arc<Mutex<Option<String>>>,
}

/// Counts a client in while it is served.
fn served(clients: &Arc<watch::Sender<usize>>) -> Visit {
    clients.send_modify(|clients| *clients += 1);
    Visit { clients: Arc::clone(clients), client: Arc::default() }
}

impl Visit {
    /// The connection's options, with what says who came.
    fn connect(&self, connect: &Connect) -> Connect {
        let (clients, client) = (Arc::clone(&self.clients), Arc::clone(&self.client));
        let greeted: Greeted = Arc::new(move |name: &str| {
            *client.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(name.to_owned());
            let connected = *clients.borrow();
            tracing::info!(target: "tinystore", client = name, clients = connected, "a client connected");
        });
        Connect { greeted: Some(greeted), ..connect.clone() }
    }
}

impl Drop for Visit {
    fn drop(&mut self) {
        self.clients.send_modify(|clients| *clients -= 1);
        let client = self.client.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(client) = client {
            let connected = *self.clients.borrow();
            tracing::info!(target: "tinystore", client, clients = connected, "a client left");
        }
    }
}

#[cfg(unix)]
struct Listener {
    listener: tokio::net::UnixListener,
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Listener {
    /// Binds the store's socket, removing one a server gone left there: this
    /// server holds `LOCK`, so no other listens on it.
    fn bind(store: &Path, server: &Path) -> io::Result<(Listener, String)> {
        let path = local::socket_path(store, server)?;
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        let endpoint = format!("unix://{}", path.display());
        Ok((Listener { listener, path }, endpoint))
    }

    async fn accept_all(
        &self,
        store: &Store,
        connect: &Connect,
        closed: &watch::Receiver<bool>,
        clients: &Arc<watch::Sender<usize>>,
    ) -> io::Result<()> {
        loop {
            // a client that fails its connection fails it alone, and the clients served go on
            let stream = match self.listener.accept().await {
                Ok((stream, _)) => stream,
                Err(error) => {
                    tracing::debug!(target: "tinystore", %error, "a client did not connect");
                    tokio::time::sleep(AGAIN).await;
                    continue;
                }
            };
            let visit = served(clients);
            let (store, connect, closed) = (store.clone(), visit.connect(connect), closed.clone());
            tokio::spawn(async move {
                let (reader, writer) = stream.into_split();
                connection::serve(reader, writer, &store, connect, closed).await;
                drop(visit);
            });
        }
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
struct Listener {
    name: String,
    first: Mutex<Option<tokio::net::windows::named_pipe::NamedPipeServer>>,
}

#[cfg(windows)]
impl Listener {
    /// Creates the store's pipe as the first instance of its name, so that a
    /// process squatting on it is found here rather than by the clients.
    fn bind(store: &Path, _server: &Path) -> io::Result<(Listener, String)> {
        let short = local::pipe_name(store);
        let name = format!(r"\\.\pipe\{short}");
        let first = Self::instance(&name, true)?;
        Ok((Listener { name, first: Mutex::new(Some(first)) }, format!("pipe:{short}")))
    }

    fn instance(name: &str, first: bool) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(name)
    }

    async fn accept_all(
        &self,
        store: &Store,
        connect: &Connect,
        closed: &watch::Receiver<bool>,
        clients: &Arc<watch::Sender<usize>>,
    ) -> io::Result<()> {
        let mut waiting = self.first.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        loop {
            let pipe = match waiting.take() {
                Some(pipe) => pipe,
                None => self.next_instance().await?,
            };
            // a client that came and went before it was taken fails alone, and the clients served go on
            if let Err(error) = pipe.connect().await {
                tracing::debug!(target: "tinystore", %error, "a client did not connect");
                continue;
            }
            // the next instance is made before this one is served, so that a client never finds none listening
            waiting = Some(self.next_instance().await?);
            let visit = served(clients);
            let (store, connect, closed) = (store.clone(), visit.connect(connect), closed.clone());
            tokio::spawn(async move {
                let (reader, writer) = tokio::io::split(pipe);
                connection::serve(reader, writer, &store, connect, closed).await;
                drop(visit);
            });
        }
    }

    /// Another instance of the pipe, tried again six times, one to thirty-two
    /// milliseconds apart, before the server gives up its name.
    async fn next_instance(&self) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        for pause in [1, 2, 4, 8, 16, 32] {
            match Self::instance(&self.name, false) {
                Err(error) => {
                    tracing::debug!(target: "tinystore", %error, "a pipe instance was not made");
                    tokio::time::sleep(Duration::from_millis(pause)).await;
                }
                made => return made,
            }
        }
        Self::instance(&self.name, false)
    }

    fn remove(&self) {}
}

/// Where the server speaks: stderr, a person's short lines on a terminal and
/// JSON anywhere else, and a file of JSON lines when `--log-file` names one.
/// Never stdout, which a private child's frames travel on.
#[derive(Clone, Copy)]
pub(crate) struct Console {
    terminal: Terminal,
    to_file: bool,
}

impl Console {
    pub(crate) fn open(file: Option<&Path>, quiet: bool) -> io::Result<Console> {
        use tracing_subscriber::Layer as _;
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;

        let terminal = Terminal::of_stderr();
        let level = lines::level();
        let file = match file {
            Some(path) => {
                // a sidecar logs into <dir>/server/, which may not be there yet
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Some(OpenOptions::new().create(true).append(true).open(path)?)
            }
            None => None,
        };
        let to_file = file.is_some();
        // a sidecar's stderr goes nowhere once its lines go to a file
        let on_stderr = terminal.terminal || !to_file;
        let shown = if quiet { level.min(tracing::level_filters::LevelFilter::WARN) } else { level };
        // tracing colours a span's fields itself unless told not to: the lines paint their own
        let stderr = on_stderr.then(|| {
            let lines = Lines { colours: terminal.colours(), person: terminal.terminal };
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .event_format(lines)
                .with_writer(io::stderr)
                .with_filter(shown)
        });
        let kept = file.map(|file| {
            let lines = Lines { colours: false, person: false };
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .event_format(lines)
                .with_writer(Mutex::new(file))
                .with_filter(level)
        });
        tracing_subscriber::registry().with(stderr).with(kept).init();
        Ok(Console { terminal, to_file })
    }

    /// Whether a person reads stderr at a terminal.
    pub(crate) fn person(self) -> bool {
        self.terminal.terminal
    }

    pub(crate) fn colours(self) -> bool {
        self.terminal.colours()
    }

    /// Says why the server failed: in its log, and on stderr too when the log
    /// is a file, so that whoever started it sees it.
    fn fail(self, message: &str) {
        tracing::error!(target: "tinystore", "{message}");
        if self.to_file && !self.terminal.terminal {
            eprintln!("tinystore serve: {message}");
        }
    }

    /// Shows a person who started a server in a terminal what it serves and
    /// how to reach it; a log gets the same as a line.
    fn serving(self, shown: &Shown, store: &Path, endpoints: &[String], sidecar: bool, ready: Duration) {
        let banner = self.terminal.terminal && !sidecar;
        if !banner || self.to_file {
            tracing::info!(target: "tinystore", dir = %store.display(), endpoints = %endpoints.join(" "), "serving");
        }
        if banner {
            eprint!("{}", banner::text(self.terminal.colours(), shown, store, endpoints, ready));
        }
    }
}
