//! `tinystore lock`: a command run while it holds a key of a lock, as a
//! cron job that must not run twice at once. The key is held through the
//! server that holds the directory, or a sidecar it starts, with a lease
//! renewed while the command runs, from a thread of its own; the command
//! gets the lease's fence in `TINYSTORE_FENCE`, is stopped if the lease is
//! lost, and the key is let go once it ends. The command's exit is this one's.
//!
//! ```text
//! tinystore lock ./data hold cron nightly-backup -- ./backup.sh
//! ```

use std::cell::Cell;
use std::process::{Child, Command, ExitCode, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tinystore::client::{Client, Hold, Lease};

use crate::asked::{self, Asked, Done, Refused, Verb, failed, wrong};
use crate::client::Stream;
use crate::{help, reach};

const COMMAND: &str = "lock hold";
/// How long a lease lasts from each renewal unless `--ttl` says.
const TTL: Duration = Duration::from_secs(30);
/// How long a hold stands in line unless `--wait` says, as the SDKs' does.
const WAIT: Duration = Duration::from_secs(10);
/// How often the command is looked at while it runs.
const LOOK: Duration = Duration::from_millis(50);
/// How long the key's release may take once the command ended.
const RELEASE_WAIT: Duration = Duration::from_secs(5);

/// The group's one verb, for its help and a misspelt verb's nearest: `hold`
/// itself runs apart, since its exit is the command's.
const VERBS: [Verb; 1] = [Verb { word: "hold", command: &help::LOCK_HOLD, run: |_| Ok(()) }];

pub(crate) fn run(args: &[String]) -> ExitCode {
    if !args.iter().any(|arg| arg == "hold") {
        return asked::run_verb("lock", &VERBS, args);
    }
    let ended = Cell::new(None);
    let exit = asked::run(&help::LOCK_HOLD, args, |asked| hold(&asked, &ended));
    ended.get().map_or(exit, ExitCode::from)
}

/// Holds the key, runs the command while it is held, and lets it go; the
/// command's exit goes to `ended`.
fn hold(asked: &Asked, ended: &Cell<Option<u8>>) -> Done {
    let (dir, rest) = asked.placed("lock", "hold", false)?;
    let [lock, key, command @ ..] = &rest[..] else {
        return Err(wrong("hold takes a lock, a key and the command: hold cron nightly-backup -- ./backup.sh"));
    };
    let Some((program, arguments)) = command.split_first() else {
        return Err(wrong("hold runs a command while it holds the key: hold cron nightly-backup -- ./backup.sh"));
    };
    let span = |flag: &str, otherwise| asked.value(flag).map_or(Ok(otherwise), asked::span).map_err(wrong);
    let mut request = [0; 16];
    getrandom::fill(&mut request).map_err(|error| failed(format!("a hold's request: {error}")))?;
    let max_hold = asked.value("--max-hold").map(asked::span).transpose().map_err(wrong)?;
    let asking = Hold {
        lock: lock.clone(),
        key: key.clone(),
        ttl: span("--ttl", TTL)?,
        wait: span("--wait", WAIT)?,
        max_hold,
        request,
    };
    reach::store_at(&dir)?;
    let mut client = reach::connected(&dir, COMMAND)?.into_client();
    let Some(lease) = client.hold(&asking).map_err(failed)? else {
        return Err(failed(format!("lock {lock}: key {key} is held by another; --wait stands in line longer")));
    };
    let child = Command::new(program)
        .args(arguments)
        .env("TINYSTORE_FENCE", &lease.fence)
        .spawn()
        .map_err(|error| failed(format!("{program}: {error}")))?;
    let keeping = Keeping::start(client, lease, asking.ttl);
    let status = watched(child, &keeping)?;
    keeping.release();
    ended.set(Some(status.code().map_or(1, |code| u8::try_from(code).unwrap_or(1))));
    Ok(())
}

/// Waits for the command, stopping it once the lease is lost.
fn watched(mut child: Child, keeping: &Keeping) -> Result<ExitStatus, Refused> {
    loop {
        if let Some(status) = child.try_wait().map_err(|error| failed(format!("the command: {error}")))? {
            return Ok(status);
        }
        if keeping.lost.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failed("the lease was lost, so the command was stopped: another may hold the key now"));
        }
        std::thread::sleep(LOOK);
    }
}

/// A lease kept on a thread of its own, renewed every third of its `ttl`, so
/// that a renewal that waits on the server never holds the command's watch;
/// past `ttl` from its last renewal, it counts as lost.
struct Keeping {
    lost: Arc<AtomicBool>,
    stop: mpsc::Sender<()>,
    released: mpsc::Receiver<()>,
}

impl Keeping {
    fn start(mut client: Client<Box<dyn Stream>>, lease: Lease, ttl: Duration) -> Keeping {
        let lost = Arc::new(AtomicBool::new(false));
        let (stop, stopped) = mpsc::channel();
        let (done, released) = mpsc::channel();
        let losing = Arc::clone(&lost);
        let deadline = Arc::new(std::sync::Mutex::new(Instant::now() + ttl));
        let watched = Arc::clone(&deadline);
        let watching = Arc::clone(&lost);
        // the renewal below may wait on the server; this counts its deadline
        std::thread::spawn(move || {
            while !watching.load(Ordering::Acquire) {
                if Instant::now() > *watched.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) {
                    watching.store(true, Ordering::Release);
                }
                std::thread::sleep(LOOK);
            }
        });
        std::thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(ttl / 3) {
                let sent = Instant::now();
                match client.renew(&lease.lease, ttl) {
                    Ok(()) => *deadline.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = sent + ttl,
                    Err(error) if error.kind() == tinystore::ErrorKind::Lost => {
                        return losing.store(true, Ordering::Release);
                    }
                    Err(error) => {
                        asked::say(COMMAND, format!("a renewal failed, tried again until the lease runs out: {error}"))
                    }
                }
            }
            let _ = client.release(&lease.lease);
            // the command has ended: the deadline's watch ends too
            losing.store(true, Ordering::Release);
            let _ = done.send(());
        });
        Keeping { lost, stop, released }
    }

    /// Lets the key go, waiting a while for the server to say so.
    fn release(self) {
        let _ = self.stop.send(());
        let _ = self.released.recv_timeout(RELEASE_WAIT);
    }
}
