//! Messages past one frame's body, on a connection whose WELCOME agreed a max
//! message: a request joined from its pieces before it is handled as one
//! frame's body would be, and an answer sent in pieces.
//!
//! ```text
//! REQUEST·MORE  stream 5, search.setMany, 2 MiB    joined, its credit given back
//! REQUEST·MORE  stream 5, search.setMany, 2 MiB    joined
//! REQUEST·END   stream 5, search.setMany, 1 MiB    the 5 MiB handled as one body
//! RESPONSE·MORE stream 5, 2 MiB                    a piece, while the host takes
//! RESPONSE·END  stream 5, 1 MiB                    the stream's end
//! ```
//!
//! What keeps memory bounded. A piece counts against the connection's credit
//! as any REQUEST does, and is copied into its message at once, so its credit
//! goes back as soon as a grant is due: a large request never holds the
//! connection's window, and other streams' frames keep coming. The bytes the
//! connection holds of joined messages, those still joining and those whole
//! and running until their stream ends, are counted together against the max
//! message: a piece past it drops what its message had and the rest that
//! comes, and the message is refused `limit` once its last piece came, so the
//! stream's number stays in use until the client is done with it.
//!
//! What an answer in pieces holds. An answer is built whole before its first
//! piece leaves, and waits in the connection's line of answers in pieces. The
//! line counts toward what the connection holds for its client, beside what
//! its output owes: past [`OUTPUT_MOST`] no frame more is read and no call
//! more starts, as for a client that does not read. And the line holds at
//! most the max message of reads' answers: a read whose answer would pass it
//! while others wait drops what it built and runs again once the line is
//! empty, from the worker the line's last piece frees, so no worker thread
//! waits for the client and no read is refused for what others hold. A
//! write's answer always joins the line: dropping it would make the write's
//! outcome unknown. So a connection holds of answers in pieces at most the
//! max message, and beside it the answers of the writes it was running,
//! which the gate above keeps from growing past those already started.
//!
//! What keeps it fair. An answer's pieces go out one at a time while the
//! output owes its client less than [`OUTPUT_LOW`]: each answer sends a piece
//! and goes to the back of the line, and what other streams queue goes out
//! between them as the host takes it.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[cfg(doc)]
use super::OUTPUT_MOST;
use super::{Input, OUTPUT_LOW, Shared, method_name};
use crate::limits::{Limit, REQUEST_BYTES};
use crate::sync::lock;
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::Failure;

/// A request whose pieces are coming, by stream.
pub(super) struct Joining {
    method: u16,
    body: Vec<u8>,
    /// The bytes its pieces brought, kept on once they are dropped.
    wanted: u64,
    /// What the connection held of others' messages when this one passed the
    /// bound: set once it is refused, its pieces dropped from then on.
    refused: Option<u64>,
}

/// An answer past one frame's body, going out a piece at a time.
pub(super) struct Pieces {
    stream: u32,
    body: Vec<u8>,
    sent: usize,
}

/// The answers whose pieces wait to go out, in the order they go, and the
/// reads that wait for the line to empty before they run again.
#[derive(Default)]
pub(super) struct Answering {
    line: VecDeque<Pieces>,
    again: Vec<Box<dyn FnOnce() + Send>>,
}

impl Shared {
    /// A piece of a request: its bytes joined to its message, which is
    /// handled once the piece without MORE came, or refused `limit` when the
    /// connection's joined messages passed the max message.
    pub(super) fn join(self: &Arc<Self>, input: &mut Input, frame: Frame) {
        let Some(most) = input.max_message else {
            return self.go_away(input, "protocol", "a REQUEST with MORE, where WELCOME agreed no max message");
        };
        let bytes = frame.body.len() as u64;
        if let Err(refused) = self.receive(bytes) {
            return self.go_away(input, "protocol", &refused);
        }
        if !input.joining.contains_key(&frame.stream) {
            if let Err(refused) = self.open_stream(frame.stream, &[]) {
                return self.go_away(input, "protocol", &refused);
            }
            let joining = Joining { method: frame.method, body: Vec::new(), wanted: 0, refused: None };
            input.joining.insert(frame.stream, joining);
        }
        // the piece is copied out of the connection's window here, at once
        self.let_go(bytes);
        self.grant_when_due();
        let joining = input.joining.get_mut(&frame.stream).expect("a joining stream was just found or made");
        if joining.method != frame.method {
            let refused = format!("a piece of stream {} for another method than its first", frame.stream);
            return self.go_away(input, "protocol", &refused);
        }
        self.take_piece(frame.stream, joining, &frame.body, most as u64);
        if frame.flags & frame::MORE != 0 {
            return;
        }
        let joined = input.joining.remove(&frame.stream).expect("a joining stream is in the map");
        match joined.refused {
            Some(others) => self.answer(frame.stream, Err(past_the_message(&joined, others, most as u64))),
            None => {
                let Joining { method, body, .. } = joined;
                let whole = Frame { kind: Kind::Request, flags: frame.flags, method, stream: frame.stream, body };
                self.admit(input, whole);
            }
        }
    }

    /// Joins a piece to its message, or drops it, and what its message had,
    /// once the connection would hold past `most` of joined messages.
    fn take_piece(&self, stream: u32, joining: &mut Joining, piece: &[u8], most: u64) {
        let bytes = piece.len() as u64;
        joining.wanted += bytes;
        if joining.refused.is_some() {
            return;
        }
        let held = self.joined.fetch_add(bytes, Ordering::AcqRel) + bytes;
        if held <= most {
            joining.body.extend_from_slice(piece);
            self.hold_joined(stream, bytes);
            return;
        }
        let dropped = joining.body.len() as u64;
        self.joined.fetch_sub(bytes, Ordering::AcqRel);
        self.let_go_of_joined(stream, dropped);
        joining.refused = Some(held - bytes - dropped);
        joining.body = Vec::new();
    }

    /// Counts `bytes` of a joined message against its stream, which gives
    /// them back as it ends.
    fn hold_joined(&self, stream: u32, bytes: u64) {
        if let Some(opened) = self.lock_open().get_mut(&stream) {
            opened.joined += bytes;
        }
    }

    /// Gives back `bytes` a stream held of the connection's joined messages.
    fn let_go_of_joined(&self, stream: u32, bytes: u64) {
        if let Some(opened) = self.lock_open().get_mut(&stream) {
            let released = bytes.min(opened.joined);
            opened.joined -= released;
            self.joined.fetch_sub(released, Ordering::AcqRel);
        }
    }

    /// A CANCEL of a request still joining: what it holds is dropped, and its
    /// stream answers `cancelled`.
    pub(super) fn cancel_joining(&self, input: &mut Input, stream: u32) -> bool {
        if input.joining.remove(&stream).is_none() {
            return false;
        }
        self.answer(stream, Err(Failure::cancelled("the client cancelled its request before its last piece")));
        true
    }

    /// What the connection holds for its client: what its output owes, and
    /// the pieces of answers that wait to join it.
    pub(super) fn held_for_client(&self) -> usize {
        self.owing.load(Ordering::SeqCst) + self.waiting_pieces.load(Ordering::SeqCst)
    }

    /// Sends an answer past the agreed body in pieces, behind the other
    /// answers that wait.
    pub(super) fn answer_in_pieces(&self, stream: u32, body: Vec<u8>) {
        self.queue_answer(&mut lock(&self.answering), stream, body);
        self.send_pieces();
    }

    /// Ends a read's stream with its answer, unless the answer goes in pieces
    /// and would take the line past the max message while others wait: then
    /// what it built is dropped, and what `again` makes runs it once the line
    /// is empty.
    pub(super) fn answer_read(
        &self,
        stream: u32,
        answered: std::result::Result<Vec<u8>, Failure>,
        again: impl FnOnce() -> Box<dyn FnOnce() + Send>,
    ) {
        let (agreed, most) = (self.agreed.load(Ordering::Acquire), self.max_message.load(Ordering::Acquire));
        let body = match answered {
            Ok(body) if body.len() > agreed && body.len() <= most => body,
            answered => return self.answer(stream, answered),
        };
        let mut answering = lock(&self.answering);
        if !answering.line.is_empty() && self.waiting_pieces.load(Ordering::SeqCst) + body.len() > most {
            answering.again.push(again());
            return;
        }
        self.queue_answer(&mut answering, stream, body);
        drop(answering);
        self.send_pieces();
    }

    /// Puts an answer at the back of the line, under the line's lock, so that
    /// what waits is counted where the line is looked at.
    fn queue_answer(&self, answering: &mut Answering, stream: u32, body: Vec<u8>) {
        self.waiting_pieces.fetch_add(body.len(), Ordering::SeqCst);
        answering.line.push_back(Pieces { stream, body, sent: 0 });
    }

    /// Sends the next piece of each answer in turn while the output owes its
    /// client less than its low mark; the host's next take sends more. The
    /// line's lock is held while a piece is queued, so that two threads never
    /// queue one answer's pieces out of their order; once the line is empty,
    /// the reads that waited for it run again.
    pub(super) fn send_pieces(&self) {
        let mut answering = lock(&self.answering);
        while self.owing.load(Ordering::SeqCst) <= OUTPUT_LOW {
            let Some(mut pieces) = answering.line.pop_front() else {
                let again = std::mem::take(&mut answering.again);
                drop(answering);
                for read in again {
                    self.workers.run(read);
                }
                return;
            };
            let agreed = self.agreed.load(Ordering::Acquire);
            let end = (pieces.sent + agreed).min(pieces.body.len());
            let piece = pieces.body[pieces.sent..end].to_vec();
            let length = piece.len();
            if end == pieces.body.len() {
                self.queue_moved(Frame::new(Kind::Response, pieces.stream, piece).ending(), Some(pieces.stream));
            } else {
                let frame = Frame { flags: frame::MORE, ..Frame::new(Kind::Response, pieces.stream, piece) };
                self.queue_moved(frame, None);
                pieces.sent = end;
                answering.line.push_back(pieces);
            }
            self.waiting_pieces.fetch_sub(length, Ordering::SeqCst);
        }
    }

    /// Lets go of every answer in pieces and every read waiting to run again.
    pub(super) fn drop_pieces(&self) {
        let mut answering = lock(&self.answering);
        let dropped = std::mem::take(&mut *answering);
        self.waiting_pieces.store(0, Ordering::SeqCst);
        drop(answering);
        drop(dropped);
    }
}

/// The `limit` of a request that passed the max message, alone or beside the
/// `others` the connection held of other messages then.
fn past_the_message(joined: &Joining, others: u64, most: u64) -> Failure {
    let (called, alone) = (method_name(joined.method), joined.wanted);
    if others == 0 || alone > most {
        let said = format!("{called}: a request of {alone} bytes, past the {most} a message holds");
        return Failure::from(Limit { bound: most, ..REQUEST_BYTES }.refuse(alone, said));
    }
    let said = format!(
        "{called}: a request of {alone} bytes beside the {others} this connection joins already, past the {most} \
         its messages hold at once"
    );
    Failure::from(Limit { bound: most, ..REQUEST_BYTES }.refuse(alone + others, said))
}

#[cfg(test)]
#[path = "joining_tests.rs"]
mod tests;
