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

use nu_ansi_term::{Color, Style};

use tinystore::pipe::{Connect, Stop};
use tinystore::{Clock, ErrorKind, Options, Store, TestClock};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;

use crate::args::{self, Transport};
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

pub(crate) fn run(args: &[String]) -> ExitCode {
    let started = Instant::now();
    let serve = match args::serve(args) {
        Ok(serve) => serve,
        Err(refused) => {
            eprintln!("tinystore serve: {refused}");
            return ExitCode::from(2);
        }
    };
    // a remote server's tokens and certificate are read first, so that a
    // mistake in them leaves no files behind
    let remote = match serve.remote.map(remote::Ready::of).transpose() {
        Ok(remote) => remote,
        Err(refused) => {
            eprintln!("tinystore serve: {refused}");
            return ExitCode::FAILURE;
        }
    };
    let console = match Console::open(serve.log.as_deref()) {
        Ok(console) => console,
        Err(error) => {
            eprintln!("tinystore serve: its log: {error}");
            return ExitCode::FAILURE;
        }
    };
    let clock = serve.clock.map(|start| Arc::new(TestClock::new(start)));
    let store = match open(&serve.dir, clock.clone(), serve.memory) {
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
                    let listening = Listening { sidecar, idle: serve.idle, console, started };
                    listen(&store, listening, remote).await
                }
            }
        })
    });
    let closed = store.close();
    let failed = match (served, closed) {
        (Ok(()), Ok(())) => return ExitCode::SUCCESS,
        (Err(error), _) => error.to_string(),
        (_, Err(error)) => format!("the store did not close cleanly: {error}"),
    };
    console.fail(&failed);
    ExitCode::FAILURE
}

/// Opens the store, or says why not: a directory another holds exits with 3.
fn open(dir: &Path, clock: Option<Arc<TestClock>>, memory: Option<u64>) -> Result<Store, (ExitCode, String)> {
    let options = Options { clock: clock.map(|clock| clock as Arc<dyn Clock>), memory, ..Options::default() };
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
    sidecar: bool,
    idle: Option<Duration>,
    console: Console,
    started: Instant,
}

/// A server for as many clients as come, until it is stopped, idles out or
/// hears Ctrl+C: on a socket or a named pipe that `SERVE` names, and on TCP
/// when `--listen` says.
async fn listen(store: &Store, listening: Listening, remote: Option<remote::Ready>) -> io::Result<()> {
    let Listening { sidecar, idle, console, started } = listening;
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
    console.serving(store.dir(), &serve.endpoints, sidecar, started.elapsed());
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
    tracing::info!(target: "tinystore", why, "stopping");
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
        let (stream, _) = listener.accept().await?;
        let _ = stream.set_nodelay(true);
        let (store, connect, closed, counted) = (store.clone(), connect.clone(), closed.clone(), served(clients));
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
            drop(counted);
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

async fn interrupted() {
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

/// Counts a client in while it is served.
fn served(clients: &Arc<watch::Sender<usize>>) -> impl Drop + Send + 'static {
    struct Counted(Arc<watch::Sender<usize>>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.send_modify(|clients| *clients -= 1);
        }
    }
    clients.send_modify(|clients| *clients += 1);
    Counted(Arc::clone(clients))
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
            let (stream, _) = self.listener.accept().await?;
            let (store, connect, closed, counted) = (store.clone(), connect.clone(), closed.clone(), served(clients));
            tokio::spawn(async move {
                let (reader, writer) = stream.into_split();
                connection::serve(reader, writer, &store, connect, closed).await;
                drop(counted);
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
        let taken = self.first.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let mut waiting = match taken {
            Some(first) => first,
            None => Self::instance(&self.name, false)?,
        };
        loop {
            waiting.connect().await?;
            let pipe = std::mem::replace(&mut waiting, Self::instance(&self.name, false)?);
            let (store, connect, closed, counted) = (store.clone(), connect.clone(), closed.clone(), served(clients));
            tokio::spawn(async move {
                let (reader, writer) = tokio::io::split(pipe);
                connection::serve(reader, writer, &store, connect, closed).await;
                drop(counted);
            });
        }
    }

    fn remove(&self) {}
}

/// Where the server speaks: its log, a file's or stderr's, and the terminal
/// a person may read on stderr.
#[derive(Clone, Copy)]
struct Console {
    terminal: Terminal,
    to_file: bool,
}

impl Console {
    /// Logs to `file`, appended and plain, or to stderr, quietly coloured when
    /// a person reads it: never stdout, which a private child's frames travel on.
    fn open(file: Option<&Path>) -> io::Result<Console> {
        let terminal = Terminal::of_stderr();
        let level = lines::level();
        match file {
            Some(path) => {
                // a sidecar logs into <dir>/server/, which may not be there yet
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let file = OpenOptions::new().create(true).append(true).open(path)?;
                let lines = Lines { colours: false, person: false };
                tracing_subscriber::fmt()
                    .event_format(lines)
                    .with_max_level(level)
                    .with_writer(Mutex::new(file))
                    .init();
            }
            None => {
                let lines = Lines { colours: terminal.colours(), person: terminal.terminal };
                tracing_subscriber::fmt().event_format(lines).with_max_level(level).with_writer(io::stderr).init();
            }
        }
        Ok(Console { terminal, to_file: file.is_some() })
    }

    /// Says why the server failed: in its log, and on stderr too when the log
    /// is a file, so that whoever started it sees it.
    fn fail(self, message: &str) {
        tracing::error!(target: "tinystore", "{message}");
        if self.to_file {
            eprintln!("tinystore serve: {message}");
        }
    }

    /// Shows a person who started a server in a terminal where it listens,
    /// as a framework's dev server does; a log gets the same as a line.
    ///
    /// ```text
    ///   tinystore 0.1.0  ready in 14 ms
    ///
    ///   ➜  store   /srv/data
    ///   ➜  local   unix:///srv/data/server/tinystore.sock
    ///   ➜  remote  tls://0.0.0.0:7443
    /// ```
    fn serving(self, store: &Path, endpoints: &[String], sidecar: bool, ready: Duration) {
        let banner = self.terminal.terminal && !sidecar;
        if !banner || self.to_file {
            tracing::info!(target: "tinystore", dir = %store.display(), endpoints = ?endpoints, "serving");
        }
        if banner {
            eprint!("{}", banner_of(self.terminal.colours(), store, endpoints, ready));
        }
    }
}

fn banner_of(colours: bool, store: &Path, endpoints: &[String], ready: Duration) -> String {
    let paint = |style: Style, text: &str| lines::paint(colours, style, text);
    let (bold, dim, arrow, address) =
        (Style::new().bold(), Style::new().dimmed(), Color::Green.normal(), Color::Cyan.normal());
    let mut banner = format!(
        "\n  {} {}  {} {}\n\n",
        paint(bold, "tinystore"),
        paint(dim, env!("CARGO_PKG_VERSION")),
        paint(dim, "ready in"),
        paint(bold, &format!("{} ms", ready.as_millis())),
    );
    let mut row = |label: &str, value: String| {
        banner.push_str(&format!("  {}  {}  {value}\n", paint(arrow, "➜"), paint(dim, &format!("{label:<6}"))));
    };
    row("store", store.display().to_string());
    for (at, endpoint) in endpoints.iter().enumerate() {
        row(if at == 0 { "local" } else { "remote" }, paint(address, endpoint));
    }
    banner.push('\n');
    banner
}
