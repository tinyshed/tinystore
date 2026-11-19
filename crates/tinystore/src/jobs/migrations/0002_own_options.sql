-- A job's own options beside its queue's, which a write may give it; null is
-- the queue's, as on every row a release before wrote, so no row is rewritten.
-- dedupe: how long it holds its id once done, a span's milliseconds or a
-- calendar's unit and zone, `day Europe/Moscow`; keep: how long it is kept
-- once it failed for good, milliseconds, 0 not at all; expires: when it
-- leaves its queue unless written again, unix milliseconds
alter table _tinystore_jobs add column dedupe text;
alter table _tinystore_jobs add column keep integer;
alter table _tinystore_jobs add column expires integer;

-- the jobs a term ends, which maintenance finds without walking every job
create index _tinystore_jobs_expiring on _tinystore_jobs (expires) where expires is not null;

-- until: when a failed job with a keep of its own goes; null for its queue's
alter table _tinystore_jobs_failed add column until integer;

create index _tinystore_jobs_failed_by_until on _tinystore_jobs_failed (until) where until is not null;
