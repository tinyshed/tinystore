package sqldb

import (
	"database/sql"
	"errors"
	"testing"

	"github.com/tinyshed/tinystore"
)

func TestSnakeCaseNamesColumns(t *testing.T) {
	for field, column := range map[string]string{
		"ID": "id", "CreatedAt": "created_at", "UserID": "user_id", "HTTPCode": "http_code",
	} {
		if got := snakeCase(field); got != column {
			t.Errorf("%s → %s, want %s", field, got, column)
		}
	}
}

type Audited struct {
	CreatedAt int64
}

type withEmbedded struct {
	*Audited
	Key   int64  `db:"id"`
	Title string `db:"title"`
	Skip  string `db:"-"`
	Body  sql.NullString
}

// A struct takes each column by its tag or its field's name, an embedded
// struct's fields counting as its own even through a nil pointer. It refuses a
// column no field takes.
func TestStructsTakeColumnsByTagOrNameAndRefuseWhatDoesNotFit(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	if _, err := db.Exec(ctx, `insert into notes (title) values ('first')`); err != nil {
		t.Fatal(err)
	}

	got, found, err := One[withEmbedded](ctx, db, `select id, title, body, 7 as created_at from notes`)
	if err != nil || !found || got.Key != 1 || got.Title != "first" || !got.Body.Valid || got.Audited == nil ||
		got.CreatedAt != 7 {
		t.Fatalf("mapped %+v, %v", got, err)
	}
	if _, _, err = One[withEmbedded](ctx, db, `select id, title as headline from notes`); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a column no field takes: %v", err)
	}
	if _, _, err = One[withEmbedded](ctx, db, `select id, 'x' as skip from notes`); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a column of a field left out: %v", err)
	}
}
