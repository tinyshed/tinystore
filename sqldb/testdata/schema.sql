CREATE TABLE users (
    id           INTEGER PRIMARY KEY,
    email        TEXT NOT NULL,
    display_name TEXT NOT NULL,
    created_at   INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX users_email ON users (email);

CREATE TABLE notes (
    id         TEXT NOT NULL PRIMARY KEY,
    author_id  INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title      TEXT NOT NULL,
    done       INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1)),
    tags       TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tags)),
    due        TEXT CHECK (due IS date(due)),
    created_at INTEGER NOT NULL,
    CHECK (length(title) > 0)
) STRICT;
CREATE INDEX notes_author_id_created_at ON notes (author_id, created_at);
CREATE INDEX notes_open ON notes (done, due);
