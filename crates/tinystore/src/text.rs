//! Text more than one part of the store writes or checks alike.

/// Bytes as lowercase hex, two digits each.
#[cfg(any(feature = "kv", feature = "backup", feature = "console"))]
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether `name` is a name of the store's: `[a-z0-9][a-z0-9_-]{0,63}`, as a
/// bucket, a queue, a database, a stream or an index is named.
#[cfg(any(
    feature = "kv",
    feature = "jobs",
    feature = "sql",
    feature = "blobs",
    feature = "records",
    feature = "search",
    feature = "channels"
))]
pub(crate) fn is_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let first = bytes.first().is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let rest =
        bytes.iter().all(|&byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-');
    first && rest && bytes.len() <= 64
}
