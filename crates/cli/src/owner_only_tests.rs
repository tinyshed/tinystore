//! Who may open what a Windows server keeps to its user, `server\`, `SERVE`
//! and the pipe: their DACLs as written, then the tries of a token that is
//! not the user's, made while the test's thread impersonates it. The store's
//! directory lets every user in, as a folder at a drive's root may, so that
//! what inherited its ACL would let them in too; a file and a pipe that every
//! user may open stand beside, so that a refusal is the DACL's and not the
//! token's. Last, the `server\` a server refuses to protect: one another user
//! made, and a link.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
use windows_sys::Win32::NetworkManagement::NetManagement::{
    NERR_Success, NetUserAdd, NetUserDel, UF_DONT_EXPIRE_PASSWD, UF_SCRIPT, USER_INFO_1, USER_PRIV_USER,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, GetEffectiveRightsFromAclW, GetNamedSecurityInfoW, GetSecurityInfo, NO_MULTIPLE_TRUSTEE,
    SE_KERNEL_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, GetSecurityDescriptorControl, ImpersonateLoggedOnUser,
    LOGON32_LOGON_INTERACTIVE, LOGON32_LOGON_NETWORK, LOGON32_PROVIDER_DEFAULT, LogonUserW, RevertToSelf,
    SE_DACL_PROTECTED, SID_AND_ATTRIBUTES, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_ELEVATION, TOKEN_IMPERSONATE,
    TokenElevation,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

use super::*;

/// Every user may do anything, and what a directory holds inherits it.
const EVERY_USER: &str = "D:P(A;OICI;FA;;;WD)";
const EVERY_USER_OF_A_PIPE: &str = "D:P(A;;FA;;;WD)";

/// A file the server's folder held before the server first protected it, as
/// an SDK's or an older release's may be.
const EARLIER: &str = "earlier";

/// Whom else a DACL might let in: the groups every local user is in, the
/// administrators and the system.
const OTHERS: [(&str, &str); 6] = [
    ("Everyone", "S-1-1-0"),
    ("Authenticated Users", "S-1-5-11"),
    ("Users", "S-1-5-32-545"),
    ("INTERACTIVE", "S-1-5-4"),
    ("Administrators", "S-1-5-32-544"),
    ("SYSTEM", "S-1-5-18"),
];

// contract: SEC-06
#[test]
fn server_serve_and_the_pipe_grant_the_user_alone() {
    let served = Served::start();
    for path in [served.server.clone(), served.server.join("SERVE")] {
        let security = Security::of_file(&path);
        assert!(security.protected(), "{}: inherits the store directory's ACL", path.display());
        security.grants_the_user_alone(&path.display().to_string());
    }
    Security::of_file(&served.server.join(EARLIER)).grants_the_user_alone("a file from before the server");
    Security::of_pipe(&served.pipe).grants_the_user_alone("the pipe");
    File::open(served.server.join("SERVE")).expect("the user reads SERVE");
    OpenOptions::new().read(true).write(true).open(&served.pipe_name).expect("the user connects to the pipe");
}

// contract: SEC-06
#[test]
fn a_token_in_the_users_groups_but_not_the_user_is_refused() {
    let served = Served::start();
    served.refuses(&this_user_disabled());
}

// contract: SEC-06
#[test]
fn another_local_user_is_refused_and_so_is_a_server_folder_it_made() {
    if !elevated() {
        // stderr itself, which the test harness does not hold back as it does eprintln's
        let skipped = "another_local_user_is_refused_and_so_is_a_server_folder_it_made: skipped, since making \
                       a local user takes an elevated run, as GitHub's Windows runner is";
        let _ = writeln!(io::stderr(), "{skipped}");
        return;
    }
    let served = Served::start();
    let stranger = Stranger::add();
    let token = stranger.log_on();
    served.refuses(&token);

    let store = tempfile::tempdir().unwrap();
    Descriptor::parse(EVERY_USER).unwrap().set_on(store.path()).unwrap();
    let squatted = store.path().join("server");
    {
        let _impersonating = Impersonating::start(&token);
        std::fs::create_dir(&squatted).unwrap();
    }
    let refused = crate::local::server_dir(store.path()).unwrap_err();
    assert_refused(&refused, &squatted, "owned by ");
    assert!(!Security::of_file(&squatted).protected(), "the stranger's folder is left as it made it");
}

// contract: SEC-06
#[test]
fn a_link_where_the_server_folder_belongs_is_refused_and_its_target_left_alone() {
    let store = tempfile::tempdir().unwrap();
    let (link, target) = (store.path().join("server"), store.path().join("elsewhere"));
    std::fs::create_dir(&target).unwrap();
    let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&target).output().unwrap();
    assert!(made.status.success(), "mklink: {}", String::from_utf8_lossy(&made.stderr));
    let refused = crate::local::server_dir(store.path()).unwrap_err();
    assert_refused(&refused, &link, "a link");
    assert!(!Security::of_file(&target).protected(), "the link's target keeps the DACL it inherits");
}

// contract: SEC-06
#[test]
fn the_user_the_administrators_and_the_system_may_own_the_server_folder_and_nobody_else() {
    let owners = [
        (user().unwrap(), true),
        ("S-1-5-32-544", true),
        ("S-1-5-18", true),
        ("S-1-1-0", false),
        ("S-1-5-11", false),
        ("S-1-5-32-545", false),
        ("S-1-5-21-1-2-3-1001", false),
    ];
    for (owner, may) in owners {
        assert_eq!(may_own(owner).unwrap(), may, "{owner}");
    }
}

// contract: SEC-06
#[test]
fn the_server_folder_held_for_its_check_is_neither_moved_nor_removed() {
    let store = tempfile::tempdir().unwrap();
    let server = store.path().join("server");
    std::fs::create_dir(&server).unwrap();
    let held = held_in_place(&server).unwrap();
    let moved = std::fs::rename(&server, store.path().join("moved"));
    assert_eq!(moved.map_err(|error| error.raw_os_error()), Err(Some(ERROR_SHARING_VIOLATION.cast_signed())));
    let removed = std::fs::remove_dir(&server);
    assert_eq!(removed.map_err(|error| error.raw_os_error()), Err(Some(ERROR_SHARING_VIOLATION.cast_signed())));
    drop(held);
    std::fs::rename(&server, store.path().join("moved")).expect("let go, it moves");
}

fn assert_refused(refused: &io::Error, path: &Path, why: &str) {
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
    let message = refused.to_string();
    assert!(message.starts_with(&format!("{}: {why}", path.display())), "{message}");
}

/// A store's directory every user may enter, with what a server keeps to its
/// user in it, and beside them a file and a pipe every user may open.
struct Served {
    pipe: NamedPipeServer,
    _open_pipe: NamedPipeServer,
    runtime: tokio::runtime::Runtime,
    pipe_name: String,
    open_pipe_name: String,
    server: PathBuf,
    dir: tempfile::TempDir,
}

impl Served {
    fn start() -> Served {
        let dir = tempfile::tempdir().unwrap();
        Descriptor::parse(EVERY_USER).unwrap().set_on(dir.path()).unwrap();
        std::fs::write(dir.path().join("open"), "anyone's").unwrap();
        std::fs::create_dir(dir.path().join("server")).unwrap();
        std::fs::write(dir.path().join("server").join(EARLIER), "the store's directory's ACL").unwrap();
        let server = crate::local::server_dir(dir.path()).unwrap();
        crate::local::publish(&server, &crate::local::Serve::new(Vec::new(), false).unwrap()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_io().build().unwrap();
        let pipe_name = format!(r"\\.\pipe\{}", crate::local::pipe_name(dir.path()));
        let open_pipe_name = format!("{pipe_name}-open");
        let (pipe, open_pipe) = {
            let _entered = runtime.enter();
            let pipe = crate::local::pipe_instance(&pipe_name, true).unwrap();
            let open = Descriptor::parse(EVERY_USER_OF_A_PIPE).unwrap();
            (pipe, open.create_pipe(&ServerOptions::new(), &open_pipe_name).unwrap())
        };
        Served { pipe, _open_pipe: open_pipe, runtime, pipe_name, open_pipe_name, server, dir }
    }

    /// Checks that `token` opens what every user may, and nothing of what the
    /// server keeps to its user.
    fn refuses(&self, token: &OwnedHandle) {
        let tried = self.tried_as(token);
        assert!(tried.open_file.is_ok(), "a file every user may read: {:?}", tried.open_file);
        assert!(tried.open_pipe.is_ok(), "a pipe every user may open: {:?}", tried.open_pipe);
        let kept = [
            ("SERVE", tried.serve),
            ("a file from before the server", tried.earlier),
            ("the pipe, to read", tried.pipe_to_read),
            ("the pipe, both ways", tried.pipe_both_ways),
            ("another instance of the pipe", tried.another_instance),
        ];
        for (what, tried) in kept {
            let denied = Err(Some(ERROR_ACCESS_DENIED.cast_signed()));
            assert_eq!(tried.map_err(|error| error.raw_os_error()), denied, "{what}");
        }
    }

    /// What `token` opens, tried while the thread impersonates it: what any
    /// user may, and what a client of the server or a squatter would.
    fn tried_as(&self, token: &OwnedHandle) -> Tried {
        let _entered = self.runtime.enter();
        let _impersonating = Impersonating::start(token);
        Tried {
            open_file: File::open(self.dir.path().join("open")).map(drop),
            open_pipe: OpenOptions::new().read(true).open(&self.open_pipe_name).map(drop),
            serve: File::open(self.server.join("SERVE")).map(drop),
            earlier: File::open(self.server.join(EARLIER)).map(drop),
            pipe_to_read: OpenOptions::new().read(true).open(&self.pipe_name).map(drop),
            pipe_both_ways: OpenOptions::new().read(true).write(true).open(&self.pipe_name).map(drop),
            another_instance: ServerOptions::new().create(&self.pipe_name).map(drop),
        }
    }
}

struct Tried {
    open_file: io::Result<()>,
    open_pipe: io::Result<()>,
    serve: io::Result<()>,
    earlier: io::Result<()>,
    pipe_to_read: io::Result<()>,
    pipe_both_ways: io::Result<()>,
    another_instance: io::Result<()>,
}

/// A security descriptor the system read and allocated, and the DACL in it.
struct Security {
    descriptor: PSECURITY_DESCRIPTOR,
    dacl: *mut ACL,
}

impl Security {
    fn of_file(path: &Path) -> Security {
        let path = wide(path.as_os_str());
        let (mut dacl, mut descriptor) = (ptr::null_mut(), ptr::null_mut());
        // SAFETY: the path is NUL-terminated UTF-16 that lives through the
        // call, which writes a pointer to the DACL and one to the descriptor
        // it allocated; the owner, the group and the SACL are not asked for.
        let failed = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &raw mut dacl,
                ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        Security::read(failed, descriptor, dacl)
    }

    fn of_pipe(pipe: &NamedPipeServer) -> Security {
        let (mut dacl, mut descriptor) = (ptr::null_mut(), ptr::null_mut());
        // SAFETY: the handle is the pipe's, which lives through the call; the
        // call writes as GetNamedSecurityInfoW does above.
        let failed = unsafe {
            GetSecurityInfo(
                pipe.as_raw_handle(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &raw mut dacl,
                ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        Security::read(failed, descriptor, dacl)
    }

    fn read(failed: u32, descriptor: PSECURITY_DESCRIPTOR, dacl: *mut ACL) -> Security {
        assert_eq!(failed, 0, "{}", io::Error::from_raw_os_error(failed.cast_signed()));
        let security = Security { descriptor, dacl };
        assert!(!dacl.is_null(), "no DACL, which lets everyone in");
        security
    }

    fn protected(&self) -> bool {
        let (mut control, mut revision) = (0, 0);
        // SAFETY: `self` keeps the descriptor; the call writes its control
        // bits and its revision.
        let done = unsafe { GetSecurityDescriptorControl(self.descriptor, &raw mut control, &raw mut revision) };
        assert_ne!(done, 0, "{}", io::Error::last_os_error());
        control & SE_DACL_PROTECTED != 0
    }

    fn grants_the_user_alone(&self, what: &str) {
        // SAFETY: the DACL is not null and lies in the descriptor `self` keeps.
        let entries = unsafe { (*self.dacl).AceCount };
        assert_eq!(entries, 1, "{what}: one entry, the user's");
        assert_eq!(self.rights_of(user().unwrap()), FILE_ALL_ACCESS, "{what}: the user's rights");
        for (others, sid) in OTHERS {
            assert_eq!(self.rights_of(sid), 0, "{what}: the rights of {others}");
        }
    }

    /// The rights the DACL gives `sid`, as SDDL spells it.
    fn rights_of(&self, sid: &str) -> u32 {
        let spelled = wide(OsStr::new(sid));
        let mut sid = ptr::null_mut();
        // SAFETY: the text is NUL-terminated UTF-16 that lives through the
        // call, which writes one pointer to a SID it allocated.
        let done = unsafe { ConvertStringSidToSidW(spelled.as_ptr(), &raw mut sid) };
        assert_ne!(done, 0, "{}", io::Error::last_os_error());
        let trustee = TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
        };
        let mut rights = 0;
        // SAFETY: `self` keeps the DACL, the trustee names the SID, freed
        // only below, and the call writes one mask.
        let failed = unsafe { GetEffectiveRightsFromAclW(self.dacl, &raw const trustee, &raw mut rights) };
        // SAFETY: ConvertStringSidToSidW allocated the SID with LocalAlloc,
        // and nothing reads it after.
        unsafe { LocalFree(sid) };
        assert_eq!(failed, 0, "{}", io::Error::from_raw_os_error(failed.cast_signed()));
        rights
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: the system allocated the descriptor with LocalAlloc, and the
        // DACL in it is read no more.
        unsafe { LocalFree(self.descriptor) };
    }
}

/// The test's thread acting with another token, until it is dropped.
struct Impersonating;

impl Impersonating {
    fn start(token: &OwnedHandle) -> Impersonating {
        // SAFETY: the handle is a token that lives through the call.
        let done = unsafe { ImpersonateLoggedOnUser(token.as_raw_handle()) };
        assert_ne!(done, 0, "{}", io::Error::last_os_error());
        Impersonating
    }
}

impl Drop for Impersonating {
    fn drop(&mut self) {
        // SAFETY: the thread impersonates, and takes the process's token back.
        unsafe { RevertToSelf() };
    }
}

/// This process's token with the user's SID kept for denying alone, and no
/// privilege but traversal: the user's groups, Everyone and Users among
/// them, as another member of them holds them.
fn this_user_disabled() -> OwnedHandle {
    let token = process_token(TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE | TOKEN_ASSIGN_PRIMARY).unwrap();
    let user = token_user(&token).unwrap();
    let disabled = SID_AND_ATTRIBUTES { Sid: user.sid(), Attributes: 0 };
    let mut restricted = ptr::null_mut();
    // SAFETY: the token's handle lives through the call; the one SID to
    // disable lies in `user`, which does too; no privilege nor restricting
    // SID is listed; the call writes one handle.
    let done = unsafe {
        CreateRestrictedToken(
            token.as_raw_handle(),
            DISABLE_MAX_PRIVILEGE,
            1,
            &raw const disabled,
            0,
            ptr::null(),
            0,
            ptr::null(),
            &raw mut restricted,
        )
    };
    assert_ne!(done, 0, "{}", io::Error::last_os_error());
    // SAFETY: the call made the handle, and nothing else owns it.
    unsafe { OwnedHandle::from_raw_handle(restricted) }
}

fn elevated() -> bool {
    let token = process_token(TOKEN_QUERY).unwrap();
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let (wanted, mut size) = (size_of::<TOKEN_ELEVATION>() as u32, 0);
    // SAFETY: the call writes one TOKEN_ELEVATION into `elevation`, whose
    // size it is given.
    let done = unsafe {
        GetTokenInformation(token.as_raw_handle(), TokenElevation, (&raw mut elevation).cast(), wanted, &raw mut size)
    };
    done != 0 && elevation.TokenIsElevated != 0
}

/// A local user made for one test, its name and its password random, and
/// deleted when it is dropped, whatever the test did.
struct Stranger {
    name: Vec<u16>,
    password: Vec<u16>,
}

impl Stranger {
    fn add() -> Stranger {
        let random = |length| {
            let mut bytes = vec![0; length];
            getrandom::fill(&mut bytes).unwrap();
            bytes
        };
        let name = wide(OsStr::new(&format!("tinystore-{}", crate::local::hex(&random(4)))));
        // a policy may ask for capitals, small letters, digits and signs
        let password = wide(OsStr::new(&format!("{}Aa1-", crate::local::base64url(&random(24)))));
        let user = USER_INFO_1 {
            usri1_name: name.as_ptr().cast_mut(),
            usri1_password: password.as_ptr().cast_mut(),
            usri1_password_age: 0,
            usri1_priv: USER_PRIV_USER,
            usri1_home_dir: ptr::null_mut(),
            usri1_comment: ptr::null_mut(),
            usri1_flags: UF_SCRIPT | UF_DONT_EXPIRE_PASSWD,
            usri1_script_path: ptr::null_mut(),
        };
        let mut wrong = 0;
        // SAFETY: level 1 reads one USER_INFO_1, whose strings are
        // NUL-terminated, live through the call and are not written; the
        // call writes the index of a field it refuses into `wrong`.
        let failed = unsafe { NetUserAdd(ptr::null(), 1, (&raw const user).cast(), &raw mut wrong) };
        assert_eq!(failed, NERR_Success, "NetUserAdd refused field {wrong}");
        Stranger { name, password }
    }

    /// The stranger's token: a network logon, which every user may make, or
    /// an interactive one where a policy denies local accounts the network.
    fn log_on(&self) -> OwnedHandle {
        let here = wide(OsStr::new("."));
        let mut refused = Vec::new();
        for kind in [LOGON32_LOGON_NETWORK, LOGON32_LOGON_INTERACTIVE] {
            let mut token = ptr::null_mut();
            // SAFETY: the name, the domain and the password are NUL-terminated
            // UTF-16 that lives through the call, which writes one handle.
            let done = unsafe {
                LogonUserW(
                    self.name.as_ptr(),
                    here.as_ptr(),
                    self.password.as_ptr(),
                    kind,
                    LOGON32_PROVIDER_DEFAULT,
                    &raw mut token,
                )
            };
            if done != 0 {
                // SAFETY: the call made the handle, and nothing else owns it.
                return unsafe { OwnedHandle::from_raw_handle(token) };
            }
            refused.push(io::Error::last_os_error());
        }
        panic!("the stranger could not log on: {refused:?}");
    }
}

impl Drop for Stranger {
    fn drop(&mut self) {
        // SAFETY: the name is NUL-terminated UTF-16 that lives through the call.
        let failed = unsafe { NetUserDel(ptr::null(), self.name.as_ptr()) };
        if failed != NERR_Success {
            // stderr itself, since the harness drops eprintln's of a test that passed
            let name = String::from_utf16_lossy(&self.name);
            let _ = writeln!(io::stderr(), "the local user {name} was not deleted: {failed}");
        }
    }
}
