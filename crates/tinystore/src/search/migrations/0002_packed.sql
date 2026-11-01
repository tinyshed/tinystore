-- a key's blocks bit-packed, a run of them a row one after another till it
-- holds 32 KiB; id: the key's id × 2²⁰ + the run's number, so that a key's
-- runs lie together and a range of ids is every run of a key
create table _tinystore_search_runs (
    id     integer primary key,
    blocks blob    not null
) strict;

-- packed: the directory of a key whose blocks are in runs, of fixed width,
-- as search's packed.rs lays it out. A key a release before wrote keeps its
-- directory and its rows of blocks until packing or a purge writes it again,
-- whole, in runs; a release before refuses the file at this step rather
-- than take a list without a directory for one that holds nothing
alter table _tinystore_search_lists add column packed blob;

-- best: of a key in runs, its postings that saturate most, ten at most, as
-- a short list keeps its postings: a search scores them first, and the
-- tenth best of what they add is a floor no hit of the best ten is below
alter table _tinystore_search_lists add column best blob;

-- the head: the postings sets wrote since packing last reached their key, a
-- row a key a set rather than one a posting; first: the row's first
-- document, count: how many postings it holds, as (doc − doc before, tf,
-- len) varints. The table of a row a posting a release before wrote is read
-- and drained, and stays
create table _tinystore_search_fresh (
    index_id integer not null,
    key      integer not null,
    first    integer not null,
    count    integer not null,
    postings blob    not null,
    primary key (index_id, key, first)
) without rowid, strict;

-- lists_version: grows with every commit that changes a list of the index,
-- packing's, a purge's or upkeep's, so that what a search keeps of its keys'
-- lists knows when it is behind
alter table _tinystore_search_indexes add column lists_version integer not null default 0;
