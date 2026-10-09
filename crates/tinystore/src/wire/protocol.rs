// Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.

use std::collections::BTreeMap;

use super::codec::{self, Message, Out, Row};
#[cfg(feature = "sql")]
use super::codec::Cell;
use super::message::Fields;

/// What a client says first.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Hello {
    /// The newest the client speaks.
    pub(crate) protocol: u64,
    /// A name and version, for logs.
    pub(crate) client: String,
    /// Required on TCP.
    pub(crate) token: Option<String>,
    /// The largest body the client takes; the server's when absent.
    pub(crate) max_body: Option<u64>,
    /// The DATA the server may send a stream before the client grants more; 2 MiB when absent.
    pub(crate) stream_credit: Option<u64>,
    /// 16 random bytes, when the client found the server through SERVE.
    pub(crate) challenge: Option<Vec<u8>>,
}

impl Message for Hello {
    const NAME: &'static str = "Hello";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5, 6];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(Hello {
            protocol: fields.get(1, "protocol", codec::uint)?.unwrap_or_default(),
            client: fields.get(2, "client", codec::str)?.unwrap_or_default(),
            token: fields.get(3, "token", codec::str)?,
            max_body: fields.get(4, "maxBody", codec::uint)?,
            stream_credit: fields.get(5, "streamCredit", codec::uint)?,
            challenge: fields.get(6, "challenge", codec::bin)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.protocol));
        out.put(2, codec::str_value(&self.client));
        out.given(3, self.token.as_ref().map(codec::str_value));
        out.given(4, self.max_body.as_ref().map(codec::uint_value));
        out.given(5, self.stream_credit.as_ref().map(codec::uint_value));
        out.given(6, self.challenge.as_ref().map(codec::bin_value));
    }
}

/// What a server answers HELLO with: what the connection agrees.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Welcome {
    /// The one this connection speaks.
    pub(crate) protocol: u64,
    /// Its version.
    pub(crate) server: String,
    /// 16 random bytes a start.
    pub(crate) instance: Vec<u8>,
    /// Admin or data.
    pub(crate) capability: String,
    /// The largest body either side sends.
    pub(crate) max_body: u64,
    /// The streams a client may have open at once.
    pub(crate) in_flight: u64,
    /// The REQUEST and DATA bytes a client may send before credit comes back.
    pub(crate) connection_credit: u64,
    /// The DATA bytes a client may send a stream before credit comes back.
    pub(crate) stream_credit: u64,
    /// What this server serves.
    pub(crate) engines: Vec<String>,
    /// The store's clock. Unix milliseconds.
    pub(crate) now: i64,
    /// The HMAC-SHA256 of HELLO's challenge, keyed with SERVE's secret.
    pub(crate) proof: Option<Vec<u8>>,
}

impl Message for Welcome {
    const NAME: &'static str = "Welcome";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(Welcome {
            protocol: fields.get(1, "protocol", codec::uint)?.unwrap_or_default(),
            server: fields.get(2, "server", codec::str)?.unwrap_or_default(),
            instance: fields.get(3, "instance", codec::bin)?.unwrap_or_default(),
            capability: fields.get(4, "capability", codec::str)?.unwrap_or_default(),
            max_body: fields.get(5, "maxBody", codec::uint)?.unwrap_or_default(),
            in_flight: fields.get(6, "inFlight", codec::uint)?.unwrap_or_default(),
            connection_credit: fields.get(7, "connectionCredit", codec::uint)?.unwrap_or_default(),
            stream_credit: fields.get(8, "streamCredit", codec::uint)?.unwrap_or_default(),
            engines: fields.get(9, "engines", codec::list(codec::str))?.unwrap_or_default(),
            now: fields.get(10, "now", codec::int)?.unwrap_or_default(),
            proof: fields.get(11, "proof", codec::bin)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.protocol));
        out.put(2, codec::str_value(&self.server));
        out.put(3, codec::bin_value(&self.instance));
        out.put(4, codec::str_value(&self.capability));
        out.put(5, codec::uint_value(&self.max_body));
        out.put(6, codec::uint_value(&self.in_flight));
        out.put(7, codec::uint_value(&self.connection_credit));
        out.put(8, codec::uint_value(&self.stream_credit));
        out.put(9, codec::list_value(&self.engines, codec::str_value));
        out.put(10, codec::int_value(&self.now));
        out.given(11, self.proof.as_ref().map(codec::bin_value));
    }
}

/// The last frame of a connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GoAway {
    pub(crate) code: String,
    pub(crate) message: String,
}

impl Message for GoAway {
    const NAME: &'static str = "GoAway";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(GoAway {
            code: fields.get(1, "code", codec::str)?.unwrap_or_default(),
            message: fields.get(2, "message", codec::str)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.code));
        out.put(2, codec::str_value(&self.message));
    }
}

/// A stream's last frame when it failed, with ERROR set: its kind's code, what
/// failed, and the item it names. A message refused is one too.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Failure {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) what: Option<BTreeMap<String, String>>,
}

impl Message for Failure {
    const NAME: &'static str = "Failure";
    const KEYS: &'static [u64] = &[1, 2, 3];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(Failure {
            code: fields.get(1, "code", codec::str)?.unwrap_or_default(),
            message: fields.get(2, "message", codec::str)?.unwrap_or_default(),
            what: fields.get(3, "what", codec::names(codec::str))?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.code));
        out.put(2, codec::str_value(&self.message));
        out.given(3, self.what.as_ref().map(|names| codec::names_value(names, codec::str_value)));
    }
}

/// What a call that opens something answers: its handle, which later calls name.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Handle {
    pub(crate) handle: u64,
}

impl Message for Handle {
    const NAME: &'static str = "Handle";
    const KEYS: &'static [u64] = &[1];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(Handle {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
    }
}

/// A message with nothing to say.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Empty {
}

impl Message for Empty {
    const NAME: &'static str = "Empty";
    const KEYS: &'static [u64] = &[];

    fn read(_fields: &Fields) -> Result<Self, Failure> {
        Ok(Empty {})
    }

    fn write(&self, _out: &mut Out) {
    }
}

/// How many jobs of a queue run at once: in all, across every worker of the
/// store, and in each group.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsConcurrency {
    pub(crate) total: Option<u64>,
    pub(crate) group: Option<u64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsConcurrency {
    const NAME: &'static str = "jobs.Concurrency";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsConcurrency {
            total: fields.get(1, "total", codec::uint)?,
            group: fields.get(2, "group", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.given(1, self.total.as_ref().map(codec::uint_value));
        out.given(2, self.group.as_ref().map(codec::uint_value));
    }
}

/// The wait before a retry: initial, doubling each time up to max.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsBackoff {
    /// Milliseconds.
    pub(crate) initial: u64,
    /// Milliseconds.
    pub(crate) max: u64,
}

#[cfg(feature = "jobs")]
impl Message for JobsBackoff {
    const NAME: &'static str = "jobs.Backoff";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsBackoff {
            initial: fields.get(1, "initial", codec::uint)?.unwrap_or_default(),
            max: fields.get(2, "max", codec::uint)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.initial));
        out.put(2, codec::uint_value(&self.max));
    }
}

/// Jobs started in any span of per.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsRate {
    pub(crate) count: u64,
    /// Milliseconds.
    pub(crate) per: u64,
}

#[cfg(feature = "jobs")]
impl Message for JobsRate {
    const NAME: &'static str = "jobs.Rate";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsRate {
            count: fields.get(1, "count", codec::uint)?.unwrap_or_default(),
            per: fields.get(2, "per", codec::uint)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.count));
        out.put(2, codec::uint_value(&self.per));
    }
}

/// Opens a queue; an option left out takes its default.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsQueueOpen {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    /// 10 when absent, the first run counted.
    pub(crate) attempts: Option<u64>,
    /// 1 s doubling to 1 h when absent.
    pub(crate) backoff: Option<JobsBackoff>,
    /// How long one run may take: a minute when absent. Milliseconds.
    pub(crate) timeout: Option<u64>,
    /// One at a time in each worker when absent.
    pub(crate) concurrency: Option<JobsConcurrency>,
    pub(crate) rate: Option<JobsRate>,
    /// How long a done job's id stays taken. Milliseconds.
    pub(crate) dedupe: Option<u64>,
    /// How long a failed job stays: a week when absent. Milliseconds.
    pub(crate) keep: Option<u64>,
    /// Ten million when absent.
    pub(crate) max_waiting: Option<u64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsQueueOpen {
    const NAME: &'static str = "jobs.QueueOpen";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5, 6, 7, 8, 9];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsQueueOpen {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            attempts: fields.get(2, "attempts", codec::uint)?,
            backoff: fields.get(3, "backoff", codec::message::<JobsBackoff>)?,
            timeout: fields.get(4, "timeout", codec::uint)?,
            concurrency: fields.get(5, "concurrency", codec::message::<JobsConcurrency>)?,
            rate: fields.get(6, "rate", codec::message::<JobsRate>)?,
            dedupe: fields.get(7, "dedupe", codec::uint)?,
            keep: fields.get(8, "keep", codec::uint)?,
            max_waiting: fields.get(9, "maxWaiting", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.given(2, self.attempts.as_ref().map(codec::uint_value));
        out.given(3, self.backoff.as_ref().map(codec::message_value));
        out.given(4, self.timeout.as_ref().map(codec::uint_value));
        out.given(5, self.concurrency.as_ref().map(codec::message_value));
        out.given(6, self.rate.as_ref().map(codec::message_value));
        out.given(7, self.dedupe.as_ref().map(codec::uint_value));
        out.given(8, self.keep.as_ref().map(codec::uint_value));
        out.given(9, self.max_waiting.as_ref().map(codec::uint_value));
    }
}

/// Opens a schedule: one repeating job the code owns, under its name, whose
/// repeat replaces the one kept.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsScheduleOpen {
    pub(crate) name: String,
    /// Milliseconds.
    pub(crate) every: Option<u64>,
    /// Five fields, with timeZone.
    pub(crate) cron: Option<String>,
    /// An IANA name, UTC among them.
    pub(crate) time_zone: Option<String>,
    pub(crate) attempts: Option<u64>,
    pub(crate) backoff: Option<JobsBackoff>,
    /// Milliseconds.
    pub(crate) timeout: Option<u64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsScheduleOpen {
    const NAME: &'static str = "jobs.ScheduleOpen";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5, 6, 7];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsScheduleOpen {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            every: fields.get(2, "every", codec::uint)?,
            cron: fields.get(3, "cron", codec::str)?,
            time_zone: fields.get(4, "timeZone", codec::str)?,
            attempts: fields.get(5, "attempts", codec::uint)?,
            backoff: fields.get(6, "backoff", codec::message::<JobsBackoff>)?,
            timeout: fields.get(7, "timeout", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.given(2, self.every.as_ref().map(codec::uint_value));
        out.given(3, self.cron.as_ref().map(codec::str_value));
        out.given(4, self.time_zone.as_ref().map(codec::str_value));
        out.given(5, self.attempts.as_ref().map(codec::uint_value));
        out.given(6, self.backoff.as_ref().map(codec::message_value));
        out.given(7, self.timeout.as_ref().map(codec::uint_value));
    }
}

/// A call on one job: an add, a set or an update.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsCall {
    pub(crate) handle: u64,
    /// 1 to 1024 bytes; a set and an update need one.
    pub(crate) id: Option<String>,
    pub(crate) value: String,
    /// Unix milliseconds.
    pub(crate) at: Option<i64>,
    /// From now; not beside at. Milliseconds.
    pub(crate) delay: Option<u64>,
    pub(crate) group: Option<String>,
    /// A repeat, which needs an id. Milliseconds.
    pub(crate) every: Option<u64>,
    pub(crate) cron: Option<String>,
    pub(crate) time_zone: Option<String>,
}

#[cfg(feature = "jobs")]
impl Message for JobsCall {
    const NAME: &'static str = "jobs.Call";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5, 6, 7, 8, 9];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsCall {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            id: fields.get(2, "id", codec::str)?,
            value: fields.get(3, "value", codec::str)?.unwrap_or_default(),
            at: fields.get(4, "at", codec::int)?,
            delay: fields.get(5, "delay", codec::uint)?,
            group: fields.get(6, "group", codec::str)?,
            every: fields.get(7, "every", codec::uint)?,
            cron: fields.get(8, "cron", codec::str)?,
            time_zone: fields.get(9, "timeZone", codec::str)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.given(2, self.id.as_ref().map(codec::str_value));
        out.put(3, codec::str_value(&self.value));
        out.given(4, self.at.as_ref().map(codec::int_value));
        out.given(5, self.delay.as_ref().map(codec::uint_value));
        out.given(6, self.group.as_ref().map(codec::str_value));
        out.given(7, self.every.as_ref().map(codec::uint_value));
        out.given(8, self.cron.as_ref().map(codec::str_value));
        out.given(9, self.time_zone.as_ref().map(codec::str_value));
    }
}

/// The job under an id.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsId {
    pub(crate) handle: u64,
    pub(crate) id: String,
}

#[cfg(feature = "jobs")]
impl Message for JobsId {
    const NAME: &'static str = "jobs.Id";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsId {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            id: fields.get(2, "id", codec::str)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.put(2, codec::str_value(&self.id));
    }
}

/// Whether a call did what it asked: an add added, an update changed, a cancel
/// found a job.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsChanged {
    pub(crate) changed: bool,
}

#[cfg(feature = "jobs")]
impl Message for JobsChanged {
    const NAME: &'static str = "jobs.Changed";
    const KEYS: &'static [u64] = &[1];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsChanged {
            changed: fields.get(1, "changed", codec::bool)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::bool_value(&self.changed));
    }
}

/// A job as its queue holds it.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsJob {
    pub(crate) found: bool,
    pub(crate) id: Option<String>,
    pub(crate) value: Option<String>,
    /// Scheduled, waiting, running, done, failed or cancelled.
    pub(crate) state: String,
    /// When it runs next; for one done or failed, when its last run was for. Unix milliseconds.
    pub(crate) at: Option<i64>,
    pub(crate) attempt: u64,
    /// The jobs that run before a waiting one, up to 10,000.
    pub(crate) ahead: u64,
    pub(crate) progress: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) group: Option<String>,
    /// As the job keeps it: @every 30s +6178ms, 10 3 * * * Europe/Berlin.
    pub(crate) repeat: Option<String>,
    /// The last run a handler finished. Unix milliseconds.
    pub(crate) started_at: Option<i64>,
    /// Unix milliseconds.
    pub(crate) ended_at: Option<i64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsJob {
    const NAME: &'static str = "jobs.Job";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsJob {
            found: fields.get(1, "found", codec::bool)?.unwrap_or_default(),
            id: fields.get(2, "id", codec::str)?,
            value: fields.get(3, "value", codec::str)?,
            state: fields.get(4, "state", codec::str)?.unwrap_or_default(),
            at: fields.get(5, "at", codec::int)?,
            attempt: fields.get(6, "attempt", codec::uint)?.unwrap_or_default(),
            ahead: fields.get(7, "ahead", codec::uint)?.unwrap_or_default(),
            progress: fields.get(8, "progress", codec::str)?,
            error: fields.get(9, "error", codec::str)?,
            group: fields.get(10, "group", codec::str)?,
            repeat: fields.get(11, "repeat", codec::str)?,
            started_at: fields.get(12, "startedAt", codec::int)?,
            ended_at: fields.get(13, "endedAt", codec::int)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::bool_value(&self.found));
        out.given(2, self.id.as_ref().map(codec::str_value));
        out.given(3, self.value.as_ref().map(codec::str_value));
        out.put(4, codec::str_value(&self.state));
        out.given(5, self.at.as_ref().map(codec::int_value));
        out.put(6, codec::uint_value(&self.attempt));
        out.put(7, codec::uint_value(&self.ahead));
        out.given(8, self.progress.as_ref().map(codec::str_value));
        out.given(9, self.error.as_ref().map(codec::str_value));
        out.given(10, self.group.as_ref().map(codec::str_value));
        out.given(11, self.repeat.as_ref().map(codec::str_value));
        out.given(12, self.started_at.as_ref().map(codec::int_value));
        out.given(13, self.ended_at.as_ref().map(codec::int_value));
    }
}

/// A page of a queue's jobs: those whose ids start with a prefix, in the byte
/// order of their ids, or with no prefix and the failed state, the last failed
/// first.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsList {
    pub(crate) handle: u64,
    pub(crate) prefix: Option<String>,
    /// Scheduled, waiting, running or failed.
    pub(crate) state: Option<String>,
    /// The page before's next.
    pub(crate) after: Option<String>,
    /// 100 when absent, 1000 at most.
    pub(crate) limit: Option<u64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsList {
    const NAME: &'static str = "jobs.List";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsList {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            prefix: fields.get(2, "prefix", codec::str)?,
            state: fields.get(3, "state", codec::str)?,
            after: fields.get(4, "after", codec::str)?,
            limit: fields.get(5, "limit", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.given(2, self.prefix.as_ref().map(codec::str_value));
        out.given(3, self.state.as_ref().map(codec::str_value));
        out.given(4, self.after.as_ref().map(codec::str_value));
        out.given(5, self.limit.as_ref().map(codec::uint_value));
    }
}

#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsPage {
    pub(crate) jobs: Vec<JobsJob>,
    pub(crate) next: Option<String>,
}

#[cfg(feature = "jobs")]
impl Message for JobsPage {
    const NAME: &'static str = "jobs.Page";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsPage {
            jobs: fields.get(1, "jobs", codec::list(codec::message::<JobsJob>))?.unwrap_or_default(),
            next: fields.get(2, "next", codec::str)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::list_value(&self.jobs, codec::message_value));
        out.given(2, self.next.as_ref().map(codec::str_value));
    }
}

/// Starts the queue's worker for the client: the server claims its jobs as they
/// fall due and hands each over as a held job, no more at once than the
/// client's handlers, and the client answers each. A stop, or the server's
/// GOAWAY, hands no job more, and the server ends the stream with DATA·END once
/// the jobs the client holds are answered and written. The client's DATA·END
/// ends the worker at once, the attempt of a job it still holds failing.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsWork {
    pub(crate) handle: u64,
    /// The handlers the client runs at once, 1024 at most: the queue's total, or one.
    pub(crate) concurrency: Option<u64>,
    /// RunDue: the server ends the stream once no job is due and none is held.
    pub(crate) until_idle: bool,
}

#[cfg(feature = "jobs")]
impl Message for JobsWork {
    const NAME: &'static str = "jobs.Work";
    const KEYS: &'static [u64] = &[1, 2, 3];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsWork {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            concurrency: fields.get(2, "concurrency", codec::uint)?,
            until_idle: fields.get(3, "untilIdle", codec::bool)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.given(2, self.concurrency.as_ref().map(codec::uint_value));
        out.put(3, codec::bool_value(&self.until_idle));
    }
}

/// A job the server hands the client's worker. Its run's number names it to the
/// answer, a step and a keep; a cancel sends it again, cancelled, so that its
/// handler stops.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsHeld {
    pub(crate) run: u64,
    pub(crate) id: Option<String>,
    pub(crate) value: Option<String>,
    /// When the run was due. Unix milliseconds.
    pub(crate) at: i64,
    /// The first being 1.
    pub(crate) attempt: u64,
    pub(crate) group: Option<String>,
    pub(crate) cancelled: bool,
}

#[cfg(feature = "jobs")]
impl Message for JobsHeld {
    const NAME: &'static str = "jobs.Held";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5, 6, 7];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsHeld {
            run: fields.get(1, "run", codec::uint)?.unwrap_or_default(),
            id: fields.get(2, "id", codec::str)?,
            value: fields.get(3, "value", codec::str)?,
            at: fields.get(4, "at", codec::int)?.unwrap_or_default(),
            attempt: fields.get(5, "attempt", codec::uint)?.unwrap_or_default(),
            group: fields.get(6, "group", codec::str)?,
            cancelled: fields.get(7, "cancelled", codec::bool)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.run));
        out.given(2, self.id.as_ref().map(codec::str_value));
        out.given(3, self.value.as_ref().map(codec::str_value));
        out.put(4, codec::int_value(&self.at));
        out.put(5, codec::uint_value(&self.attempt));
        out.given(6, self.group.as_ref().map(codec::str_value));
        out.put(7, codec::bool_value(&self.cancelled));
    }
}

/// The client's answer for a held job: how its run ended, or how far it got,
/// which settles nothing; or a stop, which names no run. An answer for a job a
/// cancel took settles nothing.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsAnswer {
    pub(crate) run: u64,
    /// Done, retry, snooze, fail, back, progress or stop.
    pub(crate) how: String,
    /// When a retry or a snooze runs again; a retry without one waits its backoff. Unix milliseconds.
    pub(crate) at: Option<i64>,
    /// Why a retry or a failure.
    pub(crate) error: Option<String>,
    /// 4 KiB at most.
    pub(crate) progress: Option<String>,
    /// From now on the server's clock; not beside at. Milliseconds.
    pub(crate) delay: Option<u64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsAnswer {
    const NAME: &'static str = "jobs.Answer";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5, 6];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsAnswer {
            run: fields.get(1, "run", codec::uint)?.unwrap_or_default(),
            how: fields.get(2, "how", codec::str)?.unwrap_or_default(),
            at: fields.get(3, "at", codec::int)?,
            error: fields.get(4, "error", codec::str)?,
            progress: fields.get(5, "progress", codec::str)?,
            delay: fields.get(6, "delay", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.run));
        out.put(2, codec::str_value(&self.how));
        out.given(3, self.at.as_ref().map(codec::int_value));
        out.given(4, self.error.as_ref().map(codec::str_value));
        out.given(5, self.progress.as_ref().map(codec::str_value));
        out.given(6, self.delay.as_ref().map(codec::uint_value));
    }
}

/// A step of a held job's run: jobs.step asks for its kept answer, jobs.keep
/// keeps one.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsStep {
    pub(crate) run: u64,
    /// 1 to 256 bytes.
    pub(crate) name: String,
    /// Jobs.keep's: 1 MiB at most.
    pub(crate) answer: Option<String>,
}

#[cfg(feature = "jobs")]
impl Message for JobsStep {
    const NAME: &'static str = "jobs.Step";
    const KEYS: &'static [u64] = &[1, 2, 3];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsStep {
            run: fields.get(1, "run", codec::uint)?.unwrap_or_default(),
            name: fields.get(2, "name", codec::str)?.unwrap_or_default(),
            answer: fields.get(3, "answer", codec::str)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.run));
        out.put(2, codec::str_value(&self.name));
        out.given(3, self.answer.as_ref().map(codec::str_value));
    }
}

#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsKept {
    pub(crate) found: bool,
    pub(crate) answer: Option<String>,
}

#[cfg(feature = "jobs")]
impl Message for JobsKept {
    const NAME: &'static str = "jobs.Kept";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(JobsKept {
            found: fields.get(1, "found", codec::bool)?.unwrap_or_default(),
            answer: fields.get(2, "answer", codec::str)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::bool_value(&self.found));
        out.given(2, self.answer.as_ref().map(codec::str_value));
    }
}

/// Opens a bucket of values by key. Its keys expire ttl after they are written,
/// or idle after they were last read or written; not both.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvBucketOpen {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    /// Milliseconds.
    pub(crate) ttl: Option<u64>,
    /// Milliseconds.
    pub(crate) idle: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvBucketOpen {
    const NAME: &'static str = "kv.BucketOpen";
    const KEYS: &'static [u64] = &[1, 2, 3];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvBucketOpen {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            ttl: fields.get(2, "ttl", codec::uint)?,
            idle: fields.get(3, "idle", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.given(2, self.ttl.as_ref().map(codec::uint_value));
        out.given(3, self.idle.as_ref().map(codec::uint_value));
    }
}

/// A call on one key of a handle's branch.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvCall {
    pub(crate) handle: u64,
    /// The branch's owners, outermost first; the bucket's root when empty.
    pub(crate) under: Vec<String>,
    pub(crate) key: String,
    /// What a set or a create writes.
    pub(crate) value: Option<Row>,
    /// The expiry a write gives, from now. Milliseconds.
    pub(crate) ttl: Option<u64>,
    /// The expiry a write gives, as a time. Unix milliseconds.
    pub(crate) expires_at: Option<i64>,
    /// The version the key must still be at.
    pub(crate) if_version: Option<Vec<u8>>,
    /// What a counter adds; the requests or uses a limit is asked for, 1 when absent.
    pub(crate) n: Option<i64>,
}

#[cfg(feature = "kv")]
impl Message for KvCall {
    const NAME: &'static str = "kv.Call";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5, 6, 7, 8];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvCall {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            under: fields.get(2, "under", codec::list(codec::key))?.unwrap_or_default(),
            key: fields.get(3, "key", codec::key)?.unwrap_or_default(),
            value: fields.get(4, "value", codec::row)?,
            ttl: fields.get(5, "ttl", codec::uint)?,
            expires_at: fields.get(6, "expiresAt", codec::int)?,
            if_version: fields.get(7, "ifVersion", codec::bin)?,
            n: fields.get(8, "n", codec::int)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.put(2, codec::list_value(&self.under, codec::str_value));
        out.put(3, codec::str_value(&self.key));
        out.given(4, self.value.as_ref().map(codec::row_value));
        out.given(5, self.ttl.as_ref().map(codec::uint_value));
        out.given(6, self.expires_at.as_ref().map(codec::int_value));
        out.given(7, self.if_version.as_ref().map(codec::bin_value));
        out.given(8, self.n.as_ref().map(codec::int_value));
    }
}

/// A key's value with what a conditional write needs.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvEntry {
    pub(crate) found: bool,
    pub(crate) value: Option<Row>,
    /// Compared only for equality.
    pub(crate) version: Option<Vec<u8>>,
    /// Absent for a key that never expires. Unix milliseconds.
    pub(crate) expires_at: Option<i64>,
    /// A page's entry alone.
    pub(crate) key: Option<String>,
}

#[cfg(feature = "kv")]
impl Message for KvEntry {
    const NAME: &'static str = "kv.Entry";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvEntry {
            found: fields.get(1, "found", codec::bool)?.unwrap_or_default(),
            value: fields.get(2, "value", codec::row)?,
            version: fields.get(3, "version", codec::bin)?,
            expires_at: fields.get(4, "expiresAt", codec::int)?,
            key: fields.get(5, "key", codec::key)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::bool_value(&self.found));
        out.given(2, self.value.as_ref().map(codec::row_value));
        out.given(3, self.version.as_ref().map(codec::bin_value));
        out.given(4, self.expires_at.as_ref().map(codec::int_value));
        out.given(5, self.key.as_ref().map(codec::str_value));
    }
}

/// What a write left: whether it wrote, and the version and expiry the key has
/// now, the live key's own when a create found one.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvWritten {
    pub(crate) written: bool,
    pub(crate) version: Vec<u8>,
    /// Unix milliseconds.
    pub(crate) expires_at: Option<i64>,
}

#[cfg(feature = "kv")]
impl Message for KvWritten {
    const NAME: &'static str = "kv.Written";
    const KEYS: &'static [u64] = &[1, 2, 3];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvWritten {
            written: fields.get(1, "written", codec::bool)?.unwrap_or_default(),
            version: fields.get(2, "version", codec::bin)?.unwrap_or_default(),
            expires_at: fields.get(3, "expiresAt", codec::int)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::bool_value(&self.written));
        out.put(2, codec::bin_value(&self.version));
        out.given(3, self.expires_at.as_ref().map(codec::int_value));
    }
}

#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvFound {
    pub(crate) found: bool,
}

#[cfg(feature = "kv")]
impl Message for KvFound {
    const NAME: &'static str = "kv.Found";
    const KEYS: &'static [u64] = &[1];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvFound {
            found: fields.get(1, "found", codec::bool)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::bool_value(&self.found));
    }
}

/// A branch of a handle, for a clear.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvBranch {
    pub(crate) handle: u64,
    pub(crate) under: Vec<String>,
}

#[cfg(feature = "kv")]
impl Message for KvBranch {
    const NAME: &'static str = "kv.Branch";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvBranch {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            under: fields.get(2, "under", codec::list(codec::key))?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.put(2, codec::list_value(&self.under, codec::str_value));
    }
}

/// A page of a branch's own keys, in the byte order of their text.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvList {
    pub(crate) handle: u64,
    pub(crate) under: Vec<String>,
    /// The key the page starts after.
    pub(crate) after: Option<String>,
    /// 100 when absent, 1000 at most.
    pub(crate) limit: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvList {
    const NAME: &'static str = "kv.List";
    const KEYS: &'static [u64] = &[1, 2, 3, 4];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvList {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            under: fields.get(2, "under", codec::list(codec::key))?.unwrap_or_default(),
            after: fields.get(3, "after", codec::key)?,
            limit: fields.get(4, "limit", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.put(2, codec::list_value(&self.under, codec::str_value));
        out.given(3, self.after.as_ref().map(codec::str_value));
        out.given(4, self.limit.as_ref().map(codec::uint_value));
    }
}

/// A page of entries, as many as the limit asks and the body holds, and where
/// the next page starts, when there is one.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvPage {
    pub(crate) entries: Vec<KvEntry>,
    pub(crate) next: Option<String>,
}

#[cfg(feature = "kv")]
impl Message for KvPage {
    const NAME: &'static str = "kv.Page";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvPage {
            entries: fields.get(1, "entries", codec::list(codec::message::<KvEntry>))?.unwrap_or_default(),
            next: fields.get(2, "next", codec::key)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::list_value(&self.entries, codec::message_value));
        out.given(2, self.next.as_ref().map(codec::str_value));
    }
}

/// Opens counters: numbers by key that only add up.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvCountersOpen {
    pub(crate) name: String,
    /// A counter lasts this long from its first add. Milliseconds.
    pub(crate) ttl: Option<u64>,
    /// Kept in memory and written every span; each add written when absent. Milliseconds.
    pub(crate) flush_every: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvCountersOpen {
    const NAME: &'static str = "kv.CountersOpen";
    const KEYS: &'static [u64] = &[1, 2, 3];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvCountersOpen {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            ttl: fields.get(2, "ttl", codec::uint)?,
            flush_every: fields.get(3, "flushEvery", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.given(2, self.ttl.as_ref().map(codec::uint_value));
        out.given(3, self.flush_every.as_ref().map(codec::uint_value));
    }
}

#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvCount {
    pub(crate) value: i64,
}

#[cfg(feature = "kv")]
impl Message for KvCount {
    const NAME: &'static str = "kv.Count";
    const KEYS: &'static [u64] = &[1];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvCount {
            value: fields.get(1, "value", codec::int)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::int_value(&self.value));
    }
}

/// Opens a rate limit: rate requests a key every per, burst at once.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvRateLimitOpen {
    pub(crate) name: String,
    pub(crate) rate: u64,
    /// Milliseconds.
    pub(crate) per: u64,
    /// The rate when absent.
    pub(crate) burst: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvRateLimitOpen {
    const NAME: &'static str = "kv.RateLimitOpen";
    const KEYS: &'static [u64] = &[1, 2, 3, 4];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvRateLimitOpen {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            rate: fields.get(2, "rate", codec::uint)?.unwrap_or_default(),
            per: fields.get(3, "per", codec::uint)?.unwrap_or_default(),
            burst: fields.get(4, "burst", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.put(2, codec::uint_value(&self.rate));
        out.put(3, codec::uint_value(&self.per));
        out.given(4, self.burst.as_ref().map(codec::uint_value));
    }
}

/// One window of a quota: a key may use up to limit every per from its first use.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvWindow {
    /// [a-z][a-z0-9_]{0,31}.
    pub(crate) name: String,
    pub(crate) limit: u64,
    /// Milliseconds.
    pub(crate) per: u64,
}

#[cfg(feature = "kv")]
impl Message for KvWindow {
    const NAME: &'static str = "kv.Window";
    const KEYS: &'static [u64] = &[1, 2, 3];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvWindow {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            limit: fields.get(2, "limit", codec::uint)?.unwrap_or_default(),
            per: fields.get(3, "per", codec::uint)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.put(2, codec::uint_value(&self.limit));
        out.put(3, codec::uint_value(&self.per));
    }
}

#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvQuotaOpen {
    pub(crate) name: String,
    /// One to eight.
    pub(crate) windows: Vec<KvWindow>,
}

#[cfg(feature = "kv")]
impl Message for KvQuotaOpen {
    const NAME: &'static str = "kv.QuotaOpen";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvQuotaOpen {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            windows: fields.get(2, "windows", codec::list(codec::message::<KvWindow>))?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.put(2, codec::list_value(&self.windows, codec::message_value));
    }
}

/// One window of a quota as a key stands in it.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvWindowUse {
    pub(crate) name: String,
    pub(crate) used: u64,
    pub(crate) limit: u64,
    pub(crate) left: u64,
    /// Absent for a window not started. Unix milliseconds.
    pub(crate) resets_at: Option<i64>,
}

#[cfg(feature = "kv")]
impl Message for KvWindowUse {
    const NAME: &'static str = "kv.WindowUse";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvWindowUse {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            used: fields.get(2, "used", codec::uint)?.unwrap_or_default(),
            limit: fields.get(3, "limit", codec::uint)?.unwrap_or_default(),
            left: fields.get(4, "left", codec::uint)?.unwrap_or_default(),
            resets_at: fields.get(5, "resetsAt", codec::int)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.put(2, codec::uint_value(&self.used));
        out.put(3, codec::uint_value(&self.limit));
        out.put(4, codec::uint_value(&self.left));
        out.given(5, self.resets_at.as_ref().map(codec::int_value));
    }
}

/// What a rate limit or a quota answers a request.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvAllowance {
    pub(crate) ok: bool,
    /// How many more would pass now.
    pub(crate) left: u64,
    /// When the request would pass; absent when it did. Unix milliseconds.
    pub(crate) retry_at: Option<i64>,
    /// A quota's, in the order it names them.
    pub(crate) windows: Vec<KvWindowUse>,
}

#[cfg(feature = "kv")]
impl Message for KvAllowance {
    const NAME: &'static str = "kv.Allowance";
    const KEYS: &'static [u64] = &[1, 2, 3, 4];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvAllowance {
            ok: fields.get(1, "ok", codec::bool)?.unwrap_or_default(),
            left: fields.get(2, "left", codec::uint)?.unwrap_or_default(),
            retry_at: fields.get(3, "retryAt", codec::int)?,
            windows: fields.get(4, "windows", codec::list(codec::message::<KvWindowUse>))?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::bool_value(&self.ok));
        out.put(2, codec::uint_value(&self.left));
        out.given(3, self.retry_at.as_ref().map(codec::int_value));
        out.put(4, codec::list_value(&self.windows, codec::message_value));
    }
}

/// Opens once's answers: a function run once a key, its answer kept.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvOnceOpen {
    pub(crate) name: String,
    /// A day when absent. Milliseconds.
    pub(crate) keep: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvOnceOpen {
    const NAME: &'static str = "kv.OnceOpen";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvOnceOpen {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            keep: fields.get(2, "keep", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.given(2, self.keep.as_ref().map(codec::uint_value));
    }
}

/// An answer of once: in the server's RESPONSE, found when one was kept, and
/// otherwise the run is the client's; in the client's last DATA, found with the
/// answer to keep, or not found when the function failed.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvAnswer {
    pub(crate) found: bool,
    pub(crate) value: Option<Row>,
}

#[cfg(feature = "kv")]
impl Message for KvAnswer {
    const NAME: &'static str = "kv.Answer";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvAnswer {
            found: fields.get(1, "found", codec::bool)?.unwrap_or_default(),
            value: fields.get(2, "value", codec::row)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::bool_value(&self.found));
        out.given(2, self.value.as_ref().map(codec::row_value));
    }
}

/// A read a transaction made, which its commit checks: the key still at the
/// version it was found at, or still absent.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvCheck {
    pub(crate) handle: u64,
    pub(crate) under: Vec<String>,
    pub(crate) key: String,
    /// Absent: the read found nothing.
    pub(crate) version: Option<Vec<u8>>,
}

#[cfg(feature = "kv")]
impl Message for KvCheck {
    const NAME: &'static str = "kv.Check";
    const KEYS: &'static [u64] = &[1, 2, 3, 4];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvCheck {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            under: fields.get(2, "under", codec::list(codec::key))?.unwrap_or_default(),
            key: fields.get(3, "key", codec::key)?.unwrap_or_default(),
            version: fields.get(4, "version", codec::bin)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.put(2, codec::list_value(&self.under, codec::str_value));
        out.put(3, codec::str_value(&self.key));
        out.given(4, self.version.as_ref().map(codec::bin_value));
    }
}

/// One write of a transaction: the method it is, and its call.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvOp {
    /// Kv.set, kv.create, kv.take, kv.delete, kv.expire, kv.clear or kv.counters.add.
    pub(crate) method: u64,
    pub(crate) call: KvCall,
}

#[cfg(feature = "kv")]
impl Message for KvOp {
    const NAME: &'static str = "kv.Op";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvOp {
            method: fields.get(1, "method", codec::uint)?.unwrap_or_default(),
            call: fields.get(2, "call", codec::message::<KvCall>)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.method));
        out.put(2, codec::message_value(&self.call));
    }
}

/// A transaction across the wire: its reads' checks, then its writes, all
/// applied or none. A failed check or write names its place in what.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvTx {
    pub(crate) checks: Vec<KvCheck>,
    pub(crate) writes: Vec<KvOp>,
}

#[cfg(feature = "kv")]
impl Message for KvTx {
    const NAME: &'static str = "kv.Tx";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvTx {
            checks: fields.get(1, "checks", codec::list(codec::message::<KvCheck>))?.unwrap_or_default(),
            writes: fields.get(2, "writes", codec::list(codec::message::<KvOp>))?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::list_value(&self.checks, codec::message_value));
        out.put(2, codec::list_value(&self.writes, codec::message_value));
    }
}

/// What one write of a transaction did.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvOutcome {
    /// A take, a delete or an expire found a live key.
    pub(crate) found: bool,
    /// A set or a create wrote.
    pub(crate) written: bool,
    /// What a take took.
    pub(crate) value: Option<Row>,
    pub(crate) version: Option<Vec<u8>>,
    /// Unix milliseconds.
    pub(crate) expires_at: Option<i64>,
    /// A counter's value after its add.
    pub(crate) count: Option<i64>,
}

#[cfg(feature = "kv")]
impl Message for KvOutcome {
    const NAME: &'static str = "kv.Outcome";
    const KEYS: &'static [u64] = &[1, 2, 3, 4, 5, 6];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvOutcome {
            found: fields.get(1, "found", codec::bool)?.unwrap_or_default(),
            written: fields.get(2, "written", codec::bool)?.unwrap_or_default(),
            value: fields.get(3, "value", codec::row)?,
            version: fields.get(4, "version", codec::bin)?,
            expires_at: fields.get(5, "expiresAt", codec::int)?,
            count: fields.get(6, "count", codec::int)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::bool_value(&self.found));
        out.put(2, codec::bool_value(&self.written));
        out.given(3, self.value.as_ref().map(codec::row_value));
        out.given(4, self.version.as_ref().map(codec::bin_value));
        out.given(5, self.expires_at.as_ref().map(codec::int_value));
        out.given(6, self.count.as_ref().map(codec::int_value));
    }
}

#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvTxResults {
    pub(crate) outcomes: Vec<KvOutcome>,
}

#[cfg(feature = "kv")]
impl Message for KvTxResults {
    const NAME: &'static str = "kv.TxResults";
    const KEYS: &'static [u64] = &[1];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvTxResults {
            outcomes: fields.get(1, "outcomes", codec::list(codec::message::<KvOutcome>))?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::list_value(&self.outcomes, codec::message_value));
    }
}

/// A private server's clock: a time to set it to, a span to move it forward by,
/// or neither to read it. The answer is the time it reads once moved.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ServerClock {
    /// Unix milliseconds.
    pub(crate) at: Option<i64>,
    /// Milliseconds.
    pub(crate) advance: Option<u64>,
}

impl Message for ServerClock {
    const NAME: &'static str = "server.Clock";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(ServerClock {
            at: fields.get(1, "at", codec::int)?,
            advance: fields.get(2, "advance", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.given(1, self.at.as_ref().map(codec::int_value));
        out.given(2, self.advance.as_ref().map(codec::uint_value));
    }
}

/// A migration as its file has it: the name that numbers it, and its SQL.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlMigration {
    /// 0001_notes.sql.
    pub(crate) name: String,
    pub(crate) sql: String,
}

#[cfg(feature = "sql")]
impl Message for SqlMigration {
    const NAME: &'static str = "sql.Migration";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlMigration {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            sql: fields.get(2, "sql", codec::str)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.put(2, codec::str_value(&self.sql));
    }
}

/// Opens a database. Migrations, when given, are applied and checked; without
/// them the file opens as it is.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlOpen {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    pub(crate) migrations: Option<Vec<SqlMigration>>,
}

#[cfg(feature = "sql")]
impl Message for SqlOpen {
    const NAME: &'static str = "sql.Open";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlOpen {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            migrations: fields.get(2, "migrations", codec::list(codec::message::<SqlMigration>))?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.given(2, self.migrations.as_ref().map(|items| codec::list_value(items, codec::message_value)));
    }
}

/// A statement's text and the values of its ?s, in order.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlText {
    pub(crate) text: String,
    pub(crate) values: Vec<Cell>,
}

#[cfg(feature = "sql")]
impl Message for SqlText {
    const NAME: &'static str = "sql.Text";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlText {
            text: fields.get(1, "text", codec::str)?.unwrap_or_default(),
            values: fields.get(2, "values", codec::list(codec::cell))?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.text));
        out.put(2, codec::list_value(&self.values, codec::cell_value));
    }
}

/// A write on a database.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlStatement {
    pub(crate) handle: u64,
    pub(crate) text: String,
    pub(crate) values: Vec<Cell>,
}

#[cfg(feature = "sql")]
impl Message for SqlStatement {
    const NAME: &'static str = "sql.Statement";
    const KEYS: &'static [u64] = &[1, 2, 3];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlStatement {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            text: fields.get(2, "text", codec::str)?.unwrap_or_default(),
            values: fields.get(3, "values", codec::list(codec::cell))?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.put(2, codec::str_value(&self.text));
        out.put(3, codec::list_value(&self.values, codec::cell_value));
    }
}

/// What a statement gives back: its rows, one row or none, or one value. A
/// write with returning runs on the writer, its rows given once it is durable.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlQuery {
    pub(crate) handle: u64,
    pub(crate) text: String,
    pub(crate) values: Vec<Cell>,
    /// All, one or scalar.
    pub(crate) want: String,
}

#[cfg(feature = "sql")]
impl Message for SqlQuery {
    const NAME: &'static str = "sql.Query";
    const KEYS: &'static [u64] = &[1, 2, 3, 4];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlQuery {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            text: fields.get(2, "text", codec::str)?.unwrap_or_default(),
            values: fields.get(3, "values", codec::list(codec::cell))?.unwrap_or_default(),
            want: fields.get(4, "want", codec::str)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.put(2, codec::str_value(&self.text));
        out.put(3, codec::list_value(&self.values, codec::cell_value));
        out.put(4, codec::str_value(&self.want));
    }
}

/// Rows a statement gave, a part of them a message: the first part names the
/// columns, and every row holds a value a column, in their order.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlRows {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<Cell>>,
}

#[cfg(feature = "sql")]
impl Message for SqlRows {
    const NAME: &'static str = "sql.Rows";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlRows {
            columns: fields.get(1, "columns", codec::list(codec::str))?.unwrap_or_default(),
            rows: fields.get(2, "rows", codec::list(codec::list(codec::cell)))?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::list_value(&self.columns, codec::str_value));
        out.put(2, codec::list_value(&self.rows, |items| codec::list_value(items, codec::cell_value)));
    }
}

/// What a write changed: the rows, and SQLite's rowid of the row it inserted,
/// 0 when it inserted none.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlDone {
    pub(crate) changes: u64,
    pub(crate) last_insert_rowid: i64,
}

#[cfg(feature = "sql")]
impl Message for SqlDone {
    const NAME: &'static str = "sql.Done";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlDone {
            changes: fields.get(1, "changes", codec::uint)?.unwrap_or_default(),
            last_insert_rowid: fields.get(2, "lastInsertRowid", codec::int)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.changes));
        out.put(2, codec::int_value(&self.last_insert_rowid));
    }
}

/// Statements known before they run, written as one in a shared commit.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlBatch {
    pub(crate) handle: u64,
    pub(crate) statements: Vec<SqlText>,
}

#[cfg(feature = "sql")]
impl Message for SqlBatch {
    const NAME: &'static str = "sql.Batch";
    const KEYS: &'static [u64] = &[1, 2];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlBatch {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
            statements: fields.get(2, "statements", codec::list(codec::message::<SqlText>))?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
        out.put(2, codec::list_value(&self.statements, codec::message_value));
    }
}

#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlBatched {
    pub(crate) done: Vec<SqlDone>,
}

#[cfg(feature = "sql")]
impl Message for SqlBatched {
    const NAME: &'static str = "sql.Batched";
    const KEYS: &'static [u64] = &[1];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlBatched {
            done: fields.get(1, "done", codec::list(codec::message::<SqlDone>))?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::list_value(&self.done, codec::message_value));
    }
}

/// Opens a transaction: it holds the database's writer until the client's last
/// DATA, five seconds at most.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlTxOpen {
    pub(crate) handle: u64,
}

#[cfg(feature = "sql")]
impl Message for SqlTxOpen {
    const NAME: &'static str = "sql.TxOpen";
    const KEYS: &'static [u64] = &[1];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlTxOpen {
            handle: fields.get(1, "handle", codec::uint)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::uint_value(&self.handle));
    }
}

/// A transaction's call, or its end: the last DATA commits, or not.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlTxCall {
    pub(crate) text: String,
    pub(crate) values: Vec<Cell>,
    /// All, one, scalar or exec; nothing in the last.
    pub(crate) want: String,
    /// In the last: true commits, false rolls back.
    pub(crate) commit: bool,
}

#[cfg(feature = "sql")]
impl Message for SqlTxCall {
    const NAME: &'static str = "sql.TxCall";
    const KEYS: &'static [u64] = &[1, 2, 3, 4];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlTxCall {
            text: fields.get(1, "text", codec::str)?.unwrap_or_default(),
            values: fields.get(2, "values", codec::list(codec::cell))?.unwrap_or_default(),
            want: fields.get(3, "want", codec::str)?.unwrap_or_default(),
            commit: fields.get(4, "commit", codec::bool)?.unwrap_or_default(),
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.text));
        out.put(2, codec::list_value(&self.values, codec::cell_value));
        out.put(3, codec::str_value(&self.want));
        out.put(4, codec::bool_value(&self.commit));
    }
}

/// A call's answer: its rows, what it changed, or why it failed, which leaves
/// the transaction as it was before the call.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlTxAnswer {
    pub(crate) rows: Option<SqlRows>,
    pub(crate) done: Option<SqlDone>,
    pub(crate) failure: Option<Failure>,
}

#[cfg(feature = "sql")]
impl Message for SqlTxAnswer {
    const NAME: &'static str = "sql.TxAnswer";
    const KEYS: &'static [u64] = &[1, 2, 3];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(SqlTxAnswer {
            rows: fields.get(1, "rows", codec::message::<SqlRows>)?,
            done: fields.get(2, "done", codec::message::<SqlDone>)?,
            failure: fields.get(3, "failure", codec::message::<Failure>)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.given(1, self.rows.as_ref().map(codec::message_value));
        out.given(2, self.done.as_ref().map(codec::message_value));
        out.given(3, self.failure.as_ref().map(codec::message_value));
    }
}

/// Every method's number, by its name in the schema.
pub(crate) mod method {
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_QUEUE_OPEN: u16 = 0x0201;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_SCHEDULE_OPEN: u16 = 0x0202;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_ADD: u16 = 0x0203;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_SET: u16 = 0x0204;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_UPDATE: u16 = 0x0205;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_CANCEL: u16 = 0x0206;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_GET: u16 = 0x0207;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_LIST: u16 = 0x0208;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_WORK: u16 = 0x0209;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_STEP: u16 = 0x020a;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_KEEP: u16 = 0x020b;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_WATCH: u16 = 0x020c;
    #[cfg(feature = "kv")]
    pub(crate) const KV_BUCKET_OPEN: u16 = 0x0101;
    #[cfg(feature = "kv")]
    pub(crate) const KV_GET: u16 = 0x0102;
    #[cfg(feature = "kv")]
    pub(crate) const KV_HAS: u16 = 0x0103;
    #[cfg(feature = "kv")]
    pub(crate) const KV_SET: u16 = 0x0104;
    #[cfg(feature = "kv")]
    pub(crate) const KV_CREATE: u16 = 0x0105;
    #[cfg(feature = "kv")]
    pub(crate) const KV_TAKE: u16 = 0x0106;
    #[cfg(feature = "kv")]
    pub(crate) const KV_DELETE: u16 = 0x0107;
    #[cfg(feature = "kv")]
    pub(crate) const KV_EXPIRE: u16 = 0x0108;
    #[cfg(feature = "kv")]
    pub(crate) const KV_CLEAR: u16 = 0x0109;
    #[cfg(feature = "kv")]
    pub(crate) const KV_LIST: u16 = 0x010a;
    #[cfg(feature = "kv")]
    pub(crate) const KV_COUNTERS_OPEN: u16 = 0x0110;
    #[cfg(feature = "kv")]
    pub(crate) const KV_COUNTERS_ADD: u16 = 0x0111;
    #[cfg(feature = "kv")]
    pub(crate) const KV_COUNTERS_GET: u16 = 0x0112;
    #[cfg(feature = "kv")]
    pub(crate) const KV_COUNTERS_DELETE: u16 = 0x0113;
    #[cfg(feature = "kv")]
    pub(crate) const KV_COUNTERS_CLEAR: u16 = 0x0114;
    #[cfg(feature = "kv")]
    pub(crate) const KV_RATE_LIMIT_OPEN: u16 = 0x0120;
    #[cfg(feature = "kv")]
    pub(crate) const KV_QUOTA_OPEN: u16 = 0x0121;
    #[cfg(feature = "kv")]
    pub(crate) const KV_ALLOW: u16 = 0x0122;
    #[cfg(feature = "kv")]
    pub(crate) const KV_PEEK: u16 = 0x0123;
    #[cfg(feature = "kv")]
    pub(crate) const KV_RESET: u16 = 0x0124;
    #[cfg(feature = "kv")]
    pub(crate) const KV_REFUND: u16 = 0x0125;
    #[cfg(feature = "kv")]
    pub(crate) const KV_ONCE_OPEN: u16 = 0x0130;
    #[cfg(feature = "kv")]
    pub(crate) const KV_ONCE_RUN: u16 = 0x0131;
    #[cfg(feature = "kv")]
    pub(crate) const KV_ONCE_GET: u16 = 0x0132;
    #[cfg(feature = "kv")]
    pub(crate) const KV_ONCE_DELETE: u16 = 0x0133;
    #[cfg(feature = "kv")]
    pub(crate) const KV_TX: u16 = 0x0140;
    pub(crate) const SERVER_STOP: u16 = 0x0001;
    pub(crate) const SERVER_CLOCK: u16 = 0x0002;
    #[cfg(feature = "sql")]
    pub(crate) const SQL_OPEN: u16 = 0x0301;
    #[cfg(feature = "sql")]
    pub(crate) const SQL_QUERY: u16 = 0x0302;
    #[cfg(feature = "sql")]
    pub(crate) const SQL_EXEC: u16 = 0x0303;
    #[cfg(feature = "sql")]
    pub(crate) const SQL_BATCH: u16 = 0x0304;
    #[cfg(feature = "sql")]
    pub(crate) const SQL_TX: u16 = 0x0305;
}

/// Every method, its name and number, for the test that the server answers each.
#[cfg(test)]
pub(crate) const METHODS: &[(&str, u16)] = &[
    #[cfg(feature = "jobs")]
    ("jobs.queue.open", 0x0201),
    #[cfg(feature = "jobs")]
    ("jobs.schedule.open", 0x0202),
    #[cfg(feature = "jobs")]
    ("jobs.add", 0x0203),
    #[cfg(feature = "jobs")]
    ("jobs.set", 0x0204),
    #[cfg(feature = "jobs")]
    ("jobs.update", 0x0205),
    #[cfg(feature = "jobs")]
    ("jobs.cancel", 0x0206),
    #[cfg(feature = "jobs")]
    ("jobs.get", 0x0207),
    #[cfg(feature = "jobs")]
    ("jobs.list", 0x0208),
    #[cfg(feature = "jobs")]
    ("jobs.work", 0x0209),
    #[cfg(feature = "jobs")]
    ("jobs.step", 0x020a),
    #[cfg(feature = "jobs")]
    ("jobs.keep", 0x020b),
    #[cfg(feature = "jobs")]
    ("jobs.watch", 0x020c),
    #[cfg(feature = "kv")]
    ("kv.bucket.open", 0x0101),
    #[cfg(feature = "kv")]
    ("kv.get", 0x0102),
    #[cfg(feature = "kv")]
    ("kv.has", 0x0103),
    #[cfg(feature = "kv")]
    ("kv.set", 0x0104),
    #[cfg(feature = "kv")]
    ("kv.create", 0x0105),
    #[cfg(feature = "kv")]
    ("kv.take", 0x0106),
    #[cfg(feature = "kv")]
    ("kv.delete", 0x0107),
    #[cfg(feature = "kv")]
    ("kv.expire", 0x0108),
    #[cfg(feature = "kv")]
    ("kv.clear", 0x0109),
    #[cfg(feature = "kv")]
    ("kv.list", 0x010a),
    #[cfg(feature = "kv")]
    ("kv.counters.open", 0x0110),
    #[cfg(feature = "kv")]
    ("kv.counters.add", 0x0111),
    #[cfg(feature = "kv")]
    ("kv.counters.get", 0x0112),
    #[cfg(feature = "kv")]
    ("kv.counters.delete", 0x0113),
    #[cfg(feature = "kv")]
    ("kv.counters.clear", 0x0114),
    #[cfg(feature = "kv")]
    ("kv.rateLimit.open", 0x0120),
    #[cfg(feature = "kv")]
    ("kv.quota.open", 0x0121),
    #[cfg(feature = "kv")]
    ("kv.allow", 0x0122),
    #[cfg(feature = "kv")]
    ("kv.peek", 0x0123),
    #[cfg(feature = "kv")]
    ("kv.reset", 0x0124),
    #[cfg(feature = "kv")]
    ("kv.refund", 0x0125),
    #[cfg(feature = "kv")]
    ("kv.once.open", 0x0130),
    #[cfg(feature = "kv")]
    ("kv.once.run", 0x0131),
    #[cfg(feature = "kv")]
    ("kv.once.get", 0x0132),
    #[cfg(feature = "kv")]
    ("kv.once.delete", 0x0133),
    #[cfg(feature = "kv")]
    ("kv.tx", 0x0140),
    ("server.stop", 0x0001),
    ("server.clock", 0x0002),
    #[cfg(feature = "sql")]
    ("sql.open", 0x0301),
    #[cfg(feature = "sql")]
    ("sql.query", 0x0302),
    #[cfg(feature = "sql")]
    ("sql.exec", 0x0303),
    #[cfg(feature = "sql")]
    ("sql.batch", 0x0304),
    #[cfg(feature = "sql")]
    ("sql.tx", 0x0305),
];

/// A body of the message `name` read and written again, for the test that the
/// vectors' bytes are what this codec writes; `None` for a name it lacks.
#[cfg(test)]
pub(crate) fn rewrite(name: &str, body: &[u8]) -> Option<Result<Vec<u8>, Failure>> {
    let rewritten = match name {
        "Hello" => Hello::decode(body).map(|message| message.encode()),
        "Welcome" => Welcome::decode(body).map(|message| message.encode()),
        "GoAway" => GoAway::decode(body).map(|message| message.encode()),
        "Failure" => Failure::decode(body).map(|message| message.encode()),
        "Handle" => Handle::decode(body).map(|message| message.encode()),
        "Empty" => Empty::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Concurrency" => JobsConcurrency::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Backoff" => JobsBackoff::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Rate" => JobsRate::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.QueueOpen" => JobsQueueOpen::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.ScheduleOpen" => JobsScheduleOpen::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Call" => JobsCall::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Id" => JobsId::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Changed" => JobsChanged::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Job" => JobsJob::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.List" => JobsList::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Page" => JobsPage::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Work" => JobsWork::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Held" => JobsHeld::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Answer" => JobsAnswer::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Step" => JobsStep::decode(body).map(|message| message.encode()),
        #[cfg(feature = "jobs")]
        "jobs.Kept" => JobsKept::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.BucketOpen" => KvBucketOpen::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Call" => KvCall::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Entry" => KvEntry::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Written" => KvWritten::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Found" => KvFound::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Branch" => KvBranch::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.List" => KvList::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Page" => KvPage::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.CountersOpen" => KvCountersOpen::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Count" => KvCount::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.RateLimitOpen" => KvRateLimitOpen::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Window" => KvWindow::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.QuotaOpen" => KvQuotaOpen::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.WindowUse" => KvWindowUse::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Allowance" => KvAllowance::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.OnceOpen" => KvOnceOpen::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Answer" => KvAnswer::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Check" => KvCheck::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Op" => KvOp::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Tx" => KvTx::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.Outcome" => KvOutcome::decode(body).map(|message| message.encode()),
        #[cfg(feature = "kv")]
        "kv.TxResults" => KvTxResults::decode(body).map(|message| message.encode()),
        "server.Clock" => ServerClock::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.Migration" => SqlMigration::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.Open" => SqlOpen::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.Text" => SqlText::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.Statement" => SqlStatement::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.Query" => SqlQuery::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.Rows" => SqlRows::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.Done" => SqlDone::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.Batch" => SqlBatch::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.Batched" => SqlBatched::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.TxOpen" => SqlTxOpen::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.TxCall" => SqlTxCall::decode(body).map(|message| message.encode()),
        #[cfg(feature = "sql")]
        "sql.TxAnswer" => SqlTxAnswer::decode(body).map(|message| message.encode()),
        _ => return None,
    };
    Some(rewritten)
}
