//! The application's files: bytes by path in named sets, written whole and
//! replaced whole, checked against their SHA-256 when read whole, and kept
//! inline in `blobs/blobs.db` up to 16 KiB or as a file each past it.
//!
//! ```no_run
//! # fn main() -> tinystore::Result<()> {
//! let store = tinystore::Store::open("data", Default::default())?;
//! let avatars = store.files("avatars").open()?;
//! avatars.key("42.png").content_type("image/png").put(b"...")?;
//! let mut avatar = avatars.get("42.png")?.expect("just written");
//! assert_eq!(avatar.info().content_type, "image/png");
//! let bytes = avatar.read_all()?; // checked against the SHA-256 taken when it was written
//! # Ok(())
//! # }
//! ```

mod change;
mod disk;
mod engine;
mod files;
#[cfg(test)]
mod fixture;
mod ids;
mod info;
mod list;
mod maintain;
mod paths;
mod read;
mod scrub;
mod upload;

pub use files::{FileCall, Files, FilesBuilder};
pub use info::{FileInfo, Page, Usage};
pub use list::{All, List, PAGE_FILES};
pub use maintain::{Maintenance, maintain};
pub use read::StoredFile;
pub use upload::Upload;

use crate::Store;

impl Store {
    /// The set of files `name`, made the first time it opens: bytes by path,
    /// written whole and replaced whole.
    pub fn files(&self, name: &str) -> FilesBuilder<'_> {
        FilesBuilder::new(self, name)
    }
}
