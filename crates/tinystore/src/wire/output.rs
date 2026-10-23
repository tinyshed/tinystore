//! What a session owes its client until the host takes it: the frames one
//! behind the other, in parts. Short frames are copied into the part they
//! follow; a long body is a part of its own, moved there rather than copied,
//! so that a file's bytes, read straight into the message that carries them,
//! are written once more only into what the host gives (issue #10):
//!
//! ```text
//! parts:  [ RESPONSE 7 · CREDIT · header of RESPONSE 8 ] [ RESPONSE 8's body ] [ DATA 9 ]
//!           copied in, one behind the other                moved in             copied
//! ```

use std::collections::VecDeque;

use super::frame::Frame;

/// A body this long goes out as a part of its own; a shorter one costs less
/// to copy than to keep apart.
const MOVED: usize = 64 << 10;
/// The room a session keeps for what it owes between two reads of it: a
/// result of megabytes read once does not stay a session's for good.
pub(super) const KEPT: usize = 1 << 20;

#[derive(Default)]
pub(super) struct Output {
    parts: VecDeque<Vec<u8>>,
    /// Whether the last part takes the next frames: it is no body moved in.
    open: bool,
    /// How many bytes of the first part a host that reads into its own has
    /// read: the rest is what it is owed.
    read: usize,
    /// A part read to its end, kept for the next frames.
    spare: Vec<u8>,
    pub(super) woken: bool,
    pub(super) ended: bool,
    /// Whether a thread waits in `take` for bytes: queueing them wakes it,
    /// and wakes nobody where the host is woken through its callback.
    pub(super) awaited: bool,
}

impl Output {
    pub(super) fn owes_nothing(&self) -> bool {
        self.parts.is_empty()
    }

    /// The bytes owed and not yet taken.
    #[cfg(feature = "channels")]
    pub(super) fn owed(&self) -> usize {
        self.parts.iter().map(Vec::len).sum::<usize>() - self.read
    }

    /// Writes a copy of `frame` behind what is owed.
    pub(super) fn copy(&mut self, frame: &Frame) {
        frame.encode_into(self.open_part());
    }

    /// Writes `frame` behind what is owed, its body moved there when long.
    pub(super) fn push(&mut self, frame: Frame) {
        if frame.body.len() < MOVED {
            return self.copy(&frame);
        }
        frame.encode_header_into(self.open_part());
        self.parts.push_back(frame.body);
        self.open = false;
    }

    /// Everything owed, as bytes of their own: the one part moved out, or
    /// the parts copied one behind the other.
    pub(super) fn take_all(&mut self) -> Vec<u8> {
        let read = std::mem::take(&mut self.read);
        self.open = false;
        if self.parts.len() == 1 {
            let mut bytes = self.parts.pop_front().unwrap_or_default();
            bytes.drain(..read);
            return bytes;
        }
        let owed = self.parts.iter().map(Vec::len).sum::<usize>() - read;
        let mut bytes = Vec::with_capacity(owed);
        for (n, part) in self.parts.drain(..).enumerate() {
            bytes.extend_from_slice(&part[if n == 0 { read } else { 0 }..]);
        }
        bytes
    }

    /// Copies what is owed into `into`, as much as fits, and says how much.
    /// A part read to its end is kept for the next frames, unless that is
    /// more room than a session keeps.
    pub(super) fn take_into(&mut self, into: &mut [u8]) -> usize {
        let mut taken = 0;
        while taken < into.len() {
            let Some(part) = self.parts.front() else { break };
            let owed = &part[self.read..];
            let copied = owed.len().min(into.len() - taken);
            into[taken..taken + copied].copy_from_slice(&owed[..copied]);
            taken += copied;
            self.read += copied;
            if self.read < part.len() {
                break;
            }
            self.read = 0;
            let part = self.parts.pop_front().unwrap_or_default();
            self.open &= !self.parts.is_empty();
            self.keep(part);
        }
        taken
    }

    /// Forgets everything owed.
    pub(super) fn clear(&mut self) {
        self.parts.clear();
        self.open = false;
        self.read = 0;
    }

    /// The part the next frame is written into: the last, or a new one.
    fn open_part(&mut self) -> &mut Vec<u8> {
        if !self.open {
            self.parts.push_back(std::mem::take(&mut self.spare));
            self.open = true;
        }
        self.parts.back_mut().expect("a part was just made open")
    }

    fn keep(&mut self, mut part: Vec<u8>) {
        if part.capacity() <= KEPT && part.capacity() > self.spare.capacity() {
            part.clear();
            self.spare = part;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::frame::Kind;

    fn frames() -> Vec<Frame> {
        let short = |stream, length| Frame::new(Kind::Data, stream, vec![stream as u8; length]);
        vec![short(1, 10), short(2, MOVED), short(3, 3), short(4, MOVED * 2), short(5, MOVED * 2), short(6, 0)]
    }

    fn written(frames: &[Frame]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for frame in frames {
            frame.encode_into(&mut bytes);
        }
        bytes
    }

    #[test]
    fn what_is_taken_is_the_frames_one_behind_the_other_whatever_the_room() {
        for room in [1, 7, 12, 4096, MOVED - 1, MOVED + 13, 1 << 20] {
            let mut output = Output::default();
            for frame in frames() {
                output.push(frame);
            }
            let mut taken = Vec::new();
            let mut into = vec![0; room];
            while !output.owes_nothing() {
                let read = output.take_into(&mut into);
                assert!(read == room || output.owes_nothing(), "a read fills its room unless nothing is left");
                taken.extend_from_slice(&into[..read]);
            }
            assert_eq!(taken, written(&frames()), "room {room}");
        }
    }

    #[test]
    fn all_of_it_taken_at_once_is_the_same_bytes_after_a_part_was_read() {
        let mut output = Output::default();
        for frame in frames() {
            output.push(frame);
        }
        let mut first = vec![0; 30];
        output.take_into(&mut first);
        let mut taken = first;
        taken.extend(output.take_all());
        assert_eq!(taken, written(&frames()));
        assert!(output.owes_nothing());
        output.copy(&Frame::new(Kind::Data, 9, b"after".to_vec()));
        assert_eq!(output.take_all(), written(&[Frame::new(Kind::Data, 9, b"after".to_vec())]));
    }

    #[test]
    fn a_long_body_is_moved_rather_than_copied() {
        let mut output = Output::default();
        let body = vec![7; MOVED];
        let at = body.as_ptr();
        output.push(Frame::new(Kind::Response, 1, body));
        assert_eq!(output.parts.len(), 2, "its header, then its body");
        assert_eq!(output.parts[1].as_ptr(), at, "the body's own bytes");
    }
}
