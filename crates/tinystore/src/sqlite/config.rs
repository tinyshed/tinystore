use std::time::Duration;

/// How a file's connections are set up. Every setting travels with every
/// connection, so a reader opened again is the same reader.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    pub(crate) durability: Durability,
    /// The page size of a new file; an existing file keeps its own.
    pub(crate) page_size: u32,
    /// The page cache of the writer and of each reader, in KiB.
    pub(crate) cache_kib: u32,
    /// Readers open at once at most. One stays open; the others close once
    /// unused for `reader_idle`.
    pub(crate) readers: usize,
    pub(crate) reader_idle: Duration,
    /// How long a read waits for a free reader before it is `Unavailable`.
    pub(crate) reader_patience: Duration,
    /// Prepared statements each connection keeps, the least recently used
    /// closed first.
    pub(crate) statements: usize,
    /// How long a statement waits for another process's lock.
    pub(crate) busy_timeout: Duration,
    pub(crate) group: GroupLimits,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            durability: Durability::Full,
            page_size: 4096,
            cache_kib: 1024,
            readers: readers_of_this_machine(),
            reader_idle: Duration::from_secs(60),
            reader_patience: Duration::from_secs(10),
            statements: 32,
            busy_timeout: Duration::from_secs(5),
            group: GroupLimits::default(),
        }
    }
}

/// The readers a file opens at most: one a processor, four to sixteen.
///
/// A caller that finds every reader taken sleeps until one comes back, and
/// that wake costs several reads: with 4 readers, 64 threads read a third as
/// many keys a second as 4 threads did, and with 8 readers 16 threads read
/// 0.6 of what 8 did (research rust-slice-2026-10-10). A reader opens when a
/// read needs it and closes a minute unused, so the bound costs nothing until
/// that many threads read at once.
fn readers_of_this_machine() -> usize {
    std::thread::available_parallelism().map_or(4, std::num::NonZero::get).clamp(4, 16)
}

/// How far a commit goes before it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Durability {
    /// WAL with `synchronous=FULL`: every commit syncs before it returns, and
    /// survives a power loss.
    Full,
    /// WAL with `synchronous=NORMAL`: a commit reaches the operating system and
    /// syncs come at checkpoints. It survives the application's crash; a power
    /// loss may take the last commits, never the file's consistency.
    Os,
}

/// How much one grouped commit carries. A write heavier than `bytes` commits
/// alone.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GroupLimits {
    pub(crate) writes: usize,
    pub(crate) bytes: usize,
}

impl Default for GroupLimits {
    fn default() -> Self {
        Self { writes: 1024, bytes: 8 << 20 }
    }
}
