//! A source's list against what a table holds, by cells: what every
//! language's mirror runs, the Rust handle's rows and the wire's alike. On the
//! writer, in one savepoint, it reads what the scope holds, compares by key in
//! memory, and writes only the difference:
//!
//! | The given row's key        | Held as                 | Written                                | Answered |
//! |----------------------------|-------------------------|----------------------------------------|----------|
//! | not held                   |                         | an insert                              | added    |
//! | held                       | marked gone             | its differing columns, the mark nulled | added    |
//! | held                       | a compared column apart | its differing given columns            | changed  |
//! | held                       | the same                | nothing                                | same     |
//! | not given, `replace` alone | live                    | the mark set to now, or a delete       | gone     |
//! | not given, `replace` alone | marked gone             | nothing                                |          |
//!
//! A row that is the same is not written at all, so no page of it is dirtied:
//! that is what a mirror saves against a delete of everything and an insert,
//! or an upsert of the whole list.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rusqlite::Connection;

use super::affinity::{Affinity, Collation};
use super::table::{self, Kept};
use crate::limits::{Limit, MIRROR_ROWS};
use crate::sql::mark::Unit;
use crate::sql::pieces::quote_one;
use crate::sql::rows::{Held, Rows};
use crate::sql::run::{self, Wanted};
use crate::sql::statement::Sql;
use crate::sql::values::Value;
use crate::{Error, Memory, Result};

/// What a mirror opened with: its table, the columns that name a row at the
/// source, the column that marks what the source dropped and the unit it
/// marks in, and the columns whose change makes a row changed.
#[derive(Clone, Debug)]
pub(crate) struct Shape {
    pub(crate) table: String,
    pub(crate) key: Vec<String>,
    pub(crate) gone: Option<String>,
    pub(crate) unit: Unit,
    pub(crate) change_on: Option<Vec<String>>,
}

/// The source's rows as cells: their columns once, and a value a column.
#[derive(Clone, Debug, Default)]
pub(crate) struct Listed {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<Value>>,
}

/// What a call compares the list with: the scope it replaces, or the rows
/// the list names alone.
#[derive(Clone, Debug)]
pub(crate) enum Mode {
    Replace { scope: Vec<(String, Value)>, min_kept: Option<f64> },
    Upsert,
}

/// What a call found, each row by `columns`: the list's columns, or the
/// table's when the list names none. A changed row is held as it was, and
/// given as it is now.
#[derive(Debug, Default)]
pub(crate) struct Found {
    pub(crate) columns: Vec<String>,
    pub(crate) added: Vec<Vec<Value>>,
    pub(crate) changed: Vec<(Vec<Value>, Vec<Value>)>,
    pub(crate) gone: Vec<Vec<Value>>,
    pub(crate) same: u64,
    pub(crate) suspect: bool,
}

impl Found {
    /// The bytes of the rows it answers with, each value's and row's with
    /// the five bytes at most that MessagePack puts before one.
    pub(crate) fn bytes(&self) -> usize {
        self.rows().map(|row| row_bytes(row)).sum()
    }

    /// Every row it answers with, the changed ones before and after.
    pub(crate) fn rows(&self) -> impl Iterator<Item = &Vec<Value>> {
        let changed = self.changed.iter().flat_map(|(before, after)| [before, after]);
        self.added.iter().chain(&self.gone).chain(changed)
    }
}

/// A row's bytes in an answer, with the five at most that MessagePack puts
/// before it and before each value.
pub(crate) fn row_bytes(row: &[Value]) -> usize {
    5 + row.iter().map(|value| value.size() + 5).sum::<usize>()
}

/// What a call's answer must fit before it writes anything, over the wire:
/// one message, or, for a run's commit, which answers in parts, each row one.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(feature = "wire"), expect(dead_code, reason = "only the wire answers within its messages"))]
pub(crate) enum Fits {
    Whole(Limit),
    Parts(Limit),
}

/// What a call runs with besides its list: the store's time in the unit of
/// the mirror's mark, the store's memory its read holds, and what its answer
/// must fit, past which it writes nothing.
pub(crate) struct Running<'a> {
    pub(crate) now: i64,
    pub(crate) memory: &'a Arc<Memory>,
    pub(crate) answer: Option<Fits>,
}

/// Compares the list with what the table holds and writes the difference on
/// `connection`, the writer inside a savepoint, so that one that fails leaves
/// nothing. A suspect answer, or one past its bound, writes nothing either.
pub(crate) fn mirror_on(
    connection: &Connection,
    shape: &Shape,
    (given, mode): (&Given, &Mode),
    running: &Running<'_>,
) -> Result<Found> {
    let kept = table::check(connection, shape)?;
    let as_kept = AsKept::of(connection, given, &kept)?;
    let held = held_rows(connection, shape, given, (mode, running.memory))?;
    let plan = Plan::of(shape, &as_kept, &held)?;
    let found = plan.found(given, &held, mode)?;
    if let Some(answer) = running.answer {
        refuse_past(&found, answer)?;
    }
    if !found.suspect {
        plan.write(connection, shape, (given, &held), running.now)?;
    }
    Ok(found)
}

/// Refuses an answer that does not fit what it must: past one message, or,
/// in parts, with a row past one.
fn refuse_past(found: &Found, answer: Fits) -> Result<()> {
    let (wanted, bound) = match answer {
        Fits::Whole(bound) => (found.bytes(), bound),
        Fits::Parts(bound) => (found.rows().map(|row| row_bytes(row)).max().unwrap_or(0), bound),
    };
    if wanted as u64 <= bound.bound {
        return Ok(());
    }
    let why = match answer {
        Fits::Whole(_) => {
            format!("its answer of {wanted} bytes passes one message's {}: mirror a scope at a time", bound.bound)
        }
        Fits::Parts(_) => format!("a row of {wanted} bytes in its answer passes one message's {}", bound.bound),
    };
    Err(bound.refuse(wanted as u64, why))
}

/// A key's value as a map holds it: an integer and a real of one number are
/// one key, as SQLite's `is` says, and so are two texts its collation calls
/// one.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum Part {
    Null,
    Integer(i64),
    Real(u64),
    Text(String),
    Blob(Vec<u8>),
}

/// 2^63, the first real past every `i64`.
const PAST_I64: f64 = 9_223_372_036_854_775_808.0;

fn part_of(value: &Value, collation: Collation) -> Part {
    match value {
        Value::Null => Part::Null,
        Value::Integer(n) => Part::Integer(*n),
        #[expect(clippy::cast_possible_truncation, reason = "a whole real within i64, which the cast keeps")]
        Value::Real(x) if x.fract() == 0.0 && (-PAST_I64..PAST_I64).contains(x) => Part::Integer(*x as i64),
        Value::Real(x) => Part::Real(x.to_bits()),
        Value::Text(text) => Part::Text(collation.folded(text)),
        Value::Blob(bytes) => Part::Blob(bytes.clone()),
    }
}

/// Whether two values are one as SQLite's `is` compares them, with no
/// column's affinity between, which a given value had applied before: an
/// integer and a real by their numbers, a text and a blob by their bytes,
/// each only with its own kind.
///
/// ```text
/// 4 and 4.0      → the same        '4' and 4      → not
/// 0.0 and -0.0   → the same        'a' and x'61'  → not
/// ```
pub(crate) fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Integer(n), Value::Real(_)) | (Value::Real(_), Value::Integer(n)) => {
            let real = if matches!(a, Value::Real(_)) { a } else { b };
            part_of(real, Collation::Binary) == Part::Integer(*n)
        }
        (Value::Real(x), Value::Real(y)) => x == y,
        _ => a == b,
    }
}

/// The list as checked before the call takes the writer: where its key's and
/// compared columns lie among its columns, and each row by its key.
#[derive(Debug)]
pub(crate) struct Given {
    listed: Listed,
    key: Vec<usize>,
    compared: Vec<usize>,
    /// The list's columns a write may set: all but the key's.
    settable: Vec<usize>,
}

/// The list without the column that marks what is gone, which a row type that
/// reads the mark back carries: null in every row, it says nothing, and goes.
/// A row that gives a mark is refused, since the mark is the mirror's to set.
fn without_gone(shape: &Shape, mut listed: Listed) -> Result<Listed> {
    let Some(gone) = shape.gone.as_deref() else { return Ok(listed) };
    let Some(at) = listed.columns.iter().position(|name| name == gone) else { return Ok(listed) };
    if let Some(row) = listed.rows.iter().position(|row| row.get(at).is_some_and(|value| *value != Value::Null)) {
        let why =
            format!("row {}: it marks {gone:?}, the column that marks what is gone, which is the mirror's", row + 1);
        return Err(Error::invalid(why));
    }
    listed.columns.remove(at);
    for row in listed.rows.iter_mut().filter(|row| row.len() > at) {
        row.remove(at);
    }
    Ok(listed)
}

impl Given {
    /// The list, refused when it passes the bound, misses a column the
    /// mirror names, holds a ragged row, a key twice or a key with a null
    /// in it, or a row outside the scope.
    pub(crate) fn checked(shape: &Shape, listed: Listed, mode: &Mode) -> Result<Given> {
        let rows = listed.rows.len() as u64;
        if rows > MIRROR_ROWS.bound {
            let why = format!("a list of {rows} rows, past {}: mirror a scope at a time", MIRROR_ROWS.bound);
            return Err(MIRROR_ROWS.refuse(rows, why));
        }
        let listed = without_gone(shape, listed)?;
        let at = |column: &str| listed.columns.iter().position(|name| name == column);
        if listed.rows.is_empty() {
            return Ok(Given { listed, key: Vec::new(), compared: Vec::new(), settable: Vec::new() });
        }
        let carried = |column: &str, what: &str| {
            at(column).ok_or_else(|| Error::invalid(format!("its rows carry no {what} column {column:?}")))
        };
        let key: Vec<usize> = shape.key.iter().map(|column| carried(column, "key")).collect::<Result<_>>()?;
        let settable: Vec<usize> = (0..listed.columns.len()).filter(|index| !key.contains(index)).collect();
        let compared = match &shape.change_on {
            Some(columns) => columns.iter().map(|column| carried(column, "compared")).collect::<Result<_>>()?,
            None => settable.clone(),
        };
        let scope: Vec<(usize, &Value)> = match mode {
            Mode::Replace { scope, .. } => {
                scope.iter().map(|(column, value)| Ok((carried(column, "scope")?, value))).collect::<Result<_>>()?
            }
            Mode::Upsert => Vec::new(),
        };
        let given = Given { listed, key, compared, settable };
        given.refuse_misfits(shape, &scope)?;
        Ok(given)
    }

    /// Refuses a ragged row, a key with a null in it, a key given twice and
    /// a row outside the scope. A null names no row: a unique index holds
    /// any number of rows whose key has one, and `is null` finds them all.
    fn refuse_misfits(&self, shape: &Shape, scope: &[(usize, &Value)]) -> Result<()> {
        let width = self.listed.columns.len();
        let mut keys = HashSet::with_capacity(self.listed.rows.len());
        for (at, row) in self.listed.rows.iter().enumerate() {
            if row.len() != width {
                return Err(Error::invalid(format!("row {}: {} values for {width} columns", at + 1, row.len())));
            }
            if let Some((column, value)) = scope.iter().find(|(column, value)| !same(&row[*column], value)) {
                let named = &self.listed.columns[*column];
                let (row, held, scoped) = (self.shown(&shape.key, row), shown(&row[*column]), shown(value));
                return Err(Error::invalid(format!("{row}: {named} {held}, outside the scope's {named} {scoped}")));
            }
            if let Some((name, _)) = shape.key.iter().zip(&self.key).find(|(_, at)| row[**at] == Value::Null) {
                return Err(Error::invalid(format!("{}: its key's {name} is null", self.shown(&shape.key, row))));
            }
            let key: Vec<Part> = self.key.iter().map(|at| part_of(&row[*at], Collation::Binary)).collect();
            if !keys.insert(key) {
                return Err(Error::invalid(format!("{}: given twice", self.shown(&shape.key, row))));
            }
        }
        Ok(())
    }

    /// The list as checked, the mark's column left out.
    pub(crate) fn into_listed(self) -> Listed {
        self.listed
    }

    /// What the list adds to a shared commit.
    pub(crate) fn weight(&self) -> usize {
        self.listed.rows.iter().flatten().map(Value::size).sum()
    }

    /// A row as an error names it, by its key: `row (course_id 1, id "a7")`.
    fn shown(&self, key: &[String], row: &[Value]) -> String {
        let parts: Vec<String> =
            key.iter().zip(&self.key).map(|(name, at)| format!("{name} {}", shown(&row[*at]))).collect();
        format!("row ({})", parts.join(", "))
    }
}

/// A value as an error shows it.
fn shown(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Integer(n) => n.to_string(),
        Value::Real(x) => x.to_string(),
        Value::Text(text) => format!("{text:?}"),
        Value::Blob(bytes) => format!("a blob of {} bytes", bytes.len()),
    }
}

/// The list's values as the table keeps them: a value its column converts as
/// it stores it, a `4` given to a text column, converted, the rest as given;
/// and how the key's columns compare their text.
struct AsKept<'g> {
    given: &'g Given,
    converted: HashMap<(usize, usize), Value>,
    collations: Vec<Collation>,
}

impl<'g> AsKept<'g> {
    fn of(connection: &Connection, given: &'g Given, kept: &Kept) -> Result<AsKept<'g>> {
        let affinities: Vec<Affinity> = given.listed.columns.iter().map(|column| kept.affinity(column)).collect();
        let mut converted = HashMap::new();
        for (at, row) in given.listed.rows.iter().enumerate() {
            for (column, (value, affinity)) in row.iter().zip(&affinities).enumerate() {
                if let Some(value) = affinity.kept(connection, value)? {
                    converted.insert((at, column), value);
                }
            }
        }
        Ok(AsKept { given, converted, collations: kept.collations.clone() })
    }

    fn rows(&self) -> &'g [Vec<Value>] {
        &self.given.listed.rows
    }

    /// A given row's value as its column keeps it.
    fn value(&self, row: usize, column: usize) -> &Value {
        self.converted.get(&(row, column)).unwrap_or(&self.given.listed.rows[row][column])
    }

    fn key_of(&self, row: usize) -> Vec<Part> {
        let parts = self.given.key.iter().zip(&self.collations);
        parts.map(|(column, collation)| part_of(self.value(row, *column), *collation)).collect()
    }

    /// Refuses a key the table holds as one another row of the list gave,
    /// `"4"` beside `4` for a text key, which its insert would refuse.
    fn refuse_twice(
        &self,
        shape: &Shape,
        keys: &mut HashSet<Vec<Part>>,
        (key, row): (Vec<Part>, &[Value]),
    ) -> Result<()> {
        if keys.insert(key) { Ok(()) } else { Err(self.twice(shape, row)) }
    }

    fn twice(&self, shape: &Shape, row: &[Value]) -> Error {
        Error::invalid(format!("{}: given twice, as the table compares its key", self.given.shown(&shape.key, row)))
    }
}

/// The rows the table holds that the list is compared with, by the table's
/// columns, and where the list's columns, the key's and the mark lie among
/// them.
struct HeldRows {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    /// Where each of the list's columns lies among the table's.
    given: Vec<usize>,
    key: Vec<usize>,
    gone: Option<usize>,
    /// The store's memory the rows hold, given back when the call ends.
    _held: Held,
}

fn held_rows(
    connection: &Connection,
    shape: &Shape,
    given: &Given,
    (mode, memory): (&Mode, &Arc<Memory>),
) -> Result<HeldRows> {
    let rows = match mode {
        Mode::Replace { scope, .. } => {
            let rows: Rows = run::write(connection, &scope_select(shape, scope)?, Wanted::All, Held::of(memory))?;
            if rows.len() as u64 > MIRROR_ROWS.bound {
                let why = format!("its scope holds more than {} rows: mirror a narrower scope", MIRROR_ROWS.bound);
                return Err(MIRROR_ROWS.refuse(rows.len() as u64, why));
            }
            rows.into_parts()
        }
        Mode::Upsert => keyed_rows(connection, shape, given, memory)?,
    };
    HeldRows::of(rows, shape, given)
}

impl HeldRows {
    fn of(
        (columns, values, held): (Arc<[String]>, Vec<Value>, Held),
        shape: &Shape,
        given: &Given,
    ) -> Result<HeldRows> {
        let at = |column: &str| {
            let found = columns.iter().position(|name| name == column);
            found.ok_or_else(|| Error::invalid(format!("its table has no column {column:?}")))
        };
        let given_at = given.listed.columns.iter().map(|column| at(column)).collect::<Result<_>>()?;
        let key = shape.key.iter().map(|column| at(column)).collect::<Result<_>>()?;
        let gone = shape.gone.as_deref().map(at).transpose()?;
        let rows = values.chunks(columns.len().max(1)).map(<[Value]>::to_vec).collect();
        Ok(HeldRows { columns: columns.to_vec(), rows, given: given_at, key, gone, _held: held })
    }

    fn key_of(&self, row: &[Value], collations: &[Collation]) -> Vec<Part> {
        self.key.iter().zip(collations).map(|(at, collation)| part_of(&row[*at], *collation)).collect()
    }

    fn is_marked(&self, row: &[Value]) -> bool {
        self.gone.is_some_and(|at| row[at] != Value::Null)
    }

    /// A held row by the answer's columns: the list's, or the table's when
    /// the list names none.
    fn answered(&self, row: &[Value], listed: &Listed) -> Vec<Value> {
        if listed.columns.is_empty() {
            return row.to_vec();
        }
        self.given.iter().map(|at| row[*at].clone()).collect()
    }
}

/// `select * from <table> where <scope>`, by the key's order, one row past
/// the bound at most.
fn scope_select(shape: &Shape, scope: &[(String, Value)]) -> Result<Sql> {
    let table = quoted(&shape.table)?;
    let wheres = scope.iter().map(|(column, _)| Ok(format!("{} is ?", quoted(column)?))).collect::<Result<Vec<_>>>()?;
    let order = shape.key.iter().map(|column| quoted(column)).collect::<Result<Vec<_>>>()?;
    let condition = if wheres.is_empty() { String::new() } else { format!(" where {}", wheres.join(" and ")) };
    let most = MIRROR_ROWS.bound + 1;
    let text = format!("select * from {table}{condition} order by {} limit {most}", order.join(", "));
    Ok(Sql::piece(text, scope.iter().map(|(_, value)| value.clone()).collect(), false))
}

/// The held row of each key the list names, a statement a key, within the
/// store's memory and one call's bound together.
fn keyed_rows(
    connection: &Connection,
    shape: &Shape,
    given: &Given,
    memory: &Arc<Memory>,
) -> Result<(Arc<[String]>, Vec<Value>, Held)> {
    let table = quoted(&shape.table)?;
    let text = format!("select * from {table} where {}", by_key(shape)?);
    let mut held = Held::of(memory);
    // the table's columns, from a read of no row
    let none = Sql::piece(format!("select * from {table} limit 0"), Vec::new(), false);
    let (columns, mut values, _) = run::write::<Rows>(connection, &none, Wanted::All, Held::default())?.into_parts();
    let mut bytes = 0;
    for row in &given.listed.rows {
        let key = given.key.iter().map(|at| row[*at].clone()).collect();
        let rows: Rows = run::write(connection, &Sql::piece(text.clone(), key, false), Wanted::One, Held::default())?;
        let found = rows.into_parts().1;
        bytes += found.iter().map(Value::size).sum::<usize>();
        values.extend(found);
        held.cover(bytes)?;
    }
    Ok((columns, values, held))
}

/// `k1 is ? and k2 is ?`: a row by its key.
fn by_key(shape: &Shape) -> Result<String> {
    let parts = shape.key.iter().map(|column| Ok(format!("{} is ?", quoted(column)?)));
    Ok(parts.collect::<Result<Vec<_>>>()?.join(" and "))
}

/// The table's name or one of its columns', quoted as the table names it: a
/// dot in it is its own, as `pragma_table_info` found it.
fn quoted(name: &str) -> Result<String> {
    quote_one(name).map_err(Error::invalid)
}

/// What a call writes, each row by its place in the list or among the held.
#[derive(Default)]
struct Plan {
    /// A given row not held.
    added: Vec<usize>,
    /// A given row held as gone, and the given columns that differ.
    returned: Vec<(usize, Vec<usize>)>,
    /// A given row held apart, its held row, and the given columns that differ.
    changed: Vec<(usize, usize, Vec<usize>)>,
    /// A held row live that the list left out.
    gone: Vec<usize>,
    same: u64,
    /// The held rows live, which the min_kept guard counts.
    live: usize,
}

impl Plan {
    /// What to write of each given row, compared as the table keeps it with
    /// the held row of its key, and of each held row the list left out.
    fn of(shape: &Shape, given: &AsKept<'_>, held: &HeldRows) -> Result<Plan> {
        let mut plan = Plan::default();
        let mut seen = vec![false; held.rows.len()];
        let by_key: HashMap<Vec<Part>, usize> =
            held.rows.iter().enumerate().map(|(at, row)| (held.key_of(row, &given.collations), at)).collect();
        let differs = |at: usize, kept: &[Value], columns: &[usize]| -> Vec<usize> {
            let apart = |column: &usize| !same(given.value(at, *column), &kept[held.given[*column]]);
            columns.iter().copied().filter(apart).collect()
        };
        let mut keys = HashSet::with_capacity(given.rows().len());
        for (at, row) in given.rows().iter().enumerate() {
            let key = given.key_of(at);
            let Some(&kept_at) = by_key.get(&key) else {
                plan.added.push(at);
                given.refuse_twice(shape, &mut keys, (key, row))?;
                continue;
            };
            if std::mem::replace(&mut seen[kept_at], true) {
                return Err(given.twice(shape, row));
            }
            let kept = &held.rows[kept_at];
            if held.is_marked(kept) {
                plan.returned.push((at, differs(at, kept, &given.given.settable)));
            } else if differs(at, kept, &given.given.compared).is_empty() {
                plan.same += 1;
            } else {
                plan.changed.push((at, kept_at, differs(at, kept, &given.given.settable)));
            }
        }
        for (kept_at, _) in held.rows.iter().enumerate().filter(|(_, kept)| !held.is_marked(kept)) {
            plan.live += 1;
            if !seen[kept_at] {
                plan.gone.push(kept_at);
            }
        }
        Ok(plan)
    }

    /// The answer, each row by the answer's columns, and whether the min_kept
    /// guard holds it back: a scope that held live rows and would keep fewer
    /// than its fraction of them.
    fn found(&self, given: &Given, held: &HeldRows, mode: &Mode) -> Result<Found> {
        let listed = &given.listed;
        let suspect = match mode {
            Mode::Replace { min_kept: Some(min_kept), .. } => {
                if !(*min_kept > 0.0 && *min_kept <= 1.0) {
                    return Err(Error::invalid(format!("a min_kept of {min_kept}: a fraction above 0, and 1 at most")));
                }
                let left = self.live - self.gone.len();
                self.live > 0 && (left as f64) < min_kept * self.live as f64
            }
            _ => false,
        };
        let added = self.added.iter().chain(self.returned.iter().map(|(at, _)| at));
        let changed = self
            .changed
            .iter()
            .map(|(at, kept_at, _)| (held.answered(&held.rows[*kept_at], listed), listed.rows[*at].clone()));
        Ok(Found {
            columns: if listed.columns.is_empty() { held.columns.clone() } else { listed.columns.clone() },
            added: added.map(|at| listed.rows[*at].clone()).collect(),
            changed: changed.collect(),
            gone: self.gone.iter().map(|kept_at| held.answered(&held.rows[*kept_at], listed)).collect(),
            same: self.same,
            suspect,
        })
    }

    fn write(
        &self,
        connection: &Connection,
        shape: &Shape,
        (given, held): (&Given, &HeldRows),
        now: i64,
    ) -> Result<()> {
        let listed = &given.listed;
        let writing = Writing { connection, shape, listed, key: &given.key };
        for at in &self.added {
            writing.insert(&listed.rows[*at])?;
        }
        for (at, columns) in &self.returned {
            writing.update(&listed.rows[*at], columns, true)?;
        }
        for (at, _, columns) in &self.changed {
            writing.update(&listed.rows[*at], columns, false)?;
        }
        for kept_at in &self.gone {
            let kept = &held.rows[*kept_at];
            writing.remove(held.key.iter().map(|at| kept[*at].clone()).collect(), now)?;
        }
        Ok(())
    }
}

/// The statements a plan runs, a row's each, on the writer in its savepoint.
struct Writing<'a> {
    connection: &'a Connection,
    shape: &'a Shape,
    listed: &'a Listed,
    key: &'a [usize],
}

impl Writing<'_> {
    fn insert(&self, row: &[Value]) -> Result<()> {
        let table = quoted(&self.shape.table)?;
        let columns = self.listed.columns.iter().map(|column| quoted(column)).collect::<Result<Vec<_>>>()?;
        let marks = vec!["?"; row.len()].join(", ");
        self.run(format!("insert into {table} ({}) values ({marks})", columns.join(", ")), row.to_vec())
    }

    /// Sets the given `columns` of a held row to the list's values, and its
    /// mark back to null when it came back.
    fn update(&self, row: &[Value], columns: &[usize], came_back: bool) -> Result<()> {
        let mut sets = Vec::new();
        let mut values = Vec::new();
        for at in columns {
            sets.push(format!("{} = ?", quoted(&self.listed.columns[*at])?));
            values.push(row[*at].clone());
        }
        if let (true, Some(gone)) = (came_back, &self.shape.gone) {
            sets.push(format!("{} = null", quoted(gone)?));
        }
        if sets.is_empty() {
            return Ok(());
        }
        values.extend(self.key.iter().map(|at| row[*at].clone()));
        let table = quoted(&self.shape.table)?;
        self.run(format!("update {table} set {} where {}", sets.join(", "), by_key(self.shape)?), values)
    }

    /// Marks a row the list left out as gone at `now`, or deletes it.
    fn remove(&self, key: Vec<Value>, now: i64) -> Result<()> {
        let table = quoted(&self.shape.table)?;
        let Some(gone) = &self.shape.gone else {
            return self.run(format!("delete from {table} where {}", by_key(self.shape)?), key);
        };
        let gone = quoted(gone)?;
        let text = format!("update {table} set {gone} = ? where {} and {gone} is null", by_key(self.shape)?);
        self.run(text, std::iter::once(Value::Integer(now)).chain(key).collect())
    }

    fn run(&self, text: String, values: Vec<Value>) -> Result<()> {
        run::exec(self.connection, &Sql::piece(text, values, false)).map(|_| ())
    }
}
