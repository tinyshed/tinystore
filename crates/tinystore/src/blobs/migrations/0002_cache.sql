-- A database's files as a cache (#64): what blobs.db's 0004_cache.sql adds, on
-- the same names, so that a set in a database's file keeps its ceiling and
-- evicts as one in blobs.db does.
--
-- used: when a read last recorded a use of the file, null for none since its
-- write; rank: what eviction orders by, lowest first, null for its modified
-- time, which a file no read used since its write ranks at
alter table _tinystore_blobs_files add column used integer;
alter table _tinystore_blobs_files add column rank integer;

create index _tinystore_blobs_files_by_rank on _tinystore_blobs_files (set_id, coalesce(rank, modified));

-- count, size: the set's rows and their sizes, a copy counted again, kept by
-- the triggers below on every path that adds, removes or resizes a row, so
-- that a write knows the set's total without a scan; an expired file or one a
-- marked clear hid counts until maintenance deletes its row;
-- max_size: the ceiling, null for none; when_full: what a write past it
-- does, 'evict' or 'refuse', null without a ceiling
alter table _tinystore_blobs_sets add column count integer not null default 0;
alter table _tinystore_blobs_sets add column size integer not null default 0;
alter table _tinystore_blobs_sets add column max_size integer;
alter table _tinystore_blobs_sets add column when_full text;

update _tinystore_blobs_sets set
    count = (select count(*) from _tinystore_blobs_files as f where f.set_id = _tinystore_blobs_sets.id),
    size = (select coalesce(sum(f.size), 0) from _tinystore_blobs_files as f where f.set_id = _tinystore_blobs_sets.id);

create trigger _tinystore_blobs_files_added after insert on _tinystore_blobs_files begin
    update _tinystore_blobs_sets set count = count + 1, size = size + new.size where id = new.set_id;
end;

create trigger _tinystore_blobs_files_removed after delete on _tinystore_blobs_files begin
    update _tinystore_blobs_sets set count = count - 1, size = size - old.size where id = old.set_id;
end;

create trigger _tinystore_blobs_files_resized after update of size on _tinystore_blobs_files
    when new.size <> old.size begin
    update _tinystore_blobs_sets set size = size + new.size - old.size where id = new.set_id;
end;
