-- records.db has 1 KiB pages, chosen when the file is created: SQLite leaves
-- a large row's remainder on a leaf page, and small pages waste little of it

create table streams (
    id   integer primary key,
    name text    not null unique
) strict;

-- a head row is one stream's flush, its records in arrival order under zstd;
-- late: more than ten seconds behind the newest record its stream showed before it;
-- input: what its records weigh against a segment's bounds; size: the body's bytes;
-- ids only grow, so a row reported damaged is never another row later
create table heads (
    id         integer primary key autoincrement,
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

-- a segment's id is its place in the order segments were sealed, the place a
-- Follow cursor names; ids only grow, so a cursor never meets one twice.
-- A place's count records begin at start among the records of the segment
-- holding them: holder, or the place itself when holder is null.
-- A segment that holds records keeps them, its places' together, in the
-- blocks first_block to last_block: held is how many they are, first_at and
-- last_at their times, input what they weigh against a segment's bounds.
-- A merge moves small segments' records into the one of lowest id and keeps
-- the others' rows as places naming it, so a holder's places come after it.
create table segments (
    id          integer primary key autoincrement,
    stream      integer not null,
    first_at    integer,
    last_at     integer,
    count       integer not null,
    holder      integer,
    start       integer not null,
    held        integer not null,
    input       integer not null,
    first_block integer not null,
    last_block  integer not null,
    body        blob    not null
) strict;

create index segments_by_end on segments (last_at) where holder is null;
create index segments_by_size on segments (stream, held) where holder is null;
-- a merged segment's places, found when it merges again, expires or is dropped
create index segments_by_holder on segments (holder) where holder is not null;

-- kind: 0 an event name, 1 an attribute key, 2 a context key
create table segment_keys (
    segment integer not null,
    kind    integer not null,
    key     text    not null,
    primary key (segment, kind, key)
) without rowid, strict;

-- span files a block by how wide its times are: it ends less than 2^span
-- nanoseconds after it begins, so a read ending at t finds it among the
-- blocks of its span that end before t + 2^span, and walks no other
create table blocks (
    id       integer primary key,
    segment  integer not null,
    stream   integer not null,
    first_at integer not null,
    last_at  integer not null,
    span     integer not null,
    levels   integer not null,
    count    integer not null,
    size     integer not null,
    body     blob    not null
) strict;

-- covers every column a query picks its blocks by, so candidates cost no row reads
create index blocks_by_time on blocks (span, last_at, first_at, levels, stream, segment, count, size);

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
