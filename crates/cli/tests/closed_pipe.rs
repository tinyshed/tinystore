//! A command whose reader closes stdout, as `| head` does once it has its
//! lines, ends quietly and as done, where `println!` panics with exit 101.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

use tinystore::{Options, Store};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

/// Far more rows than a pipe's buffer holds, so that the command is still
/// writing when its reader goes.
const ROWS: &str = "with recursive n(x) as (select 1 union all select x + 1 from n where x < 300000) select x from n";

// contract: CLI-04
#[test]
fn a_reader_that_closes_stdout_ends_the_command_quietly_and_done() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    let store = Store::open(&data, Options::default()).unwrap();
    store.database("app").open().unwrap();
    store.close().unwrap();
    let mut child = Command::new(BINARY)
        .arg("sql")
        .arg(&data)
        .args(["app", ROWS])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    // the reader is dropped at the end of the statement, closing the pipe
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut first).unwrap();
    let mut said = String::new();
    child.stderr.take().unwrap().read_to_string(&mut said).unwrap();
    let status = child.wait().unwrap();
    assert_eq!(first.trim_end(), "x");
    assert!(status.success(), "{status}: {said}");
    assert!(said.is_empty(), "{said}");
}
