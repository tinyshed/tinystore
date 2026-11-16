//! A client's getOrCreate, handed over as a once run is: the file there ends
//! the stream, a run handed to the client waits for its last DATA, and a run
//! the store holds asks again once it ends, or ends the stream with the
//! failure that run ended with.

use std::sync::Arc;

use super::{Shared, guarded};
use crate::wire::blobs::{self, Asked, ClientFill, Handed};
use crate::wire::frame::{Frame, Kind};
use crate::wire::protocol::Failure;

impl Shared {
    /// Asks for a getOrCreate's path on a worker.
    pub(super) fn fill(self: &Arc<Self>, stream: u32, body: Vec<u8>, max_body: usize) {
        let asked = self.blobs.ask_fill(stream);
        let shared = Arc::clone(self);
        self.run_call(Box::new(move || shared.ask_fill((stream, asked), body, (false, max_body))));
    }

    /// Asks for the path once, and settles the stream as it came out; a wait
    /// asks again on a worker once the run it waits for ends.
    fn ask_fill(self: &Arc<Self>, at: (u32, u64), body: Vec<u8>, (waited, max_body): (bool, usize)) {
        let again: blobs::Again = {
            let (shared, body) = (Arc::clone(self), body.clone());
            Box::new(move |failure| {
                let asking = Arc::clone(&shared);
                shared.workers.run(Box::new(move || match failure {
                    Some(failure) => asking.settle_fill(at, Asked::Answered(Err(failure))),
                    None => asking.ask_fill(at, body, (true, max_body)),
                }));
            })
        };
        let came = guarded(|| Ok(blobs::get_or_create(&self.blobs, &body, (waited, max_body), again)))
            .unwrap_or_else(|failure| Asked::Answered(Err(failure)));
        self.settle_fill(at, came);
    }

    fn settle_fill(&self, (stream, asked): (u32, u64), came: Asked) {
        self.blobs.settle_fill((stream, asked), came, |answer, ends| match (answer, ends) {
            (answer, true) => self.answer(stream, answer),
            (Ok(answer), false) => self.send(&[Frame::new(Kind::Response, stream, answer)]),
            (Err(failure), false) => self.answer(stream, Err(failure)),
        });
    }

    /// Ends a run handed over at the client's last DATA, on a worker.
    pub(super) fn filled(self: &Arc<Self>, stream: u32, handed: Box<Handed>, body: Vec<u8>) {
        let shared = Arc::clone(self);
        self.workers.run(Box::new(move || {
            let ended = guarded(|| blobs::run_ended(handed, &body));
            shared.end_stream(stream, ended);
        }));
    }

    /// Lets go of a getOrCreate the client cancelled, answering `cancelled`;
    /// says whether the stream was one.
    pub(super) fn cancel_fill(&self, stream: u32) -> bool {
        let cancelled = Failure::cancelled("the client cancelled its getOrCreate");
        match self.blobs.cancel_fill(stream) {
            Some(ClientFill::Asking(_)) => self.answer(stream, Err(cancelled)),
            Some(ClientFill::Handed(handed)) => {
                self.end_stream(stream, Err(cancelled));
                drop(handed);
            }
            None => return false,
        }
        true
    }
}
