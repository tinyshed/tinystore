#![allow(unsafe_code)]
//! The disk's free space, which only the system can say: the store keeps some
//! of it free, so that a file never fills the disk its databases write to.

use std::io;
use std::path::Path;

/// Refuses `coming` bytes more on the disk that holds `dir` when they would
/// leave it less free than the store keeps, `keep_free`, asking the system
/// how much is free now; a store that keeps none asks nothing.
#[cfg(any(feature = "blobs", feature = "backup"))]
pub(crate) fn room(dir: &Path, keep_free: u64, coming: u64) -> crate::Result<()> {
    if keep_free == 0 {
        return Ok(());
    }
    let free = available(dir).map_err(|error| crate::Error::io("the disk's free space", error))?;
    if free >= keep_free.saturating_add(coming) {
        return Ok(());
    }
    let refused = format!(
        "{coming} bytes more, where the disk has {free} free, would leave it less than the {keep_free} the store keeps"
    );
    let space = crate::limits::Limit { name: "disk space", bound: free.saturating_sub(keep_free) };
    Err(space.refuse(coming, refused))
}

/// The bytes this process may still write on the file system that holds
/// `path`, as the system counts them: a quota and a root's reserve left out.
#[cfg(all(unix, not(target_vendor = "apple")))]
pub(crate) fn available(path: &Path) -> io::Result<u64> {
    let path = c_path(path)?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: statvfs reads the NUL-terminated path, which `path` holds
    // through the call, and writes one statvfs into `stat`, made for one.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs returned 0, so it filled `stat`.
    let stat = unsafe { stat.assume_init() };
    // counted in fragments, f_frsize: NFS sets f_bsize to its transfer size
    Ok(bytes(stat.f_bavail, stat.f_frsize))
}

/// As above: statvfs counts in 32 bits on Apple's systems, which ends at
/// 16 TiB of 4 KiB blocks, where statfs counts in 64.
#[cfg(target_vendor = "apple")]
pub(crate) fn available(path: &Path) -> io::Result<u64> {
    let path = c_path(path)?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: statfs reads the NUL-terminated path, which `path` holds
    // through the call, and writes one statfs into `stat`, made for one.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statfs returned 0, so it filled `stat`.
    let stat = unsafe { stat.assume_init() };
    Ok(bytes(stat.f_bavail, stat.f_bsize))
}

#[cfg(windows)]
pub(crate) fn available(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(directory: *const u16, available: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0_u64;
    // SAFETY: the path is NUL-terminated UTF-16 that lives through the call;
    // the one count asked for is a u64 the call writes, and the two left out
    // are null, as the call allows.
    let done =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &raw mut available, std::ptr::null_mut(), std::ptr::null_mut()) };
    if done == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

/// Elsewhere the system is not asked, and the disk never runs short.
#[cfg(not(any(unix, windows)))]
pub(crate) fn available(_: &Path) -> io::Result<u64> {
    Ok(u64::MAX)
}

#[cfg(unix)]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}

/// Blocks of `size` bytes, whatever integers this system counts them in.
#[cfg(unix)]
fn bytes(blocks: impl TryInto<u64>, size: impl TryInto<u64>) -> u64 {
    let (blocks, size) = (blocks.try_into().unwrap_or(0), size.try_into().unwrap_or(0));
    blocks.saturating_mul(size)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_disk_with_room_says_how_much() {
        let dir = tempfile::tempdir().unwrap();
        let free = super::available(dir.path()).unwrap();
        assert!(free > 0 && free < u64::MAX, "{free} bytes");
        assert!(super::available(&dir.path().join("not there")).is_err());
    }
}
