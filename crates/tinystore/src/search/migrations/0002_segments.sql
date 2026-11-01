-- a segment: the postings of an index's documents from first to past,
-- every key's in one piece. A set writes the newest segment, level 0, while
-- it holds fewer than a thousand documents; merges join eight of a level
-- into one of the next. folded: its pieces' counts are in the keys' own;
-- merge: the segment a merge under way writes this one's keys into;
-- through: of a segment a merge writes, the last key it holds, null once
-- the merge is done. A release before refuses the file at this step rather
-- than take an index without lists for one that holds nothing
create table _tinystore_search_segments (
    id       integer primary key autoincrement,
    index_id integer not null,
    level    integer not null,
    first    integer not null,
    past     integer not null,
    docs     integer not null,
    folded   integer not null,
    merge    integer,
    through  integer
) strict;

create index _tinystore_search_segments_by_first on _tinystore_search_segments (index_id, first);

-- a key's postings in a segment, so that a segment's pieces lie together
-- and a set or a merge writes them one after another; count: how many it
-- holds; postings: a short piece's own, (doc − the segment's first, tf, len)
-- varints, appended to as a set writes the open segment; directory: a
-- longer one's, the header and the entries of its blocks as search's
-- packed.rs lays them out; best: its postings that saturate most
create table _tinystore_search_pieces (
    segment   integer not null,
    key       integer not null,
    count     integer not null,
    postings  blob,
    directory blob,
    best      blob,
    primary key (segment, key)
) without rowid, strict;

-- a piece's blocks bit-packed, a run of them a row one after another till
-- it holds 32 KiB; id: the segment's id × 2²⁴ + the run's number, so that a
-- segment's runs lie together and a range of ids is every run of a segment
create table _tinystore_search_runs (
    id     integer primary key,
    blocks blob    not null
) strict;

-- lists_version: grows with every commit that changes a piece a search may
-- keep, a merge's, a purge's or upkeep's, so that what a search keeps of
-- its keys' pieces knows when it is behind
alter table _tinystore_search_indexes add column lists_version integer not null default 0;
