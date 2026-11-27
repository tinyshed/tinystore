//! The body of the wire's fuzz target, which `fuzz/` runs by hand on nightly
//! through cargo-fuzz, and tests run on every vector on every platform CI
//! runs on: any bytes read as any message of the schema are refused or read
//! whole, and what was read writes within its size and reads again the same.

use std::sync::OnceLock;

use super::protocol;

/// Every message testdata/wire/protocol.json holds a vector of, which
/// crates/protocol writes for each message of `protocol/*.wire`, by name.
fn names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../../../../testdata/wire/protocol.json")).expect("the vectors' JSON");
        let messages = vectors["messages"].as_array().expect("the vectors' messages");
        let mut names: Vec<String> =
            messages.iter().filter_map(|vector| vector["message"].as_str()).map(str::to_owned).collect();
        names.sort();
        names.dedup();
        names
    })
}

/// Reads the bytes after the first two as the message they pick, by their
/// number among every message's name: a refusal is an answer, and a message
/// read writes within the size it says and reads again as the same bytes.
pub fn messages(bytes: &[u8]) {
    let Some((pick, body)) = bytes.split_first_chunk::<2>() else { return };
    let names = names();
    let name = &names[usize::from(u16::from_le_bytes(*pick)) % names.len()];
    // a message of an engine this build leaves out
    let Some(read) = protocol::rewrite(name, body) else { return };
    let Ok((written, size)) = read else { return };
    assert!(written.len() <= size, "{name}: {} bytes, past the {size} its size says", written.len());
    let again = protocol::rewrite(name, &written).expect("the message's name");
    let again = again.unwrap_or_else(|failure| panic!("{name}: what it wrote is refused: {}", failure.message));
    assert_eq!(again.0, written, "{name}: read again, it writes the same bytes");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
    }

    /// Each vector's bytes after the two that pick its message.
    fn seeds() -> Vec<Vec<u8>> {
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../../../../testdata/wire/protocol.json")).unwrap();
        let names = names();
        let picked = |message: &str| u16::try_from(names.iter().position(|name| name == message).unwrap()).unwrap();
        let vectors = vectors["messages"].as_array().unwrap();
        let seed = |vector: &serde_json::Value| {
            let mut seed = picked(vector["message"].as_str().unwrap()).to_le_bytes().to_vec();
            seed.extend(hex(vector["hex"].as_str().unwrap()));
            seed
        };
        vectors.iter().map(seed).collect()
    }

    // A build that leaves an engine out reads none of its messages.
    #[cfg(all(
        feature = "kv",
        feature = "config",
        feature = "locks",
        feature = "channels",
        feature = "jobs",
        feature = "sql",
        feature = "blobs",
        feature = "records",
        feature = "metrics",
        feature = "search"
    ))]
    #[test]
    fn every_message_of_the_schema_is_one_the_target_reads() {
        let names = names();
        assert!(names.len() > 200, "{} messages", names.len());
        for name in names {
            assert!(protocol::rewrite(name, &[0x80]).is_some(), "{name}: not read");
        }
    }

    // contract: WIRE-02
    #[test]
    fn every_vector_with_any_byte_changed_or_cut_is_read_whole_or_refused() {
        for seed in seeds() {
            messages(&seed);
            for at in 2..seed.len() {
                messages(&seed[..at]);
                for flip in [0x01, 0x80, 0xff] {
                    let mut changed = seed.clone();
                    changed[at] ^= flip;
                    messages(&changed);
                }
            }
        }
    }

    #[test]
    fn bytes_of_no_message_are_refused() {
        for bytes in [&b""[..], b"\x00", b"\x00\x00", &[0xff; 64], b"\x07\x00\xdf\xff\xff\xff\xff"] {
            messages(bytes);
        }
    }
}
