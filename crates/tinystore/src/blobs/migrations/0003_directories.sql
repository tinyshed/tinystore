-- A directory: a folder of the program's machine under a name, which the
-- store lists and never writes. Its path is that machine's and is not kept,
-- since the store moves between machines in a backup.
--
-- changes: the number the last change of its list took, which a cursor is;
-- purged: the newest removed line upkeep deleted, so that a cursor before it
-- would miss a removal and is too old
create table _tinystore_blobs_directories (
    id      integer primary key,
    name    text    not null unique,
    changes integer not null default 0,
    purged  integer not null default 0
) strict;

-- a file a scan listed, by its path in the folder, '/' between segments:
-- size, modified: as the disk gave them, modified in nanoseconds since 1970;
-- change: the number of the change that last touched the row;
-- added: the number of the change that listed it after it was not there;
-- removed: unix milliseconds by the store's clock when a scan found it gone,
-- null while it is there; the line stays 30 days, then upkeep deletes it
create table _tinystore_blobs_listed (
    id        integer primary key,
    directory integer not null,
    path      text    not null,
    size      integer not null,
    modified  integer not null,
    change    integer not null,
    added     integer not null,
    removed   integer
) strict;

create unique index _tinystore_blobs_listed_by_path on _tinystore_blobs_listed (directory, path);
create index _tinystore_blobs_listed_by_change on _tinystore_blobs_listed (directory, change);
create index _tinystore_blobs_listed_removed on _tinystore_blobs_listed (removed) where removed is not null;
