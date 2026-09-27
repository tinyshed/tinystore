package spike

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"

	sqlitedriver "modernc.org/sqlite"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// TestBlobsCopyDatabase fills a blobs.db with a gigabyte of 64 KiB inline
// bodies and copies it whole with VACUUM INTO, which rebuilds it, and with the
// online backup API the driver ships, which copies its pages
func TestBlobsCopyDatabase(t *testing.T) {
	kvMeasuring(t)
	ctx := t.Context()
	store := blobsOpenStore(t)
	data := blobsBytes(64, 64<<10)
	for first := 0; first < 16_384; first += 256 {
		err := store.db.UpdatePrepared(ctx, func(w sqlite.Writer) error {
			for n := first; n < first+256; n++ {
				if _, err := w.ExecContext(ctx, blobsInsertBody, n+1, data); err != nil {
					return err
				}
			}
			return nil
		})
		if err != nil {
			t.Fatal(err)
		}
	}
	size := kvFileBytes(store.dbPath)
	for _, way := range []string{"VACUUM INTO", "online backup"} {
		into := store.dbPath + ".copy"
		began := time.Now()
		copyErr := blobsVacuumInto(ctx, store.dbPath, into)
		if way == "online backup" {
			_ = os.Remove(into)
			began = time.Now()
			copyErr = blobsBackupInto(ctx, store.dbPath, into)
		}
		if copyErr != nil {
			t.Fatal(copyErr)
		}
		elapsed := time.Since(began)
		t.Logf("%-13s %7.1f MiB in %s, %5.2f GB/s, the copy %7.1f MiB", way, float64(size)/(1<<20),
			jobsMillis(elapsed), float64(size)/elapsed.Seconds()/1e9, float64(kvFileBytes(into))/(1<<20))
		_ = os.Remove(into)
	}
}

// blobsBackupInto copies a database with the driver's online backup API, all
// its pages in one step, from a read-only connection of its own
func blobsBackupInto(ctx context.Context, path, into string) error {
	source, err := sql.Open("sqlite", blobsReadOnly(path))
	if err != nil {
		return err
	}
	defer func() { _ = source.Close() }()
	conn, err := source.Conn(ctx)
	if err != nil {
		return err
	}
	defer func() { _ = conn.Close() }()
	return conn.Raw(func(driverConn any) error {
		backer, ok := driverConn.(interface {
			NewBackup(string) (*sqlitedriver.Backup, error)
		})
		if !ok {
			return errors.New("the driver's connection has no NewBackup")
		}
		backup, err := backer.NewBackup(into)
		if err != nil {
			return err
		}
		_, err = backup.Step(-1)
		return errorsJoin(err, backup.Finish())
	})
}

// the folders' totals Usage reads: a row for each folder of an object's path,
// updated in the transaction of every write
var blobsFolders = []string{
	`create table folders (
		bucket  integer not null,
		path    text    not null,
		objects integer not null,
		bytes   integer not null,
		primary key (bucket, path)
	) strict, without rowid`,
}

const (
	blobsCountFolder = `insert into folders (bucket, path, objects, bytes) values (1, ?1, 1, ?2)
		on conflict (bucket, path) do update set objects = objects + 1, bytes = bytes + excluded.bytes`
	blobsScanUsage = `select count(*), coalesce(sum(size), 0) from objects
		where bucket = 1 and path >= ?1 and path < ?2`
)

// TestBlobsUsage writes objects' rows with paths of 1, 4 and 16 segments from
// one and from 128 callers, with a folder's totals kept for each segment and
// without, then times a Usage scanned from the rows over 10³ to 10⁶ objects
func TestBlobsUsage(t *testing.T) {
	kvMeasuring(t)
	for _, depth := range []int{1, 4, 16} {
		for _, counted := range []bool{false, true} {
			store := blobsOpenStore(t)
			blobsCreate(t, store.db, blobsFolders)
			for _, callers := range []int{1, 128} {
				puts := blobsLoad(t, callers, blobsSeconds(), func(caller, n int) error {
					return blobsPutRow(t.Context(), store, blobsPath(depth, callers, caller, n), counted)
				})
				t.Logf("depth %2d  totals kept %-5v %3d callers  a row's Put %s", depth, counted, callers, puts)
			}
		}
	}
	blobsScanTimes(t)
}

// blobsPath is a path of depth segments: users/42/…/o7
func blobsPath(depth, callers, caller, n int) string {
	segments := []string{fmt.Sprint("c", callers), fmt.Sprint("u", caller)}
	for i := len(segments); i < depth; i++ {
		segments = append(segments, fmt.Sprint("d", n%7))
	}
	return strings.Join(append(segments[:max(depth-1, 0)], fmt.Sprint("o", n)), "/")
}

func blobsPutRow(ctx context.Context, store *blobsStore, path string, counted bool) error {
	return store.db.UpdateGrouped(ctx, 0, func(w sqlite.Writer) error {
		if _, err := w.ExecContext(ctx, blobsInsertObject, path, store.nextID.Add(1), 70_000, []byte("etag")); err != nil {
			return err
		}
		for folder := path; counted && folder != ""; {
			cut := strings.LastIndexByte(folder, '/')
			folder = folder[:max(cut, 0)]
			if _, err := w.ExecContext(ctx, blobsCountFolder, folder+"/", 70_000); err != nil {
				return err
			}
		}
		return nil
	})
}

func blobsCreate(t *testing.T, db *sqlite.File, schema []string) {
	t.Helper()
	err := db.Update(t.Context(), func(tx *sql.Tx) error {
		for _, statement := range schema {
			if _, err := tx.ExecContext(t.Context(), statement); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

// blobsScanTimes fills a folder with 10³ to 10⁶ objects' rows and times a
// Usage that scans them, twenty times each
func blobsScanTimes(t *testing.T) {
	t.Helper()
	ctx := t.Context()
	store := blobsOpenStore(t)
	written := 0
	for _, count := range []int{1_000, 10_000, 100_000, 1_000_000} {
		if count > blobsFileCount() {
			break
		}
		for ; written < count; written += 10_000 {
			err := store.db.UpdatePrepared(ctx, func(w sqlite.Writer) error {
				for n := written; n < min(written+10_000, count); n++ {
					path := fmt.Sprintf("users/42/o%07d", n)
					if _, err := w.ExecContext(ctx, blobsInsertObject, path, n, 70_000, []byte("etag")); err != nil {
						return err
					}
				}
				return nil
			})
			if err != nil {
				t.Fatal(err)
			}
		}
		written = count
		began := time.Now()
		var objects, bytes int64
		for range 20 {
			err := store.db.Lookup(ctx, func(r sqlite.Reader) error {
				return sqlite.QueryRow(ctx, r, blobsScanUsage, "users/42/", "users/420").Scan(&objects, &bytes)
			})
			if err != nil {
				t.Fatal(err)
			}
		}
		t.Logf("Usage scanned over %8d objects: %s a scan (%d counted)", count, jobsMillis(time.Since(began)/20), objects)
	}
}
