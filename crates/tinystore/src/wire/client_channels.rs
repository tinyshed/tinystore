//! A shell's messages on a channel: `channels publish`, and `channels
//! subscribe`, which receives what is published from the moment it is
//! registered, as long as the caller reads.

use std::io::{Read, Write};

use super::frame::{self, Frame, Kind};
use super::{Client, ended, unanswered};
use crate::Result;
use crate::wire::protocol::{self as wire, method};

/// What a subscription hears: that it listens, then what it receives at once.
#[derive(Debug)]
pub enum Heard {
    Registered,
    Batch {
        /// The messages lost since the last batch, which it fell too far
        /// behind to keep.
        missed: u64,
        /// Each message's branch, its owners outermost first, and its JSON.
        messages: Vec<(Vec<String>, String)>,
    },
}

impl<S: Read + Write> Client<S> {
    /// Publishes `value`, JSON's text, on the branch `under` of the channel.
    pub fn publish(&mut self, channel: &str, under: &[String], value: &str) -> Result<()> {
        let publish =
            wire::ChannelsPublish { name: channel.to_owned(), under: under.to_vec(), value: value.to_owned() };
        self.call(method::CHANNELS_PUBLISH, &publish).map(drop)
    }

    /// Subscribes to the branch `under` of the channel, and every branch
    /// below it when `recursive`, telling `each` once it listens and handing
    /// it what comes until the server ends the stream. Each batch's bytes go
    /// back as the stream's credit once `each` has it, so that a reader that
    /// keeps up never falls behind.
    pub fn subscribe(
        &mut self,
        channel: &str,
        (under, recursive): (&[String], bool),
        each: &mut dyn FnMut(Heard) -> Result<()>,
    ) -> Result<()> {
        let subscribe = wire::ChannelsSubscribe { name: channel.to_owned(), under: under.to_vec(), recursive };
        let stream = self.request(method::CHANNELS_SUBSCRIBE, &subscribe, true)?;
        loop {
            let frame = self.receive().map_err(unanswered)?;
            match frame.kind {
                _ if frame.stream == stream && frame.flags & frame::END != 0 => return ended(frame).map(drop),
                Kind::Response if frame.stream == stream => each(Heard::Registered)?,
                Kind::Data if frame.stream == stream => {
                    let length = frame.body.len() as u32;
                    let batch = super::decoded::<wire::ChannelsBatch>(&frame.body)?;
                    let messages = batch.messages.into_iter().map(|message| (message.under, message.value)).collect();
                    each(Heard::Batch { missed: batch.missed, messages })?;
                    self.send(&Frame::new(Kind::Credit, stream, length.to_le_bytes().to_vec())).map_err(unanswered)?;
                }
                _ => self.take(frame).map_err(unanswered)?,
            }
        }
    }
}
