use std::fmt;
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

impl Config {
    /// The settings of a file whose commits go as far as `durability` says:
    /// `Full` where it says nothing, as kv, jobs and sql keep theirs.
    pub(crate) fn committing(durability: Option<Durability>) -> Config {
        Config { durability: durability.unwrap_or(Durability::Full), ..Config::default() }
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

/// How far a commit goes before it returns: a setting of a store's files,
/// `kv.db`, `jobs.db` and each database's, which [`Options`](crate::Options)
/// gives for them all and a database may say for itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// Every commit is synced to the disk before it returns, and survives a
    /// crash of the operating system and a power loss. WAL with
    /// `synchronous=FULL`.
    Full,
    /// A commit is written to the operating system and returns; the disk is
    /// synced at checkpoints. It survives the program's crash. A crash of the
    /// operating system or a power loss may take the last commits, never the
    /// file's consistency. WAL with `synchronous=NORMAL`.
    Os,
}

impl fmt::Display for Durability {
    /// The word every SDK says it with: `full` or `os`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Durability::Full => "full",
            Durability::Os => "os",
        })
    }
}

impl std::str::FromStr for Durability {
    type Err = crate::Error;

    /// Reads the word every SDK, the wire and the server's command line say
    /// it with.
    fn from_str(word: &str) -> Result<Durability, crate::Error> {
        match word {
            "full" => Ok(Durability::Full),
            "os" => Ok(Durability::Os),
            other => Err(crate::Error::invalid(format!("durability {other:?}: it is full or os"))),
        }
    }
}

/// How much one grouped commit carries. A write heavier than `bytes` commits
/// alone.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GroupLimits {
    pub(crate) writes: usize,
    pub(crate) bytes: usize,
    /// The longest a commit waits for the writes it expects.
    pub(crate) gather: Duration,
}

impl Default for GroupLimits {
    fn default() -> Self {
        Self { writes: 1024, bytes: 8 << 20, gather: Duration::from_millis(2) }
    }
}
