-- quiet: the hours a job does not run in, its own beside its queue's, as
-- `23:00 07:00 Europe/Moscow`, or `none` for a job no hours hold; null is
-- its queue's, as on every row a release before wrote, so no row is
-- rewritten. A release before this one refuses the file at its open, since
-- it would retry or repeat a job inside the hours it was given.
alter table _tinystore_jobs add column quiet text;
