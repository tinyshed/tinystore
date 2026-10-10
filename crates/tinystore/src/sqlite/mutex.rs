//! The mutexes SQLite uses, given to it before it starts.
//!
//! SQLite's own are the platform's, which park a thread the moment it finds
//! one held. Every read takes the mutex of its file's WAL index twice, as it
//! begins and as it ends, for well under a microsecond each time, and a
//! parked thread is tens of microseconds from running again: two threads
//! reading one file read less than one did, and four half of what they could
//! (research rust-slice-2026-10-10). These spin first, for about as long as
//! a read takes, and park only then.
//!
//! The lock itself is safe code. What hands it to SQLite is not: the second
//! half of this file is the one place beside [`super::memory`] where the
//! adapter writes `unsafe`.

#![allow(unsafe_code)]

use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, Once, PoisonError};

use rusqlite::ffi;

/// How many times a thread looks at a held lock before it parks. Four
/// threads read as fast with 300 as with 30,000 and far slower with 30
/// (research rust-slice-2026-10-10); a lock still held after them is held
/// across something slow, and its waiters sleep.
const SPINS: u32 = 300;

/// A lock that spins before it parks.
struct Lock {
    held: AtomicBool,
    /// Threads past their spins. A thread that lets go wakes one only when
    /// there is one, so a lock nobody waits for costs no system call.
    parked: AtomicUsize,
    parking: Mutex<()>,
    freed: Condvar,
}

impl Lock {
    const fn new() -> Self {
        Self {
            held: AtomicBool::new(false),
            parked: AtomicUsize::new(0),
            parking: Mutex::new(()),
            freed: Condvar::new(),
        }
    }

    fn try_enter(&self) -> bool {
        !self.held.swap(true, Ordering::Acquire)
    }

    fn enter(&self) {
        for _ in 0..SPINS {
            if !self.held.load(Ordering::Relaxed) && self.try_enter() {
                return;
            }
            std::hint::spin_loop();
        }
        self.park_until_entered();
    }

    /// Parks until the lock is this thread's.
    ///
    /// No wake is lost. The count goes up before the last look at the lock,
    /// both under `parking`, and `leave` frees the lock before it reads the
    /// count: a thread that found the lock held was counted before it was
    /// freed, and `leave` then waits for `parking`, which this thread gives
    /// up only as it starts to wait.
    fn park_until_entered(&self) {
        let mut parking = self.parking.lock().unwrap_or_else(PoisonError::into_inner);
        self.parked.fetch_add(1, Ordering::SeqCst);
        while self.held.swap(true, Ordering::SeqCst) {
            parking = self.freed.wait(parking).unwrap_or_else(PoisonError::into_inner);
        }
        self.parked.fetch_sub(1, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.held.store(false, Ordering::SeqCst);
        if self.parked.load(Ordering::SeqCst) > 0 {
            drop(self.parking.lock().unwrap_or_else(PoisonError::into_inner));
            self.freed.notify_one();
        }
    }
}

/// A lock its holder may enter again, as SQLite asks of some.
struct Reentrant {
    lock: Lock,
    /// The thread that holds it, by `this_thread`, or 0.
    holder: AtomicUsize,
    /// How many times the holder entered; only the holder reads or writes it.
    depth: AtomicU32,
}

impl Reentrant {
    const fn new() -> Self {
        Self { lock: Lock::new(), holder: AtomicUsize::new(0), depth: AtomicU32::new(0) }
    }

    fn try_enter(&self) -> bool {
        let me = this_thread();
        if self.holder.load(Ordering::Relaxed) != me {
            if !self.lock.try_enter() {
                return false;
            }
            self.holder.store(me, Ordering::Relaxed);
        }
        self.depth.fetch_add(1, Ordering::Relaxed);
        true
    }

    fn enter(&self) {
        let me = this_thread();
        if self.holder.load(Ordering::Relaxed) != me {
            self.lock.enter();
            self.holder.store(me, Ordering::Relaxed);
        }
        self.depth.fetch_add(1, Ordering::Relaxed);
    }

    fn leave(&self) {
        if self.depth.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.holder.store(0, Ordering::Relaxed);
            self.lock.leave();
        }
    }
}

/// A number no other living thread has: the address of a value of its own.
fn this_thread() -> usize {
    thread_local!(static MARK: u8 = const { 0 });
    MARK.with(|mark| std::ptr::from_ref(mark).addr())
}

/// What a `sqlite3_mutex` pointer points to.
enum Slot {
    Fast(Lock),
    Reentrant(Reentrant),
}

impl Slot {
    fn try_enter(&self) -> bool {
        match self {
            Slot::Fast(lock) => lock.try_enter(),
            Slot::Reentrant(lock) => lock.try_enter(),
        }
    }

    fn enter(&self) {
        match self {
            Slot::Fast(lock) => lock.enter(),
            Slot::Reentrant(lock) => lock.enter(),
        }
    }

    fn leave(&self) {
        match self {
            Slot::Fast(lock) => lock.leave(),
            Slot::Reentrant(lock) => lock.leave(),
        }
    }
}

/// The mutexes SQLite asks for by a number, `SQLITE_MUTEX_STATIC_MAIN` to
/// `SQLITE_MUTEX_STATIC_VFS3`: the same one each time, never freed.
static BY_NUMBER: [Slot; 12] = [const { Slot::Fast(Lock::new()) }; 12];

static GIVEN: AtomicBool = AtomicBool::new(false);

/// Gives SQLite these mutexes, once a process, before its first connection
/// opens. A process that started SQLite itself, through its own rusqlite,
/// keeps the mutexes it started with: SQLite refuses the change, and nothing
/// here stops it to make it.
pub(crate) fn give() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let methods = ffi::sqlite3_mutex_methods {
            xMutexInit: Some(start),
            xMutexEnd: Some(start),
            xMutexAlloc: Some(alloc),
            xMutexFree: Some(free),
            xMutexEnter: Some(enter),
            xMutexTry: Some(try_enter),
            xMutexLeave: Some(leave),
            xMutexHeld: Some(always),
            xMutexNotheld: Some(always),
        };
        // SAFETY: SQLITE_CONFIG_MUTEX takes one pointer to a
        // sqlite3_mutex_methods and copies the struct before it returns, so
        // `methods` need not outlive the call. SQLite refuses the call with
        // SQLITE_MISUSE once it has started, and changes nothing then. Every
        // function named keeps the contract of its slot, as each says.
        let answer = unsafe { ffi::sqlite3_config(ffi::SQLITE_CONFIG_MUTEX, &raw const methods) };
        GIVEN.store(answer == ffi::SQLITE_OK, Ordering::Relaxed);
    });
}

/// Whether SQLite took these mutexes.
pub(crate) fn given() -> bool {
    GIVEN.load(Ordering::Relaxed)
}

/// `xMutexInit` and `xMutexEnd`: nothing to set up or to take down.
unsafe extern "C" fn start() -> c_int {
    ffi::SQLITE_OK
}

/// `xMutexHeld` and `xMutexNotheld`, which SQLite calls only inside its
/// asserts and lets an implementation answer true.
unsafe extern "C" fn always(_: *mut ffi::sqlite3_mutex) -> c_int {
    1
}

/// `xMutexAlloc`: a new mutex for `SQLITE_MUTEX_FAST` and
/// `SQLITE_MUTEX_RECURSIVE`, the one of its number for the others, and null
/// for a number SQLite does not have.
unsafe extern "C" fn alloc(kind: c_int) -> *mut ffi::sqlite3_mutex {
    let slot: *const Slot = match kind {
        ffi::SQLITE_MUTEX_FAST => Box::into_raw(Box::new(Slot::Fast(Lock::new()))),
        ffi::SQLITE_MUTEX_RECURSIVE => Box::into_raw(Box::new(Slot::Reentrant(Reentrant::new()))),
        _ => match usize::try_from(kind - ffi::SQLITE_MUTEX_STATIC_MAIN).ok().and_then(|at| BY_NUMBER.get(at)) {
            Some(slot) => slot,
            None => std::ptr::null(),
        },
    };
    slot.cast_mut().cast()
}

/// `xMutexFree`, which SQLite calls once on a mutex `alloc` made new, with
/// no thread inside it, and never on one it asked for by number.
unsafe extern "C" fn free(mutex: *mut ffi::sqlite3_mutex) {
    // SAFETY: by SQLite's contract the pointer is one `alloc` returned from
    // `Box::into_raw`, freed once and used by nobody after.
    drop(unsafe { Box::from_raw(mutex.cast::<Slot>()) });
}

unsafe extern "C" fn enter(mutex: *mut ffi::sqlite3_mutex) {
    // SAFETY: `slot` holds for every pointer SQLite passes here.
    unsafe { slot(mutex) }.enter();
}

unsafe extern "C" fn try_enter(mutex: *mut ffi::sqlite3_mutex) -> c_int {
    // SAFETY: `slot` holds for every pointer SQLite passes here.
    if unsafe { slot(mutex) }.try_enter() { ffi::SQLITE_OK } else { ffi::SQLITE_BUSY }
}

unsafe extern "C" fn leave(mutex: *mut ffi::sqlite3_mutex) {
    // SAFETY: `slot` holds for every pointer SQLite passes here.
    unsafe { slot(mutex) }.leave();
}

/// The slot behind a mutex SQLite passes back.
///
/// # Safety
///
/// `mutex` is a pointer `alloc` returned and `free` has not been given: a
/// `Slot` in a live box, or one of `BY_NUMBER`. SQLite passes no other, and
/// shares one between threads, which a `Slot` allows: all it holds is atomic
/// or behind its own lock.
unsafe fn slot<'a>(mutex: *mut ffi::sqlite3_mutex) -> &'a Slot {
    // SAFETY: the caller's contract, above.
    unsafe { &*mutex.cast::<Slot>() }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_lock_lets_one_thread_in_at_a_time() {
        let (lock, count) = (Arc::new(Lock::new()), Arc::new(AtomicU64::new(0)));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (lock, count) = (Arc::clone(&lock), Arc::clone(&count));
                thread::spawn(move || {
                    for _ in 0..20_000 {
                        lock.enter();
                        // not one step: two threads inside would lose a count
                        let seen = count.load(Ordering::Relaxed);
                        count.store(seen + 1, Ordering::Relaxed);
                        lock.leave();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(count.load(Ordering::Relaxed), 160_000);
    }

    #[test]
    fn a_thread_past_its_spins_parks_and_is_woken() {
        let lock = Arc::new(Lock::new());
        lock.enter();
        let waiting = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                lock.enter();
                lock.leave();
            })
        };
        while lock.parked.load(Ordering::SeqCst) == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(!lock.try_enter(), "it is held still");
        lock.leave();
        waiting.join().unwrap();
        assert!(lock.try_enter(), "the woken thread left it free");
    }

    #[test]
    fn a_reentrant_lock_is_its_holders_until_it_leaves_as_often_as_it_entered() {
        let lock = Arc::new(Reentrant::new());
        lock.enter();
        assert!(lock.try_enter(), "the holder enters again");
        let other = Arc::clone(&lock);
        assert!(!thread::spawn(move || other.try_enter()).join().unwrap());
        lock.leave();
        let other = Arc::clone(&lock);
        assert!(!thread::spawn(move || other.try_enter()).join().unwrap(), "one leave of two frees nothing");
        lock.leave();
        let other = Arc::clone(&lock);
        assert!(thread::spawn(move || other.try_enter()).join().unwrap());
    }

    #[test]
    fn sqlite_is_given_a_mutex_by_its_kind_and_the_same_one_by_its_number() {
        // SAFETY: the functions are called as SQLite calls them: `alloc`
        // with its kinds, `free` once on what `alloc` made new.
        unsafe {
            let main = alloc(ffi::SQLITE_MUTEX_STATIC_MAIN);
            assert_eq!(main, alloc(ffi::SQLITE_MUTEX_STATIC_MAIN));
            assert_ne!(main, alloc(ffi::SQLITE_MUTEX_STATIC_VFS3));
            assert!(alloc(ffi::SQLITE_MUTEX_STATIC_VFS3 + 1).is_null());
            for kind in [ffi::SQLITE_MUTEX_FAST, ffi::SQLITE_MUTEX_RECURSIVE] {
                let mutex = alloc(kind);
                enter(mutex);
                assert_eq!(try_enter(mutex) == ffi::SQLITE_OK, kind == ffi::SQLITE_MUTEX_RECURSIVE);
                if kind == ffi::SQLITE_MUTEX_RECURSIVE {
                    leave(mutex);
                }
                leave(mutex);
                free(mutex);
            }
        }
    }
}
