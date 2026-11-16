-- A getOrCreate's claim of a path while its function runs, so that the other
-- calls of the path wait for it rather than run their own: one row a path.
-- owner: the run's number, drawn at random as it claims;
-- until: unix milliseconds by the store's clock when its lease ends unless
-- renewed. A run lands its file only while its row names it and the path
-- holds no file, and any write or delete of the path deletes the row.
create table _tinystore_blobs_filling (
    set_id integer not null,
    path   text    not null,
    owner  integer not null,
    until  integer not null,
    primary key (set_id, path)
) strict, without rowid;
