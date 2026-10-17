//! `tinystore promote`: a node's copies made its own files, once their home
//! is known to be stopped, the node itself stopped meanwhile.
//!
//! ```text
//! tinystore promote ./data                 every whole copy the node holds
//! tinystore promote ./data kv.db sql/app.db
//! ```

use std::path::Path;
use std::process::ExitCode;

use tinystore::ErrorKind;

pub(crate) const USAGE: &str = "usage: tinystore promote <dir> [<file>...]";

pub(crate) fn run(args: &[String]) -> ExitCode {
    let Some((dir, files)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match tinystore::replication::promote(Path::new(dir), files) {
        Ok(promoted) if promoted.is_empty() => {
            eprintln!("tinystore promote: {dir} holds no copy");
            ExitCode::FAILURE
        }
        Ok(promoted) => {
            for one in promoted {
                println!("{}: this node's own from entry {}, epoch {}", one.file, one.sequence + 1, one.epoch);
            }
            ExitCode::SUCCESS
        }
        Err(error) if error.kind() == ErrorKind::InUse => {
            eprintln!("tinystore promote: {error}\nstop the node that holds {dir} first");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("tinystore promote: {error}");
            ExitCode::FAILURE
        }
    }
}
