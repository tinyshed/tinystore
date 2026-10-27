-- A database's files: blobs' tables in an application's database, named
-- _tinystore_blobs_… beside its own tables, as blobs.db names them since its
-- 0002_names.sql. What each table holds is as 0001_schema.sql has it; the
-- bytes too large to keep inline lie in the directory beside the database's
-- file, sql/<name>.blobs/objects/.

create table _tinystore_blobs_sets (
    id   integer primary key,
    name text    not null unique
) strict;

-- path: a folder's segments and the file's own, joined by '/', so that a
-- folder is one range of the table;
-- revision: the revision of blobs.db that wrote the row, which a marked clear compares;
-- sha256: the content's, so that a head or a list reads no other table;
-- modified, expires: unix milliseconds by the store's clock, expires null for never;
-- meta: a JSON object of strings, null for none
create table _tinystore_blobs_files (
    set_id   integer not null,
    path     text    not null,
    revision integer not null,
    content  integer not null,
    size     integer not null,
    sha256   blob    not null,
    type     text    not null,
    modified integer not null,
    expires  integer,
    meta     text,
    primary key (set_id, path)
) strict, without rowid;

create index _tinystore_blobs_files_by_expiry on _tinystore_blobs_files (expires, set_id, path) where expires is not null;

-- a content is bytes that files name: its id names its file under objects/
-- and is never given twice;
-- names: how many files name it, 0 once none does and its file waits to go;
-- inline: its bytes are a row of bodies instead of a file;
-- damaged: the scrub found its bytes changed or missing
create table _tinystore_blobs_contents (
    id      integer primary key,
    names   integer not null,
    size    integer not null,
    sha256  blob    not null,
    inline  integer not null,
    damaged integer not null default 0
) strict;

create index _tinystore_blobs_contents_unnamed on _tinystore_blobs_contents (id) where names = 0;

create table _tinystore_blobs_bodies (
    id    integer primary key,
    bytes blob    not null
) strict;

-- a clear past its bound marks its folder instead of deleting it: under
-- prefix, a file of the mark's revision or older is gone to every statement,
-- and maintenance deletes such rows a batch at a time, then the mark; the
-- files themselves are the empty prefix
create table _tinystore_blobs_cleared (
    set_id   integer not null,
    prefix   text    not null,
    revision integer not null,
    primary key (set_id, prefix)
) strict, without rowid;

-- revision: the high-water mark of revisions, kept by the writes that take them;
-- ids: the content ids reserved, a block at a time;
-- settled: every id at or below it is committed or has no file
create table _tinystore_blobs_meta (
    name  text    primary key,
    value integer not null
) strict, without rowid;

insert into _tinystore_blobs_meta (name, value) values ('revision', 0), ('ids', 0), ('settled', 0);

-- the scrub's place: the content it reads next, and the bytes a slice reads
-- in this pass, 0 to begin one; a content a restart cut is read again
create table _tinystore_blobs_scrub (
    content integer not null,
    pace    integer not null
) strict;

insert into _tinystore_blobs_scrub (content, pace) values (0, 0);
