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
-- under zstd; late: they came more than ten seconds behind the newest event
-- their stream showed on time, and wait in its late head; levels: a bit for
-- each level its events have; weight: what its events weigh against a
-- segment's bounds; size: the body's bytes; ids only grow, so a row reported
-- damaged is never another row later
create table heads (
    id         integer primary key autoincrement,
    stream     integer not null,
    late       integer not null,
    first_at   integer not null,
    last_at    integer not null,
    levels     integer not null,
    count      integer not null,
    weight     integer not null,
    size       integer not null,
    written_at integer not null,
    body       blob    not null
) strict;

-- a read walks the rows of its range from either end without reading a body
create index heads_by_first on heads (first_at, id, last_at, levels, stream, count, size);
create index heads_by_last on heads (last_at, id, first_at, levels, stream, count, size);
create index heads_by_stream on heads (stream, late, id);

-- what waits in each head, a stream's on time or its late, so that finding
-- what is ready to pack reads a row a head; since: when its oldest row was
-- written
create table head_state (
    stream integer not null,
    late   integer not null,
    count  integer not null,
    weight integer not null,
    since  integer not null,
    primary key (stream, late)
) without rowid, strict;

-- a segment is what one packing made of a stream's head rows: its count
-- events in the order of their times, in blocks. Its id is its place in the
-- order segments were packed, and only grows.
--
-- A merge moves small segments' events into the one of lowest id, the
-- holder, and keeps the others' rows as places naming it: a place's events
-- begin at start among the holder's. A holder's own start is where its own
-- begin, held how many it holds with its places', first_at and last_at their
-- times, weight what they weigh, and body what its blocks share, their
-- events' names, contexts and shapes, each once. A place holds nothing.
create table segments (
    id       integer primary key autoincrement,
    stream   integer not null,
    first_at integer,
    last_at  integer,
    count    integer not null,
    holder   integer,
    start    integer not null,
    held     integer not null,
    weight   integer not null,
    body     blob    not null
) strict;

create index segments_by_stream on segments (stream, last_at) where holder is null;
create index segments_by_size on segments (stream, held) where holder is null;
-- a holder's places, found when it merges again or expires
create index segments_by_holder on segments (holder) where holder is not null;

-- a block is up to 1024 of its segment's events, the first block's times
-- before the second's. Ids never repeat, so a block's bytes are known by its id
create table blocks (
    id       integer primary key autoincrement,
    segment  integer not null,
    stream   integer not null,
    first_at integer not null,
    last_at  integer not null,
    levels   integer not null,
    count    integer not null,
    size     integer not null,
    body     blob    not null
) strict;

-- cover every column a read picks its blocks by, so candidates cost no row
create index blocks_by_first on blocks (first_at, id, last_at, levels, stream, segment, count, size);
create index blocks_by_last on blocks (last_at, id, first_at, levels, stream, segment, count, size);
create index blocks_by_segment on blocks (segment);
