package main

import (
	"context"
	"database/sql"
	"embed"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"math/rand/v2"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/sqldb"
)

// the implementations a case runs on
const (
	implTable = "table"
	implKV    = "kv"
)

//go:embed migrations/*.sql
var migrationFiles embed.FS

// backend is where one implementation keeps a case's state: the application's
// own tables in sql/app.db through sqldb, or kv.db
type backend struct {
	impl  string
	dir   string
	store *tinystore.Store
	app   *sqldb.DB
	state *kv.Store
}

func openBackend(ctx context.Context, dir, impl string, options tinystore.Options) (*backend, error) {
	store, err := tinystore.Open(ctx, dir, options)
	if err != nil {
		return nil, err
	}
	b := &backend{impl: impl, dir: dir, store: store}
	if err = b.openEngine(ctx); err != nil {
		return nil, errors.Join(err, store.Close(ctx))
	}
	return b, nil
}

func (b *backend) openEngine(ctx context.Context) error {
	var err error
	if b.impl == implKV {
		b.state, err = kv.Open(ctx, b.store, kv.Options{})
		return err
	}
	migrations, err := fs.Sub(migrationFiles, "migrations")
	if err == nil {
		b.app, err = sqldb.Open(ctx, b.store, "app", migrations)
	}
	return err
}

// fileName is the database the implementation writes, inside the store
func (b *backend) fileName() string {
	if b.impl == implKV {
		return "kv.db"
	}
	return "sql/app.db"
}

func (b *backend) file() string {
	return filepath.Join(b.dir, filepath.FromSlash(b.fileName()))
}

func (b *backend) now() time.Time {
	return b.store.Now()
}

func (b *backend) close(ctx context.Context) error {
	return b.store.Close(ctx)
}

// maintain runs kv's maintenance, which in a Manual store is what writes the
// renewals and counters kv keeps in memory; the tables keep nothing waiting
func (b *backend) maintain(ctx context.Context) error {
	if b.impl != implKV {
		return nil
	}
	_, err := b.state.Maintain(ctx)
	return err
}

// sweep deletes a table's expired rows every minute, as kv's maintenance
// deletes its expired keys; a Manual store runs neither
func (b *backend) sweep(table, statement string) {
	b.store.Every("sweep "+table, time.Minute, func(ctx context.Context) error {
		_, err := b.app.Exec(ctx, statement, b.now().UnixMilli())
		return err
	})
}

// warnings is the stores' logger: what their background work reports goes to stderr
func warnings() *slog.Logger {
	return slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))
}

// errUnavailable is an implementation waiting for an API kv does not have yet
var errUnavailable = errors.New("not available")

// errMoved is a conditional write of the tables that found its row at another
// version, or none: kv's tinystore.ErrConflict, so that both answer alike
var errMoved = fmt.Errorf("%w: the row is not at the version given", tinystore.ErrConflict)

// conflictUnless is a conditional write's error: its own, or errMoved when it
// changed no row
func conflictUnless(result sql.Result, err error) error {
	if err != nil {
		return err
	}
	changed, err := result.RowsAffected()
	if err == nil && changed == 0 {
		return errMoved
	}
	return err
}

// newEtag is a version of the tables: random, so that a row deleted and written
// again does not take the version an old reader holds
func newEtag() int64 {
	return rand.Int64() //nolint:gosec // an etag must differ from the last, not be secret
}

func etagText(etag int64) string {
	return strconv.FormatInt(etag, 10)
}

func parseEtag(text string) (int64, error) {
	etag, err := strconv.ParseInt(text, 10, 64)
	if err != nil {
		return 0, fmt.Errorf("%w: a version reads %q", tinystore.ErrInvalid, text)
	}
	return etag, nil
}

// parseVersion reads back a kv version that went to a page as text
func parseVersion(text string) (kv.Version, error) {
	var version kv.Version
	err := version.UnmarshalText([]byte(text))
	return version, err
}

// fakeClock is the store's clock in a case's script, moved by the script
type fakeClock struct {
	mu  sync.Mutex
	now time.Time
}

// checkStart is where every script's clock starts
var checkStart = time.Date(2026, 9, 26, 12, 0, 0, 0, time.UTC)

func (c *fakeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeClock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}

// transcript is what a case's script answered, step by step, so that both
// implementations are held to one list of answers
type transcript struct {
	answers []string
}

// note keeps a step's answer, or the kind of its error
func (t *transcript) note(answer any, err error) {
	if err != nil {
		answer = "error: " + errorKind(err)
	}
	t.answers = append(t.answers, fmt.Sprint(answer))
}

// differs names the first step whose answer is not the one wanted
func (t *transcript) differs(want ...string) error {
	for i := range max(len(want), len(t.answers)) {
		if got, wanted := answerAt(t.answers, i), answerAt(want, i); got != wanted {
			return fmt.Errorf("step %d answered %q, want %q; all: %q", i+1, got, wanted, t.answers)
		}
	}
	return nil
}

func answerAt(answers []string, i int) string {
	if i < len(answers) {
		return answers[i]
	}
	return "(no step)"
}
