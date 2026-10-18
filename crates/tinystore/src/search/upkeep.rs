//! What maintenance does for an index's rows, the serving ones and those a
//! rebuild makes beside them alike: packing their head into blocks, purging
//! the documents gone, building again a dictionary of words that fell
//! behind, and keeping the clusters of their vectors.

use std::sync::Arc;
use std::time::Duration;

use super::clusters::{self, HEAD, Phase};
use super::correct::Corrector;
use super::engine::{Search, count};
use super::open::{Data, OpenIndex};
use super::pack;
use super::purge;
use super::score::Part;
use crate::Result;
use crate::sqlite::sql_error;
use crate::sync::lock;

/// Documents set since a head was packed that make it due: search-compact
/// packed every thousand writes, which kept `or`'s p99 at 14–16 ms through
/// 15,000 writes where a head never packed reached 37.
pub(crate) const PACK_AT: u64 = 1000;
/// How long a dictionary of an index's words serves after they change
/// before background work builds it again.
const DICTIONARY_AGE: Duration = Duration::from_secs(60);

/// The documents set since packing last reached the head, and whether the
/// head holds a posting.
const READ_WAITING: &str = "select
    (select count(*) from _tinystore_search_docs
        where index_id = ?1 and doc >= (select normed from _tinystore_search_indexes where id = ?1)),
    exists (select 1 from _tinystore_search_head where index_id = ?1)";
const READ_WORDS_VERSION: &str = "select words_version from _tinystore_search_indexes where id = ?1";

impl Search {
    /// Packs the head of an index's rows whole, a step a commit, once it is
    /// due: a thousand documents set since, or a pass that a failure stopped
    /// and no set followed.
    pub(crate) fn pack(&self, data: &Data, everything: bool) -> Result<u64> {
        let id = data.id;
        let (waiting, held) = self.file.read(|connection| {
            let failed = |error| sql_error("read the head", error);
            let mut select = connection.prepare_cached(READ_WAITING).map_err(failed)?;
            select.query_row([id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?))).map_err(failed)
        })?;
        if !held || !everything && waiting > 0 && waiting < PACK_AT as i64 {
            return Ok(0);
        }
        let mut packed = 0;
        loop {
            let step = self.file.transaction(|tx| pack::step(tx, id, pack::STEP_ROWS))?;
            packed += step.postings;
            if step.done {
                return Ok(packed);
            }
        }
    }

    /// Takes the documents gone out of an index's blocks, a step a commit,
    /// once they are due. A purge stopped by a failure goes on from where it
    /// stood at the next pass; one stopped by a restart begins again.
    pub(crate) fn purge(&self, data: &Data, everything: bool) -> Result<u64> {
        let id = data.id;
        let mut under_way = lock(&data.purge);
        if under_way.is_none() {
            if !self.file.read(|connection| purge::due(connection, id, everything))? {
                return Ok(0);
            }
            *under_way = Some(self.file.read(|connection| purge::begin(connection, id))?);
        }
        let Some(purge) = under_way.as_mut() else { return Ok(0) };
        loop {
            let at = self.file.transaction(|tx| purge.step(tx, id, purge::STEP_KEYS))?;
            if purge.advance(at) {
                break;
            }
        }
        let purged = purge.documents() as u64;
        *under_way = None;
        Ok(purged)
    }

    /// Builds again the dictionary of an index's words a search built, once
    /// its words changed and it is old enough to.
    pub(crate) fn build_dictionary(&self, data: &Data, everything: bool) -> Result<()> {
        let Some(kept) = lock(&data.kept.corrector).clone() else { return Ok(()) };
        if !everything && kept.built.elapsed() < DICTIONARY_AGE {
            return Ok(());
        }
        let parts: Vec<Part> = data.settings.fields.iter().map(|field| Part::written(field.number)).collect();
        let built = self.file.read(|connection| {
            let version = count(connection, READ_WORDS_VERSION, data.id)? as i64;
            if version == kept.version {
                return Ok(None);
            }
            Corrector::build(connection, data.id, version, &parts).map(Some)
        })?;
        if let Some(built) = built {
            *lock(&data.kept.corrector) = Some(Arc::new(built));
        }
        Ok(())
    }

    /// Builds, keeps or drops the clusters of an index's vectors, as its
    /// recall asks: trained once it holds enough vectors, every vector
    /// assigned a step a commit, the head assigned once due, a cluster grown
    /// too large split, and how many a search reads chosen again as it grows.
    pub(crate) fn cluster(&self, open: &OpenIndex, data: &Data, everything: bool) -> Result<()> {
        let Some(vectors) = data.settings.vectors else { return Ok(()) };
        let id = data.id;
        let state = self.file.read(|connection| clusters::state(connection, id))?;
        let Some(recall) = vectors.recall.filter(|_| state.phase != Phase::Dropping) else {
            while state.phase != Phase::None && !self.file.transaction(|tx| clusters::drop_step(tx, id))? {}
            return Ok(());
        };
        if state.phase == Phase::None {
            if state.vectors < clusters::BUILD_AT {
                return Ok(());
            }
            let centroids =
                self.file.read(|connection| clusters::trained(connection, id, &state, vectors.dimensions))?;
            if centroids.is_empty() {
                return Ok(());
            }
            self.file.transaction(|tx| clusters::begin(tx, id, state, &centroids))?;
        }
        self.assign(data, vectors.dimensions, everything)?;
        self.calibrate(open, data, recall)
    }

    /// Assigns what a build has not reached, or the head once it is due, a
    /// step a commit, then splits each cluster it grew too large.
    fn assign(&self, data: &Data, dimensions: usize, everything: bool) -> Result<()> {
        let id = data.id;
        let (mut touched, mut draining) = (Vec::new(), false);
        loop {
            let state = self.file.read(|connection| clusters::state(connection, id))?;
            match state.phase {
                Phase::Building => {}
                Phase::Serving => {
                    let head = self.file.read(|connection| clusters::members(connection, id, HEAD))?;
                    if head == 0 || !(draining || everything || head >= clusters::HEAD_AT) {
                        break;
                    }
                    draining = true;
                }
                Phase::None | Phase::Dropping => break,
            }
            let step = self.file.read(|connection| {
                let kept = clusters::kept(&data.kept.clusters, connection, (id, state.version, dimensions))?;
                clusters::assigned(connection, id, &kept, state)
            })?;
            match self.file.transaction(|tx| clusters::assign(tx, id, &step))? {
                // A head whose members no longer have vectors would come back alike every time.
                Some(added) if added.is_empty() && state.phase == Phase::Serving => break,
                Some(added) => touched.extend(added),
                None => {}
            }
        }
        touched.sort_unstable();
        touched.dedup();
        for cluster in touched {
            self.file.transaction(|tx| {
                let state = clusters::state(tx, id)?;
                let kept = clusters::kept(&data.kept.clusters, tx, (id, state.version, dimensions))?;
                if clusters::too_large(tx, id, cluster)? { clusters::split(tx, id, &kept, cluster) } else { Ok(()) }
            })?;
        }
        Ok(())
    }

    /// Chooses again how many clusters a search reads, once none was chosen,
    /// the recall asked for changed or the vectors doubled since.
    fn calibrate(&self, open: &OpenIndex, data: &Data, recall: f64) -> Result<()> {
        let id = data.id;
        let Some(dimensions) = data.settings.vectors.map(|vectors| vectors.dimensions) else { return Ok(()) };
        let state = self.file.read(|connection| clusters::state(connection, id))?;
        let due = state.calibrated_vectors == 0
            || state.calibrated_recall != recall
            || state.vectors >= 2 * state.calibrated_vectors.max(1);
        if state.phase != Phase::Serving || !due {
            return Ok(());
        }
        let ((probes, rescored), count) = self.file.read(|connection| {
            let kept = clusters::kept(&data.kept.clusters, connection, (id, state.version, dimensions))?;
            Ok((clusters::calibrate(connection, id, &kept, recall)?, kept.len()))
        })?;
        let calibrated = ((probes, rescored), (state.vectors, recall));
        self.file.transaction(|tx| clusters::calibrated(tx, id, calibrated.0, calibrated.1))?;
        if probes == 0 {
            tracing::info!(target: "tinystore", "search {}: no clusters reach recall {recall}; searches stay exact", open.name);
        } else {
            tracing::info!(target: "tinystore", "search {}: {count} clusters of vectors, {probes} read and {rescored} \
                scored again a search for recall {recall}", open.name);
        }
        Ok(())
    }
}
