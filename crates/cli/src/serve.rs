//! `tinystore serve`: the store opened under its `LOCK`, then served until a
//! client says `server.stop`, the server idles out, or Ctrl+C. Closing tells
//! every client `GOAWAY`, lets the streams running finish, removes `SERVE`,
//! and only then lets go of the directory.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tinystore::pipe::{Connect, Stop};
use tinystore::{Clock, ErrorKind, Options, Store, TestClock};
use tokio::sync::watch;

use crate::args::{self, Transport};
use crate::{connection, local};

/// The exit of a server that found its directory held, by a server that
/// started first or one still letting go: its client reads `SERVE` again.
const HELD: u8 = 3;

/// How long a closing server waits for its clients to leave.
const LEAVING: Duration = Duration::from_secs(10);
/// How long a server waits for its first client before idling counts: as long
/// as a client waits for the sidecar it started.
const FIRST_CLIENT: Duration = Duration::from_secs(15);

pub(crate) fn run(args: &[String]) -> ExitCode {
    let serve = match args::serve(args) {
        Ok(serve) => serve,
        Err(refused) => {
            eprintln!("tinystore serve: {refused}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = log_to(serve.log.as_deref()) {
        eprintln!("tinystore serve: its log: {error}");
        return ExitCode::FAILURE;
    }
    let clock = serve.clock.map(|start| Arc::new(TestClock::new(start)));
    let store = match open(&serve.dir, clock.clone()) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let served = tokio::runtime::Builder::new_multi_thread().enable_all().build().and_then(|runtime| {
        runtime.block_on(async {
            match serve.transport {
                Transport::Stdio => stdio(&store, clock).await,
                Transport::Local { sidecar } => listen(&store, sidecar, serve.idle).await,
            }
        })
    });
    let closed = store.close();
    match (served, closed) {
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
        (Err(error), _) => fail(&format!("{error}")),
        (_, Err(error)) => fail(&format!("the store did not close cleanly: {error}")),
    }
}

/// Opens the store, or says why not: a directory another holds exits with 3.
fn open(dir: &Path, clock: Option<Arc<TestClock>>) -> Result<Store, ExitCode> {
    let options = Options { clock: clock.map(|clock| clock as Arc<dyn Clock>), ..Options::default() };
    Store::open(dir, options).map_err(|error| {
        let code = if error.kind() == ErrorKind::InUse { ExitCode::from(HELD) } else { ExitCode::FAILURE };
        tracing::error!(target: "tinystore", %error, "the store did not open");
        eprintln!("tinystore serve: {error}");
        code
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

/// A local server: a socket or a named pipe that `SERVE` names, for as many
/// clients as come, until it is stopped, idles out or hears Ctrl+C.
async fn listen(store: &Store, sidecar: bool, idle: Option<Duration>) -> io::Result<()> {
    let server = local::server_dir(store.dir())?;
    local::unpublish(&server)?;
    let (listener, endpoint) = Listener::bind(store.dir(), &server)?;
    let serve = local::Serve::new(endpoint, sidecar)?;
    let (closing, closed) = watch::channel(false);
    let connect = Connect { prove: Some(serve.prove()), stop: Some(stop_by(closing.clone())), ..Connect::default() };
    local::publish(&server, &serve)?;
    tracing::info!(target: "tinystore", dir = %store.dir().display(), endpoint = %serve.endpoint, "serving");
    let (clients, counted) = watch::channel(0usize);
    let clients = Arc::new(clients);
    let accepted = listener.accept_all(store, &connect, &closed, &clients);
    tokio::select! {
        accepted = accepted => accepted?,
        () = stopped(closed.clone()) => {}
        () = interrupted() => {}
        () = idled(counted.clone(), idle) => tracing::info!(target: "tinystore", "idle, leaving"),
    }
    let _ = closing.send(true);
    left(counted).await;
    local::unpublish(&server)?;
    listener.remove();
    Ok(())
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

/// Logs to `file`, appended, or to stderr: never stdout, which a private
/// child's frames travel on.
fn log_to(file: Option<&Path>) -> io::Result<()> {
    let builder = tracing_subscriber::fmt().with_ansi(false).with_target(false);
    match file {
        Some(path) => {
            // a sidecar logs into <dir>/server/, which may not be there yet
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            builder.with_writer(Mutex::new(file)).init();
        }
        None => builder.with_writer(io::stderr).init(),
    }
    Ok(())
}

fn fail(message: &str) -> ExitCode {
    tracing::error!(target: "tinystore", "{message}");
    eprintln!("tinystore serve: {message}");
    ExitCode::FAILURE
}
