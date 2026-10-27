//! What `tinystore` alone, `tinystore help` and `--help` print: the commands
//! by what a person does with them, and each command's flags and examples,
//! coloured on a terminal and plain in a pipe. Help is no error, so it goes to
//! stdout and exits 0. The parser reads its flags from the same table, so a
//! flag the help names is a flag the command takes.

use nu_ansi_term::{Color, Style};

use crate::asked::Done;
use crate::lines::{Terminal, paint};

/// A command as help shows it and the parser reads it.
pub(crate) struct Command {
    /// `status`, or `node promote` for a command of a group.
    pub(crate) name: &'static str,
    pub(crate) takes: &'static str,
    pub(crate) does: &'static str,
    pub(crate) flags: &'static [Flag],
    pub(crate) examples: &'static [&'static str],
    /// What a person needs beyond the flags, a sentence or two.
    pub(crate) more: &'static str,
}

/// A flag: its name, the value it takes, `""` for a switch, what it means,
/// and the heading it is shown under, `""` for none and `HIDDEN` for a flag
/// only the SDKs pass.
pub(crate) struct Flag {
    pub(crate) name: &'static str,
    pub(crate) value: &'static str,
    pub(crate) means: &'static str,
    pub(crate) heading: &'static str,
}

/// The heading of a flag that only the SDKs pass, which help leaves out.
pub(crate) const HIDDEN: &str = "hidden";

const fn flag(name: &'static str, value: &'static str, means: &'static str) -> Flag {
    Flag { name, value, means, heading: "" }
}

const fn under(heading: &'static str, name: &'static str, value: &'static str, means: &'static str) -> Flag {
    Flag { name, value, means, heading }
}

const JSON: Flag = flag("--json", "", "one JSON document, for scripts");

pub(crate) const STATUS: Command = Command {
    name: "status",
    takes: "[dir]",
    does: "what the store holds, who serves it, how its node stands",
    flags: &[JSON],
    examples: &["tinystore status ./data", "tinystore status ./data --json"],
    more: "A directory no server holds says so and exits 0.",
};

pub(crate) const LOGS: Command = Command {
    name: "logs",
    takes: "[dir]",
    does: "the application's logs, and new ones as they arrive with -f",
    flags: &[
        flag("-f", "", "follow new lines as they come, until Ctrl+C"),
        flag("-n", "<count>", "how many of the last lines to print first; 20 unless --since"),
        flag("--level", "<level>", "trace, debug, info, warn or error, and above"),
        flag("--since", "<span>", "lines from the last span, as 30s, 15m, 1h or 2d"),
        flag("--grep", "<text>", "lines that hold the text, in any case"),
        flag("--stream", "<name>", "one stream, as api"),
        flag("--json", "", "one JSON object a line"),
    ],
    examples: &["tinystore logs ./data -f", "tinystore logs ./data --since 1h --level warn"],
    more: "It reads beside the program or server that holds the directory, and writes nothing.",
};

/// `--database`, which every kv verb takes.
const DATABASE: Flag =
    flag("--database", "<db>", "a bucket the program opened from this database, kept in sql/<db>.db");

/// What a command that writes through a server says of it.
const WRITES: &str = "It writes through the server that holds the directory, or starts one that stops 30 s after its \
                      last client; a directory a program holds inside its own process is refused.";

pub(crate) const KV_GET: Command = Command {
    name: "kv get",
    takes: "[dir] get <bucket> <key>",
    does: "one value of a bucket",
    flags: &[DATABASE, flag("--json", "", "the value with its key and expiry, as JSON")],
    examples: &["tinystore kv ./data get sessions 42", "tinystore kv ./data get drafts 7 --database app"],
    more: "It reads beside the program or server that holds the directory, and an encrypted bucket's values with the \
           key the directory holds.",
};

pub(crate) const KV_LIST: Command = Command {
    name: "kv list",
    takes: "[dir] list <bucket> [branch]",
    does: "a bucket's keys and their values, or a branch's",
    flags: &[DATABASE, flag("--json", "", "each value with its key and expiry, as JSON")],
    examples: &["tinystore kv ./data list sessions", "tinystore kv ./data list sessions 42/devices"],
    more: "It reads beside the program or server that holds the directory, the first 1000 keys.",
};

pub(crate) const KV_SET: Command = Command {
    name: "kv set",
    takes: "<dir> set <bucket> <key> <value>",
    does: "set a key's value",
    flags: &[
        flag("--as", "<type>", "text, int or json: what the value is, text when not given"),
        flag("--ttl", "<span>", "expire the key this long from now, as 1h"),
        DATABASE,
    ],
    examples: &[
        "tinystore kv ./data set flags beta on --ttl 1h",
        "tinystore kv ./data set counts hits 42 --as int",
        "echo '{\"theme\": \"dark\"}' | tinystore kv ./data set settings 42 - --as json",
    ],
    more: "A value is text unless --as says otherwise, and nothing is guessed from how it looks, so 02134 stays \
           text. - reads the value from stdin, as Windows PowerShell 5.1 drops the quotes inside a JSON argument. \
           Sending the same set again changes nothing. It writes through the server that holds the directory, or \
           starts one that stops 30 s after its last client; a directory a program holds inside its own process is \
           refused.",
};

pub(crate) const JOBS_ADD: Command = Command {
    name: "jobs add",
    takes: "<dir> add <queue> <json>",
    does: "add a job to a queue",
    flags: &[
        flag("--id", "<id>", "what names the job: a queue that holds it adds nothing, and cancel needs it"),
        flag("--delay", "<span>", "run it this long from now, as 1m"),
        flag("--at", "<time>", "run it at this time, as 14:30 or 2026-10-27T09:00:00Z"),
        flag("--database", "<db>", "a queue the program opened from this database, kept in sql/<db>.db"),
    ],
    examples: &[
        "tinystore jobs ./data add emails '{\"to\": \"ada@example.com\"}' --id welcome-42 --delay 1m",
        "echo '{\"to\": \"ada@example.com\"}' | tinystore jobs ./data add emails -",
    ],
    more: "A job is JSON; - reads it from stdin. It runs in the program whose worker takes the queue, with that \
           worker's attempts and timeout. With --id, sending the same add again adds nothing. It writes through the \
           server that holds the directory, or starts one that stops 30 s after its last client; a directory a \
           program holds inside its own process is refused.",
};

pub(crate) const JOBS_LIST: Command = Command {
    name: "jobs list",
    takes: "[dir] list <queue> [prefix]",
    does: "a queue's jobs that have ids, with their state",
    flags: &[
        flag("--state", "<state>", "scheduled, waiting, running, done or failed alone"),
        flag("--limit", "<count>", "the jobs a page holds, 100 when not given, 1000 at most"),
        flag("--after", "<id>", "the page after this id, as the end of the last one says"),
        flag("--database", "<db>", "a queue the program opened from this database, kept in sql/<db>.db"),
        JSON,
    ],
    examples: &["tinystore jobs ./data list emails --state failed", "tinystore jobs ./data list emails welcome-"],
    more: "It reads beside the program or server that holds the directory. --json gives a line a job, and a last \
           line {\"next\": …} when there are more.",
};

pub(crate) const JOBS_CANCEL: Command = Command {
    name: "jobs cancel",
    takes: "<dir> cancel <queue> <id>",
    does: "cancel a job by its id",
    flags: &[flag("--database", "<db>", "a queue the program opened from this database, kept in sql/<db>.db")],
    examples: &["tinystore jobs ./data cancel emails welcome-42"],
    more: WRITES,
};

pub(crate) const FILES_PUT: Command = Command {
    name: "files put",
    takes: "<dir> put <set> <path> <file>",
    does: "write a local file at a path of a set",
    flags: &[flag("--type", "<type>", "its content type, as image/png; from its extension when not given")],
    examples: &[
        "tinystore files ./data put avatars users/42/avatar.png ./avatar.png",
        "pg_dump app | tinystore files ./data put dumps app.sql -",
    ],
    more: "It replaces whatever the path held, so sending it again leaves the same. - reads the bytes from stdin. A \
           file without --type takes the type of its extension when it is a common one, and is bytes otherwise. It \
           writes through the server that holds the directory, or starts one that stops 30 s after its last client; \
           a directory a program holds inside its own process is refused.",
};

pub(crate) const FILES_GET: Command = Command {
    name: "files get",
    takes: "[dir] get <set> <path> <file>",
    does: "write a set's file to a local path",
    flags: &[flag("--force", "", "write over a file that is there")],
    examples: &["tinystore files ./data get avatars users/42/avatar.png ./avatar.png"],
    more: "It reads beside the program or server that holds the directory, checks the bytes against their SHA-256, \
           and writes beside the path before it renames, never to stdout, since PowerShell's > writes bytes back as \
           text.",
};

pub(crate) const FILES_LIST: Command = Command {
    name: "files list",
    takes: "[dir] list <set> [prefix]",
    does: "a set's files, their size, time and type",
    flags: &[
        flag("--limit", "<count>", "the files a page holds, 100 when not given, 1000 at most"),
        flag("--after", "<path>", "the page after this path, as the end of the last one says"),
        JSON,
    ],
    examples: &["tinystore files ./data list avatars", "tinystore files ./data list avatars users/42/"],
    more: "It reads beside the program or server that holds the directory, in the byte order of the paths.",
};

pub(crate) const FILES_DELETE: Command = Command {
    name: "files delete",
    takes: "<dir> delete <set> <path>",
    does: "delete a set's file",
    flags: &[],
    examples: &["tinystore files ./data delete avatars users/42/avatar.png"],
    more: WRITES,
};

pub(crate) const CHANNELS_PUBLISH: Command = Command {
    name: "channels publish",
    takes: "<dir> publish <channel> <json>",
    does: "hand a message to whoever listens now",
    flags: &[flag("--under", "<branch>", "the branch it goes to, its owners as 42/devices")],
    examples: &[
        "tinystore channels ./data publish chat-events '{\"kind\": \"reload\"}'",
        "tinystore channels ./data publish chat-events '{\"seen\": 7}' --under room-42",
    ],
    more: "A message is JSON; - reads it from stdin. A subscription listening now receives it, and no one else ever \
           will. It is sent once, never again after a lost answer, since a second would be delivered too. It goes \
           through the server that holds the directory, or starts one that stops 30 s after its last client.",
};

pub(crate) const CHANNELS_SUBSCRIBE: Command = Command {
    name: "channels subscribe",
    takes: "<dir> subscribe <channel>",
    does: "print each message published from now on",
    flags: &[
        flag("--for", "<span>", "stop after this long, as 1m; never when not given"),
        flag("--under", "<branch>", "listen to this branch, its owners as 42/devices"),
        flag("--recursive", "", "and to every branch below it"),
        flag("--json", "", "each message with its branch, as JSON"),
    ],
    examples: &["tinystore channels ./data subscribe chat-events --for 1m"],
    more: "A channel lives in the process that holds the store, so it listens through the server that holds the \
           directory, or one it starts. stderr says when it listens; a message published before is not received.",
};

pub(crate) const KV_DELETE: Command = Command {
    name: "kv delete",
    takes: "<dir> delete <bucket> <key>",
    does: "delete a key",
    flags: &[DATABASE],
    examples: &["tinystore kv ./data delete sessions 42"],
    more: WRITES,
};

pub(crate) const SQL: Command = Command {
    name: "sql",
    takes: "[dir] <db> \"<query>\"",
    does: "a query's rows; reads only unless --write",
    flags: &[flag("--write", "", "let the statement write, here or through the server that holds the directory"), JSON],
    examples: &[
        "tinystore sql ./data app \"select count(*) from users\"",
        "tinystore sql ./data app \"delete from users where id = 7\" --write",
    ],
    more: "Without --write it reads beside the program or server that holds the directory. With it, a store no one            holds is opened here; one a server holds is written through it, once, and there a statement that gives            rows back is refused; one a program holds inside its own process is refused.",
};

pub(crate) const SEARCH: Command = Command {
    name: "search",
    takes: "list [dir]",
    does: "the store's search indexes; drop <dir> <name> drops one",
    flags: &[],
    examples: &["tinystore search list ./data", "tinystore search drop ./data posts"],
    more: "An index no program opens any more is kept until it is dropped, since another version may open it again.",
};

pub(crate) const SERVE: Command = Command {
    name: "serve",
    takes: "<dir>",
    does: "share the store with programs here and elsewhere, until Ctrl+C",
    flags: &[
        under("local", "--stdio", "", "one client on stdin and stdout, for a program that starts the server itself"),
        under("remote", "--listen", "tls://<host>:<port>", "serve other machines over TLS"),
        under("remote", "--tls-cert", "<pem>", "the certificate chain it answers with"),
        under("remote", "--tls-key", "<pem>", "its private key"),
        under("remote", "--tokens", "<file>", "who may connect: admin, data and read tokens, a line each"),
        under("node", "--node-listen", "<host>:<port>", "where other nodes copy from this one"),
        under("node", "--from", "<host>:<port>", "a node it copies from; again for the next to try"),
        under("node", "--node-config", "<file.toml>", "the rest of what the node is"),
        under("limits", "--memory", "<size>", "what the engines may hold at once, as 512MiB; 1GiB with --listen"),
        under("limits", "--idle", "<span>", "stop once no client has been connected this long; never by default"),
        under("store", "--durability", "full|os", "how far a commit goes before it returns; each engine's own"),
        under("store", "--keep-free", "<size>", "the disk the store keeps free; 1GiB by default"),
        under("store", "--encryption-key-file", "<file>", "the store's key, kept apart from its data"),
        under("store", "--older-encryption-key", "<file>", "an older key, which opens what it sealed; again for more"),
        under("store", "--self-metrics", "", "measure the store into its own metrics, as tinystore_ names"),
        under("store", "--no-system-metrics", "", "with --self-metrics, leave out the machine it runs on"),
        under("store", "--max-series", "<count>", "the metrics series the store holds at most; 100000 by default"),
        under("output", "--log-file", "<file>", "JSON lines to a file as well"),
        under("output", "--quiet", "", "the banner, warnings and errors only"),
        under(HIDDEN, "--local", "", "a sidecar an SDK started"),
        under(HIDDEN, "--clock", "<time>", "a private server's test clock"),
    ],
    examples: &[
        "tinystore serve ./data",
        "tinystore serve ./data --listen tls://0.0.0.0:7443 --tls-cert cert.pem --tls-key key.pem --tokens tokens.txt",
        "tinystore serve ./data --node-listen 0.0.0.0:7444 --from us.internal:7444 --node-config node.toml",
    ],
    more: "Programs on this machine find it through <dir>/server/SERVE. A node's rarer settings live in its \
           --node-config file: files, token-file, catalog, advertise, synchronous, synchronous-with, failover, \
           insecure and [tls] cert, key and roots.",
};

pub(crate) const STOP: Command = Command {
    name: "stop",
    takes: "<dir>",
    does: "stop the server of a directory, its work finished",
    flags: &[],
    examples: &["tinystore stop ./data"],
    more: "The clients are told to leave and the streams running finish. With no server it says so and exits 1.",
};

pub(crate) const BACKUP: Command = Command {
    name: "backup",
    takes: "<dir> <zip>",
    does: "the whole store in one checked zip, while it runs",
    flags: &[
        flag("--include", "<name>", "a file of the program's own in the directory; again for more"),
        flag("--include-encryption-key", "", "the store's key too, which a backup leaves out by default"),
        flag("--encryption-key-file", "<file>", "where the store's key is, when not in the directory"),
    ],
    examples: &["tinystore backup ./data backup.zip"],
    more: "It reads beside a server or program that holds the directory, and alone when none does.",
};

pub(crate) const RESTORE: Command = Command {
    name: "restore",
    takes: "<zip> <dir>",
    does: "a backup into an empty directory",
    flags: &[],
    examples: &["tinystore restore backup.zip ./data2"],
    more: "Every byte is checked against the backup's manifest, or nothing is left behind.",
};

pub(crate) const MCP: Command = Command {
    name: "mcp",
    takes: "[dir]",
    does: "let an AI agent read the store, and write what --write names",
    flags: &[flag(
        "--write",
        "<tools>",
        "the tools that write it offers, as kv_set,jobs_add, or all; none when absent",
    )],
    examples: &[
        "claude mcp add tinystore -- tinystore mcp ./data",
        "claude mcp add tinystore -- tinystore mcp ./data --write kv_set,jobs_add",
    ],
    more: "The Model Context Protocol on stdin and stdout. Its tools that read: status, logs, kv_get, kv_list, \
           jobs_list, sql_query and search_list. Those that write, a tool an operation, only as --write names them: \
           kv_set, kv_delete, jobs_add, jobs_cancel, files_put, files_delete, events_append, channels_publish, \
           config_update, config_reset and sql_exec. Each is marked as writing, and as destructive where it removes \
           or replaces something, so that the agent's host asks for each; they write through the server that holds \
           the directory, or one they start.",
};

pub(crate) const TOKEN: Command = Command {
    name: "token",
    takes: "new [admin|data|read]",
    does: "a token line for serve's --tokens",
    flags: &[],
    examples: &["tinystore token new admin >> tokens.txt"],
    more: "admin may do anything, data reads and writes the store, read only reads it.",
};

pub(crate) const NODE_STATUS: Command = Command {
    name: "node status",
    takes: "[dir]",
    does: "the node's files, its links and how far behind its copies are",
    flags: &[JSON],
    examples: &["tinystore node status ./data"],
    more: "",
};

pub(crate) const NODE_PROMOTE: Command = Command {
    name: "node promote",
    takes: "<dir> [file]",
    does: "make a copy the home, once its old home is gone",
    flags: &[
        flag("--from", "<host>:<port>", "a node the copy took its files from, asked first; again for more"),
        flag("--node-config", "<file.toml>", "the node's settings, for the token and TLS that reach them"),
        flag("--force", "", "promote though a node it took from still answers"),
    ],
    examples: &["tinystore node promote ./data", "tinystore node promote ./data sql/app.db --from us.internal:7444"],
    more: "The node must be stopped. Each node named by --from is asked first: one that still serves the file would \
           make a second home, so the promotion is refused unless --force.",
};

pub(crate) const NODE_DEMOTE: Command = Command {
    name: "node demote",
    takes: "<dir> [file]",
    does: "make an old home's files copies again",
    flags: &[],
    examples: &["tinystore node demote ./data"],
    more: "The node must be stopped. Start it again with --from its new home to take each file again.",
};

pub(crate) const WITNESS: Command = Command {
    name: "witness",
    takes: "<dir> --listen <host>:<port>",
    does: "decide which node writes a file when its home goes quiet",
    flags: &[
        flag("--listen", "<host>:<port>", "where nodes reach it"),
        flag("--token-file", "<file>", "the token nodes say to it"),
        flag("--tls-cert", "<pem>", "the certificate chain it answers with"),
        flag("--tls-key", "<pem>", "its private key"),
        flag("--insecure", "", "plain TCP beyond this machine, which anyone on the way reads"),
        flag("--quiet", "", "warnings and errors only"),
    ],
    examples: &["tinystore witness ./witness --listen 0.0.0.0:7445 --token-file token"],
    more: "A witness keeps each file's lease and no data; nodes name it as failover in their node config.",
};

pub(crate) const MIGRATE: Command = Command {
    name: "migrate",
    takes: "<dir> <db> <migrations>",
    does: "apply a database's migration files",
    flags: &[],
    examples: &["tinystore migrate ./data app ./migrations"],
    more: "It applies the files the database has not applied, in one transaction, and refuses one that applied an \
           edited, renamed or missing file. No program may hold the directory meanwhile.",
};

pub(crate) const SCHEMA: Command = Command {
    name: "schema",
    takes: "<dir> [db]",
    does: "a database's tables, columns and indexes",
    flags: &[JSON],
    examples: &["tinystore schema ./data app", "tinystore schema ./data"],
    more: "Without a database it lists the store's databases.",
};

const WHERE: Flag =
    flag("--where", "<label=value>", "series whose label matches: a,b for either, != for neither, a* for a prefix");
const SINCE: Flag = flag("--since", "<span>", "the last span, as 15m, 1h or 2d; the last hour by default");
const FROM: Flag = flag("--from", "<time>", "from a time: 14:30 today, 2026-10-25T03:00, or one with its offset");
const TO: Flag = flag("--to", "<time>", "up to a time, now by default");

pub(crate) const METRICS_NAMES: Command = Command {
    name: "metrics names",
    takes: "[dir] names [prefix]",
    does: "the metrics a store holds: kind, series, keep, newest sample",
    flags: &[flag("--after", "<name>", "the page after this name, as the end of the last one says"), JSON],
    examples: &["tinystore metrics ./data names", "tinystore metrics ./data names http_"],
    more: "It reads beside the program or server that holds the directory.",
};

pub(crate) const METRICS_READ: Command = Command {
    name: "metrics read",
    takes: "[dir] read <name>",
    does: "a metric's samples, a series at a time",
    flags: &[
        WHERE,
        SINCE,
        FROM,
        TO,
        flag("--gaps", "<span>", "mark where a series had no sample for longer than the span"),
        flag("--limit", "<count>", "how many samples a page holds; 1000 by default, 100000 at most"),
        flag("--after", "<next>", "the page after this one, as the end of the last one says"),
        flag("--json", "", "one JSON object a sample, the time in UTC, and the next page's last"),
    ],
    examples: &[
        "tinystore metrics ./data read ping_rtt_ms --where target=poland --since 1h",
        "tinystore metrics ./data read ping_rtt_ms --from 14:30 --to 14:35 --gaps 5s",
    ],
    more: "It reads beside the program or server that holds the directory. A time without its offset is this \
           machine's, and the range is printed first with its zone.",
};

pub(crate) const METRICS_AGGREGATE: Command = Command {
    name: "metrics aggregate",
    takes: "[dir] aggregate <name> --op <op>",
    does: "a metric's samples in buckets: max, rate, p95 and more",
    flags: &[
        flag("--op", "<op>", "count, sum, avg, min, max, increase, rate, delta, first, last, or p95 and any rank"),
        flag("--every", "<span>", "a bucket a span, as 1m; one bucket for the range when absent"),
        flag("--by", "<label>", "a group a value of the label; again for more"),
        WHERE,
        SINCE,
        FROM,
        TO,
        flag("--limit", "<count>", "how many buckets a page holds; 1000 by default, 100000 at most"),
        flag("--after", "<next>", "the page after this one, as the end of the last one says"),
        flag("--json", "", "one JSON object a bucket, the time in UTC, and the next page's last"),
    ],
    examples: &[
        "tinystore metrics ./data aggregate ping_rtt_ms --since 1h --every 10m --op max --by target",
        "tinystore metrics ./data aggregate http_requests_total --since 24h --every 1h --op increase",
    ],
    more: "It reads beside the program or server that holds the directory. A terminal gets a table, a row a \
           bucket and a column a group.",
};

pub(crate) const METRICS_DELETE: Command = Command {
    name: "metrics delete",
    takes: "<dir> delete <name> [label=value]...",
    does: "delete one series, named by all its labels",
    flags: &[],
    examples: &["tinystore metrics ./data delete ping_rtt_ms target=poland"],
    more: "It deletes the one series whose labels are exactly these, and writes through the server that holds \
           the directory, or starts one.",
};

pub(crate) const EVENTS_LIST: Command = Command {
    name: "events list",
    takes: "[dir] list <stream>",
    does: "a stream's events, oldest first",
    flags: &[
        flag("--where", "<field=text|field:=json>", "events whose field holds the value; again for more"),
        flag("--level", "<level>", "trace, debug, info, warn or error, and above"),
        SINCE,
        FROM,
        TO,
        flag("--limit", "<count>", "how many events a page holds; 100 by default"),
        flag("--after", "<next>", "the page after one, as its end says"),
        flag("--json", "", "one JSON object an event, as logs --json prints"),
    ],
    examples: &[
        "tinystore events ./data list audit --since 1h",
        "tinystore events ./data list audit --where user:=42 --where role=admin",
    ],
    more: "It reads beside the program or server that holds the directory, and prints each event as logs does.",
};

pub(crate) const EVENTS_APPEND: Command = Command {
    name: "events append",
    takes: "<dir> append <stream> [name] [field=text|field:=json]...",
    does: "write an event, or another program's output on stdin, to a stream",
    flags: &[
        flag("--id", "<id>", "what names the event in its stream: an id it holds writes nothing"),
        flag("--at", "<time>", "when it happened: 14:30 today, 2026-10-25T03:00, or one with its offset"),
        flag("--level", "<level>", "trace, debug, info, warn or error"),
    ],
    examples: &[
        "tinystore events ./data append audit role.changed user:=42 role=admin",
        "my-worker 2>&1 | tinystore events ./data append worker",
    ],
    more: "With a name it writes one event, field=text a text and field:=json any JSON value. Without one it writes \
           stdin a line an event, a stack trace joined into one, JSON and logfmt lines kept as their fields. It \
           writes through the server that holds the directory, or starts one.",
};

pub(crate) const METRICS_INGEST: Command = Command {
    name: "metrics ingest",
    takes: "<dir> ingest",
    does: "write the Prometheus text on stdin to the store's metrics",
    flags: &[
        flag("-f", "", "follow a stream that does not end, written every second; a line it cannot read is skipped"),
        flag("--kind", "<name=type>", "a name's type where the stream gives no # TYPE, as x_total=counter; with -f"),
        flag("--label", "<name=value>", "a label every series of the text gets; again for more"),
        flag("--keep", "<span>", "how long each name of the text keeps its samples, as 30d; 30d when never given"),
    ],
    examples: &[
        "curl -s localhost:9100/metrics | tinystore metrics ./data ingest --label instance=web-1",
        "tinystore metrics ./data ingest --keep 30d < scrape.txt",
        "ping … | awk '…' | tinystore metrics ./data ingest -f --kind ping_lost_total=counter",
    ],
    more: "Every series of the text is written, or none, in Prometheus' format or OpenMetrics'. With -f, Prometheus' \
           text alone is read a line at a time and written every second or 10,000 lines; a line it cannot read, or \
           the store refuses, is said and skipped, and the command ends 1. A name with no type is a gauge. It writes \
           through the server that holds the directory, or starts one that stops 30 s after its last client; a \
           directory a program holds inside its own process is refused. A sample given no time takes the time it \
           was read, the same when a lost write is sent again.",
};

/// What each word a command takes stands for, as `help --json` and the
/// site's explain say it: every `<word>` and `[word]` of a command's `takes`
/// is here, and nothing else.
pub(crate) const WORDS: [(&str, &str); 35] = [
    ("<bucket>", "a kv bucket's name"),
    ("<channel>", "a channel's name"),
    ("<command>", "the command to run, with its own arguments"),
    ("<config>", "a config's name, as its program opened it"),
    ("<db>", "one of the application's databases, kept in sql/<db>.db"),
    ("<dir>", "the store's directory"),
    ("<file>", "a file on this machine"),
    ("<host>", "a host name or an address"),
    ("<id>", "a job's id"),
    ("<json>", "JSON: an object, a list, a string or a number; - reads it from stdin"),
    ("<key>", "a key of the bucket"),
    ("<lock>", "a lock's name"),
    ("<migrations>", "a directory of migration files, 0001_users.sql and on"),
    ("<name>", "the name of what the command reads or changes: a metric, or an index"),
    ("<op>", "what an aggregate computes: count, sum, avg, min, max, increase, rate, or p and a rank, as p95"),
    ("<path=value>", "a live setting's path and its value, as model.max_tokens=2048; path:=json for JSON"),
    ("<path>", "a file's path in its set"),
    ("<port>", "a port number"),
    ("<query>", "one SQL statement, quoted"),
    ("<queue>", "a queue's name"),
    ("<set>", "a set of files' name"),
    ("<shell>", "bash, zsh, fish or powershell"),
    ("<stream>", "a stream of events' name"),
    ("<value>", "the value: text unless --as says int or json; - reads it from stdin"),
    ("<zip>", "a backup's zip file"),
    ("[admin|data|read]", "what a token lets its client do: everything, data alone, or read"),
    ("[branch]", "the keys under a branch, its owners as 42/devices"),
    ("[db]", "one of the application's databases; every one when left out"),
    ("[dir]", "the store's directory; the current one when left out"),
    ("[field=text|field:=json]", "what an event carries: field=text for text, field:=json for JSON"),
    ("[file]", "one file of the store, as sql/app.db; every one when left out"),
    ("[label=value]", "the series' labels, exactly"),
    ("[name]", "the event's name; without one, each line of stdin is an event"),
    ("[path]", "a setting or a group; every change when left out"),
    ("[prefix]", "only the names, ids or paths that start with it"),
];

/// The commands by what a person does with them.
pub(crate) const GROUPS: [(&str, &[&Command]); 5] = [
    (
        "Look",
        &[
            &STATUS,
            &LOGS,
            &KV_GET,
            &KV_LIST,
            &JOBS_LIST,
            &FILES_LIST,
            &FILES_GET,
            &CONFIG_EXPLAIN,
            &SQL,
            &SEARCH,
            &METRICS_NAMES,
            &METRICS_READ,
            &METRICS_AGGREGATE,
            &EVENTS_LIST,
        ],
    ),
    (
        "Write",
        &[
            &KV_SET,
            &KV_DELETE,
            &JOBS_ADD,
            &JOBS_CANCEL,
            &FILES_PUT,
            &FILES_DELETE,
            &CHANNELS_PUBLISH,
            &CHANNELS_SUBSCRIBE,
            &CONFIG_UPDATE,
            &CONFIG_RESET,
            &METRICS_INGEST,
            &METRICS_DELETE,
            &EVENTS_APPEND,
        ],
    ),
    ("Run", &[&SERVE, &STOP, &BACKUP, &RESTORE, &MCP, &TOKEN, &LOCK_HOLD, &COMPLETION]),
    ("Nodes", &[&NODE_STATUS, &NODE_PROMOTE, &NODE_DEMOTE, &WITNESS]),
    ("Develop", &[&MIGRATE, &SCHEMA]),
];

/// Every command by its name, for `help <command>` and a misspelt one.
pub(crate) fn command(name: &str) -> Option<&'static Command> {
    GROUPS.iter().flat_map(|(_, commands)| commands.iter().copied()).find(|command| command.name == name)
}

/// The words a command may begin with, for a misspelt one's nearest.
pub(crate) fn first_words() -> Vec<&'static str> {
    let mut words: Vec<&str> = GROUPS
        .iter()
        .flat_map(|(_, commands)| commands.iter())
        .map(|command| command.name.split(' ').next().unwrap_or(command.name))
        .collect();
    words.extend(["help", "version"]);
    let mut seen = std::collections::HashSet::new();
    words.retain(|word| seen.insert(*word));
    words
}

/// The help, coloured when `colours`.
pub(crate) fn text(colours: bool) -> String {
    let paint = |style: Style, text: &str| paint(colours, style, text);
    let (bold, dim) = (Style::new().bold(), Style::new().dimmed());
    let (named, takes) = (Color::Green.bold(), Color::Cyan.normal());
    let mut help = format!("{} {}\n", paint(bold, "tinystore"), paint(dim, env!("CARGO_PKG_VERSION")));
    let rows = || GROUPS.iter().flat_map(|(_, commands)| commands.iter());
    let spelled = |command: &Command| {
        let (name, args) = split_name(command);
        name.len() + 1 + args.len()
    };
    let width = rows().map(|command| spelled(command)).max().unwrap_or(0);
    for (group, commands) in GROUPS {
        help.push_str(&format!("\n{}\n", paint(bold, group)));
        for command in commands {
            let (name, args) = split_name(command);
            let pad = " ".repeat(width - spelled(command) + 2);
            help.push_str(&format!("  {} {}{pad}{}\n", paint(named, &name), paint(takes, &args), command.does));
        }
    }
    help.push_str(&format!(
        "\n{}\n",
        paint(dim, "tinystore help <command> · --version · --json for scripts · docs: tinyshed.org/tinystore")
    ));
    help
}

/// The name as the list shows it: a group's command with its word after it
/// among what it takes, `node` then `status [dir]`, so that the columns line up;
/// a word that follows the directory is spelled where it goes, `metrics` then
/// `<dir> ingest`.
fn split_name(command: &Command) -> (String, String) {
    match command.name.split_once(' ') {
        Some((group, word)) if command.takes.split(' ').any(|taken| taken == word) => {
            (group.to_owned(), command.takes.to_owned())
        }
        Some((group, word)) => (group.to_owned(), format!("{word} {}", command.takes)),
        None => (command.name.to_owned(), command.takes.to_owned()),
    }
}

/// One command's help: how it is called, its flags under their headings, and
/// an example of each use.
pub(crate) fn of_command(colours: bool, command: &Command) -> String {
    let paint = |style: Style, text: &str| paint(colours, style, text);
    let (bold, dim, named, takes) =
        (Style::new().bold(), Style::new().dimmed(), Color::Green.bold(), Color::Cyan.normal());
    let (name, args) = split_name(command);
    let mut help =
        format!("{} {} {}\n{}\n", paint(bold, "tinystore"), paint(named, &name), paint(takes, &args), command.does);
    let shown: Vec<&Flag> = command.flags.iter().filter(|flag| flag.heading != HIDDEN).collect();
    let width = shown.iter().map(|flag| flag.name.len() + 1 + flag.value.len()).max().unwrap_or(0);
    let mut heading = None;
    for (at, flag) in shown.iter().enumerate() {
        if at == 0 || heading != Some(flag.heading) {
            help.push('\n');
            if !flag.heading.is_empty() {
                help.push_str(&format!("{}\n", paint(bold, flag.heading)));
            }
            heading = Some(flag.heading);
        }
        let spelled =
            if flag.value.is_empty() { flag.name.to_owned() } else { format!("{} {}", flag.name, flag.value) };
        let pad = " ".repeat(width - spelled.len() + 2);
        help.push_str(&format!("  {}{pad}{}\n", paint(takes, &spelled), flag.means));
    }
    if !command.more.is_empty() {
        help.push_str(&format!("\n{}\n", wrapped(command.more, 80)));
    }
    help.push('\n');
    for example in command.examples {
        help.push_str(&format!("  {}\n", paint(dim, example)));
    }
    help
}

/// Every command as data, for `help --all --json`: what an agent learns the
/// whole command line from, and what the site's highlighting and explain read,
/// from `web/src/lib/content/cli.json`, which `just cli-json` writes.
pub(crate) fn json() -> serde_json::Value {
    let flag = |flag: &Flag| serde_json::json!({ "name": flag.name, "value": flag.value, "means": flag.means });
    let commands: Vec<serde_json::Value> = GROUPS
        .iter()
        .flat_map(|(group, commands)| commands.iter().map(move |command| (*group, *command)))
        .map(|(group, command)| {
            let flags: Vec<serde_json::Value> =
                command.flags.iter().filter(|known| known.heading != HIDDEN).map(flag).collect();
            serde_json::json!({
                "name": command.name,
                "group": group,
                "takes": command.takes,
                "does": command.does,
                "flags": flags,
                "examples": command.examples,
                "more": command.more,
            })
        })
        .collect();
    let words: serde_json::Map<String, serde_json::Value> =
        WORDS.iter().map(|(word, means)| ((*word).to_owned(), (*means).into())).collect();
    serde_json::json!({ "commands": commands, "words": words })
}

/// `json` as the site keeps it: indented, a line end last.
pub(crate) fn json_text() -> String {
    format!("{}\n", serde_json::to_string_pretty(&json()).expect("the help is JSON"))
}

/// Text broken into lines of at most `width` columns, at spaces.
fn wrapped(text: &str, width: usize) -> String {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let line = lines.last_mut().expect("one line at least");
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(word.to_owned());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    lines.join("\n")
}

/// Prints the help to stdout, coloured on a terminal.
pub(crate) fn print() -> Done {
    out!("{}", text(Terminal::of_stdout().colours()))
}

/// Prints one command's help to stdout, coloured on a terminal.
pub(crate) fn print_command(command: &Command) -> Done {
    out!("{}", of_command(Terminal::of_stdout().colours(), command))
}

/// Prints the help of every command of a group, `metrics`, one after another.
pub(crate) const CONFIG_EXPLAIN: Command = Command {
    name: "config explain",
    takes: "[dir] explain <config>",
    does: "a config's settings as the store keeps them, and where each came from",
    flags: &[JSON],
    examples: &["tinystore config ./data explain chat"],
    more: "It reads beside the program or server that holds the directory: the last declaration a program opened \
           the config with, its defaults, and the changes made to its live settings. The program's own files and \
           environment are its own, so its value may differ; a required setting they give is said as such. A \
           secret shows as ***.",
};

pub(crate) const CONFIG_UPDATE: Command = Command {
    name: "config update",
    takes: "<dir> update <config> <path=value>…",
    does: "change a config's live settings, which its programs take at once",
    flags: &[],
    examples: &[
        "tinystore config ./data update chat model.max_tokens=2048",
        "tinystore config ./data update chat limits.window=30s 'model.stop:=[\"\\n\"]'",
    ],
    more: "A value is read by its setting's kind, as an environment variable is: 2048 an integer, 30s a span, a,b \
           a list. path:=json gives JSON as it is. Only a live setting changes, checked against its rules, as the \
           store keeps it. It goes through the server that holds the directory, or one it starts, so that every \
           program holding the config takes the change at once.",
};

pub(crate) const CONFIG_RESET: Command = Command {
    name: "config reset",
    takes: "<dir> reset <config> [path]",
    does: "drop the changes of a setting, a group, or every one",
    flags: &[],
    examples: &["tinystore config ./data reset chat model.max_tokens", "tinystore config ./data reset chat"],
    more: "A setting it resets goes back to what the program's own declaration, files and environment give. It goes \
           through the server that holds the directory, or one it starts.",
};

pub(crate) const LOCK_HOLD: Command = Command {
    name: "lock hold",
    takes: "<dir> hold <lock> <key> -- <command>",
    does: "run a command while it holds a key of a lock",
    flags: &[
        flag("--wait", "<span>", "stand in line this long for the key, 10s when not given; 0 takes it only if free"),
        flag("--ttl", "<span>", "how long the lease lasts from each renewal, 30s when not given"),
        flag("--max-hold", "<span>", "let the key go this long after it was given, however it is renewed"),
    ],
    examples: &[
        "tinystore lock ./data hold cron nightly-backup -- ./backup.sh",
        "tinystore lock ./data hold cron nightly-backup --wait 0 -- ./backup.sh",
    ],
    more: "The key is held through the server that holds the directory, or one it starts, and its lease renewed every \
           third of --ttl while the command runs. The command gets the lease's fence in TINYSTORE_FENCE, is stopped \
           if the lease is lost, and the key is let go once it ends; the command's exit is this one's. A key held \
           by another past --wait exits 1 and runs nothing.",
};

pub(crate) const COMPLETION: Command = Command {
    name: "completion",
    takes: "<shell>",
    does: "the lines that give a shell Tab's completion",
    flags: &[],
    examples: &[
        "eval \"$(tinystore completion bash)\"     # in ~/.bashrc",
        "eval \"$(tinystore completion zsh)\"      # in ~/.zshrc",
        "tinystore completion fish | source        # in ~/.config/fish/config.fish",
        "tinystore completion powershell | Out-String | Invoke-Expression   # in $PROFILE",
    ],
    more: "bash, zsh, fish or powershell. Tab then offers commands, verbs, flags and their values, and the names a \
           store holds, read beside it: buckets, queues, sets of files, streams, metrics and their labels. It makes \
           no store and writes nothing.",
};

/// Every command's help at once, for `help --all`: what an agent reads to
/// learn the whole command line in one call.
pub(crate) fn print_all() -> Done {
    for command in GROUPS.iter().flat_map(|(_, commands)| commands.iter()) {
        print_command(command)?;
        outln!()?;
    }
    Ok(())
}

/// Whether `word` names a group of commands, as `metrics` does.
pub(crate) fn is_group(word: &str) -> bool {
    GROUPS
        .iter()
        .flat_map(|(_, commands)| commands.iter())
        .any(|command| command.name.split_once(' ').is_some_and(|(group, _)| group == word))
}

pub(crate) fn print_group(group: &str) -> Done {
    let commands = GROUPS.iter().flat_map(|(_, commands)| commands.iter());
    for command in commands.filter(|command| command.name.split(' ').next() == Some(group)) {
        print_command(command)?;
        outln!()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each `<word>` and `[word]` of a command's `takes`: `<host>:<port>`
    /// is two, `"<query>"` one.
    fn bracketed(takes: &str) -> Vec<&str> {
        let mut found = Vec::new();
        let mut rest = takes;
        while let Some(start) = rest.find(['<', '[']) {
            let close = if rest[start..].starts_with('<') { '>' } else { ']' };
            let Some(end) = rest[start..].find(close) else { break };
            found.push(&rest[start..start + end + 1]);
            rest = &rest[start + end + 1..];
        }
        found
    }

    #[test]
    fn every_word_a_command_takes_says_what_it_stands_for() {
        let commands = GROUPS.iter().flat_map(|(_, commands)| commands.iter());
        let taken: std::collections::BTreeSet<&str> = commands.flat_map(|command| bracketed(command.takes)).collect();
        let said: std::collections::BTreeSet<&str> = WORDS.iter().map(|(word, _)| *word).collect();
        assert_eq!(taken, said, "a word of a command's takes and WORDS");
    }

    #[test]
    fn the_sites_copy_of_the_help_is_the_help() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/src/lib/content/cli.json");
        let kept = std::fs::read_to_string(path).unwrap_or_default().replace("\r\n", "\n");
        assert!(kept == json_text(), "web/src/lib/content/cli.json is not the help: just cli-json writes it again");
    }

    #[test]
    fn the_help_groups_every_command_and_lines_up_what_each_does() {
        let help = text(false);
        for group in ["Look", "Run", "Nodes", "Develop"] {
            assert!(help.lines().any(|line| line == group), "{group}:\n{help}");
        }
        let rows: Vec<&str> = help.lines().filter(|line| line.starts_with("  ")).collect();
        let starts: Vec<usize> = rows
            .iter()
            .map(|row| {
                let command =
                    GROUPS.iter().flat_map(|(_, commands)| commands.iter()).find(|command| row.ends_with(command.does));
                row.len() - command.map_or(0, |command| command.does.len())
            })
            .collect();
        assert!(starts.windows(2).all(|pair| pair[0] == pair[1]), "what each does starts in one column:\n{help}");
        assert!(!help.contains('\u{1b}'), "no colour without a terminal");
        assert!(text(true).contains('\u{1b}'));
    }

    #[test]
    fn a_commands_help_shows_its_flags_but_the_sdks_own() {
        let help = of_command(false, &SERVE);
        assert!(help.starts_with("tinystore serve <dir>\n"), "{help}");
        assert!(help.contains("\nremote\n  --listen tls://<host>:<port>"), "{help}");
        assert!(help.contains("--node-config <file.toml>"), "{help}");
        assert!(!help.contains("--clock") && !help.contains("--local"), "{help}");
        assert!(help.contains("  tinystore serve ./data\n"), "{help}");
        assert_eq!(command("node promote").map(|command| command.name), Some("node promote"));
        assert!(first_words().contains(&"node") && first_words().contains(&"witness"));
    }

    #[test]
    fn a_long_line_breaks_at_spaces_within_its_width() {
        assert_eq!(wrapped("one two three four", 9), "one two\nthree\nfour");
    }
}
