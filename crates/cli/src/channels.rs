//! `tinystore channels`: a channel's messages from a shell. `publish` hands
//! one to whoever listens now, and `subscribe` prints what is published from
//! the moment it listens, both through the server that holds the directory,
//! or a sidecar they start: a channel lives in the memory of the process
//! that holds the store, so nothing is read beside it.
//!
//! ```text
//! tinystore channels ./data publish chat-events '{"kind": "reload"}'
//! tinystore channels ./data subscribe chat-events --for 1m
//! ```

use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};
use tinystore::client::Heard;

use crate::asked::{self, Asked, Done, Verb, wrong};
use crate::{help, reach};

const VERBS: [Verb; 2] = [
    Verb { word: "publish", command: &help::CHANNELS_PUBLISH, run: publish },
    Verb { word: "subscribe", command: &help::CHANNELS_SUBSCRIBE, run: subscribe },
];

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run_verb("channels", &VERBS, args)
}

/// Publishes a message, JSON, `-` reading it from stdin. It is sent once: a
/// message whose answer was lost may have been delivered, and a second
/// would be delivered too.
fn publish(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("channels", "publish", false)?;
    let [channel, given] = &rest[..] else {
        return Err(wrong(
            "publish takes a channel and the message's JSON: publish chat-events '{\"kind\": \"reload\"}'",
        ));
    };
    let under = branch_of(asked.value("--under"));
    reach::store_at(&dir)?;
    let message = asked::json_given(given, "a message")?;
    let (named, at) = (channel.clone(), under.clone());
    reach::write_once(&dir, "channels publish", move |client| client.publish(&named, &at, &message))?;
    outln!("published to {channel}{}", shown_branch(&under))
}

/// Prints each message published on the channel's branch from the moment
/// it listens, until `--for` has passed, or forever.
fn subscribe(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("channels", "subscribe", false)?;
    let [channel] = &rest[..] else {
        return Err(wrong("subscribe takes a channel: subscribe chat-events"));
    };
    let under = branch_of(asked.value("--under"));
    let lasting = asked.value("--for").map(asked::span).transpose().map_err(wrong)?;
    let (json, recursive) = (asked.has("--json"), asked.has("--recursive"));
    reach::store_at(&dir)?;
    let listening = format!("listening on {channel}{}", shown_branch(&under));
    let mut printed = Ok(());
    let mut each = |heard: Heard| {
        match heard {
            Heard::Registered => {
                asked::say("channels subscribe", &listening);
                if let Some(lasting) = lasting {
                    end_after(lasting);
                }
            }
            Heard::Batch { missed, messages } => {
                if missed > 0 {
                    asked::say("channels subscribe", format!("missed {missed}: it fell behind its branch"));
                }
                for (branch, value) in messages {
                    printed = print(&branch, &value, json);
                    if printed.is_err() {
                        return Err(tinystore::Error::new(tinystore::ErrorKind::Cancelled, "the reader has gone"));
                    }
                }
            }
        }
        Ok(())
    };
    let heard =
        reach::stream(&dir, "channels subscribe", |client| client.subscribe(channel, (&under, recursive), &mut each));
    printed?;
    heard
}

/// Ends the command once `lasting` has passed, as `--for` asks: the thread
/// that reads waits on the connection, which no timeout can stop on
/// Windows, so the process ends with what it printed, every line whole.
fn end_after(lasting: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(lasting);
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::process::exit(0);
    });
}

fn print(branch: &[String], value: &str, json: bool) -> Done {
    let value: Value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_owned()));
    match (json, branch.is_empty()) {
        (true, _) => outln!("{}", json!({ "under": branch, "value": value })),
        (false, true) => outln!("{value}"),
        (false, false) => outln!("{}  {value}", branch.join("/")),
    }
}

/// `--under`'s branch, its owners written as `42/devices`.
fn branch_of(given: Option<&str>) -> Vec<String> {
    given
        .map(|branch| branch.split('/').filter(|owner| !owner.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn shown_branch(under: &[String]) -> String {
    if under.is_empty() { String::new() } else { format!(" under {}", under.join("/")) }
}
