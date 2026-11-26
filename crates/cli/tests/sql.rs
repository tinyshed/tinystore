//! `tinystore sql` as a shell runs it: a query that answers no row still
//! prints its head, the columns in the statement's order.

use std::process::{Command, Output};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

fn tinystore(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    Command::new(BINARY).args(args).output().unwrap()
}

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

// contract: SQL-16
#[test]
fn a_query_that_answers_no_row_prints_its_columns_in_the_statements_order() {
    let dirs = tempfile::tempdir().unwrap();
    let (data, migrations) = (dirs.path().join("data"), dirs.path().join("migrations"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir(&migrations).unwrap();
    let orders = "create table orders (id integer primary key, total integer not null, note text);";
    std::fs::write(migrations.join("0001_orders.sql"), orders).unwrap();
    let migrated = tinystore(&[&"migrate", &data, &"shop", &migrations]);
    assert!(migrated.status.success(), "{}", said(&migrated));

    let none = tinystore(&[&"sql", &data, &"shop", &"select total, note, id from orders"]);
    assert!(none.status.success(), "{}", said(&none));
    let lines: Vec<String> = String::from_utf8_lossy(&none.stdout).lines().map(str::to_owned).collect();
    assert_eq!(lines, ["total  note  id", "no rows"], "{}", said(&none));
    let json = tinystore(&[&"sql", &data, &"shop", &"select total, note, id from orders", &"--json"]);
    assert_eq!(String::from_utf8_lossy(&json.stdout), "", "a JSON object a row, and no row: {}", said(&json));
}
