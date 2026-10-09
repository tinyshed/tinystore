// Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.

use std::collections::BTreeMap;

use super::codec::{self, Message, Out, Row};
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
    /// Kept in memory and written every interval; each add written when absent. Milliseconds.
    pub(crate) durability: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvCountersOpen {
    const NAME: &'static str = "kv.CountersOpen";
    const KEYS: &'static [u64] = &[1, 2, 3];

    fn read(fields: &Fields) -> Result<Self, Failure> {
        Ok(KvCountersOpen {
            name: fields.get(1, "name", codec::str)?.unwrap_or_default(),
            ttl: fields.get(2, "ttl", codec::uint)?,
            durability: fields.get(3, "durability", codec::uint)?,
        })
    }

    fn write(&self, out: &mut Out) {
        out.put(1, codec::str_value(&self.name));
        out.given(2, self.ttl.as_ref().map(codec::uint_value));
        out.given(3, self.durability.as_ref().map(codec::uint_value));
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

/// Every method's number, by its name in the schema.
pub(crate) mod method {
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
}

/// Every method, its name and number, for the test that the server answers each.
#[cfg(test)]
pub(crate) const METHODS: &[(&str, u16)] = &[
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
        _ => return None,
    };
    Some(rewritten)
}
