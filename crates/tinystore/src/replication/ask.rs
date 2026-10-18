//! A node asked, before a person promotes a copy, which files it serves: one
//! that still serves the copy's file is a home alive, or a copy of one, and a
//! promotion would make a second home of the file.

use std::path::Path;
use std::time::{Duration, Instant};

use super::Node;
use super::message::{Frames, Message, VERSION, timed_out};
use super::node::State;
use super::serve::Out;
use crate::{Error, ErrorKind, Result};

/// How long a node has to answer.
const ANSWER: Duration = Duration::from_secs(5);

/// The files the node at `address` serves, its own and its whole copies, as
/// its welcome says to the node in `dir` with `node`'s token and TLS; none
/// when nothing answers there. A node that refuses the link is an error.
pub fn served_by(dir: &Path, node: Node, address: &str) -> Result<Option<Vec<String>>> {
    let state = State::new(dir, node, 0)?;
    let Ok(tcp) = super::follow::connect(address) else {
        return Ok(None);
    };
    let (mut reading, writing) = super::transport::connect(&state, address, tcp)?;
    reading.set_timeout(Duration::from_millis(100))?;
    writing.set_timeout(ANSWER)?;
    let out = Out::new(writing);
    out.send(&Message::Hello {
        version: VERSION,
        token: state.options.token.clone().unwrap_or_default(),
        node: state.id.clone(),
        address: String::new(),
    })?;
    let mut frames = Frames::default();
    let deadline = Instant::now() + ANSWER;
    loop {
        match frames.next()? {
            Some(Message::Welcome { files, .. }) => return Ok(Some(files)),
            Some(Message::Refused { why }) => {
                return Err(Error::new(ErrorKind::Invalid, format!("{address} refused the link: {why}")));
            }
            Some(_) => continue,
            None => {}
        }
        if Instant::now() > deadline {
            return Ok(None);
        }
        match frames.fill(&mut reading) {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            Err(error) if timed_out(&error) => {}
            Err(error) => return Err(Error::io(format!("a link to {address}"), error)),
        }
    }
}
