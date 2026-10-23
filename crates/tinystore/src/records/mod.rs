//! An application's logs and events: named streams of events in
//! `records.db`, written whole, read back by time and by what they hold.
//!
//! ```no_run
//! # fn main() -> tinystore::Result<()> {
//! let store = tinystore::Store::open("data", Default::default())?;
//! let audit = store.events("audit").open()?;
//! audit.append("role.changed", &serde_json::json!({ "user": 42 }))?;
//! let page = audit.list().filter("user", &42).newest().limit(50).page()?;
//! assert_eq!(page.events[0].name, "role.changed");
//! # Ok(())
//! # }
//! ```

mod ahead;
mod block;
mod bloom;
mod buffer;
mod count;
mod engine;
mod event;
mod events;
#[cfg(test)]
mod fixture;
#[cfg(any(test, feature = "fuzzing"))]
#[doc(hidden)]
pub mod fuzzing;
mod head;
mod ints;
mod level;
mod lines;
mod logfmt;
mod logger;
mod maintain;
mod merge;
mod pack;
mod poll;
mod read;
mod repair;
mod segment;
mod stamp;
mod tail;
mod text;
mod values;

pub use count::{Aggregate, Number, Row, Stats, StreamInfo, Top};
pub use event::{Event, Fields, Level, NewEvent, SpanId, TraceId};
pub use events::{AllEvents, EventCall, Events, EventsBuilder};
pub use lines::Lines;
pub use logger::LoggerBuilder;
pub use maintain::{Maintenance, maintain};
pub use poll::PollBatch;
pub use read::{All, List, PAGE_EVENTS, Page};
pub use repair::{Damage, Part};
pub use tail::{Tail, TailBatch};

use crate::{Result, Store};

impl Store {
    /// A logger of the stream `stream`: a `tracing` layer that prints each
    /// line as it is logged, and keeps it within a second without ever
    /// making its caller wait.
    ///
    /// ```no_run
    /// # fn main() -> tinystore::Result<()> {
    /// use tracing_subscriber::layer::SubscriberExt;
    /// use tracing_subscriber::util::SubscriberInitExt;
    ///
    /// let store = tinystore::Store::open("data", Default::default())?;
    /// tracing_subscriber::registry().with(store.logger("api").layer()?).init();
    /// tracing::info!(user = 42, "signed in");
    /// # Ok(())
    /// # }
    /// ```
    pub fn logger(&self, stream: &str) -> LoggerBuilder<'_> {
        LoggerBuilder::new(self, stream)
    }

    /// The stream of events `stream`, made the first time it opens: events
    /// appended whole, and read back by time and by what they hold.
    pub fn events(&self, stream: &str) -> EventsBuilder<'_> {
        EventsBuilder::new(self, stream)
    }

    /// Every stream's events, to read.
    pub fn all_events(&self) -> Result<AllEvents> {
        Ok(AllEvents { records: engine::Records::of(self)? })
    }
}
