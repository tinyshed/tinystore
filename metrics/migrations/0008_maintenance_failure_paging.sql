create index series_failed_id on series_state(series_id) where failed_at is not null;
