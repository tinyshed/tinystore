CREATE TABLE notes (
    id         INTEGER PRIMARY KEY,
    title      TEXT NOT NULL,
    tags       TEXT NOT NULL CHECK (json_valid(tags)),
    due        TEXT CHECK (due IS date(due)),
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX notes_created_at ON notes (created_at);
