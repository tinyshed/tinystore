alter table series_state add column tail blob check (tail is null or length(tail) <= 16777216);
alter table series_state add column head_start integer;
alter table series_state add column head_end integer;
update series_state set
    head_start = (select at from head where head.series_id=series_state.series_id order by at limit 1),
    head_end = (select at from head where head.series_id=series_state.series_id order by at desc limit 1);
