create table store_state (
    id integer primary key check (id = 1),
    series_count integer not null check (series_count >= 0),
    next_payload_id integer not null check (next_payload_id > 0)
) strict;
insert into store_state values (1, 0, 1);

create table series (
    id integer primary key,
    labels text not null unique,
    kind text not null check (kind in ('gauge', 'counter'))
) strict;

create table postings (
    name text not null,
    value text not null,
    series_id integer not null references series(id),
    primary key (name, value, series_id)
) strict, without rowid;

create table series_state (
    series_id integer primary key references series(id),
    max_seen_ts integer not null,
    sealed_before integer,
    version integer not null default 0 check (version >= 0),
    head_count integer not null default 0 check (head_count >= 0),
    ready integer not null default 0 check (ready in (0, 1)),
    next_gc_ts integer
) strict;
create index series_ready on series_state(series_id) where ready = 1;
create index series_due on series_state(next_gc_ts) where next_gc_ts is not null;

create table head (
    series_id integer not null references series(id),
    at integer not null,
    value blob not null check (length(value) = 8),
    primary key (series_id, at)
) strict, without rowid;

create table groups (
    series_id integer not null references series(id),
    start_ts integer not null,
    end_ts integer not null check (end_ts >= start_ts),
    directory blob not null check (length(directory) <= 8192),
    primary key (series_id, start_ts)
) strict, without rowid;

create table payloads (
    id integer primary key,
    body blob not null check (length(body) <= 8200)
) strict;
