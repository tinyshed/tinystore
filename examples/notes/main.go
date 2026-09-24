// Command notes is what a program using TinyStore looks like: one directory,
// its own SQL, its own metrics and logs, a backup, and one Close.
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
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/backup"
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
	store    *tinystore.Store
	db       *sqldb.DB
	stats    *metrics.Store
	logs     *records.Store
	logger   *slog.Logger
	created  metrics.CounterInstrument
	requests metrics.CounterInstrument
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

	a.logger = slog.New(slog.NewMultiHandler(console, a.logs.Handler()))
	a.created = a.stats.Counter("notes_created_total")
	a.requests = a.stats.Counter("requests_total")
	a.stats.GaugeFunc("notes", func(ctx context.Context) (float64, error) {
		count, err := sqldb.Scalar[int](ctx, a.db, `select count(*) from notes`)
		return float64(count), err
	})
	return nil
}

func (a *app) useNotes(ctx context.Context, out io.Writer) error {
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
	}

	err := a.db.Tx(ctx, func(tx *sqldb.Tx) error {
		if _, err := tx.Exec(ctx, `update notes set body = 'milk, bread' where title = 'groceries'`); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `delete from notes where title = 'ideas'`)
		return err
	})
	if err != nil {
		return err
	}

	notes, err := sqldb.All[Note](ctx, a.db, `select id, title, body, created_at from notes order by id`)
	for _, note := range notes {
		fmt.Fprintf(out, "note %d: %s — %s\n", note.ID, note.Title, note.Body)
	}
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

	lines, err := a.logs.Read(ctx, records.Query{From: now.Add(-time.Minute), To: now.Add(time.Minute)})
	fmt.Fprintf(out, "log lines: %d\n", len(lines))
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
