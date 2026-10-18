//! What the tests of several engines share.

use std::time::{Duration, Instant};

/// How long a test waits for what another thread does before it fails.
pub(crate) const PATIENCE: Duration = Duration::from_secs(10);

/// Waits until `holds` does, failing the test, named by `what`, once
/// `within` passed.
pub(crate) fn holds_within(within: Duration, what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !holds() {
        assert!(Instant::now() < deadline, "{what}: not within {within:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
