//! Where a local server listens and how its clients find it: `<dir>/server/`,
//! which only the user the server runs as may enter, holds `SERVE`, a hint
//! naming the endpoint and the secret a client's challenge is proven with.
//! `LOCK` is the truth; `SERVE` is written once the server listens and
//! removed before it lets go.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use tinystore::pipe::Prove;

/// The protocol `SERVE` says the server speaks.
const PROTOCOL: u64 = 1;

/// What a client needs to find and trust the server: where it listens, and
/// the secret it proves itself with.
pub(crate) struct Serve {
    pub(crate) instance: [u8; 16],
    pub(crate) secret: [u8; 32],
    /// The local endpoint first, which a client of this machine dials; a
    /// remote one after it.
    pub(crate) endpoints: Vec<String>,
    pub(crate) sidecar: bool,
}

impl Serve {
    pub(crate) fn new(endpoints: Vec<String>, sidecar: bool) -> io::Result<Serve> {
        let (mut instance, mut secret) = ([0; 16], [0; 32]);
        getrandom::fill(&mut instance).and_then(|()| getrandom::fill(&mut secret)).map_err(io::Error::other)?;
        Ok(Serve { instance, secret, endpoints, sidecar })
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
        if self.sidecar {
            published["sidecar"] = serde_json::Value::Bool(true);
        }
        published.to_string()
    }

    /// Answers a HELLO's challenge with the HMAC-SHA256 of it, keyed with
    /// the secret.
    pub(crate) fn prove(&self) -> Prove {
        let secret = self.secret;
        Arc::new(move |challenge: &[u8]| {
            let mut mac = Hmac::<Sha256>::new_from_slice(&secret).expect("HMAC takes a key of any length");
            mac.update(challenge);
            mac.finalize().into_bytes().to_vec()
        })
    }
}

/// `<dir>/server/`, made the user's alone, whatever the store's directory
/// lets others do: mode 0700, or on Windows a protected DACL of the user's,
/// which what it holds inherits. A directory another user made first is
/// refused, since its owner could let anyone in: on Unix its mode cannot be
/// changed, and on Windows its owner is read and a link in its place refused.
pub(crate) fn server_dir(store: &Path) -> io::Result<PathBuf> {
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
    crate::owner_only::protect_dir(&dir)?;
    Ok(dir)
}

/// Writes `SERVE` whole, through a temporary file renamed into place.
pub(crate) fn publish(server: &Path, serve: &Serve) -> io::Result<()> {
    let written = server.join(format!("SERVE.{}.tmp", hex(&serve.instance[..4])));
    let mut file = create_private(&written)?;
    file.write_all(serve.json().as_bytes())?;
    drop(file);
    patiently(|| fs::rename(&written, server.join("SERVE")))
}

/// A new file the user alone may open from the moment it is made: mode 0600,
/// or on Windows a protected DACL of the user's. One a server that died left
/// is removed first, since a file that is there keeps its permissions and is
/// not made again.
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
    let file = crate::owner_only::create_new(path)?;
    Ok(file)
}

/// Removes `SERVE`, if there is one: a server leaving, or one left behind,
/// whose endpoint may be another process's by now.
pub(crate) fn unpublish(server: &Path) -> io::Result<()> {
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
pub(crate) fn hash(store: &Path) -> String {
    hex(&Sha256::digest(store.to_string_lossy().as_bytes())[..8])
}

/// The Unix socket a local server listens on: `<dir>/server/tinystore.sock`
/// when that path fits a `sockaddr_un`, 104 bytes on macOS and the BSDs and
/// 108 on Linux; past it, a directory of the owner's own named by the hash.
#[cfg(unix)]
pub(crate) fn socket_path(store: &Path, server: &Path) -> io::Result<PathBuf> {
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

/// The Windows named pipe a local server listens on, by its short name.
#[cfg(windows)]
pub(crate) fn pipe_name(store: &Path) -> String {
    format!("tinystore-{}", hash(store))
}

/// An instance of the store's pipe, `\\.\pipe\<short name>`, which refuses
/// remote clients and admits the user alone. Named pipes share one namespace
/// across every user, so the first instance fails when another process holds
/// the name, and the server finds a squatter rather than its clients.
#[cfg(windows)]
pub(crate) fn pipe_instance(name: &str, first: bool) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
    options.first_pipe_instance(first).reject_remote_clients(true);
    crate::owner_only::create_pipe(&options, name)
}

/// Bytes as lowercase hex, two digits each.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Base64url without padding, as `SERVE` spells its random bytes.
pub(crate) fn base64url(bytes: &[u8]) -> String {
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

    #[test]
    fn the_proof_is_the_hmac_of_the_challenge() {
        let serve = Serve { instance: [0; 16], secret: [7; 32], endpoints: vec!["pipe:x".to_owned()], sidecar: true };
        let mut mac = Hmac::<Sha256>::new_from_slice(&[7; 32]).unwrap();
        mac.update(b"0123456789abcdef");
        assert_eq!(serve.prove()(b"0123456789abcdef"), mac.finalize().into_bytes().to_vec());
        let published: serde_json::Value = serde_json::from_str(&serve.json()).unwrap();
        assert_eq!((published["protocol"].as_u64(), published["sidecar"].as_bool()), (Some(1), Some(true)));
        assert_eq!(published["secret"].as_str().unwrap().len(), 43, "32 bytes, unpadded");
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
        let serve = Serve { instance: [0; 16], secret: [7; 32], endpoints: vec![], sidecar: false };
        publish(&server, &serve).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&server), 0o700);
        assert_eq!(mode(&server.join("SERVE")), 0o600);
    }
}
