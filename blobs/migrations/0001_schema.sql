-- blobs.db has 4 KiB pages, chosen when the file is created: an object's row
-- is its path, a few numbers, its type and its meta, and an inline content's
-- bytes are a row of bodies, which a Stat or a Scan never reads

create table buckets (
    id   integer primary key,
    name text    not null unique
) strict;

-- an object is a key of a bucket naming a content;
-- path: the owners and the key, segments joined by '/', so that a folder is one range;
-- revision: the revision of blobs.db that wrote the object, which a marked Clear compares;
-- size and etag, the first 16 bytes of the content's SHA-256, so that Stat, Scan
-- and Usage read no other table;
-- modified, expires: unix milliseconds by the store's clock, expires null for never;
-- meta: a JSON object of strings, null for none
create table objects (
    bucket   integer not null,
    path     text    not null,
    revision integer not null,
    content  integer not null,
    size     integer not null,
    etag     blob    not null,
    type     text    not null,
    modified integer not null,
    expires  integer,
    meta     text,
    primary key (bucket, path)
) strict, without rowid;

create index objects_by_expiry on objects (expires, bucket, path) where expires is not null;

-- a content is bytes that objects name: its id names its file, never given twice;
-- names: how many objects name it, 0 once none does and its file waits to be removed;
-- inline: its bytes are a row of bodies instead of a file;
-- damaged: the scrub found its bytes changed or missing
create table contents (
    id      integer primary key,
    names   integer not null,
    size    integer not null,
    sha256  blob    not null,
    inline  integer not null,
    damaged integer not null default 0
) strict;

create index contents_unnamed on contents (id) where names = 0;

create table bodies (
    id    integer primary key,
    bytes blob    not null
) strict;

-- a Clear past its bound marks its folder instead of deleting it: under
-- prefix, an object of revision cleared or older is gone to every statement,
-- and maintenance deletes such objects a batch at a time, then the mark; the
-- bucket's root is the empty prefix
create table cleared (
    bucket   integer not null,
    prefix   text    not null,
    revision integer not null,
    primary key (bucket, prefix)
) strict, without rowid;

-- revision: the high-water mark of revisions, kept with the writes that take them;
-- ids: the content ids reserved, a block at a time;
-- settled: every id at or below it is committed or has no file
create table meta (
    name  text    primary key,
    value integer not null
) strict, without rowid;

insert into meta (name, value) values ('revision', 0), ('ids', 0), ('settled', 0);

-- the scrub's place: the content it reads and how far it has read it, the
-- hash of the bytes before there, and the bytes a slice reads in this pass
create table scrub (
    content integer not null,
    offset  integer not null,
    state   blob,
    pace    integer not null
) strict;

insert into scrub (content, offset, state, pace) values (0, 0, null, 0);
