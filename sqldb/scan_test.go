package sqldb

import (
	"database/sql"
	"errors"
	"testing"
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

type audited struct {
	CreatedAt int64
}

type taggedNote struct {
	audited
	Key   int64  `db:"id"`
	Title string `db:"title"`
	Skip  string `db:"-"`
	Body  sql.NullString
}

func TestStructsTakeColumnsByTagOrNameAndRefuseWhatDoesNotFit(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	if _, err := db.Exec(ctx, `insert into notes (title) values ('first')`); err != nil {
		t.Fatal(err)
	}

	got, err := One[taggedNote](ctx, db, `select id, title, body, 7 as created_at from notes`)
	if err != nil || got.Key != 1 || got.Title != "first" || !got.Body.Valid || got.CreatedAt != 7 {
		t.Fatalf("mapped %+v, %v", got, err)
	}
	if _, err = One[taggedNote](ctx, db, `select id, title as headline from notes`); !errors.Is(err, ErrShape) {
		t.Fatalf("a column no field takes: %v", err)
	}
	if _, err = Scalar[taggedNote](ctx, db, `select id from notes`); !errors.Is(err, ErrShape) {
		t.Fatalf("a struct asked of Scalar: %v", err)
	}
	if _, err = Scalar[int64](ctx, db, `select id, title from notes`); !errors.Is(err, ErrShape) {
		t.Fatalf("two columns into one value: %v", err)
	}
	if _, err = db.Exec(ctx, `insert into notes (title) values ('second')`); err != nil {
		t.Fatal(err)
	}
	if _, err = One[taggedNote](ctx, db, `select id, title from notes`); !errors.Is(err, ErrManyRows) {
		t.Fatalf("two rows asked of One: %v", err)
	}
}
