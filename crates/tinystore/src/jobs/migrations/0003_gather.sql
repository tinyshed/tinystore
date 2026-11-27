-- latest: when a gathered job runs at the latest, unix milliseconds: the time
-- of the gather that started its list and that gather's max; null for a job
-- no gather with a max started, as on every row a release before wrote, so
-- no row is rewritten. A release before this one refuses the file at its
-- open, since it would move a gathered job without its latest time.
alter table _tinystore_jobs add column latest integer;

-- taken: how many leading items of a running job's list its run read, kept
-- once a gather appends to the list while it runs; null when none did. A run
-- that ends well drops them, one that fails keeps them for its retry.
alter table _tinystore_jobs add column taken integer;
