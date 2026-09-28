package sqldb

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"reflect"
	"strings"
	"sync"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

type toggle struct {
	ID   int64 `db:",generated"`
	On   bool
	Name string
	Due  *Date
}

// Insert writes every field as it holds it, the zero values included, and
// leaves the generated columns to the database, whatever their fields hold
func TestInsertWritesEveryFieldButTheGeneratedOnes(t *testing.T) {
	table := Table[toggle]("toggles", PrimaryKey("id"), Default("on", true), Default("name", "unnamed"))
	db := openSchema(t, Schema(table))
	ctx := t.Context()

	first, err := Insert(ctx, db, table, toggle{ID: 99})
	if err != nil {
		t.Fatal(err)
	}
	if first.ID != 1 || first.On || first.Name != "" || first.Due != nil {
		t.Fatalf("inserted %+v; want id 1 generated, and false, empty and NULL as the fields held them", first)
	}
	raw, _, err := ExecOne[toggle](ctx, db, `insert into toggles default values returning *`)
	if err != nil || !raw.On || raw.Name != "unnamed" {
		t.Fatalf("a raw insert leaving the columns out: %+v, %v; want the defaults", raw, err)
	}
	if table.insert != `INSERT INTO toggles ("on", name, due) VALUES (?, ?, ?)` {
		t.Fatalf("Insert runs %s", table.insert)
	}
}

type ticket struct {
	ID       int64  `db:",generated"`
	Code     string `db:",generated"`
	Title    string
	OpenedAt *time.Time
}

type embeddedTicket struct {
	*TicketMeta
	Title string
}

type TicketMeta struct {
	ID       int64  `db:",generated"`
	Code     string `db:",generated"`
	OpenedAt *time.Time
}

func TestInsertDoesNotChangeAPromotedPointerInTheCaller(t *testing.T) {
	table := Table[embeddedTicket]("embedded_tickets", PrimaryKey("id"), DefaultSQL("code", "'made'"))
	db := openSchema(t, Schema(table))
	opened := time.Date(2026, 9, 28, 12, 0, 0, 123_456_789, time.FixedZone("here", 3*3600))
	meta := &TicketMeta{ID: 99, Code: "caller", OpenedAt: &opened}
	row := embeddedTicket{TicketMeta: meta, Title: "title"}
	inserted, err := Insert(t.Context(), db, table, row)
	if err != nil {
		t.Fatal(err)
	}
	if meta.ID != 99 || meta.Code != "caller" || meta.OpenedAt != &opened || opened.Nanosecond() != 123_456_789 {
		t.Fatalf("Insert changed the caller's embedded value: %+v", meta)
	}
	if inserted.TicketMeta == meta || inserted.ID != 1 || inserted.Code != "made" ||
		inserted.OpenedAt.Nanosecond() != 123_000_000 {
		t.Fatalf("Insert returned %+v", inserted)
	}
}

// a generated column a default fills comes back through RETURNING, and a time
// comes back as the file keeps it, the caller's own value untouched
func TestInsertReturnsWhatTheDatabaseGenerated(t *testing.T) {
	table := Table[ticket]("tickets", PrimaryKey("id"), DefaultSQL("code", "lower(hex(randomblob(4)))"))
	if table.insert != "INSERT INTO tickets (title, opened_at) VALUES (?, ?) RETURNING id, code" {
		t.Fatalf("Insert runs %s", table.insert)
	}
	db := openSchema(t, Schema(table))
	local := time.FixedZone("here", 3*3600)
	opened := time.Date(2026, 9, 28, 12, 0, 0, 123_456_789, local)
	given := opened
	inserted, err := Insert(t.Context(), db, table, ticket{Title: "a", OpenedAt: &given})
	if err != nil {
		t.Fatal(err)
	}
	read, _, err := One[ticket](t.Context(), db, `select * from tickets where id = ?`, inserted.ID)
	if err != nil || inserted.ID != 1 || len(inserted.Code) != 8 || !reflect.DeepEqual(inserted, read) {
		t.Fatalf("Insert returned %+v, the file holds %+v, %v", inserted, read, err)
	}
	if !given.Equal(opened) || given.Location() != local || inserted.OpenedAt == &given {
		t.Fatalf("Insert changed the caller's time to %v", given)
	}
}

func openSchema(t *testing.T, schema *SchemaDef) *DB {
	t.Helper()
	migrations := map[string]string{"001_schema.sql": schema.SQL()}
	db, err := Open(t.Context(), openStore(t, t.TempDir()), "app", mapFS(migrations), schema)
	if err != nil {
		t.Fatal(err)
	}
	return db
}

// a transaction holding the writer past a group's hold fails none of the
// writes waiting behind it, and the one leading them is logged once, with the
// transaction that holds the writer
func TestALongTransactionFailsNoWriteBehindIt(t *testing.T) {
	var logged bytes.Buffer
	var logMu sync.Mutex
	store := openStoreWith(t, t.TempDir(), tinystore.Options{Manual: true, Logger: slog.New(slog.NewTextHandler(
		lockedWriter{&logged, &logMu}, &slog.HandlerOptions{Level: slog.LevelWarn}))})
	timing := tuning{
		snapshot: snapshotHold,
		writer:   sqlite.Config{GroupHold: 50 * time.Millisecond, Patience: 30 * time.Millisecond},
	}
	db, err := open(t.Context(), store, "app", notesMigrations, nil, timing)
	if err != nil {
		t.Fatal(err)
	}
	ctx := t.Context()

	holding, release := make(chan struct{}), make(chan struct{})
	transaction := make(chan error, 1)
	go func() {
		transaction <- db.Tx(ctx, func(tx *Tx) error {
			close(holding)
			<-release
			_, insertErr := tx.Exec(ctx, `insert into notes (title) values ('the transaction')`)
			return insertErr
		})
	}()
	<-holding

	errs := make([]error, 8)
	var wg sync.WaitGroup
	for n := range errs {
		wg.Go(func() { _, errs[n] = db.Exec(ctx, `insert into notes (title) values (?)`, fmt.Sprint("waiting ", n)) })
	}
	time.Sleep(250 * time.Millisecond)
	close(release)
	wg.Wait()

	if err = errors.Join(append(errs, <-transaction)...); err != nil {
		t.Fatalf("writes behind a transaction five holds long: %v", err)
	}
	if count, _ := Scalar[int](ctx, db, `select count(*) from notes`); count != 9 {
		t.Fatalf("%d notes, want the transaction's and eight", count)
	}
	logMu.Lock()
	defer logMu.Unlock()
	text := logged.String()
	if strings.Count(text, "a write has waited ten seconds behind a transaction") != 1 ||
		!strings.Contains(text, "TestALongTransactionFailsNoWriteBehindIt") || !strings.Contains(text, "insert into notes") {
		t.Fatalf("logged:\n%s", text)
	}
}

// a call on the DB inside its own Tx waits for the Tx, which waits for it,
// until the call's context ends; the Tx then goes on
func TestACallOnTheDBInsideItsOwnTxEndsWithItsContext(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	err := db.Tx(ctx, func(tx *Tx) error {
		waiting, cancel := context.WithTimeout(ctx, 50*time.Millisecond)
		defer cancel()
		if _, err := db.Exec(waiting, `insert into notes (title) values ('outside')`); !errors.Is(err, context.DeadlineExceeded) {
			return fmt.Errorf("the call on the DB returned %w", err)
		}
		_, err := tx.Exec(ctx, `insert into notes (title) values ('inside')`)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if titles, _ := Scalar[string](ctx, db, `select group_concat(title) from notes`); titles != "inside" {
		t.Fatalf("the file holds %q", titles)
	}
}

type lockedWriter struct {
	out *bytes.Buffer
	mu  *sync.Mutex
}

func (w lockedWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.out.Write(p)
}

func mapFS(files map[string]string) fstest.MapFS {
	mapped := fstest.MapFS{}
	for name, text := range files {
		mapped[name] = &fstest.MapFile{Data: []byte(text)}
	}
	return mapped
}
