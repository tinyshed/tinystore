//! `tinystore migrate` as a shell runs it on a database whose newest
//! migration is a draft: applied with a copy kept, refused once edited,
//! applied again from the copy with `--redo-drafts`, and shipped.

use std::path::Path;
use std::process::{Command, Output};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

fn tinystore(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    Command::new(BINARY).args(args).output().unwrap()
}

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

fn write(migrations: &Path, name: &str, sql: &str) {
    std::fs::write(migrations.join(name), sql).unwrap();
}

// contract: SQL-12
#[test]
fn an_edited_draft_is_refused_until_redo_drafts_applies_it_again_and_shipped_it_loses_its_copy() {
    let dirs = tempfile::tempdir().unwrap();
    let (data, migrations) = (dirs.path().join("data"), dirs.path().join("migrations"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir(&migrations).unwrap();
    let copy = data.join("sql").join("shop.before-drafts");
    write(&migrations, "0001_orders.sql", "create table orders (id integer primary key, total integer not null);");
    write(&migrations, "0002_note.draft.sql", "alter table orders add column note text;");
    let migrate = |more: &[&str]| {
        let mut args: Vec<&dyn AsRef<std::ffi::OsStr>> = vec![&"migrate", &data, &"shop", &migrations];
        args.extend(more.iter().map(|arg| arg as &dyn AsRef<std::ffi::OsStr>));
        tinystore(&args)
    };

    let applied = migrate(&[]);
    assert!(said(&applied).contains("applied 0001_orders.sql, 0002_note.draft.sql"), "{}", said(&applied));
    assert!(copy.is_file(), "the database before its draft is kept");
    let inserted =
        tinystore(&[&"sql", &data, &"shop", &"insert into orders (total, note) values (5, 'x')", &"--write"]);
    assert!(inserted.status.success(), "{}", said(&inserted));

    write(&migrations, "0002_note.draft.sql", "alter table orders add column note text not null default '';");
    let refused = migrate(&[]);
    assert!(!refused.status.success(), "{}", said(&refused));
    assert!(said(&refused).contains("--redo-drafts` takes the database back"), "{}", said(&refused));
    let redone = migrate(&["--redo-drafts"]);
    let took_back = "took back 0002_note.draft.sql, applied 0002_note.draft.sql";
    assert!(said(&redone).contains(took_back), "{}", said(&redone));
    let counted = tinystore(&[&"sql", &data, &"shop", &"select count(*) from orders"]);
    let count = String::from_utf8_lossy(&counted.stdout).lines().last().map(str::to_owned);
    assert_eq!(count.as_deref(), Some("0"), "the order written after the draft is gone: {}", said(&counted));

    std::fs::rename(migrations.join("0002_note.draft.sql"), migrations.join("0002_note.sql")).unwrap();
    let shipped = migrate(&[]);
    assert!(said(&shipped).contains("shipped 0002_note.sql"), "{}", said(&shipped));
    assert!(!copy.exists(), "a draft that shipped needs no copy");
    let again = migrate(&[]);
    assert!(said(&again).contains("up to date: 2 migrations applied"), "{}", said(&again));
}
