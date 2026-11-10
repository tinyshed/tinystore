//! The calls whose answers are streams: a RESPONSE, then DATA within the
//! client's credit, and what each stream needs of the session to send it.

use std::sync::Arc;
#[cfg(feature = "sql")]
use std::sync::atomic::Ordering;

use super::{HELLO_MOST, Input, Shared, guarded};
#[cfg(feature = "sql")]
use crate::Transaction;
#[cfg(feature = "backup")]
use crate::wire::backup;
#[cfg(feature = "blobs")]
use crate::wire::blobs;
#[cfg(feature = "channels")]
use crate::wire::channels;
use crate::wire::codec::Message;
#[cfg(feature = "config")]
use crate::wire::config;
use crate::wire::frame::{Frame, Kind};
#[cfg(feature = "jobs")]
use crate::wire::jobs;
#[cfg(feature = "metrics")]
use crate::wire::metrics;
use crate::wire::protocol::Empty;
#[cfg(feature = "sql")]
use crate::wire::protocol::Failure;
#[cfg(feature = "records")]
use crate::wire::records;
#[cfg(feature = "sql")]
use crate::wire::sql;

impl Shared {
    /// Registers a subscription: the stream's RESPONSE once the store has it,
    /// then its batches as DATA within the client's credit.
    #[cfg(feature = "channels")]
    pub(super) fn subscribe(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| channels::subscribe(&self.store, &self.channels, stream, body, link)) {
            Ok(subscribed) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                subscribed.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Starts a client's worker: the stream's `RESPONSE`, then the jobs it
    /// hands the client and the client's answers, as DATA both ways.
    #[cfg(feature = "jobs")]
    pub(super) fn exchange(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        match guarded(|| jobs::work(&self.jobs, stream, body, self.link(input, stream))) {
            Ok(work) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                work.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Starts a client's watch of a job: the stream's `RESPONSE`, then a DATA
    /// each time the job changes, until it ends.
    #[cfg(feature = "jobs")]
    pub(super) fn watch(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        match guarded(|| jobs::watch(&self.jobs, stream, body, self.link(input, stream))) {
            Ok(watch) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                watch.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// What a stream the server sends DATA on needs of the session.
    #[cfg(feature = "jobs")]
    fn link(self: &Arc<Self>, input: &Input, stream: u32) -> jobs::Link {
        let (sending, finishing, spawning) = (Arc::downgrade(self), Arc::downgrade(self), Arc::downgrade(self));
        let send: jobs::Sender = Arc::new(move |body| {
            if let Some(shared) = sending.upgrade() {
                shared.send(&[Frame::new(Kind::Data, stream, body)]);
            }
        });
        let finish: jobs::Finisher = Arc::new(move |last| {
            if let Some(shared) = finishing.upgrade() {
                shared.end_stream(stream, last);
            }
        });
        let spawn: jobs::Spawn = Arc::new(move |task| {
            if let Some(shared) = spawning.upgrade() {
                shared.workers.run(task);
            }
        });
        let max_body = input.max_body.unwrap_or(HELLO_MOST);
        jobs::Link { send, finish, spawn, credit: input.stream_credit, max_body }
    }

    /// Answers a query on a worker: its rows in the RESPONSE when they fit
    /// one message, else a RESPONSE and their parts as DATA within the
    /// client's credit.
    #[cfg(feature = "sql")]
    pub(super) fn download(self: &Arc<Self>, input: &Input, stream: u32, called: u16, body: Vec<u8>) {
        let link = self.sql_link(input, stream);
        let (shared, reading) = (Arc::clone(self), self.reading(stream, called));
        self.run_call(Box::new(move || {
            let (answering, sending) = (Arc::clone(&shared), link.clone());
            let done = move |queried: std::result::Result<sql::Queried, Failure>| match queried {
                Ok(sql::Queried::Whole(rows)) => answering.answer(stream, Ok(rows)),
                Ok(sql::Queried::Parts(parts, held)) => {
                    answering.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                    sql::download(&answering.sql, stream, (parts, held), &sending);
                }
                Err(failure) => answering.answer(stream, Err(failure)),
            };
            let queued = shared.read_under(stream, reading, || {
                guarded(|| {
                    let read_only = shared.read_only.load(Ordering::Relaxed);
                    sql::query(&shared.sql, (called, &body), &link, read_only, done);
                    Ok(())
                })
            });
            if let Err(failure) = queued {
                shared.answer(stream, Err(failure));
            }
        }));
    }

    /// Opens a client's transaction: the stream's RESPONSE, then, in its turn
    /// on its database's line, a worker that holds the writer while the
    /// client's calls come as DATA.
    #[cfg(feature = "sql")]
    pub(super) fn transaction(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.sql_link(input, stream);
        match guarded(|| sql::transaction(&self.sql, stream, body, &link)) {
            Ok((txing, running)) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                let (shared, line) = (Arc::clone(self), running.line());
                let turn = Box::new(move || {
                    let nested = |tx: &Transaction<'_>, method, body: &[u8], max_body| {
                        shared.call_in(tx, method, body, max_body)
                    };
                    txing.run(running, &shared.sql, stream, &nested);
                });
                self.workers.run_in_line(&line, turn);
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Answers a read of a file's bytes on a worker: its first piece in the
    /// RESPONSE, and the rest as pieces within the client's credit.
    #[cfg(feature = "blobs")]
    pub(super) fn read(self: &Arc<Self>, input: &Input, stream: u32, body: Vec<u8>) {
        let link = self.stream_link(input, stream);
        let shared = Arc::clone(self);
        self.run_call(Box::new(move || match guarded(|| blobs::read(&shared.blobs, stream, &body, &link)) {
            Ok(blobs::Reading::Whole(response)) => shared.answer(stream, Ok(response)),
            Ok(blobs::Reading::Begun(response, getting)) => {
                shared.queue_moved(Frame::new(Kind::Response, stream, response), None);
                getting.pump();
            }
            Err(failure) => shared.answer(stream, Err(failure)),
        }));
    }

    /// Begins an upload: the stream's RESPONSE, then the client's pieces as
    /// DATA, its last publishing the file.
    #[cfg(feature = "blobs")]
    pub(super) fn upload(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| blobs::upload(&self.blobs, stream, body, &link)) {
            Ok(()) => self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]),
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Starts a client's tail: the stream's RESPONSE, then a DATA each time
    /// events arrive that its query finds.
    #[cfg(feature = "records")]
    pub(super) fn tail(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| records::tail(&self.store, &self.records, stream, body, link)) {
            Ok(tailing) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                tailing.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Begins a client's writer of lines: the stream's RESPONSE, then its
    /// pieces as DATA, the last answered with what the writer dropped.
    #[cfg(feature = "records")]
    pub(super) fn lines(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| records::lines(&self.records, stream, body, link)) {
            Ok(()) => self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]),
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Answers a read, the newest samples or an aggregate on a worker: one
    /// part in the RESPONSE, or the RESPONSE and then the parts within the
    /// client's credit, half of what it takes at once each.
    #[cfg(feature = "metrics")]
    pub(super) fn parts(self: &Arc<Self>, input: &Input, stream: u32, called: u16, body: Vec<u8>) {
        let link = self.stream_link(input, stream);
        let (shared, reading) = (Arc::clone(self), self.reading(stream, called));
        self.run_call(Box::new(move || {
            let bound = crate::wire::part_bound(link.max_body, link.credit, 0);
            let parted =
                shared.read_under(stream, reading, || guarded(|| metrics::parts(&shared.store, called, &body, bound)));
            match parted {
                Ok(mut parts) if parts.len() == 1 => shared.answer(stream, Ok(parts.pop_front().unwrap_or_default())),
                Ok(parts) => {
                    shared.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                    metrics::download(&shared.metrics, stream, parts, link);
                }
                Err(failure) => shared.answer(stream, Err(failure)),
            }
        }));
    }

    /// Opens a client's instruments' stream: the RESPONSE says what the
    /// store knows of its session, then its batches come as DATA, each
    /// answered with one.
    #[cfg(feature = "metrics")]
    pub(super) fn instruments(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| metrics::instruments(&self.store, &self.metrics, stream, body, link)) {
            Ok(response) => self.send(&[Frame::new(Kind::Response, stream, response)]),
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Begins a text a client sends in pieces: the stream's RESPONSE, then
    /// the store's memory for the whole text, then its pieces as DATA, the
    /// last answered once the text is written. The memory is reserved on a
    /// worker once the RESPONSE has gone: its refusal, a final frame, never
    /// leaves before it.
    #[cfg(feature = "metrics")]
    pub(super) fn gather(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| metrics::gather(&self.store, &self.metrics, stream, body, link)) {
            Ok(gathering) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                gathering.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Starts a client's follow of a config: the stream's RESPONSE, then a
    /// DATA with its value now and one each time it changes.
    #[cfg(feature = "config")]
    pub(super) fn follow(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| config::follow(&self.config, stream, body, link)) {
            Ok(following) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                following.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Starts a backup of the store: the stream's RESPONSE, then its zip in
    /// pieces within the client's credit, then what it holds.
    #[cfg(feature = "backup")]
    pub(super) fn backup(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| backup::asked(&self.backups, stream, body, link)) {
            Ok(sending) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                sending.begin(&self.store, &self.backups);
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// What a stream of a file's bytes, a tail's batches, a writer's lines, a
    /// read's parts, a client's instruments, a text's pieces, a config's
    /// values or a backup's zip needs of the session.
    #[cfg(any(
        feature = "blobs",
        feature = "records",
        feature = "metrics",
        feature = "config",
        feature = "backup",
        feature = "channels"
    ))]
    fn stream_link(self: &Arc<Self>, input: &Input, stream: u32) -> crate::wire::Link {
        let (sending, finishing, granting, spawning) =
            (Arc::downgrade(self), Arc::downgrade(self), Arc::downgrade(self), Arc::downgrade(self));
        crate::wire::Link {
            send: Arc::new(move |body| {
                if let Some(shared) = sending.upgrade() {
                    shared.queue_moved(Frame::new(Kind::Data, stream, body), None);
                }
            }),
            finish: Arc::new(move |last| {
                if let Some(shared) = finishing.upgrade() {
                    shared.end_stream(stream, last);
                }
            }),
            grant: Arc::new(move |bytes: u32| {
                if let Some(shared) = granting.upgrade() {
                    shared.send(&[Frame::new(Kind::Credit, stream, bytes.to_le_bytes().to_vec())]);
                }
            }),
            spawn: Arc::new(move |task| {
                if let Some(shared) = spawning.upgrade() {
                    shared.workers.run(task);
                }
            }),
            credit: input.stream_credit,
            max_body: input.max_body.unwrap_or(HELLO_MOST),
            #[cfg(feature = "channels")]
            owed: {
                let owing = Arc::downgrade(self);
                Arc::new(move || owing.upgrade().map_or(0, |shared| shared.owed()))
            },
            #[cfg(feature = "channels")]
            when_drained: {
                let draining = Arc::downgrade(self);
                Arc::new(move |task| {
                    if let Some(shared) = draining.upgrade() {
                        shared.when_drained(task);
                    }
                })
            },
        }
    }

    /// What an sql stream that sends DATA needs of the session.
    #[cfg(feature = "sql")]
    fn sql_link(self: &Arc<Self>, input: &Input, stream: u32) -> sql::Link {
        let (sending, finishing, granting) = (Arc::downgrade(self), Arc::downgrade(self), Arc::downgrade(self));
        sql::Link {
            send: Arc::new(move |body| {
                if let Some(shared) = sending.upgrade() {
                    shared.send(&[Frame::new(Kind::Data, stream, body)]);
                }
            }),
            finish: Arc::new(move |last| {
                if let Some(shared) = finishing.upgrade() {
                    shared.end_stream(stream, last);
                }
            }),
            grant: Arc::new(move |bytes: u32| {
                if let Some(shared) = granting.upgrade() {
                    shared.send(&[Frame::new(Kind::Credit, stream, bytes.to_le_bytes().to_vec())]);
                }
            }),
            credit: input.stream_credit,
            max_body: input.max_body.unwrap_or(HELLO_MOST),
        }
    }
}
