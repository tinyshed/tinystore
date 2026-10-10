//! What makes a file's name outlive a power loss: its directory's own sync.

use std::io;
use std::path::Path;

/// Makes a directory's names durable: its own sync.
#[cfg(not(windows))]
pub(crate) fn sync_directory(dir: &Path) -> io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// Makes a directory's names durable. Windows opens a directory only for
/// backup and flushes it as a file, in about a millisecond.
#[cfg(windows)]
pub(crate) fn sync_directory(dir: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .access_mode(GENERIC_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)?
        .sync_all()
}
