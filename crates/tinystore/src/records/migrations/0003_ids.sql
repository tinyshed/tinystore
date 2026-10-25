-- From here on an event may carry an id: a head row of version 2, a segment
-- of version 2 and a block of version 4 keep it with the event, and the
-- stream holds it here, so that an append of an id it holds writes nothing.
-- at: the event's time. An id goes once no head row or segment of its stream
-- can hold its event, older than the oldest they hold; its key stays on its
-- 1 KiB page, since an id is 128 bytes at most.
create table event_ids (
    stream integer not null,
    id     text    not null,
    at     integer not null,
    primary key (stream, id)
) without rowid, strict;

create index event_ids_by_time on event_ids (stream, at);
