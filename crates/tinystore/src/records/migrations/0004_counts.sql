-- From here on each key a segment holds says how many of its events carry
-- it, events, and the kinds of value they hold there, kinds, so that a
-- stream says what its events carry without reading one. kinds has a bit a
-- kind: 1 float, 2 integer, 4 text, 8 bool, 16 object, 32 list, 64 null;
-- an event's name has none. A segment an earlier release packed has neither
-- till a step of upkeep counts its events from its blocks.
alter table segment_keys add column events integer;
alter table segment_keys add column kinds integer;

-- the segments whose keys upkeep has yet to count, none once it has
create index segment_keys_uncounted on segment_keys (segment) where events is null;
