//! Text more than one part of the store writes or checks alike.

/// Bytes as lowercase hex, two digits each.
#[cfg(any(feature = "kv", feature = "backup", feature = "console", feature = "sql", feature = "replication"))]
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

/// Refuses a name that is not one of the store's, `what` saying whose it is,
/// as `a stream's`, with the nearest name that is when there is one:
/// `focusroom.rooms` is offered as `focusroom_rooms`.
#[cfg(any(
    feature = "kv",
    feature = "jobs",
    feature = "sql",
    feature = "blobs",
    feature = "records",
    feature = "search",
    feature = "channels"
))]
pub(crate) fn check_name(what: &str, name: &str) -> crate::Result<()> {
    if is_name(name) {
        return Ok(());
    }
    let nearest = nearest_name(name).map_or_else(String::new, |nearest| format!(", as {nearest:?}"));
    Err(crate::Error::invalid(format!("{what} name {name:?}: a name is [a-z0-9][a-z0-9_-]{{0,63}}{nearest}")))
}

/// `name` in lower case, each other character an `_`, its leading `_` and `-`
/// dropped: a name, unless nothing of it is left.
#[cfg(any(
    feature = "kv",
    feature = "jobs",
    feature = "sql",
    feature = "blobs",
    feature = "records",
    feature = "search",
    feature = "channels"
))]
fn nearest_name(name: &str) -> Option<String> {
    let plain = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-';
    let lowered: String =
        name.chars().map(|c| c.to_ascii_lowercase()).map(|c| if plain(c) { c } else { '_' }).collect();
    let nearest: String = lowered.trim_start_matches(['_', '-']).chars().take(64).collect();
    is_name(&nearest).then_some(nearest)
}

/// FNV-1a 64 of `bytes`: a hash every build and every language computes the
/// same, for a phase kept in text, a jitter that must come out the same and a
/// filter's bits.
#[cfg(any(feature = "jobs", feature = "records"))]
pub(crate) fn fnv1a(bytes: impl IntoIterator<Item = u8>) -> u64 {
    let mix = |hash: u64, byte: u8| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, mix)
}

/// A span in its largest whole unit, as a message says it: 7d, 90s, 1500ms.
#[cfg(any(feature = "records", feature = "metrics"))]
pub(crate) fn span(span: std::time::Duration) -> String {
    let millis = span.as_millis();
    let units = [("d", 86_400_000), ("h", 3_600_000), ("m", 60_000), ("s", 1000)];
    match units.iter().find(|(_, unit)| millis >= *unit && millis.is_multiple_of(*unit)) {
        Some((name, unit)) => format!("{}{name}", millis / unit),
        None => format!("{millis}ms"),
    }
}

/// Whether the character at `at`, past the first, begins a word of a name
/// written in camel case: a capital after a small letter or a digit, or a
/// capital before a small letter after capitals, as `passwordHash` and
/// `APIKey` split.
#[cfg(any(feature = "config", feature = "console"))]
pub(crate) fn starts_word(chars: &[char], at: usize) -> bool {
    if !chars[at].is_uppercase() {
        return false;
    }
    let before = chars[at - 1];
    let lower_after = chars.get(at + 1).is_some_and(|c| c.is_lowercase());
    before.is_lowercase() || before.is_numeric() || before.is_uppercase() && lower_after
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(any(feature = "records", feature = "metrics"))]
    fn a_span_is_said_in_its_largest_whole_unit() {
        use std::time::Duration;
        let said = [(86_400 * 30, "30d"), (3600 * 12, "12h"), (90, "90s"), (60, "1m")];
        for (seconds, text) in said {
            assert_eq!(super::span(Duration::from_secs(seconds)), text);
        }
        assert_eq!(super::span(Duration::from_millis(1500)), "1500ms");
    }

    #[test]
    #[cfg(feature = "kv")]
    fn a_refused_name_is_offered_its_nearest() {
        let offered = |name: &str| super::check_name("a bucket's", name).unwrap_err().to_string();
        assert!(offered("focusroom.rooms").contains(r#"a bucket's name "focusroom.rooms""#));
        assert!(offered("focusroom.rooms").ends_with(r#", as "focusroom_rooms": invalid"#), "{}", offered("x"));
        assert!(offered("Plant Care").contains(r#"as "plant_care""#));
        assert!(offered("_drafts").contains(r#"as "drafts""#));
        assert!(!offered("...").contains(" as "), "nothing of it is left to offer");
        super::check_name("a bucket's", "focusroom_rooms").unwrap();
    }
}
