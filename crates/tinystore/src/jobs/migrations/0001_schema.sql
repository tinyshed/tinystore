-- jobs.db has 4 KiB pages, chosen when the file is created: a job's row is a
-- few hundred bytes, and a value past 512 of them lives in spilled. Every name
-- is _tinystore_jobs…, so that the tables can live in an application's
-- database.

-- kind: a queue or a schedule, and a name opened as the other is refused;
-- waiting: the jobs the queue holds, which maxWaiting bounds, kept by the
-- triggers below so that a transaction sees its own adds; in_group: the bound
-- of each group the queue was last opened with, so that an open with another
-- gives back every job the old one parked
create table _tinystore_jobs_queues (
    id       integer primary key,
    name     text    not null unique,
    kind     text    not null,
    waiting  integer not null default 0 check (waiting >= 0),
    in_group integer not null default 0
) strict;

-- a job that waits or runs, ordered by when it is due, so that the jobs due
-- together lie together however long before they were added:
-- next: unix milliseconds, when the job is due; at: the time its run is for;
-- key: the caller's id; attempt: the attempts counted before its lease's;
-- again: when a set asked one run more of the job while it ran; repeat: a
-- repeat's text; error: the last attempt's failure; ran and took: when the last
-- run a handler finished began, unix milliseconds, and its milliseconds; grp:
-- the group whose running jobs a queue's concurrency bounds
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
    ran     integer,
    took    integer,
    grp     text,
    primary key (queue, next, id)
) strict, without rowid;

-- a due job whose group has no room is parked: its next moves 2^60 later, past
-- every time a job may have, so that claims stop meeting it, and moves back
-- when its group frees a place. The index holds the parked rows alone, a
-- group's in their order; a query reaches it by naming the same bound.
create index _tinystore_jobs_parked on _tinystore_jobs (queue, grp, next, id) where next >= 576460752303423488;

create trigger _tinystore_jobs_count_insert after insert on _tinystore_jobs begin
    update _tinystore_jobs_queues set waiting = waiting + 1 where id = new.queue;
end;
create trigger _tinystore_jobs_count_delete after delete on _tinystore_jobs begin
    update _tinystore_jobs_queues set waiting = waiting - 1 where id = old.queue;
end;

-- a job's id and where its row lies: a job that moves takes its key along, and
-- one that leaves the queue leaves its key behind, naming no row, until
-- maintenance drops it in the order of the keys; so that settling a burst of
-- jobs with ids writes no page outside the order of their time
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

-- a job that failed for good, kept until its queue's keep has passed, its id
-- taken meanwhile, with its group for the set that starts it again
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
    ran      integer,
    took     integer,
    grp      text,
    primary key (queue, id)
) strict, without rowid;

create unique index _tinystore_jobs_failed_by_key on _tinystore_jobs_failed (queue, key) where key is not null;
create index _tinystore_jobs_failed_by_time on _tinystore_jobs_failed (queue, failed, id);

-- a done job a queue with dedupe keeps until when, its id taken meanwhile and
-- its value and last run kept for get
create table _tinystore_jobs_done (
    queue   integer not null,
    key     text    not null,
    until   integer not null,
    at      integer not null,
    attempt integer not null,
    value   blob,
    spill   integer,
    ran     integer,
    took    integer,
    grp     text,
    primary key (queue, key)
) strict, without rowid;

create index _tinystore_jobs_done_by_time on _tinystore_jobs_done (until);

create table _tinystore_jobs_spilled (
    id    integer primary key,
    value blob    not null
) strict;

-- a step a job's handler finished, its answer kept for the job's later
-- attempts: name: the handler's for it; answer: its JSON; at: when it was kept,
-- unix milliseconds. A job that leaves the queue takes its steps along, and one
-- that runs again for its repeat starts without them.
create table _tinystore_jobs_steps (
    job    integer not null,
    name   text    not null,
    answer blob    not null,
    at     integer not null,
    primary key (job, name)
) strict, without rowid;

create trigger _tinystore_jobs_steps_go after delete on _tinystore_jobs begin
    delete from _tinystore_jobs_steps where job = old.id;
end;

-- ids: the high-water mark of job ids, reserved a block at a time, never repeated
create table _tinystore_jobs_meta (
    name  text    primary key,
    value integer not null
) strict, without rowid;

insert into _tinystore_jobs_meta (name, value) values ('ids', 0);
