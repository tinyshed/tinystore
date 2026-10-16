-- an index: its settings as JSON, its fields among them; documents: how
-- many it holds; next_doc: the number its next set gives, numbers only
-- growing; normed: every document below it has its lengths in norms;
-- gone_version grows with every change to what is gone, words_version each
-- time a word as written comes into the index or leaves it, so that what a
-- search keeps of them knows when it is behind; vectors: how many it holds;
-- clusters: 0 none, 1 building, 2 serving, 3 dropping; built_through: while
-- building, every vector numbered up to it is assigned; probes: clusters a
-- search reads and rescored: members of them it scores again, chosen when
-- the index held calibrated_vectors for calibrated_recall, probes 0 for an
-- exact search when no fewer reach it; clusters_version grows with every
-- change of centroids; beside_of: the index this one is rebuilt beside,
-- whose documents numbered below copy_until it reads, those up to
-- copied_through read already, for why; failed: why its rebuild stopped;
-- dropping: its rows are dropped a step a commit; opened_at: when a program
-- last opened it, in milliseconds of the store's clock; filling: a table's
-- index reads its table's rows, 1 the first time, 2 again after its triggers
-- went with a table a migration rebuilt, and 3 then reads its documents to
-- delete those whose rows are gone; filled_through: how far, a row's key or
-- a document's number
create table _tinystore_search_indexes (
    id            integer primary key autoincrement,
    name          text    not null unique,
    settings      text    not null,
    documents     integer not null default 0,
    next_doc      integer not null default 1,
    normed        integer not null default 1,
    gone_version  integer not null default 0,
    words_version integer not null default 0,
    vectors            integer not null default 0,
    clusters           integer not null default 0,
    built_through      integer not null default 0,
    probes             integer not null default 0,
    rescored           integer not null default 0,
    calibrated_vectors integer not null default 0,
    calibrated_recall  real    not null default 0,
    clusters_version   integer not null default 0,
    beside_of          integer,
    copy_until         integer not null default 0,
    copied_through     integer not null default 0,
    why                text,
    failed             text,
    dropping           integer not null default 0,
    opened_at          integer not null default 0,
    filling            integer not null default 0,
    filled_through     any
) strict;

-- a part is a field's words as written, 2 × its number, or their stems, one
-- more; tokens: its words in every document the index holds
create table _tinystore_search_parts (
    index_id integer not null,
    part     integer not null,
    tokens   integer not null,
    primary key (index_id, part)
) without rowid, strict;

-- a key is a word of a part; documents: how many hold it now. Its id names
-- its list, its blocks and its head rows, and is never given again
create table _tinystore_search_keys (
    id        integer primary key autoincrement,
    index_id  integer not null,
    part      integer not null,
    term      text    not null,
    documents integer not null
) strict;

create unique index _tinystore_search_keys_by_term on _tinystore_search_keys (index_id, part, term);

-- a key's postings packed: the directory of its blocks, or, held by no more
-- than 16 documents, the postings themselves
create table _tinystore_search_lists (
    key       integer primary key,
    directory blob,
    inline    blob
) strict;

-- a block of up to 128 of a key's postings, from the document first
create table _tinystore_search_blocks (
    key     integer not null,
    first   integer not null,
    payload blob    not null,
    primary key (key, first)
) without rowid, strict;

-- postings sets wrote since their key was last packed, its documents after
-- every one its blocks hold
create table _tinystore_search_head (
    index_id integer not null,
    key      integer not null,
    doc      integer not null,
    tf       integer not null,
    len      integer not null,
    primary key (index_id, key, doc)
) without rowid, strict;

-- a part's lengths in 65,536 documents, two bytes each, little-endian, a
-- document's at its number's place in the row; written once a document
create table _tinystore_search_norms (
    index_id integer not null,
    part     integer not null,
    chunk    integer not null,
    lengths  blob    not null,
    primary key (index_id, part, chunk)
) without rowid, strict;

-- a document an index holds by its number: its id, the ids of the keys it
-- holds and its lengths, then its value as JSON, last so that reading the
-- others never walks it
create table _tinystore_search_docs (
    index_id integer not null,
    doc      integer not null,
    id       text    not null,
    keys     blob    not null,
    lengths  blob    not null,
    value    blob    not null,
    primary key (index_id, doc)
) without rowid, strict;

create unique index _tinystore_search_docs_by_id on _tinystore_search_docs (index_id, id);

-- a document a set replaced or a delete removed, which blocks may still
-- hold until a purge takes it out of them, with the keys it held
create table _tinystore_search_gone (
    index_id integer not null,
    doc      integer not null,
    keys     blob    not null,
    primary key (index_id, doc)
) without rowid, strict;

-- a document's vector as the program gave it, its floats little-endian, and
-- its length; key: the index's id × 2³² + the document's number, so that an
-- index's vectors lie together, in the order their documents were set
create table _tinystore_search_vectors (
    key    integer primary key,
    length real    not null,
    vector blob    not null
) strict;

-- what a search by a vector reads of every one, by the same key: a byte a
-- dimension of its direction, after the scale of a byte and the length of
-- what the bytes leave out
create table _tinystore_search_codes (
    key  integer primary key,
    code blob    not null
) strict;

-- an index's clusters of vectors: each centroid, of length one, its floats
-- little-endian
create table _tinystore_search_clusters (
    index_id integer not null,
    cluster  integer not null,
    centroid blob    not null,
    primary key (index_id, cluster)
) without rowid, strict;

-- a vector's place among the clusters, ordered by its cluster so that a
-- search reads a cluster as one range: RaBitQ's code of its offset from the
-- centroid; cluster -1 is the head, vectors set since maintenance last
-- assigned them, with an empty code
create table _tinystore_search_members (
    index_id integer not null,
    cluster  integer not null,
    doc      integer not null,
    code     blob    not null,
    primary key (index_id, cluster, doc)
) without rowid, strict;

create unique index _tinystore_search_members_by_doc on _tinystore_search_members (index_id, doc);

-- the key of a row a table's index has not read as it is: its table's
-- triggers write it in the transaction that changes the row, and the index
-- takes it out once it has the row, before that commit when the store wrote
-- it, and at the store's next write or within a second when a process
-- beside it did; name: the index's
create table _tinystore_search_pending (
    name text not null,
    key  any
) strict;
