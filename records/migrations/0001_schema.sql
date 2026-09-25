-- records.db has 1 KiB pages, chosen when the file is created: SQLite leaves
-- a large row's remainder on a leaf page, and small pages waste little of it

create table streams (
    id   integer primary key,
    name text    not null unique
) strict;

-- a head row is one stream's flush, its records in arrival order under zstd;
-- late: more than a minute behind the median of its batch; input: what its
-- records weigh against a segment's bounds; size: the body's bytes
create table heads (
    id         integer primary key,
    stream     integer not null,
    late       integer not null,
    first_at   integer not null,
    last_at    integer not null,
    levels     integer not null,
    count      integer not null,
    input      integer not null,
    size       integer not null,
    written_at integer not null,
    body       blob    not null
) strict;

create index heads_by_stream on heads (stream, late, id);
create index heads_by_time on heads (last_at, first_at, levels, stream, count, size);

-- a row per head, so that finding what is ready to seal reads one row a head;
-- since: when its oldest waiting row was written
create table head_state (
    stream integer not null,
    late   integer not null,
    count  integer not null,
    input  integer not null,
    since  integer not null,
    primary key (stream, late)
) without rowid, strict;

-- ids only grow, so a cursor never meets one twice; a segment's blocks are
-- the ids from first_block to last_block
create table segments (
    id          integer primary key autoincrement,
    stream      integer not null,
    first_at    integer not null,
    last_at     integer not null,
    count       integer not null,
    first_block integer not null,
    last_block  integer not null,
    body        blob    not null
) strict;

create index segments_by_end on segments (last_at);

-- kind: 0 an event name, 1 an attribute key, 2 a context key
create table segment_keys (
    segment integer not null,
    kind    integer not null,
    key     text    not null,
    primary key (segment, kind, key)
) without rowid, strict;

create table blocks (
    id       integer primary key,
    segment  integer not null,
    stream   integer not null,
    first_at integer not null,
    last_at  integer not null,
    levels   integer not null,
    count    integer not null,
    size     integer not null,
    body     blob    not null
) strict;

-- covers every column a query picks its blocks by, so candidates cost no row reads
create index blocks_by_time on blocks (last_at, first_at, levels, stream, segment, count, size);

create table block_traces (
    block integer primary key,
    bloom blob    not null
) strict;

create table block_filters (
    block integer not null,
    key   text    not null,
    bloom blob    not null,
    primary key (block, key)
) without rowid, strict;
