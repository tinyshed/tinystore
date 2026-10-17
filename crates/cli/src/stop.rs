//! `tinystore stop`: asks the server of a directory to stop, as a client's
//! `server.stop` does, and waits until it has let go: its clients told to
//! leave, the streams running finished, `SERVE` removed.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::{client, help};

/// How long `stop` waits for the server to let go: as long as a closing
/// server waits for its clients, and a little more.
const LETTING_GO: Duration = Duration::from_secs(15);

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::STOP, args, stop)
}

fn stop(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_needed("stop")?;
    if !rest.is_empty() {
        return Err(wrong(format!("one directory, not {} too", rest.join(" "))));
    }
    let started = Instant::now();
    let Some(found) = client::found(&dir).map_err(failed)? else {
        return Err(failed(format!("no server serves {}: it has no server/SERVE", dir.display())));
    };
    let connection = found
        .connect()
        .map_err(|why| failed(format!("server/SERVE names a server that does not answer, pid {}: {why}", found.pid)))?;
    connection.stop().map_err(failed)?;
    if !let_go(&dir) {
        return Err(failed(format!(
            "the server agreed to stop but still holds {} after {} s",
            dir.display(),
            LETTING_GO.as_secs()
        )));
    }
    println!("● {}  stopped in {:.1}s · pid {}", dir.display(), started.elapsed().as_secs_f64(), found.pid);
    Ok(())
}

/// Waits until `SERVE` is gone, which a server removes as it lets go.
fn let_go(dir: &Path) -> bool {
    let deadline = Instant::now() + LETTING_GO;
    while dir.join("server").join("SERVE").exists() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}
