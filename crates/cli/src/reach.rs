//! How a command that writes reaches a store: through the server that holds
//! its directory, or a sidecar the command starts as the SDKs do, so that a
//! write is the call an SDK makes, and several commands write at once. A
//! directory a program holds inside its own process is refused, since no
//! other process can write to it, and so is one that holds no store.
//!
//! ```text
//! SERVE names a server      connect, prove it, write
//! no SERVE, LOCK free       start `tinystore serve --local`, wait for its SERVE, write
//! no SERVE, LOCK held       the sidecar cannot open the store: a program holds it embedded
//! ```

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tinystore::ErrorKind;
use tinystore::client::Client;

use crate::asked::{Refused, failed};
use crate::client::{self, Connection, Stream};

/// How long a sidecar may take to listen.
const START_WAIT: Duration = Duration::from_secs(15);
/// How long a sidecar that could not open the store leaves for another's
/// `SERVE`, since two commands that start one at once leave one listening.
const RIVAL_WAIT: Duration = Duration::from_secs(2);
/// The exit of a server whose directory another process holds.
const HELD: i32 = 3;
/// How often a start looks for `SERVE`.
const LOOK: Duration = Duration::from_millis(20);
/// The times a write whose connection was lost is sent again.
const ATTEMPTS: usize = 3;
/// The most of a sidecar's stderr kept to say why it stopped.
const SAID: usize = 8 << 10;

/// Writes through the server of `dir` with `call`, as `Writer::write` does.
pub(crate) fn write<T: Send + 'static>(
    dir: &Path,
    command: &'static str,
    call: impl Fn(&mut Client<Box<dyn Stream>>) -> tinystore::Result<T> + Send + Sync + 'static,
) -> Result<T, Refused> {
    Writer::new(dir, command).write(call)
}

/// Writes through the server of `dir` one call after another on one
/// connection, made again when it is lost, as a stream that does not end
/// writes each of its batches.
pub(crate) struct Writer {
    dir: PathBuf,
    command: &'static str,
    connection: Option<Connection>,
}

impl Writer {
    pub(crate) fn new(dir: &Path, command: &'static str) -> Writer {
        Writer { dir: dir.to_owned(), command, connection: None }
    }

    /// Reaches the server before the first write, so that a directory no
    /// write can reach is refused before a stream is read.
    pub(crate) fn reach(&mut self) -> Result<(), Refused> {
        if self.connection.is_none() {
            self.connection = Some(connected(&self.dir, self.command)?);
        }
        Ok(())
    }

    /// Writes with `call`, the server's refusal a failure of the command.
    pub(crate) fn write<T: Send + 'static>(
        &mut self,
        call: impl Fn(&mut Client<Box<dyn Stream>>) -> tinystore::Result<T> + Send + Sync + 'static,
    ) -> Result<T, Refused> {
        self.try_write(call)?.map_err(failed)
    }

    /// Writes with `call`, sending it again on a new connection when the
    /// connection was lost: a call written to change nothing the second
    /// time, such as an ingest whose samples carry their times. What the
    /// server answered, a refusal among them, once it was reached.
    pub(crate) fn try_write<T: Send + 'static>(
        &mut self,
        call: impl Fn(&mut Client<Box<dyn Stream>>) -> tinystore::Result<T> + Send + Sync + 'static,
    ) -> Result<tinystore::Result<T>, Refused> {
        let call = Arc::new(call);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let connection = match self.connection.take() {
                Some(connection) => connection,
                None => connected(&self.dir, self.command)?,
            };
            let sent = Arc::clone(&call);
            let (kept, answer) = connection.call(self.command, move |client| sent(client));
            self.connection = kept;
            match answer {
                Err(error) if lost(error.kind()) && attempt < ATTEMPTS => continue,
                answer => return Ok(answer),
            }
        }
    }
}

/// Streams through the server of `dir` with `call`, once: what it sent is
/// gone from its input, so a lost connection is the command's failure.
pub(crate) fn stream<T>(
    dir: &Path,
    command: &str,
    call: impl FnOnce(&mut Client<Box<dyn Stream>>) -> tinystore::Result<T>,
) -> Result<T, Refused> {
    connected(dir, command)?.run(call).map_err(failed)
}

fn lost(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::OutcomeUnknown | ErrorKind::Unavailable)
}

/// Refuses a directory that holds no store, before anything is read or
/// started: a write makes no store of a typo.
pub(crate) fn store_at(dir: &Path) -> Result<(), Refused> {
    if dir.join("LOCK").is_file() {
        return Ok(());
    }
    let shown = dir.display();
    Err(failed(format!("{shown}: no store is there; a program, or `tinystore serve {shown}`, makes one")))
}

/// A connection to the server of `dir`: the one `SERVE` names, or a sidecar
/// started for it.
fn connected(dir: &Path, command: &str) -> Result<Connection, Refused> {
    store_at(dir)?;
    if let Some(connection) = served(dir)? {
        return Ok(connection);
    }
    started(dir, command)
}

/// The server `SERVE` names, when one answers and proves itself; none when
/// `SERVE` is missing or was left by a server gone.
fn served(dir: &Path) -> Result<Option<Connection>, Refused> {
    let Some(found) = client::found(dir).map_err(failed)? else {
        return Ok(None);
    };
    Ok(found.connect().ok())
}

/// Starts a sidecar for `dir` and connects once it listens. One started by
/// another command at the same moment wins as well as this one: what counts
/// is the `SERVE` that appears.
fn started(dir: &Path, command: &str) -> Result<Connection, Refused> {
    let mut sidecar = Sidecar::start(&std::path::absolute(dir).unwrap_or_else(|_| dir.to_owned()))?;
    let began = Instant::now();
    let mut stopped_at = None;
    loop {
        if let Some(found) = client::found(dir).map_err(failed)?
            && let Ok(connection) = found.connect()
        {
            if found.pid == u64::from(sidecar.child.id()) {
                crate::asked::say(
                    command,
                    format!("started a server for {}; it stops 30 s after the last client", dir.display()),
                );
            }
            return Ok(connection);
        }
        if stopped_at.is_none()
            && let Ok(Some(exit)) = sidecar.child.try_wait()
        {
            stopped_at = Some((Instant::now(), exit.code()));
        }
        match stopped_at {
            Some((at, code)) if at.elapsed() > RIVAL_WAIT => return Err(sidecar.refusal(dir, code)),
            None if began.elapsed() > START_WAIT => {
                return Err(failed(format!(
                    "no server listened for {} within {} s",
                    dir.display(),
                    START_WAIT.as_secs()
                )));
            }
            _ => std::thread::sleep(LOOK),
        }
    }
}

/// A `tinystore serve --local` this command started, and what it says on
/// stderr, read for as long as it runs: a pipe nobody reads would stop it
/// once full.
struct Sidecar {
    child: Child,
    said: Arc<Mutex<String>>,
}

impl Sidecar {
    fn start(dir: &Path) -> Result<Sidecar, Refused> {
        let binary = std::env::current_exe().map_err(|error| failed(format!("this command's own path: {error}")))?;
        let log: PathBuf = dir.join("server").join("serve.log");
        let mut serve = Command::new(binary);
        serve
            .arg("serve")
            .arg("--dir")
            .arg(dir)
            .args(["--local", "--quiet", "--log-file"])
            .arg(&log)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        detached(&mut serve);
        #[cfg(windows)]
        crate::own_handles::keep();
        let mut child = serve.spawn().map_err(|error| failed(format!("a server for {}: {error}", dir.display())))?;
        let said = Arc::new(Mutex::new(String::new()));
        if let Some(mut stderr) = child.stderr.take() {
            let kept = Arc::clone(&said);
            std::thread::spawn(move || {
                let mut read = [0; 4096];
                while let Ok(count @ 1..) = stderr.read(&mut read) {
                    let mut said = kept.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if said.len() < SAID {
                        said.push_str(&String::from_utf8_lossy(&read[..count]));
                    }
                }
            });
        }
        Ok(Sidecar { child, said })
    }

    /// Why no server listened: a store a program holds embedded, which the
    /// sidecar could not open and no other server serves, or what the
    /// sidecar said as it stopped.
    fn refusal(&self, dir: &Path, code: Option<i32>) -> Refused {
        if code == Some(HELD) {
            return failed(format!(
                "a program holds {} inside its own process, so no other process can write to it\n  open it in that \
                 program with a sidecar, or let `tinystore serve {}` hold it",
                dir.display(),
                dir.display()
            ));
        }
        let said = self.said.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).trim().to_owned();
        let why = said.lines().last().unwrap_or("it said nothing");
        failed(format!("the server started for {} stopped: {why}", dir.display()))
    }
}

/// Lets the sidecar outlive this command, and a Ctrl+C meant for it: a
/// process group of its own, and on Windows no console.
#[cfg(unix)]
fn detached(serve: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    serve.process_group(0);
}

#[cfg(windows)]
fn detached(serve: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    serve.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}
