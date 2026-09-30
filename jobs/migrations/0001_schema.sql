-- jobs.db has 4 KiB pages, chosen when the file is created: a job's row is a
-- few hundred bytes, and a value past 512 of them lives in spilled

-- kind: a queue or a schedule, and a name opened as the other is refused
create table _tinystore_jobs_queues (
    id   integer primary key,
    name text    not null unique,
    kind text    not null,
    waiting integer not null default 0 check (waiting >= 0)
) strict;

-- a job that waits or runs, ordered by when it is due, so that the jobs due
-- together lie together however long before they were enqueued:
-- next: unix milliseconds, when the job is due; at: the time it runs for;
-- attempt: the attempts counted before its lease's; again: the earliest time an
-- Enqueue asked of the job while it ran; repeat: a repeating job's cron text
-- and zone; error: the last attempt's failure
create table _tinystore_jobs (
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

-- the writer keeps MaxWaiting's count in one row, including every statement
-- of a grouped write and every call in a Tx before it commits
create trigger _tinystore_jobs_count_insert after insert on _tinystore_jobs begin
    update _tinystore_jobs_queues set waiting = waiting + 1 where id = new.queue;
end;
create trigger _tinystore_jobs_count_delete after delete on _tinystore_jobs begin
    update _tinystore_jobs_queues set waiting = waiting - 1 where id = old.queue;
end;

-- a job's key and where its row lies: a job that moves takes its key along,
-- and one that leaves the queue leaves its key behind, naming no row, until
-- maintenance drops it in the order of the keys; so that settling a burst of
-- keyed jobs writes no page outside the order of their time
create table _tinystore_jobs_keys (
    queue integer not null,
    key   text    not null,
    next  integer not null,
    id    integer not null,
    primary key (queue, key)
) strict, without rowid;

-- a claimed job's lease, beside its row so that a claim leaves the row alone:
-- attempt: the attempt it was given for, the token that settles it; until: its end
create table _tinystore_jobs_leases (
    id      integer primary key,
    queue   integer not null,
    next    integer not null,
    attempt integer not null,
    until   integer not null
) strict;

-- a job that failed for good, kept until its queue's KeepFailed has passed
create table _tinystore_jobs_failed (
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

create unique index _tinystore_jobs_failed_by_key on _tinystore_jobs_failed (queue, key) where key is not null;
create index _tinystore_jobs_failed_by_time on _tinystore_jobs_failed (queue, failed, id);

create table _tinystore_jobs_spilled (
    id    integer primary key,
    value blob    not null
) strict;

-- the keys of acknowledged jobs a queue with KeepDone remembers, until when
create table _tinystore_jobs_done (
    queue integer not null,
    key   text    not null,
    until integer not null,
    primary key (queue, key)
) strict, without rowid;

create index _tinystore_jobs_done_by_time on _tinystore_jobs_done (until);

-- ids: the high-water mark of job ids, reserved a block at a time, never repeated
create table _tinystore_jobs_meta (
    name  text    primary key,
    value integer not null
) strict, without rowid;

insert into _tinystore_jobs_meta (name, value) values ('ids', 0);
