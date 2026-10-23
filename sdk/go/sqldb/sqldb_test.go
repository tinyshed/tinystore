package sqldb_test

import (
	"context"
	"errors"
	"fmt"
	"math"
	"strings"
	"testing"
	"testing/fstest"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/jobs"
	"tinyshed.org/tinystore/kv"
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
