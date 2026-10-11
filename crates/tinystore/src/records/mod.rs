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

mod bits;
mod block;
mod bloom;
mod bytes;
mod count;
mod engine;
mod event;
mod events;
#[cfg(test)]
mod fixture;
mod head;
mod ints;
mod maintain;
mod merge;
mod pack;
mod read;
mod segment;
mod stamp;
mod text;
mod values;

pub use count::{Aggregate, Number, Row, Stats, StreamInfo, Top};
pub use event::{Event, Fields, Level, NewEvent, SpanId, TraceId};
pub use events::{AllEvents, EventCall, Events, EventsBuilder};
pub use maintain::{Maintenance, maintain};
pub use read::{All, List, PAGE_EVENTS, Page};

use crate::{Result, Store};

impl Store {
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
