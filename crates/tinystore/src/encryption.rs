//! The store's encryption keys, and the values sealed with them.
//!
//! A key is 32 random bytes, kept as 64 hex digits in a file. The store seals
//! with one: `encryption.key` of its directory, which the store makes when an
//! encrypted bucket opens and no key is there yet, or the file the store's
//! options name, which it only reads. Its older keys, which the options name
//! too, only open what they sealed before: a key being rotated out, or one
//! found again after a restore that left it out. A value
//! is sealed with XChaCha20-Poly1305 under a nonce of its own and bound to its
//! place, so that it opens nowhere else:
//!
//! ```text
//! 1 | the key's id, 4 bytes | the nonce, 24 bytes | the value sealed, 16 bytes longer than the value
//! ```
//!
//! The id is the first bytes of a SHA-256 over the key. It says which key
//! sealed a value, so that the store opens it with that key, and one whose
//! key the store does not hold says so rather than calling the value corrupt.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use chacha20poly1305::Key as CipherKey;
use chacha20poly1305::aead::{Aead, Generate, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};

use crate::durable::sync_directory;
use crate::{Error, Result};

/// The file a store makes its key in, in its directory.
pub(crate) const FILE: &str = "encryption.key";

/// What finds the id of a key that values in a store's directory are sealed
/// with, which kv knows: the store makes no key while one is.
pub(crate) type SealedIn = fn(&Path) -> Result<Option<String>>;

const FORMAT: u8 = 1;
const ID: usize = 4;
const NONCE: usize = 24;
const TAG: usize = 16;
/// What a sealed value has before its cipher: the format, the key's id and
/// the nonce.
const HEAD: usize = 1 + ID + NONCE;

/// A key and what it seals and opens.
pub(crate) struct EncryptionKey {
    cipher: XChaCha20Poly1305,
    id: [u8; ID],
}

impl EncryptionKey {
    /// The key of `file`, which is there already.
    pub(crate) fn read(file: &Path) -> Result<EncryptionKey> {
        let text = fs::read_to_string(file).map_err(|error| Error::io(shown(file), error))?;
        key_of(&text).map(|key| EncryptionKey::of(&key)).map_err(|error| error.within(shown(file)))
    }

    /// The key of `file`, made when the file is not there: durable before it
    /// is used, since a value sealed with a key that a power loss took opens
    /// never again.
    pub(crate) fn read_or_make(file: &Path) -> Result<EncryptionKey> {
        if file.exists() {
            return EncryptionKey::read(file);
        }
        let key = CipherKey::try_generate()
            .map_err(|error| Error::internal(format!("{}: the system's random bytes: {error}", shown(file))))?;
        write_new(file, &crate::text::hex(&key)).map_err(|error| Error::io(shown(file), error))?;
        let made = EncryptionKey::of(&key);
        tracing::info!(
            target: "tinystore",
            file = %file.display(),
            key = %made.id(),
            "made the store's encryption key: keep a copy apart from the data, since nothing else opens what it seals"
        );
        Ok(made)
    }

    /// The key's id, as an error and a bucket's row name it.
    pub(crate) fn id(&self) -> String {
        crate::text::hex(&self.id)
    }

    fn of(key: &CipherKey) -> EncryptionKey {
        let digest = Sha256::new().chain_update(b"tinystore encryption key id").chain_update(key).finalize();
        let mut id = [0; ID];
        id.copy_from_slice(&digest[..ID]);
        EncryptionKey { cipher: XChaCha20Poly1305::new(key), id }
    }

    /// `value` sealed for `place`, which nothing but the same place opens.
    pub(crate) fn seal(&self, place: &[u8], value: &[u8]) -> Result<Vec<u8>> {
        let nonce =
            XNonce::try_generate().map_err(|error| Error::internal(format!("the system's random bytes: {error}")))?;
        let cipher = self
            .cipher
            .encrypt(&nonce, Payload { msg: value, aad: place })
            .map_err(|_| Error::internal("a value too long to seal"))?;
        let mut sealed = Vec::with_capacity(HEAD + cipher.len());
        sealed.push(FORMAT);
        sealed.extend_from_slice(&self.id);
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&cipher);
        Ok(sealed)
    }

    /// The value `sealed` holds, which this key sealed for `place`: bytes
    /// changed since, or sealed for another place, are `Corrupt`.
    fn open(&self, place: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        let (nonce, cipher) = sealed[HEAD - NONCE..].split_at(NONCE);
        let nonce = XNonce::try_from(nonce).map_err(|_| Error::internal("a nonce of another length"))?;
        self.cipher
            .decrypt(&nonce, Payload { msg: cipher, aad: place })
            .map_err(|_| Error::corrupt("its value does not open: it was changed, or sealed for another place"))
    }
}

/// The store's keys: the one that seals every value written, and the older
/// ones, which only open what they sealed.
pub(crate) struct Keyring {
    sealing: EncryptionKey,
    older: Vec<EncryptionKey>,
}

impl Keyring {
    /// The keys, an older one that is the sealing key or given twice kept once.
    pub(crate) fn of(sealing: EncryptionKey, older: impl IntoIterator<Item = EncryptionKey>) -> Keyring {
        let mut ring = Keyring { sealing, older: Vec::new() };
        for key in older {
            if !ring.holds(&key.id()) {
                ring.older.push(key);
            }
        }
        ring
    }

    /// The id of the key that seals.
    pub(crate) fn id(&self) -> String {
        self.sealing.id()
    }

    /// Whether one of the keys is the key of the id `id`.
    pub(crate) fn holds(&self, id: &str) -> bool {
        self.keys().any(|key| key.id() == id)
    }

    fn keys(&self) -> impl Iterator<Item = &EncryptionKey> {
        std::iter::once(&self.sealing).chain(&self.older)
    }

    /// `value` sealed with the sealing key for `place`.
    pub(crate) fn seal(&self, place: &[u8], value: &[u8]) -> Result<Vec<u8>> {
        self.sealing.seal(place, value)
    }

    /// The value `sealed` holds, opened with the key that sealed it for
    /// `place`. One sealed with a key the store does not hold is `Invalid`
    /// and names it; bytes changed since, or sealed for another place, are
    /// `Corrupt`.
    pub(crate) fn open(&self, place: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        let id = sealed_with(sealed).ok_or_else(|| Error::corrupt("its value is not one the store sealed"))?;
        let Some(key) = self.keys().find(|key| key.id == id) else {
            return Err(Error::invalid(format!(
                "its value was sealed with the key {}, which the store does not hold, and it seals with {}: \
                 give the store that key among its older encryption keys",
                crate::text::hex(&id),
                self.id()
            )));
        };
        key.open(place, sealed)
    }

    /// Whether `sealed` was sealed with the key that seals now, as nothing
    /// left to seal again.
    pub(crate) fn sealed_it(&self, sealed: &[u8]) -> bool {
        sealed_with(sealed) == Some(self.sealing.id)
    }
}

impl fmt::Debug for Keyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let older: Vec<String> = self.older.iter().map(EncryptionKey::id).collect();
        f.debug_struct("Keyring").field("sealing", &self.id()).field("older", &older).finish()
    }
}

/// The id of the key that sealed `sealed`, as its head says; none for bytes
/// that are no value the store sealed.
fn sealed_with(sealed: &[u8]) -> Option<[u8; ID]> {
    if sealed.len() < HEAD + TAG || sealed[0] != FORMAT {
        return None;
    }
    sealed[1..=ID].try_into().ok()
}

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EncryptionKey({})", self.id())
    }
}

/// `N` bytes from the system's source of random bytes, the one the store's key
/// and its nonces come from.
#[cfg(feature = "locks")]
pub(crate) fn random<const N: usize>() -> Result<[u8; N]> {
    <[u8; N]>::try_generate().map_err(|error| Error::internal(format!("the system's random bytes: {error}")))
}

fn shown(file: &Path) -> String {
    format!("the encryption key {}", file.display())
}

/// The key a file's text holds: 64 hex digits, and spaces around them.
fn key_of(text: &str) -> Result<CipherKey> {
    let digits = text.trim();
    let bytes: Option<Vec<u8>> = match digits.len() == 64 && digits.bytes().all(|digit| digit.is_ascii_hexdigit()) {
        true => (0..32).map(|at| u8::from_str_radix(&digits[at * 2..at * 2 + 2], 16).ok()).collect(),
        false => None,
    };
    bytes
        .and_then(|bytes| CipherKey::try_from(bytes.as_slice()).ok())
        .ok_or_else(|| Error::invalid("a key is 64 hex digits, 32 random bytes, as `openssl rand -hex 32` writes"))
}

/// Writes a file that is not there, its owner's alone where the system has
/// owners, and makes it and its name durable. It is written beside its name
/// and renamed once whole: a crash between creating and writing it would
/// leave an empty key that no open reads again.
fn write_new(file: &Path, line: &str) -> io::Result<()> {
    let mut beside = file.as_os_str().to_owned();
    beside.push(".new");
    let beside = std::path::PathBuf::from(beside);
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut made = options.open(&beside)?;
    made.write_all(format!("{line}\n").as_bytes())?;
    made.sync_all()?;
    drop(made);
    fs::rename(&beside, file)?;
    file.parent().map_or(Ok(()), sync_directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    fn key() -> EncryptionKey {
        EncryptionKey::of(&CipherKey::try_generate().unwrap())
    }

    // contract: SEC-03
    #[test]
    fn a_value_opens_only_at_its_place_with_its_key_and_unchanged() {
        let ring = Keyring::of(key(), []);
        let sealed = ring.seal(b"passwords/42", b"hunter2").unwrap();
        assert_eq!(ring.open(b"passwords/42", &sealed).unwrap(), b"hunter2");
        assert_eq!(sealed.len(), HEAD + b"hunter2".len() + TAG);
        assert!(!sealed.windows(7).any(|window| window == b"hunter2"));

        assert_eq!(ring.open(b"passwords/43", &sealed).unwrap_err().kind(), ErrorKind::Corrupt);
        let mut changed = sealed.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert_eq!(ring.open(b"passwords/42", &changed).unwrap_err().kind(), ErrorKind::Corrupt);
        assert_eq!(ring.open(b"passwords/42", b"hunter2").unwrap_err().kind(), ErrorKind::Corrupt);

        let other = Keyring::of(key(), []).open(b"passwords/42", &sealed).unwrap_err();
        assert_eq!(other.kind(), ErrorKind::Invalid);
        assert!(other.to_string().contains(&ring.id()), "{other}");
    }

    // contract: SEC-04
    #[test]
    fn a_ring_seals_with_its_one_key_and_opens_with_any() {
        let (old, new) = (CipherKey::try_generate().unwrap(), CipherKey::try_generate().unwrap());
        let before = Keyring::of(EncryptionKey::of(&old), []);
        let sealed_before = before.seal(b"passwords/42", b"hunter2").unwrap();
        let ring = Keyring::of(EncryptionKey::of(&new), [EncryptionKey::of(&old), EncryptionKey::of(&new)]);
        assert_eq!(ring.older.len(), 1, "the sealing key given again is kept once");
        assert_eq!(ring.open(b"passwords/42", &sealed_before).unwrap(), b"hunter2", "an older key opens");
        assert!(!ring.sealed_it(&sealed_before));

        let sealed = ring.seal(b"passwords/42", b"hunter2").unwrap();
        assert!(ring.sealed_it(&sealed));
        let without = before.open(b"passwords/42", &sealed).unwrap_err();
        assert_eq!(without.kind(), ErrorKind::Invalid, "a store without the new key opens none of its values");
        assert!(without.to_string().contains(&ring.id()), "{without}");
        assert!(!without.to_string().contains("  "), "one sentence: {without}");
    }

    #[test]
    fn a_value_sealed_twice_is_two_ciphers() {
        let ring = Keyring::of(key(), []);
        assert_ne!(ring.seal(b"p", b"hunter2").unwrap(), ring.seal(b"p", b"hunter2").unwrap());
    }

    #[test]
    fn a_key_is_made_once_and_read_after() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        let made = EncryptionKey::read_or_make(&file).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.trim().len() == 64 && text.trim().chars().all(|digit| digit.is_ascii_hexdigit()), "{text}");
        assert_eq!(EncryptionKey::read_or_make(&file).unwrap().id, made.id);
        assert_eq!(EncryptionKey::read(&file).unwrap().id, made.id);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn a_file_that_is_not_a_key_or_not_there_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let missing = EncryptionKey::read(&dir.path().join("none.key")).unwrap_err();
        assert_eq!(missing.kind(), ErrorKind::Io);
        for text in ["", "hunter2", &"zz".repeat(32), &"ab".repeat(31), &"é".repeat(32), &"+f".repeat(32)] {
            let file = dir.path().join("bad.key");
            fs::write(&file, text).unwrap();
            let refused = EncryptionKey::read(&file).unwrap_err();
            assert_eq!(refused.kind(), ErrorKind::Invalid, "{text:?}");
            assert!(refused.to_string().contains("64 hex digits"), "{refused}");
        }
    }
}
