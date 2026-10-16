-- an index: its settings as JSON, its fields among them; documents: how
-- many it holds; next_doc: the number its next set gives, numbers only
-- growing; normed: every document below it has its lengths in norms;
-- gone_version grows with every change to what is gone, words_version each
-- time a word as written comes into the index or leaves it, so that what a
-- search keeps of them knows when it is behind
create table _tinystore_search_indexes (
    id            integer primary key autoincrement,
    name          text    not null unique,
    settings      text    not null,
    documents     integer not null default 0,
    next_doc      integer not null default 1,
    normed        integer not null default 1,
    gone_version  integer not null default 0,
    words_version integer not null default 0
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
