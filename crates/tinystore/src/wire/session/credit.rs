//! What the client may send and what it is owed: the connection's and each
//! stream's credit, and the frames queued for the host to take.

use std::sync::atomic::Ordering;
use std::sync::{MutexGuard, PoisonError};

use super::{CONNECTION_CREDIT, IN_FLIGHT, OUTPUT_LOW, OpenStream, STREAM_CREDIT, Shared};
use crate::limits::{ANSWER_BYTES, Limit};
use crate::sync::lock;
use crate::wire::codec::Message;
use crate::wire::frame::{Frame, Kind};
use crate::wire::output::Output;
use crate::wire::protocol::Failure;

impl Shared {
    /// A client's grant of DATA on a stream: a worker's or a watch's sends
    /// what waited for it. No other stream sends DATA a client grants credit
    /// for: a call answers in one message, within the body agreed.
    #[cfg_attr(
        not(any(
            feature = "jobs",
            feature = "sql",
            feature = "blobs",
            feature = "records",
            feature = "metrics",
            feature = "config",
            feature = "backup",
            feature = "channels"
        )),
        expect(
            unused_variables,
            reason = "only the streams of jobs, sql, blobs, records, metrics, config, backup and channels take credit"
        )
    )]
    pub(super) fn credit(&self, frame: &Frame) {
        let Ok(granted) = <[u8; 4]>::try_from(frame.body.as_slice()) else {
            return;
        };
        let granted = u32::from_le_bytes(granted);
        #[cfg(feature = "jobs")]
        self.jobs.grant(frame.stream, granted);
        #[cfg(feature = "sql")]
        self.sql.grant(frame.stream, granted);
        #[cfg(feature = "blobs")]
        self.blobs.grant(frame.stream, granted);
        #[cfg(feature = "records")]
        self.records.grant(frame.stream, granted);
        #[cfg(feature = "metrics")]
        self.metrics.grant(frame.stream, granted);
        #[cfg(feature = "config")]
        self.config.grant(frame.stream, granted);
        #[cfg(feature = "backup")]
        self.backups.grant(frame.stream, granted);
        #[cfg(feature = "channels")]
        self.channels.grant(frame.stream, granted);
    }

    /// Ends a stream with its RESPONSE.
    pub(super) fn answer(&self, stream: u32, answered: std::result::Result<Vec<u8>, Failure>) {
        let response = match self.within_agreed(answered) {
            Ok(body) => Frame::new(Kind::Response, stream, body).ending(),
            Err(failure) => Frame::new(Kind::Response, stream, failure.encode()).failing(),
        };
        self.send_last(response);
    }

    /// Ends a stream whose RESPONSE went out already, with its last DATA.
    pub(super) fn end_stream(&self, stream: u32, answered: std::result::Result<Vec<u8>, Failure>) {
        let data = match self.within_agreed(answered) {
            Ok(body) => Frame::new(Kind::Data, stream, body).ending(),
            Err(failure) => Frame::new(Kind::Data, stream, failure.encode()).failing(),
        };
        self.send_last(data);
    }

    pub(super) fn send(&self, frames: &[Frame]) {
        self.queue(frames, None);
    }

    /// Runs what waited for the output once it owes less than its low mark.
    pub(super) fn drained(&self) {
        let mut waiting = self.waiting_output.lock().unwrap_or_else(PoisonError::into_inner);
        if waiting.is_empty() || self.owed() > OUTPUT_LOW {
            return;
        }
        for task in std::mem::take(&mut *waiting) {
            self.workers.run(task);
        }
    }

    /// The bytes queued for the client and not yet taken by the host.
    pub(super) fn owed(&self) -> usize {
        self.lock_output().owed()
    }

    /// Runs `task` once the output drains; at once when it has already.
    pub(super) fn when_drained(&self, task: Box<dyn FnOnce() + Send>) {
        self.waiting_output.lock().unwrap_or_else(PoisonError::into_inner).push(task);
        self.drained();
    }

    /// Sends a stream's last frame.
    fn send_last(&self, frame: Frame) {
        let stream = frame.stream;
        self.queue_moved(frame, Some(stream));
    }

    /// Takes a stream number into use, refusing one in use already or one
    /// past the streams in flight WELCOME allowed.
    pub(super) fn open_stream(&self, stream: u32, body: &[u8]) -> std::result::Result<(), String> {
        let mut open = self.lock_open();
        if open.contains_key(&stream) {
            return Err(format!("a REQUEST on stream {stream}, which is in use"));
        }
        if open.len() >= IN_FLIGHT as usize {
            return Err(format!("a REQUEST past the {IN_FLIGHT} streams in flight WELCOME allowed"));
        }
        open.insert(stream, OpenStream { left: STREAM_CREDIT, held: body.len() as u64 });
        Ok(())
    }

    /// Counts a body against the connection's credit.
    pub(super) fn receive(&self, bytes: u64) -> std::result::Result<(), String> {
        let outstanding = self.outstanding.fetch_add(bytes, Ordering::AcqRel) + bytes;
        if outstanding > CONNECTION_CREDIT {
            return Err(format!("{outstanding} bytes sent past the connection's credit of {CONNECTION_CREDIT}"));
        }
        Ok(())
    }

    /// Counts a DATA body against its stream's credit; one past it is the
    /// client's fault. A DATA of a stream no longer in use is dropped, as the
    /// protocol says, and counts against nothing.
    pub(super) fn spend_stream_credit(&self, stream: u32, bytes: u64) -> std::result::Result<(), String> {
        let mut open = self.lock_open();
        let Some(opened) = open.get_mut(&stream) else {
            drop(open);
            self.let_go(bytes);
            self.grant_when_due();
            return Ok(());
        };
        opened.left =
            opened.left.checked_sub(bytes).ok_or_else(|| format!("a DATA on stream {stream} past its credit"))?;
        opened.held += bytes;
        Ok(())
    }

    /// Gives the connection back `bytes` a stream holds of it, the stream's
    /// own credit left as it is: what the server read and let go of before
    /// it grants the stream more.
    #[cfg(feature = "jobs")]
    pub(super) fn let_go_of(&self, stream: u32, bytes: u64) {
        let released = match self.lock_open().get_mut(&stream) {
            Some(opened) => {
                let released = bytes.min(opened.held);
                opened.held -= released;
                released
            }
            None => return,
        };
        self.let_go(released);
        self.grant_when_due();
    }

    /// Bytes the server let go of, which the next grant gives back.
    fn let_go(&self, bytes: u64) {
        self.to_grant.fetch_add(bytes, Ordering::AcqRel);
    }

    /// Grants the connection's credit back once half its window gathered.
    fn grant_when_due(&self) {
        let gathered = self.to_grant.load(Ordering::Acquire);
        if gathered < CONNECTION_CREDIT / 2 {
            return;
        }
        let granted = self.to_grant.swap(0, Ordering::AcqRel);
        if granted == 0 {
            return;
        }
        self.outstanding.fetch_sub(granted, Ordering::AcqRel);
        let granted = u32::try_from(granted).unwrap_or(u32::MAX);
        self.send(&[Frame::new(Kind::Credit, 0, granted.to_le_bytes().to_vec())]);
    }

    /// A body as it may go out: one past the agreed body is `limit`, which a
    /// client takes, rather than a frame it would end the connection over.
    fn within_agreed(&self, answered: std::result::Result<Vec<u8>, Failure>) -> std::result::Result<Vec<u8>, Failure> {
        let agreed = self.agreed.load(Ordering::Acquire);
        match answered {
            Ok(body) if body.len() > agreed => {
                let refused = format!("an answer of {} bytes, past the {agreed} a body holds", body.len());
                Err(Failure::from(Limit { bound: agreed as u64, ..ANSWER_BYTES }.refuse(body.len() as u64, refused)))
            }
            answered => answered,
        }
    }

    /// A write's answer as it may go out. Past the agreed body it is
    /// `outcome_unknown`, never `limit`: the write ran, what it took is gone,
    /// and a client that sent it again, as it does a `limit`, would write
    /// twice.
    pub(super) fn written_within_agreed(
        &self,
        answered: std::result::Result<Vec<u8>, Failure>,
    ) -> std::result::Result<Vec<u8>, Failure> {
        let agreed = self.agreed.load(Ordering::Acquire);
        match answered {
            Ok(body) if body.len() > agreed => Err(Failure {
                code: "outcome_unknown".to_owned(),
                message: format!(
                    "the write ran, and its answer of {} bytes is past the {agreed} a body holds: read what it wrote",
                    body.len()
                ),
                what: None,
            }),
            answered => answered,
        }
    }

    /// Queues frames for the host to take, `ended` of them their streams'
    /// last. Those streams leave the count under the output's lock, after
    /// their frames are queued: whoever read a stream's last frame finds
    /// `streams` without it, and whoever finds no stream finds every last
    /// frame queued. Counted after the lock instead, a client that had read
    /// its answer could still see its stream in flight.
    fn queue(&self, frames: &[Frame], ended: Option<u32>) {
        let mut output = self.lock_output();
        let dropped = output.ended && frames.iter().all(|frame| frame.kind != Kind::GoAway);
        if !dropped {
            for frame in frames {
                output.copy(frame);
            }
        }
        self.queued(output, dropped, frames, ended);
    }

    /// Queues one frame as `queue` does, a long body moved behind what is
    /// owed rather than copied: a file's mebibyte is written once into the
    /// message that carries it, and once more into the host's bytes.
    pub(super) fn queue_moved(&self, frame: Frame, ended: Option<u32>) {
        debug_assert!(frame.kind != Kind::Credit, "a grant is counted where it is queued");
        let mut output = self.lock_output();
        let dropped = output.ended && frame.kind != Kind::GoAway;
        if !dropped {
            output.push(frame);
        }
        self.queued(output, dropped, &[], ended);
    }

    /// What follows the frames' queueing, under the output's lock: their
    /// streams' grants and ends counted, and the host woken.
    fn queued(&self, mut output: MutexGuard<'_, Output>, dropped: bool, frames: &[Frame], ended: Option<u32>) {
        self.owing.store(output.owed(), Ordering::SeqCst);
        let mut let_go = 0;
        {
            let mut open = self.lock_open();
            // a grant of the server's on a stream is room its client may send
            // into, and says the server let go of as many of its bytes
            for frame in frames.iter().filter(|frame| frame.kind == Kind::Credit && frame.stream != 0) {
                if let (Some(opened), Ok(granted)) = (open.get_mut(&frame.stream), <[u8; 4]>::try_from(&frame.body[..]))
                {
                    let granted = u64::from(u32::from_le_bytes(granted));
                    opened.left = opened.left.saturating_add(granted);
                    let released = granted.min(opened.held);
                    opened.held -= released;
                    let_go += released;
                }
            }
            if let Some(opened) = ended.and_then(|stream| open.remove(&stream)) {
                let_go += opened.held;
            }
        }
        if let_go > 0 {
            self.let_go(let_go);
        }
        if dropped {
            return;
        }
        let wake = !std::mem::replace(&mut output.woken, true);
        let awaited = output.awaited;
        drop(output);
        if awaited {
            self.ready.notify_all();
        }
        if wake && let Some(wake) = &*lock(&self.wake) {
            wake();
        }
        // the output's lock is let go: a grant goes out as a frame of its own
        if let_go > 0 {
            self.grant_when_due();
        }
    }
}
