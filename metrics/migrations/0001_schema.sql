create table store_state (
    id              integer primary key check (id = 1),
    series_count    integer not null check (series_count >= 0),
    next_payload_id integer not null check (next_payload_id > 0)
) strict;
insert into store_state values (1, 0, 1);

-- identity is '@' and the base64 SHA-256 of the canonical labels; label_ids are
-- the gap-coded dictionary ids every digest match is confirmed against. The
-- check admits 'histogram' ahead of the engine, which ingests no such series
-- yet, since SQLite changes a check only by rebuilding its table.
create table series (
    id        integer primary key,
    identity  text not null unique,
    label_ids blob not null,
    kind      text not null check (kind in ('gauge', 'counter', 'histogram'))
) strict;

create table label_values (
    id            integer primary key,
    name          text not null,
    value         text not null,
    posting_count integer not null default 0 check (posting_count >= 0),
    unique (name, value)
) strict;

create table postings (
    label_id  integer not null references label_values(id),
    series_id integer not null references series(id),
    primary key (label_id, series_id)
) strict, without rowid;

create table series_state (
    series_id      integer primary key references series(id),
    max_seen_ts    integer not null,
    sealed_before  integer,
    version        integer not null default 0 check (version >= 0),
    head_count     integer not null default 0 check (head_count >= 0),
    ready          integer not null default 0 check (ready in (0, 1)),
    next_gc_ts     integer,
    model_scale    integer not null default -2,
    tail           blob check (tail is null or length(tail) <= 16777216),
    head_start     integer,
    head_end       integer,
    failed_at      integer,
    failure_reason text check (failure_reason is null or length(failure_reason) <= 1024)
) strict;
create index series_ready on series_state(series_id) where ready = 1 and failed_at is null;
create index series_due on series_state(next_gc_ts) where next_gc_ts is not null and failed_at is null;
create index series_failed on series_state(failed_at, series_id) where failed_at is not null;
create index series_failed_id on series_state(series_id) where failed_at is not null;

create table clocks (
    id     integer primary key,
    digest blob not null unique check (length(digest) = 32),
    refs   integer not null check (refs >= 0),
    body   blob not null check (length(body) <= 65536)
) strict;

-- clock_id carries no foreign key: releasing a clock would scan groups without an index
create table groups (
    series_id integer not null references series(id),
    start_ts  integer not null,
    end_ts    integer not null check (end_ts >= start_ts),
    directory blob not null check (length(directory) <= 8192),
    clock_id  integer not null,
    primary key (series_id, start_ts)
) strict, without rowid;

create table payloads (
    id   integer primary key,
    body blob not null check (length(body) <= 8200)
) strict;

-- what a metric's name means; it belongs to the name, not to a series
create table descriptions (
    name text primary key,
    unit text not null check (length(unit) <= 32),
    help text not null check (length(help) <= 1024)
) strict, without rowid;
