-- the state an application keeps without kv: a table a case, each with its own
-- expires_at in unix milliseconds that every read compares and its sweeper deletes
-- by, keyed as its reads find a row: without a rowid where rows are small, as kv's
-- cells are, so that the two files differ by design rather than by care

create table sessions (
    user_id    integer not null,
    token      text    not null,
    device     text    not null,
    since      integer not null,
    expires_at integer not null,
    primary key (user_id, token)
) strict, without rowid;

create index sessions_expiry on sessions (expires_at);

-- digest: a sign-in code's sha-256, so that a copy of the file holds no working link
create table login_codes (
    digest     blob    primary key,
    user_id    integer not null,
    expires_at integer not null
) strict, without rowid;

create index login_codes_expiry on login_codes (expires_at);

-- scope: 'ip' or 'email'; expires_at: fifteen minutes from a window's first attempt
create table login_attempts (
    scope      text    not null,
    subject    text    not null,
    attempts   integer not null,
    expires_at integer not null,
    primary key (scope, subject)
) strict, without rowid;

create index login_attempts_expiry on login_attempts (expires_at);

-- claim: the random token of the handler that holds the event, so that an event
-- deleted and claimed again does not answer to an old one
create table webhook_events (
    event_id   text    primary key,
    claim      integer not null,
    expires_at integer not null
) strict, without rowid;

create index webhook_events_expiry on webhook_events (expires_at);

-- note_id is the rowid, since without one a row over a kilobyte of a 4 KiB page
-- spills; etag: random, as claim is; saves: the saves that made the draft, which
-- the harness counts to find a lost update
create table drafts (
    note_id    integer primary key,
    user_id    integer not null,
    body       text    not null,
    saves      integer not null,
    etag       integer not null,
    expires_at integer not null
) strict;

create index drafts_expiry on drafts (expires_at);
