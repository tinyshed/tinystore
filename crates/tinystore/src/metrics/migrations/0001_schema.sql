-- The count of series against the store's bound, and the id the next payload
-- takes: never given twice, so that a directory's address of one never moves.
create table store_state (
    id              integer primary key check (id = 1),
    series_count    integer not null check (series_count >= 0),
    next_payload_id integer not null check (next_payload_id > 0)
) strict;
insert into store_state (id, series_count, next_payload_id) values (1, 0, 1);

-- What a name owns: its kind while it has series, how long its samples stay
-- (null: 30 days), a timer's or a histogram's bounds, and what its values mean.
create table names (
    id      integer primary key,
    name    text not null unique,
    kind    text check (kind in ('gauge', 'counter', 'histogram')),
    keep    integer check (keep is null or keep > 0),
    buckets text,
    unit    text not null default '' check (length(unit) <= 32),
    help    text not null default '' check (length(help) <= 1024)
) strict;

-- A series by the SHA-256 of its canonical labels, its name among them, and
-- the ids of its label pairs gap-coded. Its id is never given again, so that
-- an id kept in memory never finds another series.
create table series (
    id        integer primary key autoincrement,
    identity  blob not null unique check (length(identity) = 32),
    name      integer not null references names (id),
    label_ids blob not null
) strict;

-- The dictionary of label pairs, a pair kept once, its count of series
-- choosing the shortest postings to start a match from.
create table label_values (
    id            integer primary key,
    name          text not null,
    value         text not null,
    posting_count integer not null default 0 check (posting_count >= 0),
    unique (name, value)
) strict;

-- Which series hold a pair, so that nothing scans every series. No foreign
-- key: removing a series would walk every posting for one of its, having no
-- index by series; the engine writes and removes a series' postings by their
-- key, from the pairs its row names.
create table postings (
    label_id  integer not null,
    series_id integer not null,
    primary key (label_id, series_id)
) strict, without rowid;

-- A series' state, fixed in size: the newest time it has seen, what it has
-- packed (null: nothing), a version every change of its head moves, whether a
-- block's worth waits to be packed, when its next sample expires, whether
-- retention has taken samples of it, so that the step from one into its
-- oldest is known not to be counted, the scale its next block's grid tries
-- first, its head's extent, and why it stopped taking samples when it did.
create table series_state (
    series_id     integer primary key references series (id),
    max_seen      integer not null,
    sealed_before integer,
    version       integer not null default 0 check (version >= 0),
    ready         integer not null default 0 check (ready in (0, 1)),
    due           integer,
    trimmed       integer not null default 0 check (trimmed in (0, 1)),
    scale         integer not null default -2 check (scale between -2 and 15),
    head_count    integer not null default 0 check (head_count >= 0),
    head_start    integer,
    head_end      integer,
    failed_at     integer,
    failure       text check (failure is null or length(failure) <= 1024)
) strict;
create index series_ready on series_state (series_id) where ready = 1 and failed_at is null;
create index series_due on series_state (due) where due is not null and failed_at is null;
create index series_failed on series_state (series_id) where failed_at is not null;

-- A series' samples not packed yet, rewritten as they come: a table of their
-- own, so that their growing and shrinking leaves the state rows' pages full.
create table heads (
    series_id integer primary key references series (id),
    body      blob not null check (length(body) <= 1048576)
) strict;

-- The times of a group's blocks, shared by the groups of series written at
-- the same ticks: by digest, its bytes compared before it is.
create table clocks (
    id     integer primary key,
    digest blob not null unique check (length(digest) = 32),
    refs   integer not null check (refs >= 0),
    body   blob not null check (length(body) <= 65536)
) strict;

-- Up to 32 blocks of a series, packed: a rowid table, since a directory past
-- what a page holds of a row without one spills into a page mostly empty.
-- clock carries no foreign key: releasing a clock would scan groups without
-- an index.
create table groups (
    id        integer primary key,
    series_id integer not null references series (id),
    start_ts  integer not null,
    end_ts    integer not null check (end_ts >= start_ts),
    clock     integer not null,
    directory blob not null check (length(directory) <= 8192)
) strict;
create unique index groups_by_series on groups (series_id, start_ts);

-- A block's values past what its directory keeps inline.
create table payloads (
    id   integer primary key,
    body blob not null check (length(body) <= 8200)
) strict;
