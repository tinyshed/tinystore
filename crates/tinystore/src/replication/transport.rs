//! A link's bytes: plain TCP, or TLS over it with the `replication-tls`
//! feature. One thread reads a link and others write it, so with TLS the
//! session's state sits behind a mutex held only while bytes pass through it,
//! and the socket is read and written through two handles of its own.

use std::io::{self, Read, Write};
use std::net::TcpStream;
#[cfg(feature = "replication-tls")]
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(feature = "replication-tls")]
use std::time::Instant;

#[cfg(feature = "replication-tls")]
use super::message::timed_out;
use super::node::State;
#[cfg(feature = "replication-tls")]
use crate::sync::lock;
use crate::{Error, Result};

/// How long a link's TLS handshake may take.
#[cfg(feature = "replication-tls")]
const HANDSHAKE: Duration = Duration::from_secs(10);

/// What a link's bytes pass through.
#[derive(Clone)]
enum Session {
    Plain,
    #[cfg(feature = "replication-tls")]
    Tls(Arc<Mutex<rustls::Connection>>),
}

/// The end of a link one thread reads.
pub(crate) struct Reading {
    tcp: TcpStream,
    /// Where what TLS answers by itself, as a key's update, is written.
    #[cfg_attr(not(feature = "replication-tls"), expect(dead_code, reason = "plain TCP answers nothing by itself"))]
    answers: TcpStream,
    session: Session,
    /// What was read of the socket and not given to TLS yet.
    #[cfg_attr(not(feature = "replication-tls"), expect(dead_code, reason = "plain TCP keeps nothing back"))]
    unread: Vec<u8>,
    /// How long a read waits for a record whole, as the socket's for a byte.
    timeout: Option<Duration>,
}

/// The end of a link its threads write, one at a time.
pub(crate) struct Writing {
    tcp: TcpStream,
    session: Session,
}

/// The two ends of a link a node accepted, TLS's handshake made when the node
/// serves with a certificate.
pub(crate) fn accept(state: &State, tcp: TcpStream) -> Result<(Reading, Writing)> {
    tcp.set_nodelay(true).map_err(|error| Error::io("a link", error))?;
    #[cfg(feature = "replication-tls")]
    if let Some(config) = &state.tls.server {
        let connection = rustls::ServerConnection::new(Arc::clone(config)).map_err(tls_error)?;
        return ends(tcp, Some(rustls::Connection::Server(connection)));
    }
    let _ = state;
    ends(tcp, None)
}

/// The two ends of a link to `address`, TLS's handshake made when the node
/// trusts roots for the nodes it takes from.
pub(crate) fn connect(state: &State, address: &str, tcp: TcpStream) -> Result<(Reading, Writing)> {
    tcp.set_nodelay(true).map_err(|error| Error::io("a link", error))?;
    #[cfg(feature = "replication-tls")]
    if let Some(config) = &state.tls.client {
        let host = address.rsplit_once(':').map_or(address, |(host, _)| host).trim_matches(['[', ']']);
        let name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|_| Error::invalid(format!("{address}: no name TLS checks a certificate against")))?;
        let connection = rustls::ClientConnection::new(Arc::clone(config), name).map_err(tls_error)?;
        return ends(tcp, Some(rustls::Connection::Client(connection)));
    }
    refuse_plain_beyond_this_machine(state, &tcp, address)?;
    ends(tcp, None)
}

/// Refuses a plain link to another machine, which anyone on the way reads and
/// changes, unless the node said `insecure`: a link to loopback stays on it.
fn refuse_plain_beyond_this_machine(state: &State, tcp: &TcpStream, address: &str) -> Result<()> {
    let peer = tcp.peer_addr().map_err(|error| Error::io(format!("a link to {address}"), error))?;
    if peer.ip().is_loopback() || state.options.insecure {
        return Ok(());
    }
    Err(Error::invalid(format!("a link to {address}: {}", plain_refused())))
}

/// Why a plain link beyond this machine is refused, and what lets it through.
pub(crate) fn plain_refused() -> &'static str {
    if cfg!(feature = "replication-tls") {
        "a link beyond this machine takes TLS, whose roots and certificate a node names, or a node that says \
         insecure for plain TCP that anyone on the way reads"
    } else {
        "a link beyond this machine takes TLS, which this build leaves out: build with the replication-tls feature, \
         or say insecure for plain TCP that anyone on the way reads"
    }
}

#[cfg(feature = "replication-tls")]
fn ends(tcp: TcpStream, tls: Option<rustls::Connection>) -> Result<(Reading, Writing)> {
    let session = match tls {
        Some(mut connection) => {
            handshake(&mut connection, &tcp)?;
            Session::Tls(Arc::new(Mutex::new(connection)))
        }
        None => Session::Plain,
    };
    split(tcp, session)
}

/// Makes a link's TLS handshake within `HANDSHAKE`, however its bytes trickle
/// in: rustls' `complete_io` reads on until the handshake ends, so a peer
/// sending a byte a second would hold the link, and a stranger's slot, for good.
#[cfg(feature = "replication-tls")]
fn handshake(connection: &mut rustls::Connection, tcp: &TcpStream) -> Result<()> {
    let deadline = Instant::now() + HANDSHAKE;
    let too_long = || Error::new(crate::ErrorKind::Unavailable, "a link's TLS handshake took too long");
    let failed =
        |error: io::Error| if timed_out(&error) { too_long() } else { Error::io("a link's TLS handshake", error) };
    let mut socket = tcp;
    loop {
        // each wait on the socket ends by the deadline
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(too_long());
        }
        socket.set_read_timeout(Some(left)).and_then(|()| socket.set_write_timeout(Some(left))).map_err(failed)?;
        while connection.wants_write() {
            connection.write_tls(&mut socket).map_err(failed)?;
        }
        if !connection.is_handshaking() {
            return Ok(());
        }
        match connection.read_tls(&mut socket) {
            Ok(0) => return Err(Error::new(crate::ErrorKind::Unavailable, "a link ended in its TLS handshake")),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(failed(error)),
        }
        if let Err(error) = connection.process_new_packets() {
            // the alert that says why, if the socket takes it
            let _ = connection.write_tls(&mut socket);
            return Err(tls_error(error));
        }
    }
}

#[cfg(not(feature = "replication-tls"))]
fn ends(tcp: TcpStream, tls: Option<()>) -> Result<(Reading, Writing)> {
    let _ = tls;
    split(tcp, Session::Plain)
}

fn split(tcp: TcpStream, session: Session) -> Result<(Reading, Writing)> {
    let clone = || tcp.try_clone().map_err(|error| Error::io("a link", error));
    let reading =
        Reading { tcp: clone()?, answers: clone()?, session: session.clone(), unread: Vec::new(), timeout: None };
    Ok((reading, Writing { tcp, session }))
}

impl Reading {
    /// How long a read waits for bytes before it says it timed out: over TLS,
    /// for a record whole, however its bytes trickle in.
    pub(crate) fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.timeout = Some(timeout);
        self.tcp.set_read_timeout(Some(timeout)).map_err(|error| Error::io("a link", error))
    }
}

impl Read for Reading {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &self.session {
            Session::Plain => self.tcp.read(buf),
            #[cfg(feature = "replication-tls")]
            Session::Tls(tls) => {
                let tls = Arc::clone(tls);
                self.read_records(&tls, buf)
            }
        }
    }
}

#[cfg(feature = "replication-tls")]
impl Reading {
    /// Reads what TLS decrypted, reading the socket until a record is whole or
    /// the read's timeout passed: a record whose bytes trickle in times out as
    /// a silent socket does, so that the caller checks its own bound, a
    /// hello's above all, where a read that waited for it whole would wait on.
    fn read_records(&mut self, tls: &Mutex<rustls::Connection>, buf: &mut [u8]) -> io::Result<usize> {
        let began = Instant::now();
        loop {
            match lock(tls).reader().read(buf) {
                Ok(read) => return Ok(read),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
            if self.unread.is_empty() {
                if self.timeout.is_some_and(|timeout| began.elapsed() >= timeout) {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "no TLS record came whole in time"));
                }
                let mut raw = [0; 16 << 10];
                let read = self.tcp.read(&mut raw)?;
                if read == 0 {
                    return Ok(0);
                }
                self.unread.extend_from_slice(&raw[..read]);
            }
            // Fed only until a record's plaintext is ready, the rest kept for
            // the next turn: rustls refuses bytes while more than 16 KiB of
            // plaintext waits unread, and one read may finish two records.
            let mut connection = lock(tls);
            let mut rest = &self.unread[..];
            let mut closed = false;
            while !rest.is_empty() {
                closed = connection.read_tls(&mut rest)? == 0;
                let ready = connection.process_new_packets().map_err(io::Error::other)?.plaintext_bytes_to_read();
                if closed || ready > 0 {
                    break;
                }
            }
            let fed = if closed { self.unread.len() } else { self.unread.len() - rest.len() };
            self.unread.drain(..fed);
            while connection.wants_write() {
                connection.write_tls(&mut self.answers)?;
            }
        }
    }
}

impl Writing {
    /// How long a write waits for room before it fails.
    pub(crate) fn set_timeout(&self, timeout: Duration) -> Result<()> {
        self.tcp.set_write_timeout(Some(timeout)).map_err(|error| Error::io("a link", error))
    }

    pub(crate) fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &self.session {
            Session::Plain => self.tcp.write_all(bytes),
            // rustls takes at most 64 KiB until what it holds is written, so a
            // message goes a piece at a time, each written before the next
            #[cfg(feature = "replication-tls")]
            Session::Tls(tls) => {
                let mut rest = bytes;
                while !rest.is_empty() {
                    let mut connection = lock(tls);
                    let taken = connection.writer().write(rest)?;
                    while connection.wants_write() {
                        connection.write_tls(&mut self.tcp)?;
                    }
                    if taken == 0 {
                        return Err(io::Error::new(io::ErrorKind::WriteZero, "a link's TLS took none of a message"));
                    }
                    rest = &rest[taken..];
                }
                Ok(())
            }
        }
    }
}

/// The TLS a node's links speak, made once from its options.
#[derive(Default)]
pub(crate) struct Tls {
    #[cfg(feature = "replication-tls")]
    pub(crate) server: Option<Arc<rustls::ServerConfig>>,
    #[cfg(feature = "replication-tls")]
    pub(crate) client: Option<Arc<rustls::ClientConfig>>,
}

impl Tls {
    /// Whether the node's server answers over TLS.
    pub(crate) fn serves(&self) -> bool {
        #[cfg(feature = "replication-tls")]
        return self.server.is_some();
        #[cfg(not(feature = "replication-tls"))]
        false
    }

    /// The TLS `node` asks for: a server's with its certificate and key, a
    /// follower's with the roots it trusts. Without the feature, any is
    /// refused rather than links left plain.
    pub(crate) fn of(node: &super::Node) -> Result<Tls> {
        let Some(asked) = &node.tls else {
            return Ok(Tls::default());
        };
        #[cfg(not(feature = "replication-tls"))]
        {
            let _ = asked;
            Err(Error::invalid("a node's TLS, and the library was built without the replication-tls feature"))
        }
        #[cfg(feature = "replication-tls")]
        {
            let server = match (&asked.cert, &asked.key) {
                (Some(cert), Some(key)) => Some(Arc::new(server_config(cert, key)?)),
                (None, None) => None,
                _ => return Err(Error::invalid("a node's TLS serves with a certificate and its key, both")),
            };
            let client = asked.roots.as_deref().map(client_config).transpose()?.map(Arc::new);
            Ok(Tls { server, client })
        }
    }
}

#[cfg(feature = "replication-tls")]
fn server_config(cert: &std::path::Path, key: &std::path::Path) -> Result<rustls::ServerConfig> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .and_then(Iterator::collect)
        .map_err(|error| Error::invalid(format!("{}: {error}", cert.display())))?;
    if chain.is_empty() {
        return Err(Error::invalid(format!("{}: no certificate", cert.display())));
    }
    let key =
        PrivateKeyDer::from_pem_file(key).map_err(|error| Error::invalid(format!("{}: {error}", key.display())))?;
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_no_client_auth().with_single_cert(chain, key))
        .map_err(tls_error)
}

#[cfg(feature = "replication-tls")]
pub(super) fn client_config(roots: &std::path::Path) -> Result<rustls::ClientConfig> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;
    let mut store = rustls::RootCertStore::empty();
    for root in
        CertificateDer::pem_file_iter(roots).map_err(|error| Error::invalid(format!("{}: {error}", roots.display())))?
    {
        let root = root.map_err(|error| Error::invalid(format!("{}: {error}", roots.display())))?;
        store.add(root).map_err(tls_error)?;
    }
    if store.is_empty() {
        return Err(Error::invalid(format!("{}: no certificate to trust", roots.display())));
    }
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(tls_error)?
        .with_root_certificates(store)
        .with_no_client_auth())
}

#[cfg(feature = "replication-tls")]
fn tls_error(error: rustls::Error) -> Error {
    Error::invalid(format!("a link's TLS: {error}"))
}

#[cfg(all(test, feature = "replication-tls"))]
mod tests {
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;

    use super::{client_config, ends, server_config};

    // A message a byte past a TLS record's 16 KiB is a full record and a short
    // one: a read of the socket that ends past a short one holds more
    // plaintext than rustls takes before it is read.
    #[test]
    fn a_tls_link_reads_messages_whose_records_end_anywhere_in_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        std::fs::write(&cert, certified.cert.pem()).unwrap();
        std::fs::write(&key, certified.signing_key.serialize_pem()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let message: Vec<u8> = (0..(16 << 10) + 100).map(|i| (i % 251) as u8).collect();
        let (sent, count) = (message.clone(), 64);
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let connection = rustls::ServerConnection::new(Arc::new(server_config(&cert, &key).unwrap())).unwrap();
            let (_reading, mut writing) = ends(tcp, Some(rustls::Connection::Server(connection))).unwrap();
            for _ in 0..count {
                writing.write_all(&sent).unwrap();
            }
        });
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let roots = Arc::new(client_config(&dir.path().join("cert.pem")).unwrap());
        let connection = rustls::ClientConnection::new(roots, name).unwrap();
        let (mut reading, _writing) =
            ends(TcpStream::connect(address).unwrap(), Some(rustls::Connection::Client(connection))).unwrap();
        let mut got = Vec::new();
        let mut buffer = vec![0; 64 << 10];
        while got.len() < message.len() * count {
            // the sender runs ahead, so that each read of the socket is a full one
            std::thread::sleep(Duration::from_millis(2));
            let read = reading.read(&mut buffer).unwrap();
            assert!(read > 0, "the link ended after {} bytes", got.len());
            got.extend_from_slice(&buffer[..read]);
        }
        server.join().unwrap();
        assert!(got.chunks(message.len()).all(|one| one == message), "each message came whole");
    }
}
