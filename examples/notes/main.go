// Command notes is what a program using TinyStore looks like: one directory,
// its own SQL, its own metrics and logs, its drafts, its jobs, its notes'
// attachments, a backup, and one Close.
//
//	go run ./examples/notes -dir ./data
package main

import (
	"context"
	"embed"
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"log"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/backup"
	"github.com/tinyshed/tinystore/blobs"
	"github.com/tinyshed/tinystore/jobs"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/metrics"
	"github.com/tinyshed/tinystore/records"
	"github.com/tinyshed/tinystore/sqldb"
)

//go:embed migrations/*.sql
var files embed.FS

type Note struct {
	ID        int64
	Title     string
	Body      string
	CreatedAt int64 // created_at, by the field's name
}

func main() {
	dir := flag.String("dir", "./data", "the store's directory")
	flag.Parse()
	if err := run(context.Background(), *dir, os.Stdout); err != nil {
		log.Fatal(err)
	}
}

// app is the handles a program keeps: the store has no accessors
type app struct {
	store     *tinystore.Store
	db        *sqldb.DB
	stats     *metrics.Store
	logs      *records.Store
	state     *kv.Store
	drafts    *kv.Bucket[string]
	queues    *jobs.Store
	indexing  *jobs.Queue[int64] // a note to index for search, now
	reminders *jobs.Queue[int64] // a note to look at again tomorrow, keyed by the note
	objects   *blobs.Store
	files     *blobs.Bucket // a note's attachments, under notes/<id>/
	logger    *slog.Logger
	created   metrics.CounterInstrument
	requests  metrics.CounterInstrument
}

func run(ctx context.Context, dir string, out io.Writer) (err error) {
	a, err := open(ctx, dir, out)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, a.store.Close(context.WithoutCancel(ctx))) }()

	if err = a.useNotes(ctx, out); err != nil {
		return err
	}
	if err = a.useDrafts(ctx, out); err != nil {
		return err
	}
	if err = a.useAttachments(ctx, out); err != nil {
		return err
	}
	if err = a.useJobs(ctx, out); err != nil {
		return err
	}
	if err = a.readBack(ctx, out); err != nil {
		return err
	}
	return a.backUp(ctx, filepath.Join(dir, "..", "notes-backup.zip"), out)
}

// open opens the store, then each engine against it
func open(ctx context.Context, dir string, out io.Writer) (*app, error) {
	console := slog.NewTextHandler(out, &slog.HandlerOptions{Level: slog.LevelWarn})
	store, err := tinystore.Open(ctx, dir, tinystore.Options{Logger: slog.New(console), Memory: 256 << 20})
	if err != nil {
		return nil, err
	}
	a := &app{store: store}
	if err = a.openEngines(ctx, console); err != nil {
		return nil, errors.Join(err, store.Close(ctx))
	}
	return a, nil
}

func (a *app) openEngines(ctx context.Context, console slog.Handler) error {
	migrations, err := fs.Sub(files, "migrations")
	if err != nil {
		return err
	}
	if a.db, err = sqldb.Open(ctx, a.store, "app", migrations); err != nil {
		return err
	}
	if a.stats, err = metrics.Open(ctx, a.store, metrics.Options{Retention: 90 * 24 * time.Hour}); err != nil {
		return err
	}
	if a.logs, err = records.Open(ctx, a.store, records.Options{}); err != nil {
		return err
	}
	if a.state, err = kv.Open(ctx, a.store, kv.Options{}); err != nil {
		return err
	}
	if a.drafts, err = kv.OpenBucket[string](ctx, a.state, "drafts", kv.DefaultTTL(7*24*time.Hour)); err != nil {
		return err
	}
	if err = a.openQueues(ctx); err != nil {
		return err
	}
	if a.objects, err = blobs.Open(ctx, a.store, blobs.Options{}); err != nil {
		return err
	}
	if a.files, err = blobs.OpenBucket(ctx, a.objects, "attachments"); err != nil {
		return err
	}

	a.logger = slog.New(slog.NewMultiHandler(console, a.logs.Handler("notes")))
	a.created = a.stats.Counter("notes_created_total")
	a.requests = a.stats.Counter("requests_total")
	a.stats.GaugeFunc("notes", func(ctx context.Context) (float64, error) {
		count, err := sqldb.Scalar[int](ctx, a.db, `select count(*) from notes`)
		return float64(count), err
	})
	return nil
}

func (a *app) openQueues(ctx context.Context) (err error) {
	if a.queues, err = jobs.Open(ctx, a.store, jobs.Options{}); err != nil {
		return err
	}
	if a.indexing, err = jobs.OpenQueue[int64](ctx, a.queues, "indexing"); err != nil {
		return err
	}
	a.reminders, err = jobs.OpenQueue[int64](ctx, a.queues, "reminders")
	return err
}

func (a *app) useNotes(ctx context.Context, out io.Writer) error {
	var ideas int64
	for _, title := range []string{"groceries", "ideas"} {
		note, err := sqldb.ExecOne[Note](ctx, a.db,
			`insert into notes (title, created_at) values (?, ?) returning id, title, body, created_at`,
			title, time.Now().UnixMilli())
		if err != nil {
			return err
		}
		a.created.Inc()
		a.requests.With("route", "/notes", "method", "POST").Inc()
		a.logger.Info("note created", "id", note.ID, "title", note.Title)
		if err = a.scheduleFor(ctx, note.ID); err != nil {
			return err
		}
		ideas = note.ID
	}

	err := a.db.Tx(ctx, func(tx *sqldb.Tx) error {
		if _, err := tx.Exec(ctx, `update notes set body = 'milk, bread' where title = 'groceries'`); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `delete from notes where title = 'ideas'`)
		return err
	})
	if err == nil {
		_, err = a.reminders.Cancel(ctx, fmt.Sprint("note:", ideas))
	}
	// the row first, its files after: a crash between leaves files no row names
	if err == nil {
		err = a.files.Of("notes", ideas).Clear(ctx)
	}
	if err != nil {
		return err
	}

	notes, err := sqldb.All[Note](ctx, a.db, `select id, title, body, created_at from notes order by id`)
	for _, note := range notes {
		fmt.Fprintf(out, "note %d: %s — %s\n", note.ID, note.Title, note.Body)
	}
	return err
}

// scheduleFor asks for a new note to be indexed now and looked at again
// tomorrow; the reminder is keyed by the note, so that deleting it cancels it
func (a *app) scheduleFor(ctx context.Context, note int64) error {
	if err := a.indexing.Enqueue(ctx, note); err != nil {
		return err
	}
	tomorrow := time.Now().Add(24 * time.Hour)
	return a.reminders.Enqueue(ctx, note, jobs.At(tomorrow), jobs.Key(fmt.Sprint("note:", note)))
}

// useJobs indexes the notes waiting for it, as a worker would all day, and
// counts the reminders still ahead
func (a *app) useJobs(ctx context.Context, out io.Writer) error {
	var indexed atomic.Int64
	err := a.indexing.Work(ctx, func(context.Context, jobs.Job[int64]) error {
		indexed.Add(1) // a search index would read the note, and skip one deleted since
		return nil
	}, jobs.Workers(2), jobs.UntilIdle())
	if err != nil {
		return err
	}
	page, err := a.reminders.Scan(ctx, jobs.Query{Prefix: "note:"})
	if err != nil {
		return err
	}
	_, err = fmt.Fprintf(out, "notes indexed: %d, reminders ahead: %d\n", indexed.Load(), len(page.Entries))
	return err
}

// useDrafts keeps a note's unsaved draft for a week: a tab's save goes through
// only at the version it read, so a stale tab cannot save over a newer draft
func (a *app) useDrafts(ctx context.Context, out io.Writer) error {
	mine := a.drafts.Of("user-1")
	read, err := mine.SetEntry(ctx, 1, "milk, bread, eggs")
	if err != nil {
		return err
	}
	if _, err = mine.SetEntry(ctx, 1, "milk, bread, eggs, tea", kv.IfVersion(read.Version)); err != nil {
		return err
	}
	if err = mine.Set(ctx, 1, "milk", kv.IfVersion(read.Version)); !errors.Is(err, tinystore.ErrConflict) {
		return errors.Join(err, errors.New("a stale tab saved over the draft"))
	}
	draft, _, err := mine.Get(ctx, 1)
	fmt.Fprintf(out, "draft of note 1: %s\n", draft)
	return err
}

// useAttachments keeps a note's file beside its row: the row names the note
// and the bucket holds the bytes; an edit made from a stale read is refused,
// as a stale draft's save is
func (a *app) useAttachments(ctx context.Context, out io.Writer) error {
	files := a.files.Of("notes", 1)
	list, err := files.Put(ctx, "list.txt", strings.NewReader("milk, bread, eggs, tea"),
		blobs.ContentType("text/plain; charset=utf-8"))
	if err != nil {
		return err
	}
	_, err = files.Put(ctx, "list.txt", strings.NewReader("milk"), blobs.IfMatch(`"read-long-ago"`))
	if !errors.Is(err, tinystore.ErrConflict) {
		return errors.Join(err, errors.New("a stale edit replaced the list"))
	}

	file, found, err := files.Open(ctx, "list.txt")
	if err != nil || !found {
		return errors.Join(err, errors.New("the list is missing"))
	}
	defer file.Close()
	head := make([]byte, 4)
	if _, err = file.ReadAt(head, 0); err != nil {
		return err
	}
	usage, err := files.Usage(ctx)
	fmt.Fprintf(out, "attachment %s: %d bytes, starts %q; note 1 holds %d\n", list.Key, list.Size, head, usage.Bytes)
	return err
}

// readBack flushes what background work would write every few seconds, then
// reads the numbers and the log lines back
func (a *app) readBack(ctx context.Context, out io.Writer) error {
	if err := errors.Join(a.stats.Flush(ctx), a.logs.Flush(ctx)); err != nil {
		return err
	}

	now := time.Now()
	results, err := a.stats.Read(ctx, metrics.Range{
		Matchers: []metrics.Label{{Name: "__name__", Value: "notes_created_total"}},
		From:     now.Add(-time.Minute).UnixMilli(), To: now.Add(time.Minute).UnixMilli(),
	})
	if err != nil || len(results) != 1 {
		return errors.Join(err, errors.New("notes_created_total was not flushed"))
	}
	fmt.Fprintf(out, "notes created: %v\n", results[0].Samples[len(results[0].Samples)-1].Value)

	page, err := a.logs.Read(ctx, records.Query{From: now.Add(-time.Minute), To: now.Add(time.Minute)})
	fmt.Fprintf(out, "log lines: %d\n", len(page.Records))
	return err
}

func (a *app) backUp(ctx context.Context, path string, out io.Writer) (err error) {
	file, err := os.Create(path) //nolint:gosec // a path the program chose
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, file.Close()) }()
	if err = backup.Write(ctx, a.store, file); err != nil {
		return err
	}
	fmt.Fprintln(out, "backup written")
	return nil
}
