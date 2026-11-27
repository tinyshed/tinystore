//! One question asked of several indexes in one call, as a "find anything"
//! box asks it: each index searched with its own options, its hits a group
//! of their own, and every group's hits merged into one list by their ranks.
//!
//! The scores of two indexes say nothing of each other, since each scores by
//! its own documents' words, so the merged list never compares them: it is
//! reciprocal rank fusion, the hit at rank `r` of its group weighing
//! `boost / (60 + r)`.
//!
//! ```text
//! courses, boost 2: c1 c2     pages: p1 p2
//! c1 2/61 = .0328   c2 2/62 = .0323   p1 1/61 = .0164   p2 1/62 = .0161
//! merged: c1 c2 p1 p2
//! ```

use std::sync::Arc;

#[cfg(feature = "sql")]
use serde::Serialize;
#[cfg(feature = "sql")]
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::engine::{InFiles, Search};
use super::filter::Condition;
use super::index::{self, Found, Hit, Index};
use super::matches::Unit;
use super::open::OpenIndex;
use super::read::{Prefix, Query};
#[cfg(feature = "sql")]
use super::table_index::{ReadRows, TableIndex};
use crate::{Error, Result, Store};

/// The indexes one call asks at most.
pub(crate) const MOST_AMONG: usize = 32;
/// The hits a group answers at most, and unless it says.
const MOST_LIMIT: usize = 100;
const LIMIT: usize = 10;
/// Reciprocal rank fusion's constant: the first few of each group stay near
/// one another, and no group's tail outweighs another's head.
const FUSION: f64 = 60.0;

impl Store {
    /// One question asked of several indexes in one call, each named with
    /// [`among`](ManySearch::among) and its own options, run by
    /// [`run`](ManySearch::run):
    ///
    /// ```no_run
    /// # fn main() -> tinystore::Result<()> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// # let courses = store.index::<serde_json::Value>("courses").field("title", 2.0).open()?;
    /// # let pages = store.index::<serde_json::Value>("pages").field("plain", 1.0).filters(["space_id"]).open()?;
    /// # let people = store.index::<serde_json::Value>("people").field("name", 1.0).open()?;
    /// use tinystore::search::eq;
    ///
    /// let found = store
    ///     .search("algebra")
    ///     .among(&courses, |a| a.boost(2.0).limit(5))
    ///     .among(&pages, |a| a.filter(eq("space_id", &7)).limit(5).snippet("plain", 60))
    ///     .among(&people, |a| a.limit(5))
    ///     .run()?;
    /// for merged in &found.merged {
    ///     println!("{} {}", merged.index, merged.hit.id);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Each group holds its index's best by its own scores, and says whether
    /// it has more; [`merged`](ManyFound::merged) holds them merged by
    /// reciprocal rank fusion, since scores of two indexes cannot be compared. An index that
    /// fails is its group's error, and the others answer; the call fails
    /// only when every group does. The store's indexes of documents are
    /// searched in one snapshot of search.db, so that no write lands between
    /// two of their groups; a table's index is searched in a snapshot of its
    /// database's file, and groups from different files are not one moment.
    pub fn search(&self, text: &str) -> ManySearch<'_> {
        ManySearch { store: self, text: text.to_owned(), among: Vec::new() }
    }
}

/// One question asked of several indexes, run by [`ManySearch::run`].
#[must_use = "a search runs at `run`"]
pub struct ManySearch<'s> {
    store: &'s Store,
    text: String,
    among: Vec<(Index<Value>, Among)>,
}

impl std::fmt::Debug for ManySearch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.among.iter().map(|(index, _)| index.name()).collect();
        f.debug_struct("ManySearch").field("text", &self.text).field("among", &names).finish()
    }
}

impl ManySearch<'_> {
    /// Asks `index` too, its search as `options` says: `|a| a.limit(5)`. The
    /// same index may be asked twice, with another filter.
    pub fn among(mut self, index: &impl Searched, options: impl FnOnce(Among) -> Among) -> Self {
        self.among.push((index.erased(), options(Among::default())));
        self
    }

    /// Searches every index asked, 1 to 32, each group in the order asked.
    pub fn run(self) -> Result<ManyFound> {
        search_among(self.store, &self.text, &self.among)
    }

    /// The call's store, text and indexes, for an awaited search.
    #[cfg(feature = "async")]
    pub(crate) fn asked(self) -> (String, Vec<(Index<Value>, Among)>) {
        (self.text, self.among)
    }
}

/// What a group of a [`ManySearch`] asks of its index, as a search of it
/// alone would: the text is the call's.
#[derive(Clone, Debug)]
#[must_use = "an index's options are given to `among`"]
pub struct Among {
    boost: f64,
    limit: usize,
    filter: Vec<Condition>,
    prefix: Prefix,
    matches: bool,
    snippet: Option<(String, usize)>,
    unit: Unit,
}

impl Default for Among {
    fn default() -> Self {
        Among {
            boost: 1.0,
            limit: LIMIT,
            filter: Vec::new(),
            prefix: Prefix::Whole,
            matches: false,
            snippet: None,
            unit: Unit::Bytes,
        }
    }
}

impl Among {
    /// What the group's hits weigh in [`ManyFound::merged`], a positive number,
    /// 1 unless said: a hit of a group of 2 weighs what a hit of half its
    /// rank would in a group of 1, near enough.
    pub fn boost(mut self, boost: f64) -> Self {
        self.boost = boost;
        self
    }

    /// The hits the group answers, 1 to 100 and 10 unless said; its
    /// [`more`](Group::more) says whether the index has more.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// As [`SearchCall::filter`](super::SearchCall::filter).
    pub fn filter(mut self, condition: Condition) -> Self {
        self.filter.push(condition);
        self
    }

    /// As [`SearchCall::prefix`](super::SearchCall::prefix).
    pub fn prefix(mut self) -> Self {
        self.prefix = Prefix::Last;
        self
    }

    /// As [`SearchCall::prefix_every`](super::SearchCall::prefix_every).
    pub fn prefix_every(mut self) -> Self {
        self.prefix = Prefix::Every;
        self
    }

    /// As [`SearchCall::matches`](super::SearchCall::matches).
    pub fn matches(mut self) -> Self {
        self.matches = true;
        self
    }

    /// As [`SearchCall::snippet`](super::SearchCall::snippet).
    pub fn snippet(mut self, field: &str, around: usize) -> Self {
        self.snippet = Some((field.to_owned(), around));
        self
    }

    /// Says where each hit matched, its ranges and its snippet's marks
    /// counted in `unit`, as a client of the wire slices its strings.
    #[cfg(feature = "wire")]
    pub(crate) fn counted_in(mut self, unit: Unit) -> Self {
        self.unit = unit;
        self
    }

    /// The search of `index` this group asks, `limit + 1` hits so that it
    /// knows whether there are more.
    fn query(&self, index: &Index<Value>, text: &str) -> Result<Query> {
        let mut call = index.search(text).limit(self.limit + 1).counted_in(self.unit);
        for condition in &self.filter {
            call = call.filter(condition.clone());
        }
        call = match self.prefix {
            Prefix::Whole => call,
            Prefix::Last => call.prefix(),
            Prefix::Every => call.prefix_every(),
        };
        if self.matches {
            call = call.matches();
        }
        if let Some((field, around)) = &self.snippet {
            call = call.snippet(field, *around);
        }
        call.query()
    }

    /// Refuses a boost or a limit no group takes, naming the index.
    fn check(&self, index: &Index<Value>) -> Result<()> {
        let refused = |what: String| Err(Error::invalid(what).within(format!("index {}", index.name())));
        if !(1..=MOST_LIMIT).contains(&self.limit) {
            return refused(format!("a group's limit of {}: 1 to {MOST_LIMIT}", self.limit));
        }
        if !self.boost.is_finite() || self.boost <= 0.0 {
            return refused(format!("a boost of {}: a positive number", self.boost));
        }
        Ok(())
    }
}

/// An index a [`ManySearch`] asks: an index of documents or a table's, each
/// hit's value its document or its row as JSON, since the indexes of one
/// call keep values of different types. A table's index reads each row as
/// its type and writes it as JSON again, so that type is `Serialize` too.
pub trait Searched: sealed::Sealed {
    #[doc(hidden)]
    fn erased(&self) -> Index<Value>;
}

/// Keeps [`Searched`] to the crate's own indexes.
pub(crate) mod sealed {
    pub trait Sealed {}
}

impl<T> sealed::Sealed for Index<T> {}

#[cfg(feature = "sql")]
impl<T> sealed::Sealed for TableIndex<T> {}

impl<T> Searched for Index<T> {
    /// An index a program holds is one of documents: a table's is a
    /// [`TableIndex`](super::TableIndex), which reads its rows as their type.
    fn erased(&self) -> Index<Value> {
        erased(&self.search, &self.open, None)
    }
}

#[cfg(feature = "sql")]
impl<T: Serialize + DeserializeOwned + 'static> Searched for TableIndex<T> {
    fn erased(&self) -> Index<Value> {
        let inner = self.inner();
        let as_json = inner.read_rows.clone().map(|read_rows| -> ReadRows<Value> {
            Arc::new(move |connection, memory, keys| {
                let rows = read_rows(connection, memory, keys)?;
                rows.into_iter().map(|row| row.map(|row| json_of(&row)).transpose()).collect()
            })
        });
        erased(&inner.search, &inner.open, as_json)
    }
}

/// What a group's hits are: the documents of `search`'s index `open`, or a
/// table's rows where `read_rows` reads them.
fn erased(search: &Arc<Search>, open: &Arc<OpenIndex>, read_rows: Option<RowsAsJson>) -> Index<Value> {
    #[cfg(not(feature = "sql"))]
    let _: Option<RowsAsJson> = read_rows;
    Index {
        search: Arc::clone(search),
        open: Arc::clone(open),
        #[cfg(feature = "sql")]
        read_rows,
        _value: std::marker::PhantomData,
    }
}

#[cfg(feature = "sql")]
type RowsAsJson = ReadRows<Value>;
/// No table without a database.
#[cfg(not(feature = "sql"))]
type RowsAsJson = std::convert::Infallible;

/// A row read as its type, made JSON again for a group's hit.
#[cfg(feature = "sql")]
fn json_of<T: Serialize>(row: &T) -> Result<Value> {
    serde_json::to_value(row).map_err(|error| Error::invalid(format!("a row as JSON: {error}")))
}

/// What a [`ManySearch`] found: a group an index, in the order asked, and
/// every group's hits in one order.
#[derive(Debug)]
#[non_exhaustive]
pub struct ManyFound {
    pub groups: Vec<Group>,
    /// Every group's hits by reciprocal rank fusion, never by score: the hit
    /// at rank `r` of its group, from 1, weighs `boost / (60 + r)`, and of
    /// equal weights the group asked first comes first, then the higher rank.
    pub merged: Vec<Merged>,
}

/// An index's hits in a [`ManySearch`], the best by its own scores first, or
/// why its search failed.
#[derive(Debug)]
#[non_exhaustive]
pub struct Group<V = Value> {
    /// The index's name.
    pub index: String,
    pub hits: Vec<Hit<V>>,
    /// Whether the index has hits past the group's limit, exactly.
    pub more: bool,
    /// The text the index read in fact, when it read a misspelt word as its
    /// nearest, as [`Found::corrected`] says it.
    pub corrected: Option<String>,
    /// Why the index's search failed; its group then holds no hit.
    pub error: Option<Error>,
}

/// A hit of [`ManyFound::merged`]: its group's index, the group's place among
/// the groups, and the hit as its group holds it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Merged {
    pub index: String,
    pub group: usize,
    pub hit: Hit<Value>,
}

/// How a group's hits' values are read: a document's from its JSON, a
/// table's row in the snapshot its search read.
pub(crate) trait Reading<V> {
    fn document(&self, value: &[u8]) -> Result<V>;

    #[cfg(feature = "sql")]
    fn table(&self, index: &Index<Value>, query: Query) -> Result<Found<V>>;
}

/// Each value as JSON, a table's row as its index's type reads it.
struct Json;

impl Reading<Value> for Json {
    fn document(&self, value: &[u8]) -> Result<Value> {
        index::decode(value)
    }

    #[cfg(feature = "sql")]
    fn table(&self, index: &Index<Value>, query: Query) -> Result<Found<Value>> {
        let read_rows = index.read_rows.as_ref().ok_or_else(|| Error::internal("a table's index without its rows"))?;
        super::table_index::found(index, &**read_rows, query, None)
    }
}

/// The groups of a search, and `merged` as each hit's group and its place there.
pub(crate) struct Many<V> {
    pub(crate) groups: Vec<Group<V>>,
    pub(crate) merged: Vec<(usize, usize)>,
}

/// Runs a search of `text` among the indexes asked, its hits' values JSON.
pub(crate) fn search_among(store: &Store, text: &str, among: &[(Index<Value>, Among)]) -> Result<ManyFound> {
    let Many { groups, merged } = searched(store, text, among, &Json)?;
    let merged = merged.into_iter().map(|(group, at)| Merged {
        index: groups[group].index.clone(),
        group,
        hit: groups[group].hits[at].clone(),
    });
    Ok(ManyFound { merged: merged.collect(), groups })
}

/// Runs a search of `text` among the indexes asked, each hit's value read as
/// `reading` says.
pub(crate) fn searched<V>(
    store: &Store,
    text: &str,
    among: &[(Index<Value>, Among)],
    reading: &impl Reading<V>,
) -> Result<Many<V>> {
    check_among(among)?;
    refuse_others(store, among)?;
    let found = found_each(text, among, reading)?;
    let groups = among.iter().zip(found).map(|((index, among), found)| {
        let found = found.map_err(|error| error.within(format!("index {}", index.name())));
        group(index.name(), among.limit, found)
    });
    let mut groups: Vec<Group<V>> = groups.collect();
    if groups.iter().all(|group| group.error.is_some()) {
        let first = groups.iter_mut().find_map(|group| group.error.take());
        return Err(first.unwrap_or_else(|| Error::internal("a search among no index")));
    }
    let merged = fused(&groups, among.iter().map(|(_, among)| among.boost));
    Ok(Many { groups, merged })
}

/// Refuses a call of no index, or past 32, and a group's options no group takes.
pub(crate) fn check_among(among: &[(Index<Value>, Among)]) -> Result<()> {
    if among.is_empty() {
        return Err(Error::invalid("a search among no index: it asks 1 to 32"));
    }
    if among.len() > MOST_AMONG {
        return Err(Error::invalid(format!("a search among {} indexes, past {MOST_AMONG}", among.len())));
    }
    among.iter().try_for_each(|(index, among)| among.check(index))
}

/// Refuses an index of another store: the call reads this store's files.
fn refuse_others(store: &Store, among: &[(Index<Value>, Among)]) -> Result<()> {
    let mut documents: Option<Arc<Search>> = None;
    let mut in_files: Option<Vec<Arc<Search>>> = None;
    for (index, _) in among {
        let ours = if is_table(index) {
            let in_files = match &in_files {
                Some(engines) => engines,
                None => in_files.insert(InFiles::of(store)?.engines()),
            };
            in_files.iter().any(|search| Arc::ptr_eq(search, &index.search))
        } else {
            let search = match &documents {
                Some(search) => search,
                None => documents.insert(Search::of(store)?),
            };
            Arc::ptr_eq(search, &index.search)
        };
        if !ours {
            let refused = Error::invalid("an index of another store, in a search among this one's");
            return Err(refused.within(format!("index {}", index.name())));
        }
    }
    Ok(())
}

fn is_table(index: &Index<Value>) -> bool {
    #[cfg(feature = "sql")]
    return index.read_rows.is_some();
    #[cfg(not(feature = "sql"))]
    {
        let _ = index;
        false
    }
}

/// What each index found, in the order asked: the indexes of documents in
/// one snapshot of search.db, then each table's in its database's file. A
/// snapshot that cannot be read at all, the store closed or the call
/// cancelled, fails the call, since it fails every group alike.
fn found_each<V>(
    text: &str,
    among: &[(Index<Value>, Among)],
    reading: &impl Reading<V>,
) -> Result<Vec<Result<Found<V>>>> {
    let mut queries: Vec<Option<Result<Query>>> =
        among.iter().map(|(index, among)| Some(among.query(index, text))).collect();
    let mut found: Vec<Option<Result<Found<V>>>> = among.iter().map(|_| None).collect();
    let mut asked = Vec::new();
    let mut asked_at = Vec::new();
    for (at, (index, _)) in among.iter().enumerate().filter(|(_, (index, _))| !is_table(index)) {
        match queries[at].take() {
            Some(Ok(query)) => {
                asked.push((&*index.open, query));
                asked_at.push(at);
            }
            Some(Err(refused)) => found[at] = Some(Err(refused)),
            None => {}
        }
    }
    if let Some(&first) = asked_at.first() {
        let answers = among[first].0.search.search_each(asked)?;
        for (at, answer) in asked_at.into_iter().zip(answers) {
            found[at] = Some(answer.and_then(|answer| index::found_in(answer, |value| reading.document(value))));
        }
    }
    #[cfg(feature = "sql")]
    for (at, query) in queries.iter_mut().enumerate() {
        if let Some(query) = query.take() {
            found[at] = Some(query.and_then(|query| reading.table(&among[at].0, query)));
        }
    }
    Ok(found.into_iter().map(|found| found.unwrap_or_else(|| Err(Error::internal("a group never searched")))).collect())
}

/// A group of what an index found: its hits within `limit`, and whether it
/// found one more.
fn group<V>(index: &str, limit: usize, found: Result<Found<V>>) -> Group<V> {
    match found {
        Ok(Found { mut hits, corrected, .. }) => {
            let more = hits.len() > limit;
            hits.truncate(limit);
            Group { index: index.to_owned(), hits, more, corrected, error: None }
        }
        Err(error) => {
            Group { index: index.to_owned(), hits: Vec::new(), more: false, corrected: None, error: Some(error) }
        }
    }
}

/// Every group's hits in one order, each its group and its place there, by
/// reciprocal rank fusion: the hit at rank `r`, from 1, weighs
/// `boost / (60 + r)`; of equal weights the group asked first comes first,
/// then the higher rank.
fn fused<V>(groups: &[Group<V>], boosts: impl Iterator<Item = f64>) -> Vec<(usize, usize)> {
    let mut weighed: Vec<(f64, usize, usize)> = Vec::new();
    for (at, (group, boost)) in groups.iter().zip(boosts).enumerate() {
        weighed.extend((0..group.hits.len()).map(|rank| (boost / (FUSION + (rank + 1) as f64), at, rank)));
    }
    weighed.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    weighed.into_iter().map(|(_, group, rank)| (group, rank)).collect()
}

#[cfg(test)]
thread_local! {
    /// What a test runs before each group of search.db's snapshot but the first.
    static BEFORE_GROUP: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}

/// Runs a test's hook before the group `at` of one snapshot, past the first.
#[cfg(test)]
pub(super) fn before_group(at: usize) {
    if at > 0 {
        BEFORE_GROUP.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook();
            }
        });
    }
}

#[cfg(not(test))]
pub(super) fn before_group(_: usize) {}

/// Runs `hook` on this thread before each group of a snapshot but the first,
/// until the test ends it with `None`.
#[cfg(test)]
pub(super) fn set_before_group(hook: Option<Box<dyn FnMut()>>) {
    BEFORE_GROUP.with(|kept| *kept.borrow_mut() = hook);
}

#[cfg(test)]
#[path = "many_tests.rs"]
mod tests;
