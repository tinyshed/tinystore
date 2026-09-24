create table records (
    id      integer primary key,
    at      integer not null,
    level   integer not null,
    message text    not null,
    attrs   text    not null
) strict;

create index records_at on records(at);
