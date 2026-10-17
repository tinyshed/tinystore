//! The catalog: `catalog.db`, which names each file's home, written by one
//! node a person chose and copied like any other file.
//!
//! A node that makes a file no node holds claims it there first, and writes
//! it only once its claim won: of two nodes claiming one name in the same
//! second, the catalog's home commits one, and the other copies the file from
//! the node that won.
//!
//! ```text
//! Europe opens sql/users.db, which no node holds
//!   ──claim sql/users.db for Europe──▶ the catalog's home   (a write like any other)
//!   ◀──home: Europe────────────────── committed once, the first claim kept
//! Europe makes sql/users.db and writes it; the others read its home in their copy
//! ```
//!
//! The catalog routes and never grants the right to write: a node writes a
//! file because its log is its own, and a promotion moves that, not the
//! catalog. While the catalog's home is out of reach a node reads its copy,
//! and makes no new file of the network.

use std::sync::Arc;

use rusqlite::{OptionalExtension, params};

use super::message::{In, Out};
use crate::sqlite::{Config, File, sql_error};
use crate::{Error, Result, Store};

/// The catalog's file in the store's directory.
pub(crate) const FILE: &str = "catalog.db";

const SCHEMA: &str = "create table if not exists homes (
    file    text    primary key,
    node    text    not null,
    address text    not null,
    epoch   integer not null
) strict, without rowid";
// a node that claims a file again, its address moved, keeps it; a claim at
// a later epoch, a promoted copy's, moves the route to its node
const CLAIM: &str = "insert into homes (file, node, address, epoch) values (?1, ?2, ?3, ?4)
    on conflict (file) do update
        set node = excluded.node, address = excluded.address, epoch = max(homes.epoch, excluded.epoch)
        where homes.node = excluded.node or excluded.epoch > homes.epoch";
const HOME_OF: &str = "select node, address, epoch from homes where file = ?1";

/// A file's home as the catalog names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Home {
    /// The node's id, which it made once and keeps in its `log/node`.
    pub(crate) node: String,
    /// Where other nodes reach it, `host:port`; empty for a node that serves
    /// nothing.
    pub(crate) address: String,
    pub(crate) epoch: u64,
}

/// A node's catalog: the file, its home's or a copy.
pub(crate) struct Catalog {
    file: File,
}

impl Catalog {
    /// Opens the store's catalog: made here on the node that is its home, a
    /// copy waited for on every other.
    pub(crate) fn open(store: &Store) -> Result<Catalog> {
        let file = store.open_file(FILE, Config::default())?;
        if !file.is_copy() {
            file.transaction(|tx| tx.execute_batch(SCHEMA).map_err(|error| sql_error("the catalog", error)))?;
        }
        Ok(Catalog { file })
    }

    /// The home the catalog names of `name`, as this node's copy holds it.
    pub(crate) fn home_of(&self, name: &str) -> Result<Option<Home>> {
        self.file
            .read(|connection| {
                connection
                    .prepare_cached(HOME_OF)
                    .and_then(|mut select| select.query_row([name], read_home).optional())
                    .map_err(|error| sql_error("the catalog", error))
            })
            .map_err(|error| error.within(format!("catalog: {name}")))
    }

    /// Claims `name` for `node`, whose log of it is at `epoch`, at the
    /// catalog's home: the home it names once the claim committed, `node` if
    /// no node held the name or held it at an older epoch, and the node that
    /// did otherwise.
    pub(crate) fn claim(&self, store: &Store, claim: &Claim<'_>) -> Result<Home> {
        if self.file.is_copy() {
            let answer = super::forward::pass_on(store, FILE, claim_op(claim))?;
            return decode_home(&answer).map_err(|error| error.within(format!("catalog: {}", claim.file)));
        }
        self.claim_here(claim)
    }

    fn claim_here(&self, claim: &Claim<'_>) -> Result<Home> {
        let (name, node, address) = (claim.file.to_owned(), claim.node.to_owned(), claim.address.to_owned());
        let epoch = i64::try_from(claim.epoch).map_err(|_| Error::invalid("an epoch past 2^63"))?;
        let shown = format!("catalog: {name}");
        self.file
            .write(name.len() + address.len(), move |tx| {
                tx.execute(CLAIM, params![name, node, address, epoch]).map_err(|error| sql_error("a claim", error))?;
                tx.prepare_cached(HOME_OF)
                    .and_then(|mut select| select.query_row([&name], read_home))
                    .map_err(|error| sql_error("a claim", error))
            })
            .map_err(|error| error.within(shown))
    }

    pub(crate) fn close(&self) {
        let _ = self.file.close();
    }
}

fn read_home(row: &rusqlite::Row<'_>) -> rusqlite::Result<Home> {
    Ok(Home { node: row.get(0)?, address: row.get(1)?, epoch: row.get::<_, i64>(2)? as u64 })
}

/// A node's claim of a file's name.
pub(crate) struct Claim<'a> {
    pub(crate) file: &'a str,
    pub(crate) node: &'a str,
    pub(crate) address: &'a str,
    /// The epoch of the node's log of the file: 1 for a file it makes, the
    /// next for a copy a person promoted.
    pub(crate) epoch: u64,
}

/// A claim as it passes from a copy of the catalog toward its home.
fn claim_op(claim: &Claim<'_>) -> Vec<u8> {
    let mut out = Out(vec![super::forward::CATALOG]);
    out.text(claim.file);
    out.text(claim.node);
    out.text(claim.address);
    out.long(claim.epoch);
    out.0
}

/// Runs a claim another node passed on, at the catalog's home or toward it:
/// the answer's bytes, and the file they wrote.
pub(crate) fn run(store: &Store, body: &[u8]) -> Result<(Vec<u8>, String)> {
    let mut at = In { bytes: body, at: 0 };
    let (file, node, address, epoch) = (at.text()?, at.text()?, at.text()?, at.long()?);
    let catalog = store.node().ok_or_else(|| Error::invalid("a claim for the catalog of a store that is no node"))?;
    let claim = Claim { file: &file, node: &node, address: &address, epoch };
    let home = catalog.catalog(store)?.claim(store, &claim)?;
    Ok((encode_home(&home), FILE.to_owned()))
}

fn encode_home(home: &Home) -> Vec<u8> {
    let mut out = Out(Vec::new());
    out.text(&home.node);
    out.text(&home.address);
    out.long(home.epoch);
    out.0
}

fn decode_home(bytes: &[u8]) -> Result<Home> {
    let mut at = In { bytes, at: 0 };
    Ok(Home { node: at.text()?, address: at.text()?, epoch: at.long()? })
}

/// The node's catalog, opened the first time it is asked for.
pub(crate) type Held = std::sync::Mutex<Option<Arc<Catalog>>>;
