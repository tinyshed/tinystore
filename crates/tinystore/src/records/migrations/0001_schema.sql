-- records.db has 1 KiB pages, chosen when the file is created: SQLite leaves
-- a large row's remainder on a leaf page, and small pages waste little of it

-- keep: how long the stream keeps its events, in milliseconds; null until an
-- open gives one, and the engine's fourteen days till then
create table streams (
    id   integer primary key,
    name text    not null unique,
    keep integer
) strict;

-- a head row is one stream's events of one write, in the order they came,
-- under zstd; levels: a bit for each level its events have; size: the body's
-- bytes; ids only grow, so a row reported damaged is never another row later
create table heads (
    id         integer primary key autoincrement,
    stream     integer not null,
    first_at   integer not null,
    last_at    integer not null,
    levels     integer not null,
    count      integer not null,
    size       integer not null,
    written_at integer not null,
    body       blob    not null
) strict;

-- a read walks the rows of its range from either end without reading a body
create index heads_by_first on heads (first_at, id, last_at, levels, stream, count, size);
create index heads_by_last on heads (last_at, id, first_at, levels, stream, count, size);
create index heads_by_stream on heads (stream, last_at);
