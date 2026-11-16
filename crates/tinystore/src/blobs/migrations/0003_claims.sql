-- A database's files: a getOrCreate's claim of a path while its function
-- runs, as blobs.db's 0005_claims.sql keeps it.
create table _tinystore_blobs_filling (
    set_id integer not null,
    path   text    not null,
    owner  integer not null,
    until  integer not null,
    primary key (set_id, path)
) strict, without rowid;
