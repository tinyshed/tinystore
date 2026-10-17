//! A link's bytes: plain TCP, or TLS over it with the `replication-tls`
//! feature. One thread reads a link and others write it, so with TLS the
//! session's state sits behind a mutex held only while bytes pass through it,
//! and the socket is read and written through two handles of its own.

use std::io::{self, Read, Write};
use std::net::TcpStream;
#[cfg(feature = "replication-tls")]
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::node::State;
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
            let mut handshaking = tcp.try_clone().map_err(|error| Error::io("a link", error))?;
            handshaking.set_read_timeout(Some(HANDSHAKE)).map_err(|error| Error::io("a link", error))?;
            while connection.is_handshaking() {
                connection.complete_io(&mut handshaking).map_err(|error| Error::io("a link's TLS handshake", error))?;
            }
            Session::Tls(Arc::new(Mutex::new(connection)))
        }
        None => Session::Plain,
    };
    split(tcp, session)
}

#[cfg(not(feature = "replication-tls"))]
fn ends(tcp: TcpStream, tls: Option<()>) -> Result<(Reading, Writing)> {
    let _ = tls;
    split(tcp, Session::Plain)
}

fn split(tcp: TcpStream, session: Session) -> Result<(Reading, Writing)> {
    let clone = || tcp.try_clone().map_err(|error| Error::io("a link", error));
    let reading = Reading { tcp: clone()?, answers: clone()?, session: session.clone() };
    Ok((reading, Writing { tcp, session }))
}

impl Reading {
    /// How long a read waits for bytes before it says it timed out.
    pub(crate) fn set_timeout(&self, timeout: Duration) -> Result<()> {
        self.tcp.set_read_timeout(Some(timeout)).map_err(|error| Error::io("a link", error))
    }
}

impl Read for Reading {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &self.session {
            Session::Plain => self.tcp.read(buf),
            #[cfg(feature = "replication-tls")]
            Session::Tls(tls) => loop {
                match lock(tls).reader().read(buf) {
                    Ok(read) => return Ok(read),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error),
                }
                let mut raw = [0; 16 << 10];
                let read = self.tcp.read(&mut raw)?;
                if read == 0 {
                    return Ok(0);
                }
                let mut connection = lock(tls);
                let mut rest = &raw[..read];
                while !rest.is_empty() {
                    connection.read_tls(&mut rest)?;
                    connection.process_new_packets().map_err(io::Error::other)?;
                }
                while connection.wants_write() {
                    connection.write_tls(&mut self.answers)?;
                }
            },
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
            #[cfg(feature = "replication-tls")]
            Session::Tls(tls) => {
                let mut connection = lock(tls);
                connection.writer().write_all(bytes)?;
                while connection.wants_write() {
                    connection.write_tls(&mut self.tcp)?;
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
fn client_config(roots: &std::path::Path) -> Result<rustls::ClientConfig> {
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

#[cfg(feature = "replication-tls")]
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
