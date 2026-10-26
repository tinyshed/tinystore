//! A client of the server that holds a store: the server `<dir>/server/SERVE`
//! names, reached on its local endpoint, its proof checked, and its calls
//! asked through `tinystore::client`, which writes and reads the protocol's
//! own messages. `status` asks whether it runs, `stop` asks it to stop, and a
//! command that writes writes through it.
//!
//! ```text
//! HELLO    {1: 1, 2: "tinystore-cli/0.2.0", 6: <16 random bytes>}
//! WELCOME  {2: server's version, 11: HMAC-SHA256(challenge) keyed with SERVE's secret, …}
//! REQUEST  stream 1, END, the call            RESPONSE stream 1, END, its answer or a failure
//! ```

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use tinystore::client::Client;
use tinystore::{Error, ErrorKind};

/// How long a command waits for the server to answer a HELLO or a call.
const ANSWER_WAIT: Duration = if cfg!(test) { Duration::from_secs(1) } else { Duration::from_secs(10) };

/// What `SERVE` says of the server that published it.
#[derive(Debug)]
pub(crate) struct Found {
    pub(crate) pid: u64,
    pub(crate) endpoints: Vec<String>,
    /// When it was written: when the server began listening.
    pub(crate) since: SystemTime,
    secret: Vec<u8>,
}

/// The server `<dir>/server/SERVE` names, or none when there is no `SERVE`.
pub(crate) fn found(dir: &Path) -> Result<Option<Found>, String> {
    let path = dir.join("server").join("SERVE");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let unreadable = |why: &str| format!("{}: {why}", path.display());
    let serve: serde_json::Value = serde_json::from_str(&text).map_err(|error| unreadable(&error.to_string()))?;
    let field = |name: &str| serve.get(name).ok_or_else(|| unreadable(&format!("no {name}")));
    let endpoints = field("endpoints")?.as_array().ok_or_else(|| unreadable("endpoints that are no list"))?;
    let since = std::fs::metadata(&path).and_then(|metadata| metadata.modified()).unwrap_or_else(|_| SystemTime::now());
    Ok(Some(Found {
        pid: field("pid")?.as_u64().unwrap_or_default(),
        endpoints: endpoints.iter().filter_map(|endpoint| endpoint.as_str().map(str::to_owned)).collect(),
        since,
        secret: base64url(field("secret")?.as_str().unwrap_or_default())
            .ok_or_else(|| unreadable("a secret that is no base64url"))?,
    }))
}

/// A byte stream to the server, both ways.
pub(crate) trait Stream: Read + Write + Send {}

impl<T: Read + Write + Send> Stream for T {}

/// A connection the server welcomed, its proof checked.
pub(crate) struct Connection {
    client: Client<Box<dyn Stream>>,
    /// The server's version, as its WELCOME says.
    pub(crate) server: String,
    /// `full` or `os` when the store was opened with one; each engine's own
    /// when absent.
    pub(crate) durability: Option<String>,
}

impl Found {
    /// Reaches the server on its local endpoint and says HELLO: a server
    /// gone that left `SERVE` behind is an error, and so is a process on its
    /// endpoint that cannot prove it read `SERVE`.
    pub(crate) fn connect(&self) -> Result<Connection, String> {
        let endpoint = self.endpoints.first().ok_or("SERVE names no endpoint")?.clone();
        let secret = self.secret.clone();
        answered_within(&endpoint.clone(), move || hello(&endpoint, &secret))
    }
}

fn hello(endpoint: &str, secret: &[u8]) -> Result<Connection, String> {
    let stream = dial(endpoint).map_err(|error| format!("{endpoint}: {error}"))?;
    let mut challenge = [0; 16];
    getrandom::fill(&mut challenge).map_err(|error| error.to_string())?;
    let name = format!("tinystore-cli/{}", env!("CARGO_PKG_VERSION"));
    let (client, welcome) =
        Client::hello(stream, &name, Some(challenge)).map_err(|error| format!("{endpoint}: {error}"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(&challenge);
    let proof = welcome.proof.ok_or("the server gave no proof")?;
    mac.verify_slice(&proof).map_err(|_| "a process on the endpoint did not prove it is the store's server")?;
    Ok(Connection { client, server: welcome.server, durability: welcome.durability })
}

impl Connection {
    /// Asks the server to stop, and returns once it agreed.
    pub(crate) fn stop(self) -> Result<(), String> {
        let mut connection = self;
        answered_within("server.stop", move || connection.client.stop().map_err(|error| error.to_string()))
    }

    /// Runs `call` on the connection with no bound on its wait: a stream as
    /// long as its input, which a server gone ends with an error.
    pub(crate) fn run<T>(
        self,
        call: impl FnOnce(&mut Client<Box<dyn Stream>>) -> tinystore::Result<T>,
    ) -> tinystore::Result<T> {
        let mut connection = self;
        call(&mut connection.client)
    }

    /// Runs `call` on the connection and gives the connection back for the
    /// next. A server that does not answer within the wait may have run the
    /// call: its outcome is unknown.
    pub(crate) fn call<T: Send + 'static>(
        self,
        what: &str,
        call: impl FnOnce(&mut Client<Box<dyn Stream>>) -> tinystore::Result<T> + Send + 'static,
    ) -> tinystore::Result<(Connection, T)> {
        let mut connection = self;
        let timed_out = |why: String| Error::new(ErrorKind::OutcomeUnknown, why);
        within(what, timed_out, move || call(&mut connection.client).map(|answer| (connection, answer)))
    }
}

/// Runs `exchange` on a thread of its own and waits for it at most
/// `ANSWER_WAIT`: a Windows pipe's read takes no timeout, and a second handle
/// to read it with would wait behind the first, so a server that never
/// answers would otherwise hold the command for good. The thread left waiting
/// ends with the command.
fn answered_within<T: Send + 'static>(
    what: &str,
    exchange: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    within(what, |why| why, exchange)
}

fn within<T: Send + 'static, E: Send + 'static>(
    what: &str,
    timed_out: impl FnOnce(String) -> E,
    exchange: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, E> {
    let (done, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || drop(done.send(exchange())));
    answer.recv_timeout(ANSWER_WAIT).unwrap_or_else(|_| {
        Err(timed_out(format!("{what}: the server did not answer within {} s", ANSWER_WAIT.as_secs())))
    })
}

#[cfg(unix)]
fn dial(endpoint: &str) -> io::Result<Box<dyn Stream>> {
    let path = endpoint.strip_prefix("unix://").ok_or_else(|| io::Error::other("not a local socket"))?;
    let stream = std::os::unix::net::UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    Ok(Box::new(stream))
}

/// Opens the server's named pipe, waiting a little while every instance is
/// busy with another client.
#[cfg(windows)]
fn dial(endpoint: &str) -> io::Result<Box<dyn Stream>> {
    let name = endpoint.strip_prefix("pipe:").ok_or_else(|| io::Error::other("not a named pipe"))?;
    let path = format!(r"\\.\pipe\{name}");
    let mut tries = 0;
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(pipe) => return Ok(Box::new(pipe)),
            // ERROR_PIPE_BUSY: every instance has a client
            Err(error) if error.raw_os_error() == Some(231) && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Base64url without padding, as `SERVE` spells its secret.
fn base64url(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut held = 0;
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = bits << 6 | u32::from(value);
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push((bits >> held) as u8);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_that_takes_the_connection_and_never_answers_is_said_within_the_wait() {
        let (endpoint, _listening) = silent_endpoint();
        let found = Found { pid: 0, endpoints: vec![endpoint], since: SystemTime::now(), secret: vec![1] };
        let began = std::time::Instant::now();
        let refused = found.connect().err().expect("no answer came");
        assert!(refused.contains("did not answer within"), "{refused}");
        assert!(began.elapsed() < ANSWER_WAIT * 5, "{:?}", began.elapsed());
    }

    /// An endpoint that takes a connection and says nothing, and what keeps it open.
    #[cfg(unix)]
    fn silent_endpoint() -> (String, Box<dyn std::any::Any>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("silent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        (format!("unix://{}", path.display()), Box::new((dir, listener)))
    }

    #[cfg(windows)]
    fn silent_endpoint() -> (String, Box<dyn std::any::Any>) {
        let name = format!("tinystore-silent-{}", std::process::id());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let pipe = runtime.block_on(async {
            tokio::net::windows::named_pipe::ServerOptions::new().create(format!(r"\\.\pipe\{name}"))
        });
        (format!("pipe:{name}"), Box::new((pipe.unwrap(), runtime)))
    }

    // contract: CLI-03
    #[test]
    fn a_secret_reads_as_serve_spells_it() {
        assert_eq!(base64url("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(base64url("-_8").unwrap(), [0xfb, 0xff]);
        assert_eq!(base64url("a+b"), None);
    }
}
