//! Where a local endpoint listens and how its clients find it, for the
//! command line's server and for a store opened with `cli` alike, so that
//! both publish one format: `<dir>/server/`, which only the user the endpoint
//! runs as may enter, holds `SERVE`, a hint naming the endpoint and the secret
//! a client's challenge is proven with. `LOCK` is the truth; `SERVE` is
//! written once the endpoint listens and removed before `LOCK` is let go.
//!
//! The command line reaches it through this hidden module, which promises a
//! program nothing; a program asks for the endpoint with [`crate::Options`]'s
//! `cli`.

mod endpoint;
#[cfg(windows)]
mod windows;

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::{Digest as _, Sha256};

pub(crate) use endpoint::Endpoint;

/// The protocol `SERVE` says its endpoint speaks.
const PROTOCOL: u64 = 1;

/// Who published a `SERVE`, which says what a client may ask of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    /// `tinystore serve`, which a person or a program started.
    Server,
    /// `tinystore serve --local` an SDK started, which an SDK of a newer
    /// release stops and replaces.
    Sidecar,
    /// A program that opened its store with `cli`: an SDK does not attach to
    /// it, since its life is that program's, and nobody stops it but that
    /// program.
    Embedded,
}

/// What a client needs to find and trust the endpoint: where it listens, and
/// the secret it proves itself with.
pub struct Serve {
    pub instance: [u8; 16],
    pub secret: [u8; 32],
    /// The local endpoint first, which a client of this machine dials; a
    /// remote one after it.
    pub endpoints: Vec<String>,
    pub holder: Holder,
}

impl std::fmt::Debug for Serve {
    /// Everything but the secret, which nothing prints (SEC-05).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let instance = hex(&self.instance);
        let serve = &mut f.debug_struct("Serve");
        serve.field("instance", &instance).field("endpoints", &self.endpoints).field("holder", &self.holder).finish()
    }
}

impl Serve {
    pub fn new(endpoints: Vec<String>, holder: Holder) -> io::Result<Serve> {
        let random = |error: crate::Error| io::Error::other(error.to_string());
        let (instance, secret) =
            (crate::encryption::random().map_err(random)?, crate::encryption::random().map_err(random)?);
        Ok(Serve { instance, secret, endpoints, holder })
    }

    fn json(&self) -> String {
        let mut published = serde_json::json!({
            "protocol": PROTOCOL,
            "server": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "instance": base64url(&self.instance),
            "secret": base64url(&self.secret),
            "endpoints": self.endpoints,
        });
        match self.holder {
            Holder::Server => {}
            Holder::Sidecar => published["sidecar"] = serde_json::Value::Bool(true),
            Holder::Embedded => published["embedded"] = serde_json::Value::Bool(true),
        }
        published.to_string()
    }

    /// Answers a HELLO's challenge with the HMAC-SHA256 of it, keyed with
    /// the secret.
    pub fn prove(&self) -> crate::pipe::Prove {
        let secret = self.secret;
        Arc::new(move |challenge: &[u8]| mac(&secret, challenge).finalize().into_bytes().to_vec())
    }
}

/// Whether `proof` is what the endpoint `secret` belongs to answers
/// `challenge` with, compared in constant time.
pub fn proven(secret: &[u8], challenge: &[u8], proof: &[u8]) -> bool {
    mac(secret, challenge).verify_slice(proof).is_ok()
}

fn mac(secret: &[u8], challenge: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(challenge);
    mac
}

/// `<dir>/server/`, made the user's alone, whatever the store's directory
/// lets others do: mode 0700, or on Windows a protected DACL of the user's,
/// which what it holds inherits. A directory another user made first is
/// refused, since its owner could let anyone in: on Unix its mode cannot be
/// changed, and on Windows its owner is read and a link in its place refused.
pub fn server_dir(store: &Path) -> io::Result<PathBuf> {
    let dir = store.join("server");
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|error| {
            io::Error::new(error.kind(), format!("{}: making it the user's alone: {error}", dir.display()))
        })?;
    }
    #[cfg(windows)]
    windows::protect_dir(&dir)?;
    Ok(dir)
}

/// Writes `SERVE` whole, through a temporary file renamed into place.
pub fn publish(server: &Path, serve: &Serve) -> io::Result<()> {
    let written = server.join(format!("SERVE.{}.tmp", hex(&serve.instance[..4])));
    let mut file = create_private(&written)?;
    file.write_all(serve.json().as_bytes())?;
    drop(file);
    patiently(|| fs::rename(&written, server.join("SERVE")))
}

/// A new file the user alone may open from the moment it is made: mode 0600,
/// or on Windows a protected DACL of the user's. One an endpoint that died
/// left is removed first, since a file that is there keeps its permissions
/// and is not made again.
fn create_private(path: &Path) -> io::Result<fs::File> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?
    };
    #[cfg(windows)]
    let file = windows::create_new(path)?;
    Ok(file)
}

/// Removes `SERVE`, if there is one: an endpoint leaving, or one left
/// behind, whose endpoint may be another process's by now.
pub fn unpublish(server: &Path) -> io::Result<()> {
    match patiently(|| fs::remove_file(server.join("SERVE"))) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        done => done,
    }
}

/// On Windows a client reading `SERVE` holds a rename or a removal off, and
/// so may a scanner: it is tried again, eight times, 1 to 64 ms apart.
fn patiently(change: impl Fn() -> io::Result<()>) -> io::Result<()> {
    let mut pause = Duration::from_millis(1);
    for _ in 0..7 {
        match change() {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => thread::sleep(pause),
            done => return done,
        }
        pause = (pause * 2).min(Duration::from_millis(64));
    }
    change()
}

/// The first eight bytes of the SHA-256 of the store's absolute path, in hex:
/// the name a pipe or a socket of the store's takes where its own path cannot.
pub fn hash(store: &Path) -> String {
    hex(&Sha256::digest(store.to_string_lossy().as_bytes())[..8])
}

/// The Unix socket a local endpoint listens on: `<dir>/server/tinystore.sock`
/// when that path fits a `sockaddr_un`, 104 bytes on macOS and the BSDs and
/// 108 on Linux; past it, a directory of the owner's own named by the hash.
#[cfg(unix)]
fn socket_path(store: &Path, server: &Path) -> io::Result<PathBuf> {
    let fits = if cfg!(target_os = "linux") { 108 } else { 104 };
    let inside = server.join("tinystore.sock");
    if inside.as_os_str().len() < fits {
        return Ok(inside);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let own = runtime.join(format!("tinystore-{}", hash(store)));
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&own)?;
        fs::set_permissions(&own, fs::Permissions::from_mode(0o700))?;
    }
    Ok(own.join("tinystore.sock"))
}

/// The store's socket, bound, and the endpoint `SERVE` names it by.
#[cfg(unix)]
#[derive(Debug)]
pub struct Socket {
    pub listener: std::os::unix::net::UnixListener,
    pub path: PathBuf,
    pub endpoint: String,
}

/// Binds the store's socket, removing one a process gone left there: the
/// caller holds `LOCK`, so no other listens on it.
#[cfg(unix)]
pub fn bind_socket(store: &Path, server: &Path) -> io::Result<Socket> {
    let path = socket_path(store, server)?;
    match fs::remove_file(&path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    let endpoint = format!("unix://{}", path.display());
    Ok(Socket { listener, path, endpoint })
}

/// The store's named pipe, `\\.\pipe\tinystore-<hash>`, and the endpoint
/// `SERVE` names it by, `pipe:tinystore-<hash>`.
#[cfg(windows)]
pub fn pipe_names(store: &Path) -> (String, String) {
    let short = format!("tinystore-{}", hash(store));
    (format!(r"\\.\pipe\{short}"), format!("pipe:{short}"))
}

/// An instance of the store's pipe, `\\.\pipe\<short name>`, open for
/// overlapped calls, which refuses remote clients and admits the user alone.
/// Named pipes share one namespace across every user, so the first instance
/// fails when another process holds the name, and the endpoint finds a
/// squatter rather than its clients.
#[cfg(windows)]
pub fn pipe_instance(name: &str, first: bool) -> io::Result<std::os::windows::io::OwnedHandle> {
    windows::create_pipe(name, first)
}

/// Bytes as lowercase hex, two digits each.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Base64url without padding, as `SERVE` spells its random bytes.
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let joined =
            chunk.iter().enumerate().fold(0u32, |joined, (at, byte)| joined | u32::from(*byte) << (16 - 8 * at));
        for at in 0..=chunk.len() {
            text.push(char::from(ALPHABET[(joined >> (18 - 6 * at) & 63) as usize]));
        }
    }
    text
}

/// Base64url without padding read back, as a client reads `SERVE`'s secret:
/// none for a character outside its alphabet.
pub fn from_base64url(text: &str) -> Option<Vec<u8>> {
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
    fn random_bytes_are_spelled_as_serve_spells_them() {
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(hex(&[0x0f, 0xa0]), "0fa0");
    }

    // contract: CLI-03
    #[test]
    fn a_secret_reads_as_serve_spells_it() {
        assert_eq!(from_base64url("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(from_base64url("-_8").unwrap(), [0xfb, 0xff]);
        assert_eq!(from_base64url(&base64url(&[7; 32])).unwrap(), [7; 32]);
        assert_eq!(from_base64url("a+b"), None);
    }

    #[test]
    fn the_proof_is_the_hmac_of_the_challenge() {
        let serve =
            Serve { instance: [0; 16], secret: [7; 32], endpoints: vec!["pipe:x".to_owned()], holder: Holder::Sidecar };
        // RFC 4231's second case: the key "Jefe"
        let proof = |key: &[u8], data: &[u8]| hex(&mac(key, data).finalize().into_bytes());
        assert_eq!(
            proof(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let challenge = b"0123456789abcdef";
        let answered = serve.prove()(challenge);
        assert!(proven(&[7; 32], challenge, &answered), "the client's check takes the endpoint's proof");
        assert!(!proven(&[8; 32], challenge, &answered), "and no other secret's");
        let published: serde_json::Value = serde_json::from_str(&serve.json()).unwrap();
        assert_eq!((published["protocol"].as_u64(), published["sidecar"].as_bool()), (Some(1), Some(true)));
        assert_eq!(published["secret"].as_str().unwrap().len(), 43, "32 bytes, unpadded");
        let embedded = Serve { holder: Holder::Embedded, ..serve };
        let published: serde_json::Value = serde_json::from_str(&embedded.json()).unwrap();
        assert_eq!((published["embedded"].as_bool(), published.get("sidecar")), (Some(true), None));
    }

    // contract: SEC-06
    #[cfg(unix)]
    #[test]
    fn server_and_serve_are_the_users_alone_whatever_the_store_lets_others_do() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let server = server_dir(dir.path()).unwrap();
        fs::write(server.join(format!("SERVE.{}.tmp", hex(&[0; 4]))), "left by a server that died").unwrap();
        let serve = Serve { instance: [0; 16], secret: [7; 32], endpoints: vec![], holder: Holder::Server };
        publish(&server, &serve).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&server), 0o700);
        assert_eq!(mode(&server.join("SERVE")), 0o600);
    }
}
