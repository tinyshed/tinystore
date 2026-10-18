//! config's methods of protocol 2, protocol/config.wire: a program's SDK
//! opens a config with its declaration and its layers as data, changes it,
//! and follows it on one stream a config and a process, whose values the SDK
//! hands to every handle and watcher of that config in the process.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::codec::Message;
use super::protocol::{ConfigLine, ConfigOpen, ConfigReset, ConfigUpdate, ConfigValue, Failure, Handle, method};
use super::{Link, Route};
use crate::Store;
use crate::config::{Config, Sent, Snapshot, Watching};
use crate::sync::lock;

pub(crate) type Answered = Result<Vec<u8>, Failure>;

/// What a connection opened of configs, by handle, and the streams that
/// follow them, by stream.
#[derive(Debug, Default)]
pub(crate) struct Configs {
    open: Mutex<HashMap<u64, Config<Value>>>,
    last: AtomicU64,
    follows: Mutex<HashMap<u32, Arc<Following>>>,
}

pub(crate) fn route(called: u16) -> Route {
    match called {
        method::CONFIG_FOLLOW => Route::Follow,
        _ => Route::Worker,
    }
}

/// Answers a call of one message.
pub(crate) fn call(store: &Store, configs: &Configs, called: u16, body: &[u8], read_only: bool) -> Answered {
    match called {
        method::CONFIG_OPEN => open(store, configs, &ConfigOpen::decode(body)?, read_only),
        method::CONFIG_UPDATE => {
            let update = ConfigUpdate::decode(body)?;
            let config = configs.config(update.handle)?;
            config.update_data(update.version, &json(&update.changes, "changes")?)?;
            Ok(value(update.handle, &config))
        }
        method::CONFIG_RESET => {
            let reset = ConfigReset::decode(body)?;
            let config = configs.config(reset.handle)?;
            if reset.path.is_empty() {
                config.reset_all()?
            } else {
                config.reset(&reset.path)?
            }
            Ok(value(reset.handle, &config))
        }
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

/// Opens a config on this connection; one that may only read keeps no
/// declaration in kv.db.
fn open(store: &Store, configs: &Configs, open: &ConfigOpen, read_only: bool) -> Answered {
    let (declaration, layers) = (json(&open.declaration, "declaration")?, json(&open.layers, "layers")?);
    let sent = Sent {
        name: &open.name,
        declaration: &declaration,
        layers: &layers,
        strict: open.strict,
        remember: !read_only,
    };
    let config = store.config_sent(&sent)?;
    let handle = configs.last.fetch_add(1, Ordering::Relaxed) + 1;
    let answer = value(handle, &config);
    lock(&configs.open).insert(handle, config);
    Ok(answer)
}

fn json(text: &str, what: &str) -> Result<Value, Failure> {
    serde_json::from_str(text).map_err(|error| Failure::invalid(format!("a config's {what}: {error}")))
}

/// The config's value as it is now, as the wire carries it.
fn value(handle: u64, config: &Config<Value>) -> Vec<u8> {
    message(handle, &config.get(), config).encode()
}

/// A value of the config and where each of its settings came from.
fn message(handle: u64, snapshot: &Snapshot<Value>, config: &Config<Value>) -> ConfigValue {
    let explained = config.explain();
    let lines = explained.settings.into_iter().map(|line| ConfigLine {
        path: line.path,
        value: line.value,
        from: line.from,
        over: line.over,
    });
    ConfigValue {
        handle,
        version: snapshot.version(),
        value: snapshot.to_string(),
        lines: lines.collect(),
        notes: explained.notes,
    }
}

impl Configs {
    fn config(&self, handle: u64) -> Result<Config<Value>, Failure> {
        let open = lock(&self.open);
        open.get(&handle)
            .cloned()
            .ok_or_else(|| Failure::invalid(format!("handle {handle} names no config opened here")))
    }

    /// Credit the client granted a follow's stream.
    pub(crate) fn grant(&self, stream: u32, credit: u32) {
        let following = lock(&self.follows).get(&stream).cloned();
        if let Some(following) = following {
            following.grant(credit);
        }
    }

    /// Ends a follow the client cancelled; says whether there was one.
    pub(crate) fn cancel(&self, stream: u32) -> bool {
        let following = lock(&self.follows).remove(&stream);
        if let Some(following) = following {
            following.close(Err(Failure::cancelled("the client cancelled its follow")));
            return true;
        }
        false
    }

    /// Ends every follow: the server is closing, and the client follows
    /// again on another connection.
    pub(crate) fn go_away(&self) {
        let follows: Vec<_> = lock(&self.follows).drain().map(|(_, following)| following).collect();
        for following in follows {
            following.close(Err(Failure::unavailable("the server is closing; follow again on another connection")));
        }
    }

    /// Lets go of every follow and every config at the session's end.
    pub(crate) fn end(&self) {
        let follows: Vec<_> = lock(&self.follows).drain().map(|(_, following)| following).collect();
        for following in follows {
            following.end();
        }
        lock(&self.open).clear();
    }
}

/// Starts the follow a `config.follow` asks for; it sends nothing until
/// [`Following::begin`], once the stream's RESPONSE went out.
pub(crate) fn follow(configs: &Configs, number: u32, body: &[u8], link: Link) -> Result<Arc<Following>, Failure> {
    let handle = Handle::decode(body)?.handle;
    let config = configs.config(handle)?;
    let state = Mutex::new(FollowState { credit: link.credit, ..FollowState::default() });
    let following = Arc::new(Following { handle, config, watching: Mutex::new(None), state, link });
    lock(&configs.follows).insert(number, Arc::clone(&following));
    Ok(following)
}

/// A stream that follows a config: its value now, then each change, as DATA
/// within the client's credit. One that has no credit keeps the newest
/// value and sends it when credit comes, as a watcher that falls behind gets
/// the newest.
pub(crate) struct Following {
    handle: u64,
    config: Config<Value>,
    watching: Mutex<Option<Watching>>,
    state: Mutex<FollowState>,
    link: Link,
}

#[derive(Default)]
struct FollowState {
    credit: u64,
    waiting: Option<Vec<u8>>,
    ended: bool,
}

impl Following {
    /// Watches the config: its value now, then each change.
    pub(crate) fn begin(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let watching = self.config.watch(move |next, _| {
            if let Some(following) = me.upgrade() {
                following.push(message(following.handle, next, &following.config).encode());
            }
        });
        *lock(&self.watching) = Some(watching);
    }

    fn push(&self, body: Vec<u8>) {
        let mut state = lock(&self.state);
        if !state.ended {
            state.waiting = Some(body);
            self.flush(&mut state);
        }
    }

    fn grant(&self, credit: u32) {
        let mut state = lock(&self.state);
        state.credit += u64::from(credit);
        self.flush(&mut state);
    }

    /// Sends the value waiting once the credit takes it.
    fn flush(&self, state: &mut FollowState) {
        let credit = state.credit;
        if let Some(body) = state.waiting.take_if(|body| body.len() as u64 <= credit) {
            state.credit -= body.len() as u64;
            (self.link.send)(body);
        }
    }

    /// Ends the follow without a frame: the client cancelled it or left.
    /// Says whether it was still going.
    fn end(&self) -> bool {
        if std::mem::replace(&mut lock(&self.state).ended, true) {
            return false;
        }
        drop(lock(&self.watching).take());
        true
    }

    /// Ends the follow and its stream with `last`, unless it ended already.
    fn close(&self, last: Answered) {
        if self.end() {
            (self.link.finish)(last);
        }
    }
}

impl fmt::Debug for Following {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Following").field("handle", &self.handle).finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
