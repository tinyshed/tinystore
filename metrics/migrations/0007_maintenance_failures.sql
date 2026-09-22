alter table series_state add column failed_at integer;
alter table series_state add column failure_reason text check (failure_reason is null or length(failure_reason) <= 1024);

drop index series_ready;
drop index series_due;
create index series_ready on series_state(series_id) where ready = 1 and failed_at is null;
create index series_due on series_state(next_gc_ts) where next_gc_ts is not null and failed_at is null;
create index series_failed on series_state(failed_at,series_id) where failed_at is not null;
