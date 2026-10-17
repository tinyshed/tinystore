//! `tinystore promote`: a node's copies made its own files, once their home
//! is known to be stopped, the node itself stopped meanwhile; and `tinystore
//! demote`: an old home's files made copies again, which it then takes from
//! its new home.
//!
//! ```text
//! tinystore promote ./data                 every whole copy the node holds
//! tinystore promote ./data kv.db sql/app.db
//! tinystore demote ./data                  every file it writes but its records and metrics
//! ```

use std::path::Path;
use std::process::ExitCode;

use tinystore::ErrorKind;

pub(crate) const USAGE: &str = "usage: tinystore promote <dir> [<file>...]
       tinystore demote <dir> [<file>...]";

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

pub(crate) fn demote(args: &[String]) -> ExitCode {
    let Some((dir, files)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match tinystore::replication::demote(Path::new(dir), files) {
        Ok(demoted) if demoted.is_empty() => {
            eprintln!("tinystore demote: {dir} writes no file to make a copy");
            ExitCode::FAILURE
        }
        Ok(demoted) => {
            for one in demoted {
                match one.last {
                    Some((sequence, epoch)) => println!(
                        "{}: a copy again; its log stood at entry {sequence}, epoch {epoch}, and what its new home \
                         never had of it is lost",
                        one.file
                    ),
                    None => println!("{}: a copy again", one.file),
                }
            }
            println!("start the node with --from its new home to take each file again");
            ExitCode::SUCCESS
        }
        Err(error) if error.kind() == ErrorKind::InUse => {
            eprintln!("tinystore demote: {error}\nstop the node that holds {dir} first");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("tinystore demote: {error}");
            ExitCode::FAILURE
        }
    }
}
