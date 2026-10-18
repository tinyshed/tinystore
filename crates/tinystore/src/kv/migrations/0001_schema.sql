-- kv.db has 4 KiB pages, chosen when the file is created: at 1 KiB a row with
-- a 256-byte value no longer fits what a page keeps of a row. Every name is
-- _tinystore_kv_…, so that the tables can live in an application's database.

-- role: what a bucket is for: values, encrypted values, counters, a rate
-- limit, a quota or once; a name keeps its role;
-- key: the id of the encryption key that the values of an encrypted bucket
-- are sealed with, kept from its first write until the bucket is cleared whole
create table _tinystore_kv_buckets (
    id   integer primary key,
    name text    not null unique,
    role text    not null,
    key  text
) strict;

-- every value in one narrow table: value holds bytes, an integer or nothing;
-- path: the owners and the key, each after a mark, so that a branch is one range;
-- version: the revision of the file that wrote the value, never repeated;
-- expires: unix milliseconds by the store's clock, null for never;
-- spill: the row of _tinystore_kv_spilled holding a value over 512 bytes
create table _tinystore_kv_cells (
    bucket  integer not null,
    path    blob    not null,
    version integer not null,
    expires integer,
    value   any,
    spill   integer,
    primary key (bucket, path)
) strict, without rowid;

create index _tinystore_kv_cells_expiry on _tinystore_kv_cells (expires, bucket, path) where expires is not null;

create table _tinystore_kv_spilled (
    id    integer primary key,
    value blob    not null
) strict;

-- a clear past its bound marks its branch instead of deleting it: under the
-- prefix, a row whose version is cleared or older is gone to every statement,
-- and maintenance deletes such rows a batch at a time, then the mark; the
-- bucket's root is the empty prefix
create table _tinystore_kv_branches (
    bucket  integer not null,
    prefix  blob    not null,
    cleared integer not null,
    primary key (bucket, prefix)
) strict, without rowid;

-- a config: its version, which every change of it moves, and the last
-- declaration a program opened it with, as JSON, so that a command can read
-- a config while no program runs
create table _tinystore_kv_configs (
    name        text    primary key,
    version     integer not null,
    declaration text
) strict, without rowid;

-- a change made while programs ran, a row a setting of a config: its value
-- as JSON, the config's version that made it, and when, in unix milliseconds
create table _tinystore_kv_config_changes (
    name    text    not null,
    path    text    not null,
    value   text    not null,
    version integer not null,
    at      integer not null,
    primary key (name, path)
) strict, without rowid;

-- the last op of each node a home applied of those the node queued, an add of
-- counters that answered at once there, kept in the commit that applies it so
-- that an op sent twice counts once
create table _tinystore_kv_queued (
    node    text    primary key,
    applied integer not null
) strict, without rowid;

-- revision: the high-water mark of versions, kept with the writes that take them
create table _tinystore_kv_meta (
    name  text    primary key,
    value integer not null
) strict, without rowid;

insert into _tinystore_kv_meta (name, value) values ('revision', 0);
