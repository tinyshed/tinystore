//! search.searchMany: one question of several indexes a connection opened,
//! each group's hits as a search's own, `merged` as each hit's group and place.

use serde_json::Value;

use super::super::Answered;
use super::super::codec::Message;
use super::super::protocol::{
    Failure, SearchAmong, SearchGroup, SearchHit, SearchManyFound, SearchManyQuery, SearchMerged,
};
use super::{Handles, matches, one_unit, snippet, statement, unit};
use crate::search::{self, Among, Group, Index, Many, Reading};
use crate::{Result, Store};

/// A hit's value as the wire sends it: a document's JSON as it is kept, or
/// a table's row by its cells.
enum Sent {
    Json(String),
    #[cfg(feature = "sql")]
    Row(crate::sql::Cells),
}

struct Wire;

impl Reading<Sent> for Wire {
    fn document(&self, value: &[u8]) -> Result<Sent> {
        let json =
            std::str::from_utf8(value).map_err(|_| crate::Error::corrupt("a document whose JSON is not UTF-8"))?;
        Ok(Sent::Json(json.to_owned()))
    }

    #[cfg(feature = "sql")]
    fn table(&self, index: &Index<Value>, query: search::Query) -> Result<search::Found<Sent>> {
        let found = search::table_found(index, &crate::sql::cells_on, query, None)?;
        let hits = found.hits.into_iter().map(|hit| search::Hit {
            id: hit.id,
            value: Sent::Row(hit.value),
            score: hit.score,
            matches: hit.matches,
            snippet: hit.snippet,
            text_score: hit.text_score,
            similarity: hit.similarity,
        });
        Ok(search::Found { hits: hits.collect(), corrected: found.corrected, total: found.total })
    }
}

pub(super) fn searched_many(store: &Store, handles: &Handles, asked: SearchManyQuery) -> Answered {
    let among = asked.among.iter().map(|among| asked_of(handles, among)).collect::<Result<Vec<_>, Failure>>()?;
    let listed: Vec<bool> = asked.among.iter().map(|among| among.query.matches.is_some()).collect();
    let Many { groups, merged } = search::searched(store, &asked.text, &among, &Wire)?;
    let groups = groups.into_iter().zip(listed).map(|(group, listed)| group_of(group, listed));
    let merged = merged.into_iter().map(|(group, hit)| SearchMerged { group: wide(group), hit: wide(hit) });
    Ok(SearchManyFound { groups: groups.collect(), merged: merged.collect() }.encode())
}

/// An index a call asks, and its search's options, refusing what only a
/// search of one index takes.
fn asked_of(handles: &Handles, asked: &SearchAmong) -> Result<(Index<Value>, Among), Failure> {
    let query = &asked.query;
    let index = handles.index(query.handle)?;
    let refused = |what: &str| Err(Failure::invalid(format!("index {}: {what}", index.name())));
    if !query.text.is_empty() {
        return refused("a search among several indexes asks them one text, the call's, and an index its own");
    }
    if query.vector.is_some() || query.vector_weight.is_some() || query.rerank || query.min_similarity.is_some() {
        return refused("a search among several indexes by a vector");
    }
    if query.total {
        return refused("a total of a search among several indexes: each group says whether it has more");
    }
    let offset = usize::try_from(query.offset).unwrap_or(usize::MAX);
    let mut among = Among::default().boost(asked.boost.unwrap_or(1.0)).offset(offset);
    if let Some(limit) = query.limit {
        among = among.limit(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    if query.prefix_every {
        among = among.prefix_every();
    } else if query.prefix {
        among = among.prefix();
    }
    one_unit(query)?;
    if let Some(named) = &query.matches {
        among = among.counted_in(unit(named)?).matches();
    }
    if let Some(cut) = &query.snippet {
        let around = cut.around.map_or(search::AROUND, |around| usize::try_from(around).unwrap_or(usize::MAX));
        among = among.counted_in(unit(&cut.unit)?).snippet(&cut.field, around);
    }
    if let Some(conditions) = &query.r#where {
        let statements = query.statements.iter().map(statement).collect::<Result<Vec<_>, _>>()?;
        for condition in search::conditions(conditions, &statements)? {
            among = among.filter(condition);
        }
    }
    Ok((index, among))
}

/// A group as the wire sends it, a table's rows' columns once.
fn group_of(group: Group<Sent>, listed: bool) -> SearchGroup {
    let Group { index, hits, more, corrected, error, .. } = group;
    #[cfg_attr(not(feature = "sql"), expect(unused_mut, reason = "only a table's rows have columns"))]
    let mut columns: Option<Vec<String>> = None;
    let hits = hits.into_iter().map(|hit| {
        let (value, row) = match hit.value {
            Sent::Json(json) => (json, None),
            #[cfg(feature = "sql")]
            Sent::Row((names, cells)) => {
                columns.get_or_insert(names);
                ("null".to_owned(), Some(cells.into_iter().map(super::super::sql::cell_of).collect()))
            }
        };
        SearchHit {
            id: hit.id,
            value,
            score: hit.score,
            row,
            matches: listed.then(|| matches(hit.matches)),
            snippet: hit.snippet.map(snippet),
            text_score: hit.text_score,
            similarity: hit.similarity,
        }
    });
    let hits = hits.collect();
    SearchGroup { index, hits, corrected, columns, more, failure: error.map(Failure::from) }
}

fn wide(at: usize) -> u64 {
    u64::try_from(at).unwrap_or(u64::MAX)
}
