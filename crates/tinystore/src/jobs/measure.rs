//! What jobs tells the store's own metrics, docs/guides/metrics/self-metrics.md: each
//! queue's jobs done, failed and retried, how long a run took, how late a
//! claim took a job past its time, and how many wait and run now.

use rusqlite::Connection;

use super::claim::{How, Lease, Settled};
use super::engine::Jobs;
use crate::probe::{Probe, Sample};
use crate::sqlite::sql_error;

const QUEUES: &str = "select q.name, q.waiting, (select count(*) from _tinystore_jobs_leases l where l.queue = q.id)
    from _tinystore_jobs_queues q";

impl Jobs {
    /// How late each claimed job was taken past the time it was due.
    pub(super) fn measure_claims(&self, queue: &str, leases: &[Lease], now: i64) {
        for lease in leases {
            let late = std::time::Duration::from_millis(u64::try_from(now - lease.next).unwrap_or(0));
            self.probe().time("tinystore_jobs_claim_delay_ms", &[("queue", queue)], late);
        }
    }

    /// What a written settlement made of its job; one whose lease another
    /// claim took, or that only extended its lease, made nothing.
    pub(super) fn measure_settled(&self, queue: &str, lease: &Lease, how: &How, settled: &Settled, now: i64) {
        if settled.lost {
            return;
        }
        let labels = [("queue", queue)];
        let ended = match how {
            How::Done => "tinystore_jobs_completed_total",
            How::Retry { .. } if settled.failed.is_none() => "tinystore_jobs_retried_total",
            How::Retry { .. } | How::Fail { .. } => "tinystore_jobs_failed_total",
            How::Snooze { .. } | How::GiveBack | How::Extend { .. } => return,
        };
        self.probe().count(ended, &labels, 1.0);
        if let (_, Some(took)) = lease.last_run(now) {
            let took = std::time::Duration::from_millis(u64::try_from(took).unwrap_or(0));
            self.probe().time("tinystore_jobs_run_ms", &labels, took);
        }
    }
}

impl Sample for Jobs {
    /// The jobs each queue holds waiting and running now.
    fn sample(&self, probe: &Probe) {
        let read = self.file().read(|c: &Connection| {
            let mut select = c.prepare_cached(QUEUES).map_err(|error| sql_error("jobs' queues", error))?;
            let rows = select
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)))
                .and_then(Iterator::collect::<rusqlite::Result<Vec<_>>>)
                .map_err(|error| sql_error("jobs' queues", error))?;
            Ok(rows)
        });
        for (queue, waiting, running) in read.unwrap_or_default() {
            probe.set("tinystore_jobs_waiting", &[("queue", &queue)], waiting as f64);
            probe.set("tinystore_jobs_running", &[("queue", &queue)], running as f64);
        }
    }
}
