//! A statement's rows by their columns: what each column is told to hold.

use super::{Column, Columns, Holds};
use crate::sql::Answer;
use crate::sql::{Database, Sql, Value, Wanted};
use crate::{Options, Store, sql};

const SAMPLES: &str = "create table samples (
    ts    integer not null,
    cpu   real,
    hits  integer,
    host  text,
    raw   blob,
    extra any
) strict";

const LOOSE: &str =
    "create table loose (price numeric, total decimal(10, 2), ratio float, big bigint, name varchar(20), bare)";

struct Fixture {
    db: Database,
    // a database lives while its store does
    _store: Store,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let db = store.database("app").migrations([("0001_samples.sql", SAMPLES), ("0002_loose.sql", LOOSE)]).open();
    Fixture { db: db.unwrap(), _store: store, _dir: dir }
}

fn columns(db: &Database, statement: Sql) -> Vec<Column> {
    let columns: Columns = db.read(&statement, Wanted::All).unwrap().expect("a query");
    columns.columns.into_iter().map(|(_, column)| column).collect()
}

fn text(text: &str) -> Value {
    Value::Text(text.to_owned())
}

#[test]
fn a_column_of_integers_or_of_reals_alone_is_packed_and_any_other_keeps_its_values() {
    let f = fixture();
    f.db.exec(sql!(
        "insert into samples (ts, cpu, hits, host, raw) values (1, 0.5, 7, 'a', x'01'), (2, 1.5, 8, 'b', x'02')"
    ))
    .unwrap();

    let read = sql!(
        "select ts, cpu, host, raw, case ts when 1 then 1 else 2.5 end as either, case ts when 1 then 1.5 else 2 end as other from samples order by ts"
    );
    let by_columns: Columns = f.db.read(&read, Wanted::All).unwrap().expect("a query");
    assert_eq!((by_columns.len(), by_columns.width()), (2, 6));
    let names: Vec<&str> = by_columns.columns.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["ts", "cpu", "host", "raw", "either", "other"]);
    let held: Vec<Column> = by_columns.columns.into_iter().map(|(_, column)| column).collect();
    assert_eq!(
        held,
        [
            Column::Integers { values: vec![1, 2], nulls: vec![] },
            Column::Reals { values: vec![0.5, 1.5], nulls: vec![] },
            Column::Values(vec![text("a"), text("b")]),
            Column::Values(vec![Value::Blob(vec![1]), Value::Blob(vec![2])]),
            // an INTEGER beside a REAL is read as neither: 1 stays 1, and not 1.0
            Column::Values(vec![Value::Integer(1), Value::Real(2.5)]),
            Column::Values(vec![Value::Real(1.5), Value::Integer(2)]),
        ]
    );
}

#[test]
fn a_null_among_numbers_is_marked_and_is_zero_or_a_nan() {
    let f = fixture();
    f.db.exec(sql!(
        "insert into samples (ts, cpu, hits) values (1, null, 7), (2, 0.5, null), (3, null, 9), (4, 2.5, 10)"
    ))
    .unwrap();

    let held = columns(&f.db, sql!("select cpu, hits from samples order by ts"));
    let [Column::Reals { values: cpu, nulls: no_cpu }, Column::Integers { values: hits, nulls: no_hits }] = &held[..]
    else {
        panic!("reals and integers, and it is {held:?}");
    };
    assert!(cpu[0].is_nan() && cpu[2].is_nan(), "{cpu:?}");
    assert_eq!((cpu[1], cpu[3]), (0.5, 2.5));
    assert_eq!(no_cpu, &[true, false, true, false]);
    assert_eq!(hits, &[7, 0, 9, 10]);
    assert_eq!(no_hits, &[false, true, false, false], "the rows after the last NULL are marked too");
}

#[test]
fn a_column_that_holds_no_value_is_what_its_declaration_says() {
    let f = fixture();
    let no_row = sql!("select cpu, hits, host, extra, cpu + 1 as more from samples where 0");
    assert_eq!(
        columns(&f.db, no_row),
        [
            Column::Reals { values: vec![], nulls: vec![] },
            Column::Integers { values: vec![], nulls: vec![] },
            Column::Values(vec![]),
            Column::Values(vec![]),
            // an expression declares nothing
            Column::Values(vec![]),
        ]
    );

    f.db.exec(sql!("insert into samples (ts) values (1)")).unwrap();
    let nulls_alone = columns(&f.db, sql!("select hits, host, raw, cpu + 1 as more, cpu from samples"));
    assert_eq!(
        nulls_alone[..4],
        [
            Column::Integers { values: vec![0], nulls: vec![true] },
            Column::Values(vec![Value::Null]),
            Column::Values(vec![Value::Null]),
            Column::Values(vec![Value::Null]),
        ]
    );
    assert!(matches!(&nulls_alone[4], Column::Reals { values, nulls } if values[0].is_nan() && nulls == &[true]));

    // a declaration is read as SQLite reads a column's affinity from it
    let loose = columns(&f.db, sql!("select price, total, ratio, big, name, bare from loose"));
    let packed = |column: &Column| match column {
        Column::Integers { .. } => Holds::Integers,
        Column::Reals { .. } => Holds::Reals,
        Column::Values(_) => Holds::Any,
    };
    let held: Vec<Holds> = loose.iter().map(packed).collect();
    assert_eq!(held, [Holds::Reals, Holds::Reals, Holds::Reals, Holds::Integers, Holds::Any, Holds::Any]);
    assert_eq!(Holds::declared(Some("FLOATING POINT")), Holds::Integers, "INT anywhere, as SQLite has it");
}
