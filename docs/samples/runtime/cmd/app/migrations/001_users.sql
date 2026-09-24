create table users (
    id         integer primary key,
    name       text    not null,
    email      text    not null unique,
    created_at integer not null
) strict;
