#![allow(unsafe_code)]
//! What a local endpoint asks of Windows, which only its C API answers, so
//! that this module is a home of `unsafe`: what it keeps to the user it runs
//! as, `server\`, `SERVE` and the named pipe, each with a DACL that grants that
//! user every right and nobody else any, as Unix says with a mode; and the
//! pipe's own calls, which std does not make.
//!
//! The DACL is protected: `server\` inherits nothing of the store's directory,
//! which may let every user in, as a folder at a drive's root does, and what
//! `server\` holds inherits its DACL alone. SYSTEM gets no entry, since the
//! server and its clients run as the user and nothing of Windows opens these
//! for them; the services that back up or scan every file read past a DACL
//! by their privileges.
//!
//! An owner may read and change its object's DACL whatever the DACL says, so
//! no DACL keeps anyone out of a `server\` another user owns: one made first
//! by a user the store's directory lets make folders is refused, not
//! protected.
//!
//! A pipe's handle is open for overlapped calls, so that a read waiting for
//! its client and a write waiting for room run at once, which a handle of
//! blocking calls serializes, and so that a wait looks up every tick at
//! whether to give up: each call waits for its own completion, cancelled or
//! not, before it returns.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE, LocalFree, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_ACCESS_MASK,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, PIPE_ACCESS_DUPLEX, READ_CONTROL, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken};

/// The bytes a pipe's buffers hold each way, as tokio's server gives them.
const PIPE_BUFFER: u32 = 64 << 10;

/// Who besides the user may own `server\`: the administrators, who own what
/// an elevated run makes and whom SEC-06 leaves out, and the system.
const ALSO_TRUSTED: [&str; 2] = ["S-1-5-32-544", "S-1-5-18"];

/// Gives the directory `path` a protected DACL of the user's alone, which
/// what it holds now and is given later inherits, once it is sure the DACL
/// will hold: a directory another user owns is refused, and so is a link,
/// whose own owner is not its target's.
///
/// The directory is held open from its check to its DACL without sharing
/// deletion, so that nobody removes or renames it and puts another in its
/// place between the two.
pub(super) fn protect_dir(path: &Path) -> io::Result<()> {
    let refused = |why: &str| io::Error::new(io::ErrorKind::PermissionDenied, format!("{}: {why}", path.display()));
    let held = held_in_place(path).map_err(|error| within(path, "opening it", &error))?;
    let attributes = held.metadata().map_err(|error| within(path, "reading its attributes", &error))?;
    // std calls only a name surrogate a link, a symlink or a junction: OneDrive's
    // and ProjFS's placeholders are reparse points too, and are the folder itself
    if attributes.file_type().is_symlink() {
        return Err(refused("a link, where the server keeps a directory of its own"));
    }
    let owner = owner_of(&held).map_err(|error| within(path, "reading its owner", &error))?;
    if !may_own(&owner)? {
        return Err(refused(&format!(
            "owned by {owner} rather than the user the server runs as, so its owner could let anyone in \
             whatever its DACL says; remove it, and serve makes it again"
        )));
    }
    // D:P, a protected DACL; A, an entry that allows; OICI, inherited by files
    // and directories; FA, every right a file has
    let descriptor = Descriptor::parse(&format!("D:P(A;OICI;FA;;;{})", user()?))?;
    descriptor.set_on(path).map_err(|error| within(path, "making it the user's alone", &error))?;
    drop(held);
    Ok(())
}

/// Creates the file `path`, which must not be there yet, with a protected
/// DACL of the user's alone from its first moment: a DACL set after it would
/// leave a moment in which it has its directory's, and a handle opened then
/// keeps its access.
pub(super) fn create_new(path: &Path) -> io::Result<File> {
    let descriptor = Descriptor::parse(&format!("D:P(A;;FA;;;{})", user()?))?;
    descriptor.create_file(path).map_err(|error| within(path, "creating it the user's alone", &error))
}

/// Creates an instance of the pipe `name`, the first of its name when
/// `first` says, with a DACL of the user's alone, so that no other user
/// connects to it or makes an instance of its own.
pub(super) fn create_pipe(name: &str, first: bool) -> io::Result<OwnedHandle> {
    Descriptor::parse(&format!("D:P(A;;FA;;;{})", user()?))?.create_pipe(name, first)
}

/// An instance of a pipe this process made, open for overlapped calls: the
/// server's end of one client's connection once a client connected.
pub(super) struct PipeEnd(OwnedHandle);

impl PipeEnd {
    pub(super) fn new(handle: OwnedHandle) -> PipeEnd {
        PipeEnd(handle)
    }

    /// Waits for a client to connect: false when `stop` said to give up first.
    pub(super) fn connect(&self, tick: Duration, stop: &dyn Fn() -> bool) -> io::Result<bool> {
        // SAFETY: the handle is this pipe's, and the OVERLAPPED is the one
        // `overlapped` waits on until the call is done.
        let connected =
            self.overlapped(tick, stop, |overlapped| unsafe { ConnectNamedPipe(self.0.as_raw_handle(), overlapped) });
        match connected {
            Ok(done) => Ok(done.is_some()),
            // a client that connected between the instance's making and this call
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED.cast_signed()) => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// Reads what the client sent, up to `into`'s length: `Some(0)` once it
    /// left, `None` when `stop` said to give up before a byte came.
    pub(super) fn read(&self, into: &mut [u8], tick: Duration, stop: &dyn Fn() -> bool) -> io::Result<Option<usize>> {
        let length = u32::try_from(into.len()).unwrap_or(u32::MAX);
        // SAFETY: the buffer outlives the call, since `overlapped` returns
        // only once the read is done or cancelled, and the system writes no
        // more than `length` bytes of it.
        let read = self.overlapped(tick, stop, |overlapped| unsafe {
            ReadFile(self.0.as_raw_handle(), into.as_mut_ptr(), length, ptr::null_mut(), overlapped)
        });
        match read {
            Err(error) if gone(&error) => Ok(Some(0)),
            read => read.map(|read| read.map(|count| count as usize)),
        }
    }

    /// Writes what the pipe's buffer takes of `bytes`: `None` when `stop`
    /// said to give up before it took any.
    pub(super) fn write(&self, bytes: &[u8], tick: Duration, stop: &dyn Fn() -> bool) -> io::Result<Option<usize>> {
        let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        // SAFETY: as for a read, the bytes outlive the call, which reads no
        // more than `length` of them.
        let written = self.overlapped(tick, stop, |overlapped| unsafe {
            WriteFile(self.0.as_raw_handle(), bytes.as_ptr(), length, ptr::null_mut(), overlapped)
        });
        written.map(|written| written.map(|count| count as usize))
    }

    /// Starts one overlapped call and waits for it, a tick at a time, until
    /// it is done, or until `stop` says to give up, when it is cancelled and
    /// waited for again: a call cancelled once it was done keeps its count.
    fn overlapped(
        &self,
        tick: Duration,
        stop: &dyn Fn() -> bool,
        start: impl FnOnce(*mut OVERLAPPED) -> windows_sys::core::BOOL,
    ) -> io::Result<Option<u32>> {
        let event = Event::new()?;
        let mut overlapped = OVERLAPPED { hEvent: event.0.as_raw_handle(), ..OVERLAPPED::default() };
        let handle = self.0.as_raw_handle();
        let mut count = 0;
        if start(&raw mut overlapped) == 0 {
            // SAFETY: GetLastError reads this thread's last error, which the call set.
            let failed = unsafe { GetLastError() };
            if failed != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(failed.cast_signed()));
            }
        }
        let millis = u32::try_from(tick.as_millis()).unwrap_or(u32::MAX);
        loop {
            // SAFETY: the OVERLAPPED is the call's, which lives on this frame
            // until the call is done, and its event is `event`'s.
            let done = unsafe { GetOverlappedResultEx(handle, &raw const overlapped, &raw mut count, millis, 0) };
            if done != 0 {
                return Ok(Some(count));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(WAIT_TIMEOUT.cast_signed()) {
                return Err(error);
            }
            if stop() {
                break;
            }
        }
        // SAFETY: the call to cancel is this OVERLAPPED's, on this handle.
        unsafe { CancelIoEx(handle, &raw const overlapped) };
        // SAFETY: as above; waiting, the call returns once the system is done
        // with the OVERLAPPED and the bytes, cancelled or completed.
        let done = unsafe { GetOverlappedResult(handle, &raw const overlapped, &raw mut count, 1) };
        if done != 0 {
            return Ok(Some(count));
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code) if code == ERROR_OPERATION_ABORTED.cast_signed() => Ok(None),
            _ => Err(error),
        }
    }
}

/// Whether a pipe's call failed because its client left.
fn gone(error: &io::Error) -> bool {
    let gone = [ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED, ERROR_NO_DATA].map(|code| Some(code.cast_signed()));
    gone.contains(&error.raw_os_error())
}

/// An event a call's completion sets, made reset by hand, as an OVERLAPPED
/// whose completion is waited for takes one.
struct Event(OwnedHandle);

impl Event {
    fn new() -> io::Result<Event> {
        // SAFETY: no attributes and no name are given; the call makes an
        // event, not signalled, reset by hand.
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call made the handle, and nothing else owns it.
        Ok(Event(unsafe { OwnedHandle::from_raw_handle(event) }))
    }
}

/// The SID of the user this process runs as, as SDDL spells it: read once,
/// since a process keeps its user.
fn user() -> io::Result<&'static str> {
    static USER: OnceLock<String> = OnceLock::new();
    if let Some(user) = USER.get() {
        return Ok(user);
    }
    let user = spelled(token_user(&process_token(TOKEN_QUERY)?)?.sid())?;
    Ok(USER.get_or_init(|| user))
}

fn may_own(owner: &str) -> io::Result<bool> {
    Ok(owner == user()? || ALSO_TRUSTED.contains(&owner))
}

/// `path` itself, a link rather than its target, open to read its owner and
/// its attributes, and held in place: listing, unlike reading its owner, is
/// a right that sharing governs, so an open that lists it and does not share
/// deletion keeps out every other open that would remove or rename it.
fn held_in_place(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// The owner of what `file` is open on, as SDDL spells a SID.
fn owner_of(file: &File) -> io::Result<String> {
    let (mut owner, mut descriptor) = (ptr::null_mut(), ptr::null_mut());
    // SAFETY: the handle, open with READ_CONTROL, lives through the call,
    // which writes a pointer to the descriptor it allocated and one to the
    // owner inside it; the group, the DACL and the SACL are not asked for.
    let failed = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if failed != 0 {
        return Err(io::Error::from_raw_os_error(failed.cast_signed()));
    }
    let spelled = spelled(owner);
    // SAFETY: the call allocated the descriptor with LocalAlloc, and the owner
    // inside it is read no more.
    unsafe { LocalFree(descriptor) };
    spelled
}

fn within(path: &Path, doing: &str, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {doing}: {error}", path.display()))
}

/// A security descriptor the system built from SDDL, freed with it.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn parse(sddl: &str) -> io::Result<Descriptor> {
        let sddl = wide(OsStr::new(sddl));
        let mut descriptor = ptr::null_mut();
        // SAFETY: the SDDL is NUL-terminated UTF-16 that lives through the
        // call, which writes one pointer into `descriptor`; the size it may
        // also write is not asked for, which null says.
        let done = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                ptr::null_mut(),
            )
        };
        if done == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Descriptor(descriptor))
    }

    /// Puts the descriptor's DACL on `path`, protected from its parent's, and
    /// on what it holds that inherits.
    fn set_on(&self, path: &Path) -> io::Result<()> {
        let (mut present, mut dacl, mut defaulted) = (0, ptr::null_mut::<ACL>(), 0);
        // SAFETY: the descriptor is valid while `self` lives, and the call
        // writes two flags and a pointer into it, each to its own variable.
        let done = unsafe { GetSecurityDescriptorDacl(self.0, &raw mut present, &raw mut dacl, &raw mut defaulted) };
        if done == 0 {
            return Err(io::Error::last_os_error());
        }
        let path = wide(path.as_os_str());
        let information = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        // SAFETY: the path is NUL-terminated UTF-16 that lives through the
        // call; the DACL lies inside the descriptor, which `self` keeps; the
        // owner, the group and the SACL are null, since only the DACL is set.
        let failed = unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                information,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        match failed {
            0 => Ok(()),
            failed => Err(io::Error::from_raw_os_error(failed.cast_signed())),
        }
    }

    /// Creates an instance of the pipe `name` with this descriptor, as tokio's
    /// server makes one: both ways, of bytes, remote clients refused, and the
    /// first of its name when `first` says.
    fn create_pipe(&self, name: &str, first: bool) -> io::Result<OwnedHandle> {
        let attributes = self.attributes();
        let name = wide(OsStr::new(name));
        let first = if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        // SAFETY: the name is NUL-terminated UTF-16 and `attributes` a whole
        // SECURITY_ATTRIBUTES, whose descriptor `self` keeps, both living
        // through the call.
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | first,
                mode,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                &raw const attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call made the handle, and nothing else owns it.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    /// Creates a file to write, refused when one is there, with this
    /// descriptor from its creation.
    fn create_file(&self, path: &Path) -> io::Result<File> {
        let attributes = self.attributes();
        let path = wide(path.as_os_str());
        let shared = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        // SAFETY: the path is NUL-terminated UTF-16 and `attributes` a whole
        // SECURITY_ATTRIBUTES, whose descriptor `self` keeps, both living
        // through the call; no template file is given.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_WRITE,
                shared,
                &raw const attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call opened the handle, and nothing else owns it.
        Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the system allocated the descriptor with LocalAlloc, and
        // nothing reads it after its owner is dropped.
        unsafe { LocalFree(self.0) };
    }
}

/// This process's token, opened for `access`.
fn process_token(access: TOKEN_ACCESS_MASK) -> io::Result<OwnedHandle> {
    let mut token = ptr::null_mut();
    // SAFETY: GetCurrentProcess's pseudo handle needs no closing, and the
    // call writes one handle into `token`.
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call opened the handle, and nothing else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

/// A token's TOKEN_USER, whose SID lies in the same buffer: words of eight
/// bytes, so that its pointer is aligned.
struct UserOf(Vec<u64>);

impl UserOf {
    fn sid(&self) -> PSID {
        // SAFETY: GetTokenInformation filled the buffer, aligned for it,
        // with a TOKEN_USER.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

fn token_user(token: &OwnedHandle) -> io::Result<UserOf> {
    let mut size = 0;
    // SAFETY: asked with no buffer, the call writes only the size it needs.
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, ptr::null_mut(), 0, &raw mut size) };
    let mut buffer = vec![0_u64; (size as usize).div_ceil(8)];
    // SAFETY: the buffer holds at least `size` bytes, aligned for a
    // TOKEN_USER, and the call writes no more than `size`.
    let done = unsafe {
        GetTokenInformation(token.as_raw_handle(), TokenUser, buffer.as_mut_ptr().cast(), size, &raw mut size)
    };
    if done == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(UserOf(buffer))
}

/// A SID as SDDL spells it, `S-1-5-21-…`.
fn spelled(sid: PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    // SAFETY: the SID is valid while its buffer lives, through the call,
    // which writes one pointer to a string it allocated.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the string is NUL-terminated UTF-16 that the call allocated
    // with LocalAlloc: read up to its NUL, then freed once.
    let spelled = unsafe {
        let length = (0..).take_while(|&at| *text.add(at) != 0).count();
        let spelled = OsString::from_wide(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        spelled
    };
    spelled.into_string().map_err(|_| io::Error::other("a SID spelled other than in ASCII"))
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
