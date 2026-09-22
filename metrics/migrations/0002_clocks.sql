create table clocks (
    id integer primary key,
    digest blob not null unique check (length(digest) = 32),
    refs integer not null check (refs >= 0),
    body blob not null check (length(body) <= 65536)
) strict;
alter table groups add column clock_id integer not null default 0;
alter table series_state add column model_scale integer not null default -2;
