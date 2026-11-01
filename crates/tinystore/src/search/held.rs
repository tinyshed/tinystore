//! What each index of search.db holds, for the command line's `status
//! --names`: its documents and what its rows weigh, read an index at a time,
//! so that a big store keeps no reader open for long.

use rusqlite::params;

use super::engine::Search;
use super::vectors;
use crate::engine::Held;
use crate::sqlite::{query_cached, sql_error};
use crate::{Result, Store};

const SELECT_INDEXES: &str = "select id, name, documents from _tinystore_search_indexes
    where dropping = 0 and beside_of is null order by name";

/// The bytes SQLite keeps an integer of a row in: none for 0 and 1, else the
/// fewest of 1, 2, 3, 4, 6 and 8 that hold it.
macro_rules! integer_bytes {
    ($column:literal) => {
        concat!(
            "case when ",
            $column,
            " in (0, 1) then 0",
            " when ",
            $column,
            " between -128 and 127 then 1",
            " when ",
            $column,
            " between -32768 and 32767 then 2",
            " when ",
            $column,
            " between -8388608 and 8388607 then 3",
            " when ",
            $column,
            " between -2147483648 and 2147483647 then 4",
            " when ",
            $column,
            " between -140737488355328 and 140737488355327 then 6 else 8 end"
        )
    };
}

/// What an index's rows weigh: its documents, with their ids, keys and
/// lengths; its words and their postings, packed and in the head, where a
/// posting is a row of integers until a thousand documents set since the
/// last packing pack it; its lengths; its vectors, their codes and their
/// places among clusters.
const SELECT_BYTES: &str = concat!(
    "select
    (select coalesce(sum(octet_length(id) + length(keys) + length(lengths) + length(value)), 0)
        from _tinystore_search_docs where index_id = ?1)
    + (select coalesce(sum(octet_length(k.term) + coalesce(length(l.directory), 0) + coalesce(length(l.inline), 0)
        + coalesce(length(l.packed), 0) + coalesce(length(l.best), 0)), 0)
        from _tinystore_search_keys as k left join _tinystore_search_lists as l on l.key = k.id where k.index_id = ?1)
    + (select coalesce(sum(length(b.payload)), 0)
        from _tinystore_search_keys as k join _tinystore_search_blocks as b on b.key = k.id where k.index_id = ?1)
    + (select coalesce(sum(length(r.blocks)), 0) from _tinystore_search_keys as k
        join _tinystore_search_runs as r on r.id >= k.id << 20 and r.id < (k.id + 1) << 20 where k.index_id = ?1)
    + (select coalesce(sum(",
    integer_bytes!("key"),
    " + ",
    integer_bytes!("doc"),
    " + ",
    integer_bytes!("tf"),
    " + ",
    integer_bytes!("len"),
    "), 0) from _tinystore_search_head where index_id = ?1)
    + (select coalesce(sum(length(lengths)), 0) from _tinystore_search_norms where index_id = ?1)
    + (select coalesce(sum(length(vector)), 0) from _tinystore_search_vectors where key >= ?2 and key < ?3)
    + (select coalesce(sum(length(code)), 0) from _tinystore_search_codes where key >= ?2 and key < ?3)
    + (select coalesce(sum(length(code)), 0) from _tinystore_search_members where index_id = ?1)"
);

/// Each index by name with its documents and what they weigh. None while the
/// store has no search.db, which a read beside would otherwise try to make.
pub(crate) fn held(store: &Store) -> Result<Vec<Held>> {
    if !store.dir().join("search.db").exists() {
        return Ok(Vec::new());
    }
    let search = Search::of(store)?;
    let indexes: Vec<(i64, String, i64)> = search.file.read(|c| {
        query_cached(c, (SELECT_INDEXES, "list the indexes"), [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    })?;
    let held_in = |(id, name, documents): (i64, String, i64)| {
        let vectors = (vectors::key(id, 0), vectors::key(id + 1, 0));
        let bytes: i64 = search
            .file
            .read(|c| {
                c.prepare_cached(SELECT_BYTES)
                    .and_then(|mut select| select.query_row(params![id, vectors.0, vectors.1], |row| row.get(0)))
                    .map_err(|error| sql_error("read what an index holds", error))
            })
            .map_err(|error| error.within(format!("index {name}")))?;
        Ok(Held { name, count: documents.unsigned_abs(), bytes: bytes.unsigned_abs() })
    };
    indexes.into_iter().map(held_in).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_integer_weighs_what_sqlite_keeps_it_in() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let sql = concat!("select ", integer_bytes!("x"), " from (select ?1 as x)");
        let weighs = |x: i64| -> i64 { connection.query_row(sql, [x], |row| row.get(0)).unwrap() };
        let weighed = [0, 1, 2, 127, 128, -129, 40_000, 1 << 40, i64::MIN].map(weighs);
        assert_eq!(weighed, [0, 0, 1, 1, 2, 2, 3, 6, 8]);
    }
}
