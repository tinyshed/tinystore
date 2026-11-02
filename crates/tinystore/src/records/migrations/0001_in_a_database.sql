-- A database's streams: the events a transaction or a batch of an
-- application's database appended to a stream of records.db, kept in the
-- database's file until they are moved there, so that they commit with its
-- rows or roll back with them. Named _tinystore_records_… beside the
-- application's own tables; records.db's history is a history of its own.
--
-- seq: the order they were appended in, which their move keeps;
-- stream: the stream's name in records.db;
-- id: the event's, the program's or one the store gave, which makes a
-- second move of it write nothing;
-- at: its time, nanoseconds since 1970, taken at the append;
-- level: slog's number; trace_id, span_id: 16 and 8 bytes;
-- context, fields: a JSON object each, every value spelled as it was written
create table _tinystore_records_outbox (
    seq      integer primary key,
    stream   text    not null,
    id       text    not null,
    at       integer not null,
    name     text    not null,
    level    integer,
    message  text,
    trace_id blob,
    span_id  blob,
    context  text    not null,
    fields   text    not null
) strict;
