-- blobs' tables take names of their own, _tinystore_blobs_…, so that the same
-- tables live in an application's database beside its own, as kv's and jobs'
-- do: a database's files are made with these names from their first step.
-- An index keeps its name through a rename, so each is made again.

alter table sets rename to _tinystore_blobs_sets;
alter table files rename to _tinystore_blobs_files;
alter table contents rename to _tinystore_blobs_contents;
alter table bodies rename to _tinystore_blobs_bodies;
alter table cleared rename to _tinystore_blobs_cleared;
alter table meta rename to _tinystore_blobs_meta;
alter table scrub rename to _tinystore_blobs_scrub;

drop index files_by_expiry;
create index _tinystore_blobs_files_by_expiry on _tinystore_blobs_files (expires, set_id, path) where expires is not null;
drop index contents_unnamed;
create index _tinystore_blobs_contents_unnamed on _tinystore_blobs_contents (id) where names = 0;
