-- jobs.db has 4 KiB pages, chosen when the file is created: a job's row is a
-- few hundred bytes, and a value past 512 of them lives in spilled

-- kind: a queue or a schedule, and a name opened as the other is refused
create table queues (
    id   integer primary key,
    name text    not null unique,
    kind text    not null
) strict;

-- a job that waits or runs, ordered by when it is due, so that the jobs due
-- together lie together however long before they were enqueued:
-- next: unix milliseconds, when the job is due; at: the time it runs for;
-- attempt: the attempts counted before its lease's; again: the earliest time an
-- Enqueue asked of the job while it ran; repeat: a repeating job's cron text
-- and zone; error: the last attempt's failure
create table jobs (
    queue   integer not null,
    next    integer not null,
    id      integer not null,
    key     text,
    at      integer not null,
    attempt integer not null,
    again   integer,
    repeat  text,
    error   text,
    value   blob,
    spill   integer,
    primary key (queue, next, id)
) strict, without rowid;

-- a job's key and where its row lies: a job that moves takes its key along,
-- and one that leaves the queue leaves its key behind, naming no row, until
-- maintenance drops it in the order of the keys; so that settling a burst of
-- keyed jobs writes no page outside the order of their time
create table keys (
    queue integer not null,
    key   text    not null,
    next  integer not null,
    id    integer not null,
    primary key (queue, key)
) strict, without rowid;

-- a claimed job's lease, beside its row so that a claim leaves the row alone:
-- attempt: the attempt it was given for, the token that settles it; until: its end
create table leases (
    id      integer primary key,
    queue   integer not null,
    next    integer not null,
    attempt integer not null,
    until   integer not null
) strict;

-- a job that failed for good, kept until its queue's KeepFailed has passed
create table failed (
    queue    integer not null,
    id       integer not null,
    key      text,
    at       integer not null,
    attempt  integer not null,
    failed   integer not null,
    error    text    not null,
    value    blob,
    spill    integer,
    primary key (queue, id)
) strict, without rowid;

create unique index failed_by_key on failed (queue, key) where key is not null;
create index failed_by_time on failed (queue, failed, id);

create table spilled (
    id    integer primary key,
    value blob    not null
) strict;

-- the keys of acknowledged jobs a queue with KeepDone remembers, until when
create table done (
    queue integer not null,
    key   text    not null,
    until integer not null,
    primary key (queue, key)
) strict, without rowid;

create index done_by_time on done (until);

-- ids: the high-water mark of job ids, reserved a block at a time, never repeated
create table meta (
    name  text    primary key,
    value integer not null
) strict, without rowid;

insert into meta (name, value) values ('ids', 0);
