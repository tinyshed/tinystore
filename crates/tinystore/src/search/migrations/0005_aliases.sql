-- a document's aliases: each a word that stands for the text of one of its
-- fields, named by the field's path, as its set gave them. Its postings
-- hold the word in that field's parts, counted in no length; the row goes
-- with the document's number, so that a set again or a delete takes it,
-- and a rebuild reads it with the document. An index whose documents have
-- no alias holds no row and reads as before. A release before this step
-- refuses the file at its open, rather than read such a document again
-- without its aliases.
create table _tinystore_search_aliases (
    index_id integer not null,
    doc      integer not null,
    alias    text    not null,
    field    text    not null,
    primary key (index_id, doc, alias)
) without rowid, strict;
