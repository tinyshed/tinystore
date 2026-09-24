create table posts (
    id      integer primary key,
    user_id integer not null references users(id),
    title   text    not null
) strict;
