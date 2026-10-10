//! A remote server: TCP, with TLS unless the operator trusts the network, and
//! tokens that decide what each client may do. Its tokens and certificate are
//! read before the store opens, so a mistake leaves no files behind.

use std::io;
use std::path::Path;
use std::sync::Arc;

use tinystore::pipe::{Admit, Capability};

use crate::args::Remote;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::ring;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// A token: 32 random bytes in base64url without padding.
const TOKEN_TEXT: usize = 43;

/// A remote server ready to listen: its address, the tokens that admit its
/// clients, and its TLS when the address is `tls://`.
pub(crate) struct Ready {
    pub(crate) address: Address,
    pub(crate) admit: Admit,
    pub(crate) tls: Option<TlsAcceptor>,
}

impl Ready {
    /// Reads the tokens and the certificate, before the store opens.
    pub(crate) fn of(remote: Remote) -> Result<Ready, String> {
        let admit = Tokens::read(&remote.tokens)?.admit();
        let tls = remote.tls.as_ref().map(|(cert, key)| acceptor(cert, key)).transpose()?;
        Ok(Ready { address: remote.address, admit, tls })
    }
}

/// The tokens file: a capability and a token a line, `#` a comment.
///
/// ```text
/// # who may connect
/// admin 4dGQ6tR2kq0SxVZpLWn8E1yHf7cJmB3aUoN9Tz5Ki0E
/// data  Yq8wHn2xLz5Rb7Tc0Vm3Kj6Fd9Gs1Ap4Ue8Wo2Ni5Qt
/// ```
#[derive(Debug)]
pub(crate) struct Tokens(Vec<(Capability, String)>);

impl Tokens {
    pub(crate) fn read(path: &Path) -> Result<Tokens, String> {
        let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        Tokens::parse(&text).map_err(|refused| format!("{}: {refused}", path.display()))
    }

    fn parse(text: &str) -> Result<Tokens, String> {
        let mut known = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            known.push(token(line).map_err(|refused| format!("line {}: {refused}", number + 1))?);
        }
        if known.is_empty() {
            return Err("no token, so no client could connect".to_owned());
        }
        Ok(Tokens(known))
    }

    /// Admits a HELLO by its token, every known token compared in full, so
    /// that the time taken says nothing of which matched.
    pub(crate) fn admit(self) -> Admit {
        Arc::new(move |given: Option<&str>| {
            let given = given?;
            let mut found = None;
            for (capability, token) in &self.0 {
                if same(token.as_bytes(), given.as_bytes()) {
                    found = Some(*capability);
                }
            }
            found
        })
    }
}

fn token(line: &str) -> Result<(Capability, String), String> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [capability, token] = fields[..] else {
        return Err(format!("a line is a capability and a token, not {} fields", fields.len()));
    };
    let capability = match capability {
        "admin" => Capability::Admin,
        "data" => Capability::Data,
        "read" => Capability::Read,
        other => return Err(format!("a capability is admin, data or read, not {other}")),
    };
    let base64url = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_';
    if token.len() != TOKEN_TEXT || !token.bytes().all(base64url) {
        return Err("a token is 32 random bytes in base64url, without padding".to_owned());
    }
    Ok((capability, token.to_owned()))
}

/// Whether two texts of a token's length are equal, every byte compared.
fn same(known: &[u8], given: &[u8]) -> bool {
    known.len() == given.len() && known.iter().zip(given).fold(0, |differ, (a, b)| differ | (a ^ b)) == 0
}

/// The TLS a `tls://` server answers with: its certificate chain and key, PEM.
pub(crate) fn acceptor(cert: &Path, key: &Path) -> Result<TlsAcceptor, String> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .and_then(Iterator::collect)
        .map_err(|error| format!("{}: {error}", cert.display()))?;
    if chain.is_empty() {
        return Err(format!("{}: no certificate", cert.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(|error| format!("{}: {error}", key.display()))?;
    let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_no_client_auth().with_single_cert(chain, key))
        .map_err(|error| format!("the certificate and its key: {error}"))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Where `--listen` says to listen, and whether with TLS.
#[derive(Debug, PartialEq)]
pub(crate) struct Address {
    pub(crate) tls: bool,
    pub(crate) host: String,
    pub(crate) port: u16,
}

impl Address {
    /// `tls://0.0.0.0:7443`, `tcp://127.0.0.1:0`, `tls://[::]:7443`.
    pub(crate) fn parse(text: &str) -> Result<Address, String> {
        let refused = || format!("the address {text}: as tls://0.0.0.0:7443 or tcp://127.0.0.1:7070");
        let (tls, rest) = match text.split_once("://") {
            Some(("tls", rest)) => (true, rest),
            Some(("tcp", rest)) => (false, rest),
            _ => return Err(refused()),
        };
        let (host, port) = rest.rsplit_once(':').ok_or_else(refused)?;
        let port = port.parse().map_err(|_| refused())?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            return Err(refused());
        }
        Ok(Address { tls, host: host.to_owned(), port })
    }

    /// The endpoint a client dials, with the port the listener took.
    pub(crate) fn endpoint(&self, port: u16) -> String {
        let scheme = if self.tls { "tls" } else { "tcp" };
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        format!("{scheme}://{host}:{port}")
    }
}

/// Binds the listener; port 0 takes one the system gives.
pub(crate) async fn bind(address: &Address) -> io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind((address.host.as_str(), address.port)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADMIN: &str = "4dGQ6tR2kq0SxVZpLWn8E1yHf7cJmB3aUoN9Tz5Ki0E";
    const DATA: &str = "Yq8wHn2xLz5Rb7Tc0Vm3Kj6Fd9Gs1Ap4Ue8Wo2Ni5Qt";
    const READ: &str = "9xK2pQ7vLm4Ns8Bt1Cw5Dz3Fy6Gh0Jk2Al9Ro4Su7Ve";

    #[test]
    fn a_token_admits_its_capability_and_nothing_else_admits() {
        let tokens =
            Tokens::parse(&format!("# who may connect\nadmin {ADMIN}\n\ndata  {DATA}\nread  {READ}\n")).unwrap();
        let admit = tokens.admit();
        assert_eq!(admit(Some(ADMIN)), Some(Capability::Admin));
        assert_eq!(admit(Some(DATA)), Some(Capability::Data));
        assert_eq!(admit(Some(READ)), Some(Capability::Read));
        assert_eq!(admit(Some(&ADMIN[1..])), None);
        assert_eq!(admit(None), None);
    }

    #[test]
    fn a_tokens_file_refuses_what_is_not_a_token() {
        for text in ["admin short", &format!("root {ADMIN}"), &format!("admin {ADMIN} extra"), "", "# none"] {
            assert!(Tokens::parse(text).is_err(), "taken: {text}");
        }
        assert!(Tokens::parse(&format!("data {}=", &DATA[..42])).is_err(), "padding is not base64url's");
    }

    #[test]
    fn an_address_is_read_and_its_endpoint_written_with_the_port_taken() {
        let tls = Address::parse("tls://0.0.0.0:7443").unwrap();
        assert_eq!((tls.tls, tls.host.as_str(), tls.port), (true, "0.0.0.0", 7443));
        assert_eq!(Address::parse("tcp://[::1]:0").unwrap().endpoint(5555), "tcp://[::1]:5555");
        assert!(Address::parse("http://host:1").is_err());
        assert!(Address::parse("tcp://host").is_err());
    }
}
