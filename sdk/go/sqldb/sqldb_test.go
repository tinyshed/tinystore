package sqldb_test

import (
	"context"
	"errors"
	"fmt"
	"math"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/fstest"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/jobs"
	"tinyshed.org/tinystore/kv"
	"tinyshed.org/tinystore/records"
	"tinyshed.org/tinystore/sqldb"
)

const notesTable = `create table notes (
    id         text primary key,
    author_id  integer not null,
    title      text not null,
    done       integer not null default 0 check (done in (0, 1)),
    tags       text not null default '[]' check (json_valid(tags)),
    created_at integer not null default 0,
    raw        blob
) strict`

type Note struct {
	ID        string
	AuthorID  int64
	Title     string
	Done      bool
	Tags      []string
	CreatedAt time.Time
	Raw       []byte
}

func app(t *testing.T, store *tinystore.Store) *sqldb.DB {
	t.Helper()
	migrations := fstest.MapFS{"0001_notes.sql": {Data: []byte(notesTable)}}
	return ways.Must(sqldb.Open(context.Background(), store, "app", sqldb.Migrations(migrations)))
}

func is(t *testing.T, err error, kind error) {
	t.Helper()
	if !errors.Is(err, kind) {
		t.Fatalf("got %v, want %v", err, kind)
	}
}

func TestAWriteReturnsOnceItIsDurableAndTheVerbsSayWhatComesBack(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		written := ways.Must(db.Exec(ctx, `insert into notes (id, author_id, title) values (?, ?, ?)`, "n1", 42, "Buy milk"))
		if written.Changes != 1 {
			t.Fatalf("written %+v", written)
		}
		ways.Must(db.Exec(ctx, `insert into notes (id, author_id, title) values (?, ?, ?)`, "n2", 42, "Call mom"))
		mine := ways.Must(sqldb.All[Note](ctx, db, `select * from notes where author_id = ? order by id`, 42))
		if len(mine) != 2 || mine[0].Title != "Buy milk" || mine[1].Title != "Call mom" {
			t.Fatalf("mine %+v", mine)
		}
		if _, found := ways.Must2(sqldb.One[sqldb.Row](ctx, db, `select * from notes where id = ?`, "none")); found {
			t.Fatal("no row is none")
		}
		if ways.Must(sqldb.Scalar[int64](ctx, db, `select count(*) from notes`)) != 2 {
			t.Fatal("two notes")
		}
		done, found := ways.Must2(sqldb.One[Note](ctx, db, `update notes set done = 1 where id = ? returning *`, "n1"))
		if !found || !done.Done {
			t.Fatalf("done %+v", done)
		}
		_, _, err := sqldb.One[sqldb.Row](ctx, db, `update notes set done = 1 returning id`)
		is(t, err, tinystore.ErrInvalid)
		var count int64
		for note, err := range sqldb.Each[Note](ctx, db, `select * from notes order by id`) {
			if err != nil {
				t.Fatal(err)
			}
			count++
			if note.AuthorID != 42 {
				t.Fatalf("note %+v", note)
			}
		}
		if count != 2 {
			t.Fatalf("each read %d", count)
		}
	})
}

// contract: SQL-13
func TestEachReadsRowsPastWhatAllHoldsAsTheServerReadsThem(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		ways.Must(db.Exec(ctx, `create table big (id integer primary key, data blob not null)`))
		ways.Must(db.Exec(ctx, `with recursive n(i) as (select 1 union all select i + 1 from n where i < 280)
			insert into big (id, data) select i, zeroblob(262144) from n`))
		type Big struct {
			ID   int64
			Data []byte
		}
		_, err := sqldb.All[Big](ctx, db, `select id, data from big`)
		is(t, err, tinystore.ErrLimit)

		var rows, bytes int64
		for row, err := range sqldb.OpenTable[Big](db, "big").OrderBy("id", sqldb.Asc).Each(ctx) {
			if err != nil {
				t.Fatal(err)
			}
			rows++
			if row.ID != rows {
				t.Fatalf("row %d after %d", row.ID, rows-1)
			}
			bytes += int64(len(row.Data))
		}
		if rows != 280 || bytes != 280*262144 {
			t.Fatalf("each read %d rows of %d bytes", rows, bytes)
		}
		var raw int64
		for row, err := range sqldb.Each[Big](ctx, db, `select id, data from big`) {
			if err != nil {
				t.Fatal(err)
			}
			raw += int64(len(row.Data))
		}
		if raw != 280*262144 {
			t.Fatalf("each read %d bytes", raw)
		}
	})
}

func TestAKeyAlreadyHeldIsAConflictNamingTheDatabaseAndTheConstraint(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		ways.Must(db.Exec(ctx, `insert into notes (id, author_id, title) values ('k1', 1, 'a')`))
		_, err := db.Exec(ctx, `insert into notes (id, author_id, title) values ('k1', 1, 'b')`)
		var e *tinystore.Error
		if !errors.As(err, &e) || e.Kind != tinystore.Conflict || !strings.Contains(err.Error(), "sql app") {
			t.Fatalf("got %v", err)
		}
		if e.Constraint == nil || e.Constraint.Kind != "primaryKey" || e.Constraint.Table != "notes" || e.Constraint.Columns[0] != "id" {
			t.Fatalf("constraint %+v", e.Constraint)
		}
		err = db.Tx(ctx, func(tx *sqldb.Tx) error {
			_, err := tx.Exec(ctx, `insert into notes (id, author_id, title, done) values ('k2', 1, 'b', 7)`)
			return err
		})
		if !errors.As(err, &e) || e.Kind != tinystore.Invalid || e.Constraint == nil || e.Constraint.Kind != "check" {
			t.Fatalf("got %v", err)
		}
	})
}

func TestABatchWritesAllOfItsStatementsOrNone(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		ways.Must(0, db.Batch(ctx,
			sqldb.SQL(`insert into notes (id, author_id, title) values (?, 7, 'a')`, "b1"),
			sqldb.SQL(`insert into notes (id, author_id, title) values (?, ?, ?)`, "b2", 7, "b")))
		err := db.Batch(ctx,
			sqldb.SQL(`insert into notes (id, author_id, title) values ('b3', 7, 'c')`),
			sqldb.SQL(`insert into notes (id, author_id, title) values ('b1', 7, 'taken')`))
		is(t, err, tinystore.ErrConflict)
		if ways.Must(sqldb.Scalar[int64](ctx, db, `select count(*) from notes where author_id = 7`)) != 2 {
			t.Fatal("a batch writes all or none")
		}
		for _, text := range []string{"attach database 'other.db' as other", "commit", "pragma query_only = off"} {
			_, err := sqldb.All[sqldb.Row](ctx, db, text)
			is(t, err, tinystore.ErrInvalid)
			_, err = db.Exec(ctx, text)
			is(t, err, tinystore.ErrInvalid)
		}
	})
}

type Title struct {
	Title string
}

func TestATransactionSeesItsOwnWritesCommitsAndRollsBackAnError(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		var titles []Title
		ways.Must(0, db.Tx(ctx, func(tx *sqldb.Tx) error {
			if _, err := tx.Exec(ctx, `insert into notes (id, author_id, title) values ('t1', 9, 'a')`); err != nil {
				return err
			}
			if _, err := tx.Exec(ctx, `insert into notes (id, author_id, title) values ('t1', 9, 'b')`); !errors.Is(err, tinystore.ErrConflict) {
				t.Errorf("a taken key is a conflict, inside the transaction too: %v", err)
			}
			var err error
			titles, err = sqldb.All[Title](ctx, tx, `select title from notes where author_id = 9`)
			return err
		}))
		if len(titles) != 1 || titles[0].Title != "a" {
			t.Fatalf("titles %+v", titles)
		}
		soldOut := errors.New("sold out")
		err := db.Tx(ctx, func(tx *sqldb.Tx) error {
			if _, err := tx.Exec(ctx, `delete from notes where author_id = 9`); err != nil {
				return err
			}
			return soldOut
		})
		if !errors.Is(err, soldOut) {
			t.Fatalf("got %v", err)
		}
		if ways.Must(sqldb.Scalar[int64](ctx, db, `select count(*) from notes where author_id = 9`)) != 1 {
			t.Fatal("an error rolls back")
		}
	})
}

// contract: SQL-14
func TestAReadInATransactionCarriesTheRowsItCarriesOutsideOnePastOneAnswer(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		ways.Must(db.Exec(ctx, `with recursive n(i) as (select 1 union all select i + 1 from n where i < 20000)
			insert into notes (id, author_id, title) select printf('tx%05d', i), 14, printf('%0200d', i) from n`))
		const read = `select id, author_id, title from notes where author_id = 14 order by id`
		outside := ways.Must(sqldb.All[Note](ctx, db, read))
		var inside []Note
		ways.Must(0, db.Tx(ctx, func(tx *sqldb.Tx) error {
			var err error
			if inside, err = sqldb.All[Note](ctx, tx, read); err != nil {
				return err
			}
			_, err = tx.Exec(ctx, `delete from notes where author_id = 14`)
			return err
		}))
		if len(inside) != 20000 || !slices.EqualFunc(inside, outside, func(a, b Note) bool { return a.ID == b.ID && a.Title == b.Title }) {
			t.Fatalf("%d rows inside, %d outside", len(inside), len(outside))
		}
		if ways.Must(sqldb.Scalar[int64](ctx, db, `select count(*) from notes where author_id = 14`)) != 0 {
			t.Fatal("the transaction committed its delete")
		}
	})
}

func TestTransactionsPastTheThreadsOfTheStoreWaitTheirTurnAndTheOneHoldingTheWriterGoesOn(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		other := ways.Must(sqldb.Open(ctx, store, "other"))
		gate := make(chan struct{})
		holding := make(chan error, 1)
		go func() {
			holding <- db.Tx(ctx, func(tx *sqldb.Tx) error {
				if _, err := tx.Exec(ctx, `insert into notes (id, author_id, title) values ('turn-0', 1, 'held')`); err != nil {
					return err
				}
				<-gate
				// another database's query runs on one of the store's threads, while the others wait for the writer
				_, err := sqldb.Scalar[int64](ctx, other, `select 1`)
				return err
			})
		}()
		waiting := make(chan error, 40)
		for n := 1; n <= 40; n++ {
			go func() {
				waiting <- db.Tx(ctx, func(tx *sqldb.Tx) error {
					_, err := tx.Exec(ctx, `insert into notes (id, author_id, title) values (?, 1, 'waited')`, fmt.Sprintf("turn-%d", n))
					return err
				})
			}()
		}
		// time for every transaction to reach the store before the one holding the writer goes on
		time.Sleep(100 * time.Millisecond)
		close(gate)
		if err := <-holding; err != nil {
			t.Fatalf("the transaction holding the writer: %v", err)
		}
		for range 40 {
			if err := <-waiting; err != nil {
				t.Fatalf("a transaction that waited its turn: %v", err)
			}
		}
		if count := ways.Must(sqldb.Scalar[int64](ctx, db, `select count(*) from notes where id like 'turn-%'`)); count != 41 {
			t.Fatalf("%d transactions committed", count)
		}
	})
}

func TestAValueGoesAsSQLiteKeepsItAndOneItWouldChangeIsRefused(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		raw := []byte{0, 255, 7}
		at := time.Date(2026, 10, 10, 12, 0, 0, 123_000_000, time.UTC)
		ways.Must(db.Exec(ctx, `insert into notes (id, author_id, title, done, tags, created_at, raw) values (?, ?, ?, ?, ?, ?, ?)`,
			"v1", int64(1)<<60, "x", true, sqldb.JSON([]string{"home"}), at, raw))
		kept, _ := ways.Must2(sqldb.One[sqldb.Row](ctx, db, `select * from notes where id = ?`, "v1"))
		if kept["author_id"] != int64(1)<<60 || kept["done"] != int64(1) || kept["tags"] != `["home"]` || kept["created_at"] != int64(1791633600123) {
			t.Fatalf("kept %v", kept)
		}
		typed, _ := ways.Must2(sqldb.One[Note](ctx, db, `select * from notes where id = ?`, "v1"))
		if !typed.Done || len(typed.Tags) != 1 || !typed.CreatedAt.Equal(at) || string(typed.Raw) != string(raw) {
			t.Fatalf("typed %+v", typed)
		}
		for _, refused := range []any{[]string{"a"}, map[string]int{"a": 1}, uint64(math.MaxUint64), math.NaN()} {
			_, err := sqldb.All[sqldb.Row](ctx, db, `select ?`, refused)
			is(t, err, tinystore.ErrInvalid)
		}
		_, err := sqldb.All[sqldb.Row](ctx, db, `select ?, ?`, 1)
		is(t, err, tinystore.ErrInvalid)
		for _, n := range []int64{0, 127, -33, 65_536, 1 << 53, math.MinInt64, math.MaxInt64} {
			if ways.Must(sqldb.Scalar[int64](ctx, db, `select ?`, n)) != n {
				t.Fatalf("%d does not come back", n)
			}
		}
	})
}

type Samples struct {
	TS     []int64
	CPU    []*float64
	Hits   []*int64
	Host   []*string
	Raw    [][]byte
	Either []float64
}

func TestAStatementGivesByItsColumnsWhatItGivesByItsRows(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		ways.Must(db.Exec(ctx, `create table samples (ts integer not null, cpu real, hits integer, host text, raw blob) strict`))
		ways.Must(0, db.Batch(ctx,
			sqldb.SQL(`insert into samples values (?, ?, ?, ?, ?)`, 1000, 0.5, 7, "a", []byte{1}),
			sqldb.SQL(`insert into samples values (?, ?, ?, ?, ?)`, 2000, nil, nil, nil, nil),
			sqldb.SQL(`insert into samples values (?, ?, ?, ?, ?)`, 3000, 2.5, 9, "c", nil)))
		cols := ways.Must(sqldb.Columns[Samples](ctx, db, `select ts, cpu, hits, host, raw, coalesce(cpu, 0) as either from samples order by ts`))
		if fmt.Sprint(cols.TS) != "[1000 2000 3000]" || cols.CPU[1] != nil || *cols.CPU[2] != 2.5 || cols.Hits[1] != nil || *cols.Host[0] != "a" {
			t.Fatalf("columns %+v", cols)
		}
		if string(cols.Raw[0]) != "\x01" || cols.Raw[1] != nil || fmt.Sprint(cols.Either) != "[0.5 0 2.5]" {
			t.Fatalf("columns %+v", cols)
		}
		_, err := sqldb.Columns[Samples](ctx, db, `select ts, ts from samples`)
		is(t, err, tinystore.ErrInvalid)
		big := ways.Must(sqldb.Columns[struct{ I []int64 }](ctx, db, `with recursive n(i) as (select 1 union all select i + 1 from n where i < ?) select i from n`, 150_000))
		if len(big.I) != 150_000 || big.I[149_999] != 150_000 {
			t.Fatalf("a column past one message is whole: %d", len(big.I))
		}
	})
}

// contract: SQL-16
func TestDescribeNamesTheColumnsOfAStatementThatAnswersNoRowInItsOrder(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		statement := `select title, id, length(title) as size from notes where author_id = ?`
		columns := ways.Must(sqldb.Describe(ctx, db, statement))
		want := []sqldb.Column{{Name: "title", Type: "TEXT"}, {Name: "id", Type: "TEXT"}, {Name: "size"}}
		if !slices.Equal(columns, want) {
			t.Fatalf("columns %+v, want %+v", columns, want)
		}
		if rows := ways.Must(sqldb.All[map[string]any](ctx, db, statement, 7)); len(rows) != 0 {
			t.Fatalf("rows %v", rows)
		}
		if again := ways.Must(sqldb.Describe(ctx, db, statement, 7)); !slices.Equal(again, want) {
			t.Fatalf("with its value, columns %+v", again)
		}
		_, err := sqldb.Describe(ctx, db, `delete from notes returning id`)
		is(t, err, tinystore.ErrInvalid)
		_, err = sqldb.Describe(ctx, db, `select titel from notes`)
		is(t, err, tinystore.ErrInvalid)
		if !strings.Contains(err.Error(), "no such column: titel") {
			t.Fatalf("a mistyped column: %v", err)
		}
	})
}

type Work struct {
	CourseID int64
	ID       string
	Title    string
	Grade    *float64
}

// contract: SQL-17, SQL-20
func TestAMirrorAnswersWhatWasAddedChangedAndGoneAndItsGuardWritesNothing(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		ways.Must(db.Exec(ctx, `create table works (course_id integer, id text, title text, grade real, gone_at integer, primary key (course_id, id))`))
		works := sqldb.OpenMirror[Work](db, "works", sqldb.MirrorOptions{Key: []string{"course_id", "id"}, Gone: "gone_at", ChangeOn: []string{"grade"}})
		grade := func(g float64) *float64 { return &g }
		work := func(id string, g *float64, title string) Work {
			return Work{CourseID: 1, ID: id, Title: title, Grade: g}
		}
		scope := map[string]any{"course_id": 1}
		first := ways.Must(works.Replace(ctx, scope, []Work{work("a", grade(4), "Essay"), work("b", nil, "Quiz"), work("c", grade(3), "Lab")}))
		if len(first.Added) != 3 || first.Same != 0 || first.Suspect {
			t.Fatalf("first %+v", first)
		}
		diff := ways.Must(works.Replace(ctx, scope, []Work{work("a", grade(4), "Essay (v2)"), work("b", grade(5), "Quiz"), work("d", nil, "Exam")}))
		if len(diff.Added) != 1 || diff.Added[0].ID != "d" || len(diff.Gone) != 1 || diff.Gone[0].ID != "c" || diff.Same != 1 {
			t.Fatalf("diff %+v", diff)
		}
		if len(diff.Changed) != 1 || diff.Changed[0].Before.Grade != nil || *diff.Changed[0].After.Grade != 5 {
			t.Fatalf("changed %+v", diff.Changed)
		}
		held := ways.Must(works.Replace(ctx, scope, nil, sqldb.MinKept(0.5)))
		if !held.Suspect || len(held.Gone) != 3 {
			t.Fatalf("held %+v", held)
		}
		if live := ways.Must(sqldb.Scalar[int64](ctx, db, `select count(*) from works where gone_at is null`)); live != 3 {
			t.Fatalf("a suspect answer wrote: %d live", live)
		}
		upserted := ways.Must(works.Upsert(ctx, []Work{work("e", grade(1), "New")}))
		if len(upserted.Added) != 1 || len(upserted.Gone) != 0 {
			t.Fatalf("upserted %+v", upserted)
		}
		_, err := works.Replace(ctx, map[string]any{"course_id": 2}, []Work{work("a", grade(4), "Essay")})
		is(t, err, tinystore.ErrInvalid)
		if !strings.Contains(err.Error(), `sql app: mirror works: row (course_id 1, id "a")`) {
			t.Fatalf("outside the scope: %v", err)
		}
	})
}

func TestATableInsertsUpdatesAndDeletesWhatItsConditionSays(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		notes := sqldb.OpenTable[Note](db, "notes")
		at := time.UnixMilli(1_791_547_200_123).UTC()
		note := ways.Must(notes.Insert(ctx, Note{ID: "tb1", AuthorID: 5, Title: "Buy milk", Tags: []string{"home"}, CreatedAt: at}))
		if note.ID != "tb1" || note.Done || !note.CreatedAt.Equal(at) || note.Tags[0] != "home" {
			t.Fatalf("note %+v", note)
		}
		two := ways.Must(notes.InsertMany(ctx, []Note{
			{ID: "tb2", AuthorID: 5, Title: "Call mom", Done: true, Tags: []string{}, CreatedAt: at},
			{ID: "tb3", AuthorID: 5, Title: "Read", Tags: []string{"books"}, CreatedAt: at},
		}))
		if len(two) != 2 || !two[0].Done || two[1].Done {
			t.Fatalf("two %+v", two)
		}
		if ways.Must(notes.Where(sqldb.Eq{"author_id": 5, "done": false}).Count(ctx)) != 2 {
			t.Fatal("two not done")
		}
		titles := ways.Must(sqldb.Select[Title](notes.Where(sqldb.Eq{"author_id": 5}).OrderBy("id", sqldb.Desc), "title").All(ctx))
		if len(titles) != 3 || titles[0].Title != "Read" {
			t.Fatalf("titles %+v", titles)
		}
		_, err := notes.Insert(ctx, Note{ID: "tb1", AuthorID: 5, Title: "again", CreatedAt: at})
		is(t, err, tinystore.ErrConflict)
		var seen []Note
		for row, err := range notes.Where(sqldb.Eq{"author_id": 5}).Where(sqldb.Has("tags", "home")).Each(ctx) {
			if err != nil {
				t.Fatal(err)
			}
			seen = append(seen, row)
		}
		if len(seen) != 1 || seen[0].ID != "tb1" {
			t.Fatalf("seen %+v", seen)
		}
		updated := ways.Must(notes.Where(sqldb.Eq{"id": "tb1"}).Update(ctx, sqldb.Set{"done": true, "title": sqldb.SQL("title || '!'")}))
		changed, _ := ways.Must2(notes.Where(sqldb.Eq{"id": "tb1"}).One(ctx))
		if updated.Changes != 1 || !changed.Done || changed.Title != "Buy milk!" {
			t.Fatalf("changed %+v", changed)
		}
		_, err = notes.Delete(ctx)
		is(t, err, tinystore.ErrInvalid)
		_, err = notes.Where(sqldb.And()).Update(ctx, sqldb.Set{"title": "x"})
		is(t, err, tinystore.ErrInvalid)
		if ways.Must(notes.Where(sqldb.Eq{"author_id": 5, "done": true}).Delete(ctx)).Changes != 2 {
			t.Fatal("the done notes are deleted")
		}
		made := ways.Must(notes.Upsert(ctx, Note{ID: "up1", AuthorID: 1, Title: "mine", Tags: []string{}}, sqldb.Conflict("id"), sqldb.Update("title"), sqldb.Where(sqldb.Eq{"author_id": 1})))
		stolen := ways.Must(notes.Upsert(ctx, Note{ID: "up1", AuthorID: 2, Title: "stolen", Tags: []string{}}, sqldb.Conflict("id"), sqldb.Update("title"), sqldb.Where(sqldb.Eq{"author_id": 2})))
		kept, _ := ways.Must2(notes.Where(sqldb.Eq{"id": "up1"}).One(ctx))
		if made.Changes != 1 || stolen.Changes != 0 || kept.Title != "mine" {
			t.Fatalf("made %+v stolen %+v kept %+v", made, stolen, kept)
		}
	})
}

// contract: SQL-15
func TestAnUpsertOfRowsWritesEveryRowOrNoneInOneStatement(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		notes := sqldb.OpenTable[Note](db, "notes")
		row := func(id string, author int64, title string) Note {
			return Note{ID: id, AuthorID: author, Title: title, Tags: []string{}}
		}
		mine := []sqldb.UpsertOption{sqldb.Conflict("id"), sqldb.Update("title"), sqldb.Where(sqldb.Eq{"author_id": 1})}
		ways.Must(notes.InsertMany(ctx, []Note{row("ul1", 1, "mine"), row("ul2", 2, "theirs")}))
		written := ways.Must(notes.UpsertMany(ctx, []Note{row("ul1", 1, "still mine"), row("ul2", 1, "stolen"), row("ul3", 1, "new")}, mine...))
		kept, _ := ways.Must2(notes.Where(sqldb.Eq{"id": "ul2"}).One(ctx))
		if written.Changes != 2 || kept.Title != "theirs" {
			t.Fatalf("written %+v kept %+v", written, kept)
		}
		ways.Must(db.Exec(ctx, `create trigger no_ul5 before insert on notes when new.id = 'ul5' begin select raise(abort, 'no'); end`))
		_, err := notes.UpsertMany(ctx, []Note{row("ul4", 1, "a"), row("ul5", 1, "b")}, mine...)
		is(t, err, tinystore.ErrInvalid)
		if _, found := ways.Must2(notes.Where(sqldb.Eq{"id": "ul4"}).One(ctx)); found {
			t.Fatal("one statement: none of its rows")
		}
		if none := ways.Must(notes.UpsertMany(ctx, nil, mine...)); none.Changes != 0 {
			t.Fatalf("none %+v", none)
		}
		_, err = notes.UpsertMany(ctx, []Note{row("ul6", 1, "a")}, sqldb.Conflict("id"))
		if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "the columns its update sets") {
			t.Fatalf("got %v", err)
		}
	})
}

func TestAPageReadsAfterTheLastRowOfTheOneBeforeAndOnlyItsOwnQuery(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		notes := sqldb.OpenTable[Note](db, "notes")
		for n := range 5 {
			ways.Must(notes.Insert(ctx, Note{ID: fmt.Sprintf("pg%d", n), AuthorID: 8, Title: fmt.Sprint(n), Tags: []string{}, CreatedAt: time.Unix(int64(1+n/2), 0)}))
		}
		newest := notes.Where(sqldb.Eq{"author_id": 8}).OrderBy("created_at", sqldb.Desc).OrderBy("id", sqldb.Desc)
		first := ways.Must(newest.List(ctx, sqldb.Limit(2)))
		second := ways.Must(newest.List(ctx, sqldb.Limit(2), sqldb.After(first.Next)))
		third := ways.Must(newest.List(ctx, sqldb.Limit(2), sqldb.After(second.Next)))
		var ids []string
		for _, page := range []sqldb.Page[Note]{first, second, third} {
			for _, row := range page.Rows {
				ids = append(ids, row.ID)
			}
		}
		if strings.Join(ids, ",") != "pg4,pg3,pg2,pg1,pg0" || third.Next != "" {
			t.Fatalf("ids %v next %q", ids, third.Next)
		}
		other := notes.Where(sqldb.Eq{"author_id": 9}).OrderBy("created_at", sqldb.Desc).OrderBy("id", sqldb.Desc)
		_, err := other.List(ctx, sqldb.Limit(2), sqldb.After(first.Next))
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestPagesReadOnPastANullAndAnInfinityInTheirOrder(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := ways.Must(sqldb.Open(ctx, store, "shop", sqldb.MigrationFiles(map[string]string{
			"0001_orders.sql": "create table orders (id integer primary key, shipped_at integer, total real)",
		})))
		ways.Must(db.Exec(ctx, `insert into orders values (1, null, 1.0), (2, null, 9e999), (3, null, -9e999),
			(4, 10, 2.0), (5, 20, 9e999)`))
		type Order struct {
			ID int64
		}
		orders := sqldb.OpenTable[Order](db, "orders")
		paged := func(query *sqldb.Query[Order]) string {
			var ids []string
			var after []sqldb.ListOption
			for {
				page := ways.Must(query.List(ctx, append(after, sqldb.Limit(2))...))
				for _, row := range page.Rows {
					ids = append(ids, fmt.Sprint(row.ID))
				}
				if page.Next == "" {
					return strings.Join(ids, ",")
				}
				after = []sqldb.ListOption{sqldb.After(page.Next)}
			}
		}
		for _, each := range []struct {
			query *sqldb.Query[Order]
			want  string
		}{
			{orders.OrderBy("shipped_at", sqldb.Asc).OrderBy("id", sqldb.Asc), "1,2,3,4,5"},
			{orders.OrderBy("shipped_at", sqldb.Desc).OrderBy("id", sqldb.Desc), "5,4,3,2,1"},
			{orders.OrderBy("total", sqldb.Asc).OrderBy("id", sqldb.Asc), "3,1,4,2,5"},
		} {
			if got := paged(each.query); got != each.want {
				t.Fatalf("pages read %s, want %s", got, each.want)
			}
		}
	})
}

type Post struct {
	ID    int64
	Title string
	Score *float64
	Big   *int64
	Raw   []byte
	Done  bool
}

type Author struct {
	ID    int64
	Name  string
	Posts []Post
}

func TestAJoinASubqueryAndAnIncludedQueryGiveEachRowItsRows(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := ways.Must(sqldb.Open(ctx, store, "included", sqldb.MigrationFiles(map[string]string{
			"0001_included.sql": `create table authors (id integer primary key, name text not null, active integer not null default 1) strict;
			create table posts (id integer primary key, author_id integer not null, title text not null,
				score real, big integer, raw blob, done integer not null default 0) strict;`,
		})))
		odd := "it's, (odd)\x00 \"text\""
		ways.Must(db.Exec(ctx, `insert into authors (id, name, active) values (1, 'ann', 1), (2, 'bob', 0), (3, 'eve', 1)`))
		ways.Must(db.Exec(ctx, `insert into posts (id, author_id, title, score, big, raw, done) values
			(1, 1, 'first', ?, ?, ?, 1), (2, 1, ?, 1e308, null, null, 0), (3, 1, 'third', null, -5, x'', 0),
			(4, 2, 'only', 2.5, 7, null, 1)`, 0.1+0.2, int64(1)<<60+1, []byte{0, 255, 39}, odd))
		joined := ways.Must(sqldb.OpenTable[sqldb.Row](db, "posts as p").Join("authors as a", "a.id = p.author_id").Where(sqldb.Eq{"a.active": 1}).OrderBy("p.id", sqldb.Asc).All(ctx))
		if len(joined) != 3 || joined[0]["title"] != "first" {
			t.Fatalf("joined %v", joined)
		}
		written := sqldb.Select[sqldb.Row](sqldb.OpenTable[sqldb.Row](db, "posts as p"), "1").Where(sqldb.SQL("p.author_id = a.id"))
		writers := ways.Must(sqldb.OpenTable[Author](db, "authors as a").Where(sqldb.SQL("exists ?", written)).OrderBy("name", sqldb.Asc).All(ctx))
		if len(writers) != 2 || writers[1].Name != "bob" {
			t.Fatalf("writers %+v", writers)
		}
		latest := sqldb.Select[Post](sqldb.OpenTable[Post](db, "posts as p"), "p.id, p.title, p.score, p.big, p.raw, p.done").
			Where(sqldb.SQL("p.author_id = a.id")).OrderBy("p.id", sqldb.Desc).Limit(2)
		authors := ways.Must(sqldb.OpenTable[Author](db, "authors as a").Include("posts", latest).OrderBy("a.id", sqldb.Asc).All(ctx))
		if len(authors) != 3 || len(authors[0].Posts) != 2 || len(authors[1].Posts) != 1 || len(authors[2].Posts) != 0 {
			t.Fatalf("authors %+v", authors)
		}
		third, second := authors[0].Posts[0], authors[0].Posts[1]
		if second.ID != 2 || second.Title != odd || *second.Score != 1e308 || second.Big != nil || second.Raw != nil {
			t.Fatalf("second %+v", second)
		}
		if third.Score != nil || *third.Big != -5 || third.Raw == nil || len(third.Raw) != 0 {
			t.Fatalf("third %+v", third)
		}
		ann, _ := ways.Must2(sqldb.OpenTable[Author](db, "authors as a").Include("posts", latest.Limit(3)).Where(sqldb.Eq{"a.id": 1}).One(ctx))
		first := ann.Posts[2]
		if *first.Score != 0.1+0.2 || *first.Big != int64(1)<<60+1 || string(first.Raw) != "\x00\xff'" || !first.Done {
			t.Fatalf("first %+v", first)
		}
		_, err := sqldb.OpenTable[Author](db, "authors as a").Include("posts", sqldb.OpenTable[Post](db, "posts as p").Limit(1)).All(ctx)
		is(t, err, tinystore.ErrInvalid)
	})
}

// contract: KV-14
func TestAnUpdateOfABucketTheDatabaseKeepsKeepsBothChangesAndRunsInsideItsTransaction(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		carts := ways.Must(kv.OpenBucket[[]string](db, "carts"))
		add := func(item string) func([]string, bool) ([]string, bool) {
			return func(cart []string, _ bool) ([]string, bool) { return append(cart, item), true }
		}
		var both sync.WaitGroup
		for _, item := range []string{"tea", "milk"} {
			both.Go(func() {
				if _, _, err := carts.Update(ctx, "ann", add(item)); err != nil {
					t.Error(err)
				}
			})
		}
		both.Wait()
		ways.Must(0, db.Tx(ctx, func(tx *sqldb.Tx) error {
			cart, _, err := carts.In(tx).Update(ctx, "ann", add("bread"))
			if err == nil && len(cart) != 3 {
				err = fmt.Errorf("cart %v", cart)
			}
			return err
		}))
		if cart, _ := ways.Must2(carts.Get(ctx, "ann")); len(cart) != 3 || !slices.Contains(cart, "tea") || !slices.Contains(cart, "milk") {
			t.Fatalf("cart %v", cart)
		}
	})
}

func TestABucketAndAQueueOpenedFromTheDatabaseCommitWithItsRows(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		sessions := ways.Must(kv.OpenBucket[map[string]string](db, "sessions"))
		emails := ways.Must(jobs.OpenQueue[map[string]string](db, "emails"))
		ways.Must(0, sessions.Set(ctx, "t-0", map[string]string{"user": "ann"}))
		declined := errors.New("declined")
		err := db.Tx(ctx, func(tx *sqldb.Tx) error {
			if _, err := tx.Exec(ctx, `insert into notes (id, author_id, title) values ('q1', 5, 'a')`); err != nil {
				return err
			}
			if err := sessions.In(tx).Set(ctx, "t-1", map[string]string{"user": "bob"}); err != nil {
				return err
			}
			if added, err := emails.In(tx).Add(ctx, map[string]string{"note": "q1"}, jobs.ID("q1")); err != nil || !added {
				return fmt.Errorf("add: %v %v", added, err)
			}
			if got, _, err := sessions.In(tx).Get(ctx, "t-1"); err != nil || got["user"] != "bob" {
				return fmt.Errorf("a read in the transaction sees its write: %v %v", got, err)
			}
			if job, found, err := emails.In(tx).Get(ctx, "q1"); err != nil || !found || job.State != jobs.StateWaiting {
				return fmt.Errorf("job %+v %v %v", job, found, err)
			}
			return declined
		})
		if !errors.Is(err, declined) {
			t.Fatalf("got %v", err)
		}
		if ways.Must(sessions.Has(ctx, "t-1")) {
			t.Fatal("a rolled back write is gone")
		}
		if _, found := ways.Must2(emails.Get(ctx, "q1")); found {
			t.Fatal("a rolled back job is gone")
		}
		ways.Must(0, db.Tx(ctx, func(tx *sqldb.Tx) error {
			if err := sessions.In(tx).Set(ctx, "t-1", map[string]string{"user": "bob"}); err != nil {
				return err
			}
			_, err := emails.In(tx).Add(ctx, map[string]string{"note": "q1"}, jobs.ID("q1"))
			return err
		}))
		if got, _ := ways.Must2(sessions.Get(ctx, "t-1")); got["user"] != "bob" {
			t.Fatal("a committed write stays")
		}
		elsewhere := ways.Must(kv.OpenBucket[int64](store, "sessions"))
		err = db.Tx(ctx, func(tx *sqldb.Tx) error { return elsewhere.In(tx).Set(ctx, "k", 1) })
		is(t, err, tinystore.ErrInvalid)
		err = store.Tx(ctx, func(tx *tinystore.Tx) error { return sessions.In(tx).Set(ctx, "k", map[string]string{}) })
		is(t, err, tinystore.ErrInvalid)
		err = db.Tx(ctx, func(tx *sqldb.Tx) error {
			late, err := kv.OpenBucket[int64](db, "late")
			if err != nil {
				return err
			}
			return late.In(tx).Set(ctx, "k", 1)
		})
		if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "open it before db.Tx") {
			t.Fatalf("a bucket made after the transaction began is refused: %v", err)
		}
	})
}

// contract: JOB-02
func TestABatchWritesItsStatementsAndAQueuesAddsTogetherOrNoneOfThem(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		indexing := ways.Must(jobs.OpenQueue[map[string]string](db, "indexing"))
		insert := func(id string) sqldb.Statement {
			return sqldb.SQL(`insert into notes (id, author_id, title) values (?, 13, 'a')`, id)
		}
		ways.Must(0, db.Batch(ctx, insert("j1"), indexing.Adding(map[string]string{"note": "j1"}, jobs.ID("j1"))))
		if job, found := ways.Must2(indexing.Get(ctx, "j1")); !found || job.Value["note"] != "j1" {
			t.Fatalf("the job commits with the row: %+v %v", job, found)
		}

		err := db.Batch(ctx, indexing.Adding(map[string]string{"note": "j2"}, jobs.ID("j2")), insert("j1"))
		is(t, err, tinystore.ErrConflict)
		if _, found := ways.Must2(indexing.Get(ctx, "j2")); found {
			t.Fatal("the job rolled back with the batch")
		}

		inJobs := ways.Must(jobs.OpenQueue[map[string]string](store, "indexing"))
		err = db.Batch(ctx, insert("j3"), inJobs.Adding(map[string]string{"note": "j3"}))
		if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "kept in jobs.db, outside this batch of sql app") {
			t.Fatalf("a queue of jobs.db is refused: %v", err)
		}
		_, err = sqldb.All[sqldb.Row](ctx, db, `select * from notes where ?`, indexing.Adding(map[string]string{}))
		is(t, err, tinystore.ErrInvalid)
		if ways.Must(sqldb.Scalar[int64](ctx, db, `select count(*) from notes where author_id = 13`)) != 1 {
			t.Fatal("a refused batch writes nothing")
		}
	})
}

// contract: REC-13
func TestAStreamOpenedThroughTheDatabaseAppendsWithItsTransactionAndItsBatchOrNever(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := app(t, store)
		audit := ways.Must(records.OpenEvents(db, "notes-audit"))
		inRecords := ways.Must(records.OpenEvents(store, "notes-audit"))
		streamed := func() []string {
			page := ways.Must(inRecords.List(ctx, records.Query{}))
			ids := make([]string, len(page.Events))
			for i, event := range page.Events {
				ids[i] = event.ID
			}
			return ids
		}
		insert := func(tx *sqldb.Tx, id string) error {
			_, err := tx.Exec(ctx, `insert into notes (id, author_id, title) values (?, 21, 'a')`, id)
			return err
		}
		err := db.Tx(ctx, func(tx *sqldb.Tx) error {
			if err := insert(tx, "e1"); err != nil {
				return err
			}
			if kept, err := audit.In(tx).Append(ctx, "note.added", map[string]string{"note": "e1"}, records.ID("e0")); !kept || err != nil {
				t.Fatalf("kept for the commit: %v %v", kept, err)
			}
			return insert(tx, "e1")
		})
		is(t, err, tinystore.ErrConflict)
		if ids := streamed(); len(ids) != 0 {
			t.Fatalf("a rolled back event reached the stream: %v", ids)
		}

		ways.Must(0, db.Tx(ctx, func(tx *sqldb.Tx) error {
			if err := insert(tx, "e1"); err != nil {
				return err
			}
			if kept, err := audit.In(tx).Append(ctx, "note.added", map[string]string{"note": "e1"}, records.ID("e1")); !kept || err != nil {
				t.Fatalf("kept for the commit: %v %v", kept, err)
			}
			if again, err := audit.In(tx).Append(ctx, "note.added", nil, records.ID("e1")); again || err != nil {
				t.Fatalf("an id the transaction holds: %v %v", again, err)
			}
			return nil
		}))
		if ids := streamed(); !slices.Equal(ids, []string{"e1"}) {
			t.Fatalf("in the stream when db.Tx returned: %v", ids)
		}

		ways.Must(0, db.Batch(ctx, sqldb.SQL(`insert into notes (id, author_id, title) values ('e2', 21, 'b')`),
			audit.Appending("note.added", map[string]string{"note": "e2"}, records.ID("e2"))))
		if ids := streamed(); !slices.Equal(ids, []string{"e1", "e2"}) {
			t.Fatalf("in the stream when db.Batch returned: %v", ids)
		}
		err = db.Batch(ctx, inRecords.Appending("never", nil))
		if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "kept in records.db, outside this batch of sql app") {
			t.Fatalf("a stream opened on the store is refused: %v", err)
		}
		if count := ways.Must(audit.Count(ctx, records.Query{})); count != 2 {
			t.Fatalf("%d events", count)
		}
	})
}

// contract: JOB-02
func TestAWorkerRunsAJobItsBatchAddedOnceTheBatchCommits(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx, cancel := context.WithCancel(context.Background())
		db := app(t, store)
		sent := ways.Must(jobs.OpenQueue[map[string]string](db, "batched"))
		seen := make(chan string, 1)
		done := make(chan error, 1)
		go func() {
			done <- sent.Work(ctx, func(_ context.Context, email map[string]string, _ *jobs.Run) error {
				seen <- email["note"]
				return nil
			})
		}()
		ways.Must(0, db.Batch(ctx, sqldb.SQL(`insert into notes (id, author_id, title) values ('w2', 14, 'a')`),
			sent.Adding(map[string]string{"note": "w2"})))
		select {
		case note := <-seen:
			if note != "w2" {
				t.Fatalf("ran %q", note)
			}
		case <-time.After(10 * time.Second):
			t.Fatal("the worker slept past the commit")
		}
		cancel()
		if err := <-done; err != nil {
			t.Fatalf("work: %v", err)
		}
	})
}

func TestADatabaseOpensWithoutMigrationsAndRefusesOneChangedAfterItWasApplied(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		app(t, store)
		scratch := ways.Must(sqldb.Open(ctx, store, "scratch"))
		if ways.Must(sqldb.Scalar[int64](ctx, scratch, `select 1`)) != 1 {
			t.Fatal("a database without migrations opens as it is")
		}
		_, err := sqldb.Open(ctx, store, "app", sqldb.MigrationFiles(map[string]string{"0001_notes.sql": notesTable + ";"}))
		if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "0001_notes.sql changed after it was applied") {
			t.Fatalf("got %v", err)
		}
	})
}

// contract: WIRE-10
func TestQueriesWhoseContextEndsStopOnTheServer(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		db := app(t, store)
		// more queries of minutes than the server has threads and the file readers
		const slow = `with recursive c(x) as (select 1 union all select x + 1 from c where x < 1000000000) select count(*) from c`
		visit, leave := context.WithCancel(context.Background())
		panels := make(chan error, 20)
		for range 20 {
			go func() {
				_, err := sqldb.Scalar[int64](visit, db, slow)
				panels <- err
			}()
		}
		time.Sleep(100 * time.Millisecond)
		leave()
		for range 20 {
			if err := <-panels; !errors.Is(err, context.Canceled) {
				t.Fatalf("a query its context cancelled: %v", err)
			}
		}
		timed, done := context.WithTimeout(context.Background(), 100*time.Millisecond)
		defer done()
		if _, err := sqldb.Scalar[int64](timed, db, slow); !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("a query past its context's deadline: %v", err)
		}
		started := time.Now()
		if ways.Must(sqldb.Scalar[int64](context.Background(), db, `select 1`)) != 1 {
			t.Fatal("select 1")
		}
		if waited := time.Since(started); waited > 2*time.Second {
			t.Fatalf("the next query waited %v", waited)
		}
	})
}

const tasksTable = `create table tasks (
    id         integer primary key,
    title      text not null,
    deleted_at integer,
    version    integer not null default 0
) strict`

type Task struct {
	ID        int64
	Title     string
	DeletedAt *int64
	Version   int64
}

func sp(t *testing.T, store *tinystore.Store) *sqldb.DB {
	t.Helper()
	migrations := fstest.MapFS{"0001_tasks.sql": {Data: []byte(tasksTable)}}
	return ways.Must(sqldb.Open(context.Background(), store, "sp", sqldb.Migrations(migrations)))
}

func titles(tasks []Task) []string {
	out := make([]string, len(tasks))
	for i, task := range tasks {
		out[i] = task.Title
	}
	return out
}

// contract: SQL-21, SQL-23, SQL-24
func TestASecondWriterFromTheOldVersionIsRefusedAndADeletedRowIsGoneFromTheHandle(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := sp(t, store)
		tasks := sqldb.OpenTable[Task](db, "tasks", sqldb.Deleted("deleted_at"), sqldb.Version("version"))
		ways.Must(tasks.Insert(ctx, Task{ID: 1, Title: "first"}))
		one := tasks.Where(sqldb.Eq{"id": 1})
		seen, _ := ways.Must2(one.One(ctx))
		if done := ways.Must(one.Update(ctx, sqldb.Set{"title": "from a"}, sqldb.IfVersion(seen.Version))); done.Changes != 1 {
			t.Fatalf("a %+v", done)
		}
		_, err := one.Update(ctx, sqldb.Set{"title": "from b"}, sqldb.IfVersion(seen.Version))
		is(t, err, tinystore.ErrConflict)
		if err.Error() != "sql sp: table tasks: the row is at version 1, not 0" {
			t.Fatalf("b %v", err)
		}
		if got, _ := ways.Must2(one.One(ctx)); got.Title != "from a" {
			t.Fatalf("b wrote %+v", got)
		}

		if done := ways.Must(one.Delete(ctx)); done.Changes != 1 {
			t.Fatalf("delete %+v", done)
		}
		if n := ways.Must(tasks.Count(ctx)); n != 0 {
			t.Fatalf("count %d", n)
		}
		if _, found := ways.Must2(one.One(ctx)); found {
			t.Fatal("a deleted row is gone from the handle")
		}
		marked, _ := ways.Must2(tasks.WithDeleted().Where(sqldb.Eq{"id": 1}).One(ctx))
		if marked.Version != 2 || marked.DeletedAt == nil {
			t.Fatalf("marked %+v", marked)
		}
		if done := ways.Must(one.Restore(ctx)); done.Changes != 1 {
			t.Fatalf("restore %+v", done)
		}
		if got, _ := ways.Must2(one.One(ctx)); got.Version != 3 {
			t.Fatalf("restored %+v", got)
		}
		if n := ways.Must(sqldb.Scalar[int64](ctx, db, `select count(*) from tasks`)); n != 1 {
			t.Fatalf("by hand %d", n)
		}
	})
}

// contract: SQL-21, SQL-22
func TestEveryCallOfAHandleWithADeletedColumnPassesTheMarkedRowsByUnlessAsked(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := sp(t, store)
		tasks := sqldb.OpenTable[Task](db, "tasks", sqldb.Deleted("deleted_at"))
		ways.Must(tasks.InsertMany(ctx, []Task{{ID: 1, Title: "a"}, {ID: 2, Title: "b"}, {ID: 3, Title: "c"}}))
		two := tasks.Where(sqldb.Eq{"id": 2})
		if first, again := ways.Must(two.Delete(ctx)), ways.Must(two.Delete(ctx)); first.Changes != 1 || again.Changes != 0 {
			t.Fatalf("deletes %+v %+v", first, again)
		}
		byID := tasks.OrderBy("id", sqldb.Asc)
		if got := titles(ways.Must(byID.All(ctx))); !slices.Equal(got, []string{"a", "c"}) {
			t.Fatalf("all %v", got)
		}
		if page := ways.Must(byID.List(ctx, sqldb.Limit(10))); !slices.Equal(titles(page.Rows), []string{"a", "c"}) {
			t.Fatalf("list %+v", page)
		}
		var walked []Task
		for row, err := range byID.Each(ctx) {
			walked = append(walked, ways.Must(row, err))
		}
		if !slices.Equal(titles(walked), []string{"a", "c"}) {
			t.Fatalf("each %v", walked)
		}
		positive := sqldb.SQL("id > ?", 0)
		if done := ways.Must(tasks.Where(positive).Update(ctx, sqldb.Set{"title": "x"})); done.Changes != 2 {
			t.Fatalf("update %+v", done)
		}
		if done := ways.Must(two.Update(ctx, sqldb.Set{"title": "x"})); done.Changes != 0 {
			t.Fatalf("update of a marked row %+v", done)
		}
		if got := titles(ways.Must(tasks.DeletedOnly().All(ctx))); !slices.Equal(got, []string{"b"}) {
			t.Fatalf("deleted only %v", got)
		}
		if n := ways.Must(tasks.WithDeleted().Count(ctx)); n != 3 {
			t.Fatalf("with deleted %d", n)
		}

		_, err := tasks.Where(positive).Purge(ctx)
		is(t, err, tinystore.ErrInvalid)
		if !strings.Contains(err.Error(), "sql sp: table tasks: a purge deletes for good") {
			t.Fatalf("live %v", err)
		}
		if done := ways.Must(tasks.DeletedOnly().Where(positive).Purge(ctx)); done.Changes != 1 {
			t.Fatalf("purge %+v", done)
		}
		ways.Must(tasks.Where(sqldb.Eq{"id": 3}).Delete(ctx))
		if done := ways.Must(tasks.WithDeleted().Where(positive).Purge(ctx)); done.Changes != 2 {
			t.Fatalf("purge of both %+v", done)
		}
		_, err = sqldb.OpenTable[Task](db, "tasks").WithDeleted().All(ctx)
		is(t, err, tinystore.ErrInvalid)
	})
}

// contract: SQL-21, SQL-24
func TestAnInsertStartsARowAtVersion0AndAMarkedRowKeepsItsKeyFromAnInsert(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := sp(t, store)
		tasks := sqldb.OpenTable[Task](db, "tasks", sqldb.Deleted("deleted_at"), sqldb.Version("version"))
		if got := ways.Must(tasks.Insert(ctx, Task{ID: 1, Title: "a", Version: 7})); got.Version != 0 {
			t.Fatalf("insert %+v", got)
		}
		ways.Must(tasks.Upsert(ctx, Task{ID: 2, Title: "b", Version: 5}, sqldb.Conflict("id"), sqldb.Update("title")))
		if got, _ := ways.Must2(tasks.Where(sqldb.Eq{"id": 2}).One(ctx)); got.Version != 0 {
			t.Fatalf("an upsert's insert %+v", got)
		}
		ways.Must(tasks.Where(sqldb.Eq{"id": 1}).Delete(ctx))
		_, err := tasks.Insert(ctx, Task{ID: 1, Title: "again"})
		is(t, err, tinystore.ErrConflict)
		ways.Must(tasks.Upsert(ctx, Task{ID: 1, Title: "again"}, sqldb.Conflict("id"), sqldb.Update("title")))
		if back, _ := ways.Must2(tasks.Where(sqldb.Eq{"id": 1}).One(ctx)); back.Title != "again" || back.DeletedAt != nil {
			t.Fatalf("back %+v", back)
		}
	})
}

// contract: SQL-23, SQL-24
func TestAnUpsertOntoAMarkedRowBringsItBackAndAnIfVersionWritesEveryRowOrNone(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := sp(t, store)
		tasks := sqldb.OpenTable[Task](db, "tasks", sqldb.Deleted("deleted_at"), sqldb.Version("version"))
		ways.Must(tasks.InsertMany(ctx, []Task{{ID: 1, Title: "a"}, {ID: 2, Title: "b"}}))
		ways.Must(tasks.Where(sqldb.Eq{"id": 1}).Delete(ctx))
		ways.Must(tasks.Upsert(ctx, Task{ID: 1, Title: "back"}, sqldb.Conflict("id"), sqldb.Update("title")))
		if back, _ := ways.Must2(tasks.Where(sqldb.Eq{"id": 1}).One(ctx)); back.Title != "back" || back.DeletedAt != nil || back.Version != 2 {
			t.Fatalf("back %+v", back)
		}
		_, err := tasks.Upsert(ctx, Task{ID: 1, Title: "v"}, sqldb.Conflict("id"), sqldb.Update("version"))
		is(t, err, tinystore.ErrInvalid)
		_, err = tasks.Where(sqldb.Eq{"id": 1}).Update(ctx, sqldb.Set{"version": 9})
		is(t, err, tinystore.ErrInvalid)

		both := tasks.Where(sqldb.Eq{"id": []int64{1, 2}})
		_, err = both.Update(ctx, sqldb.Set{"title": "z"}, sqldb.IfVersion(0))
		is(t, err, tinystore.ErrConflict)
		if !strings.Contains(err.Error(), "the row is at version 2, not 0") {
			t.Fatalf("refused %v", err)
		}
		if b, _ := ways.Must2(tasks.Where(sqldb.Eq{"id": 2}).One(ctx)); b.Title != "b" {
			t.Fatalf("none %+v", b)
		}
		ways.Must(tasks.Where(sqldb.Eq{"id": 2}).Update(ctx, sqldb.Set{"title": "b2"}))
		ways.Must(tasks.Where(sqldb.Eq{"id": 2}).Update(ctx, sqldb.Set{"title": "b3"}))
		if done := ways.Must(both.Update(ctx, sqldb.Set{"title": "z"}, sqldb.IfVersion(2))); done.Changes != 2 {
			t.Fatalf("all %+v", done)
		}
		if done := ways.Must(tasks.Where(sqldb.Eq{"id": 9}).Update(ctx, sqldb.Set{"title": "z"}, sqldb.IfVersion(0))); done.Changes != 0 {
			t.Fatalf("no row %+v", done)
		}
		_, err = sqldb.OpenTable[Task](db, "tasks").Where(sqldb.Eq{"id": 1}).Update(ctx, sqldb.Set{"title": "z"}, sqldb.IfVersion(3))
		is(t, err, tinystore.ErrInvalid)

		err = db.Tx(ctx, func(tx *sqldb.Tx) error {
			one := sqldb.OpenTable[Task](tx, "tasks", sqldb.Deleted("deleted_at"), sqldb.Version("version")).Where(sqldb.Eq{"id": 1})
			if done, err := one.Update(ctx, sqldb.Set{"title": "in tx"}, sqldb.IfVersion(3)); err != nil || done.Changes != 1 {
				return fmt.Errorf("update %+v: %w", done, err)
			}
			if _, err := one.Delete(ctx, sqldb.IfVersion(3)); !errors.Is(err, tinystore.ErrConflict) {
				return fmt.Errorf("delete from 3: %w", err)
			}
			_, err := one.Delete(ctx, sqldb.IfVersion(4))
			return err
		})
		if err != nil {
			t.Fatal(err)
		}
		if got, _ := ways.Must2(tasks.DeletedOnly().Where(sqldb.Eq{"id": 1}).One(ctx)); got.Title != "in tx" {
			t.Fatalf("in tx %+v", got)
		}
	})
}
