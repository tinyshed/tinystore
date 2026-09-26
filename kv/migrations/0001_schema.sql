-- kv.db has 4 KiB pages, chosen when the file is created: at 1 KiB a row with
-- a 256-byte value no longer fits what a page keeps of a row

-- kind: what a bucket's values are, and a bucket opened as another is refused
create table buckets (
    id   integer primary key,
    name text    not null unique,
    kind text    not null
) strict;

-- every kind in one narrow table: value holds a blob, an integer or nothing;
-- path: the owners and the key, each after a mark, so that a branch is one range;
-- version: the revision of kv.db that wrote the value, never repeated;
-- expires: unix milliseconds by the store's clock, null for never;
-- spill: the row of spilled that holds a value over 512 bytes
create table cells (
    bucket  integer not null,
    path    blob    not null,
    version integer not null,
    expires integer,
    value   any,
    spill   integer,
    primary key (bucket, path)
) strict, without rowid;

create index cells_expiry on cells (expires, bucket, path) where expires is not null;

create table spilled (
    id    integer primary key,
    value blob    not null
) strict;

-- a Clear past its bound marks its branch instead of deleting it: under
-- prefix, a row whose version is cleared or older is gone to every statement,
-- and maintenance deletes such rows a batch at a time, then the mark; the
-- bucket's root is the empty prefix
create table branches (
    bucket  integer not null,
    prefix  blob    not null,
    cleared integer not null,
    primary key (bucket, prefix)
) strict, without rowid;

-- revision: the high-water mark of versions, kept with the writes that take them
create table meta (
    name  text    primary key,
    value integer not null
) strict, without rowid;

insert into meta (name, value) values ('revision', 0);
