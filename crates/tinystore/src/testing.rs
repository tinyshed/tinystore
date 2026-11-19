//! A store for a program's tests, in a directory of its own that its close
//! removes, with the `testing` feature; and what the library's own tests of
//! several engines share.
//!
//! ```no_run
//! # fn main() -> tinystore::Result<()> {
//! let store = tinystore::testing::open_temp(tinystore::Options::default())?;
//! println!("the test's store is in {}", store.dir().display());
//! store.close()?; // or drop it: either removes the directory
//! # Ok(())
//! # }
//! ```

#[cfg(all(test, any(feature = "channels", feature = "locks", feature = "jobs", feature = "mcp")))]
use std::time::{Duration, Instant};

#[cfg(feature = "testing")]
#[path = "testing_temp.rs"]
mod temp;
#[cfg(feature = "testing")]
pub use temp::{TempStore, open_temp};

/// How long a test waits for what another thread does before it fails.
#[cfg(all(test, any(feature = "channels", feature = "locks", feature = "jobs", feature = "mcp")))]
pub(crate) const PATIENCE: Duration = Duration::from_secs(10);

/// Waits until `holds` does, failing the test, named by `what`, once
/// `within` passed.
#[cfg(all(test, any(feature = "channels", feature = "locks", feature = "jobs", feature = "mcp")))]
pub(crate) fn holds_within(within: Duration, what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !holds() {
        assert!(Instant::now() < deadline, "{what}: not within {within:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Closes `store`, and says what the close answered and how long it took: a
/// close from inside a handler it waits for answers at once.
#[cfg(all(test, any(feature = "channels", feature = "jobs")))]
pub(crate) fn timed_close(store: &crate::Store) -> (crate::Result<()>, Duration) {
    let began = Instant::now();
    let closed = store.close();
    (closed, began.elapsed())
}
