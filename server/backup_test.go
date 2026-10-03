package server

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"testing/fstest"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/backup"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
	"github.com/tinyshed/tinystore/sqldb"
)

// downloadBackup asks the server for a backup and gives back the zip's bytes
func downloadBackup(t *testing.T, conn *client.Conn) ([]byte, error) {
	t.Helper()
	st, err := conn.Open(t.Context(), wire.ServerBackup, wire.Empty{}, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = st.Response(t.Context()); err != nil {
		return nil, err
	}
	var zipped []byte
	for {
		chunk, last, err := st.Next(t.Context())
		if err != nil {
			return zipped, err
		}
		zipped = append(zipped, chunk...)
		if last {
			return zipped, nil
		}
	}
}

// a backup over the wire holds every engine whose file the directory holds,
// a database no client opened included, and makes no file of an engine that
// was not there
func TestABackupOverTheWireHoldsEveryEngineOnDisk(t *testing.T) {
	ctx := t.Context()
	root := t.TempDir()
	migrations := fstest.MapFS{notesMigration.Name: {Data: []byte(notesMigration.Text)}}
	before, err := tinystore.Open(ctx, root, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	if err = writeNoteAndCode(ctx, before, migrations); err != nil {
		t.Fatal(errors.Join(err, before.Close(ctx)))
	}
	if err = before.Close(ctx); err != nil {
		t.Fatal(err)
	}

	ts := serveTestStore(t, root, mustOpen(t, root), Options{})
	conn := ts.dial(t, wire.Hello{})
	zipped, err := downloadBackup(t, conn)
	if err != nil {
		t.Fatal(err)
	}
	for _, absent := range []string{"metrics.db", "records.db", "jobs.db", "blobs"} {
		if _, statErr := os.Stat(filepath.Join(root, absent)); !errors.Is(statErr, os.ErrNotExist) {
			t.Errorf("a backup made %s: %v", absent, statErr)
		}
	}

	restored := filepath.Join(t.TempDir(), "restored")
	if err = backup.Restore(ctx, restored, bytes.NewReader(zipped), int64(len(zipped))); err != nil {
		t.Fatal(err)
	}
	back := mustOpen(t, restored)
	defer back.Close(context.Background())
	notes, err := sqldb.Open(ctx, back, "app", migrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	count, err := sqldb.Scalar[int](ctx, notes, `select count(*) from notes`)
	if err != nil || count != 1 {
		t.Fatalf("the restored database holds %d notes: %v", count, err)
	}
	restoredKV, err := kv.Open(ctx, back, kv.Options{})
	if err != nil {
		t.Fatal(err)
	}
	restoredCodes, err := kv.OpenBucket[string](ctx, restoredKV, "codes")
	if err != nil {
		t.Fatal(err)
	}
	if code, found, err := restoredCodes.Get(ctx, "K7Q2"); err != nil || !found || code != "42" {
		t.Fatalf("the restored kv holds %q, %v: %v", code, found, err)
	}
}

func mustOpen(t *testing.T, dir string) *tinystore.Store {
	t.Helper()
	store, err := tinystore.Open(t.Context(), dir, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	return store
}

// writeNoteAndCode keeps a note in the database app and a code in kv
func writeNoteAndCode(ctx context.Context, store *tinystore.Store, migrations fstest.MapFS) error {
	db, err := sqldb.Open(ctx, store, "app", migrations, nil)
	if err != nil {
		return err
	}
	if _, err = db.Exec(ctx, `insert into notes (title) values ('kept')`); err != nil {
		return err
	}
	state, err := kv.Open(ctx, store, kv.Options{})
	if err != nil {
		return err
	}
	codes, err := kv.OpenBucket[string](ctx, state, "codes")
	if err != nil {
		return err
	}
	return codes.Set(ctx, "K7Q2", "42")
}
