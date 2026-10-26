#![allow(unsafe_code)]
//! What a local server keeps to the user it runs as, on Windows: `server\`,
//! `SERVE` and the named pipe, each with a DACL that grants that user every
//! right and nobody else any. Unix says as much with a mode; Windows takes a
//! security descriptor, which only its C API builds, so this module is one of
//! the command line's two homes of `unsafe`, beside `own_handles`.
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

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree};
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
    CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

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
pub(crate) fn protect_dir(path: &Path) -> io::Result<()> {
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
pub(crate) fn create_new(path: &Path) -> io::Result<File> {
    let descriptor = Descriptor::parse(&format!("D:P(A;;FA;;;{})", user()?))?;
    descriptor.create_file(path).map_err(|error| within(path, "creating it the user's alone", &error))
}

/// Creates an instance of the pipe `name` as `options` say, with a DACL of
/// the user's alone, so that no other user connects to it or makes an
/// instance of its own.
pub(crate) fn create_pipe(options: &ServerOptions, name: &str) -> io::Result<NamedPipeServer> {
    Descriptor::parse(&format!("D:P(A;;FA;;;{})", user()?))?.create_pipe(options, name)
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

    fn create_pipe(&self, options: &ServerOptions, name: &str) -> io::Result<NamedPipeServer> {
        let mut attributes = self.attributes();
        // SAFETY: `attributes` is a whole SECURITY_ATTRIBUTES that lives
        // through the call, and its descriptor lives as long as `self`.
        unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
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
#[path = "owner_only_tests.rs"]
mod tests;
