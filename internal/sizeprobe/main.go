// Command sizeprobe links the public API so task size can report what importing it costs.
package main

import (
	"context"
	"embed"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"strings"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/blobs"
	"github.com/tinyshed/tinystore/codec"
	"github.com/tinyshed/tinystore/jobs"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/metrics"
	"github.com/tinyshed/tinystore/records"
	"github.com/tinyshed/tinystore/sqldb"
)

//go:embed migrations/*.sql
var migrations embed.FS

func main() {
	blocks, err := codec.New()
	if err != nil {
		panic(err)
	}
	defer func() { _ = blocks.Close() }()

	head, payload, err := blocks.Encode([]codec.Sample{{At: 1, Value: 1}})
	if err != nil {
		panic(err)
	}
	if _, err = blocks.Decode(head, payload); err != nil {
		panic(err)
	}
	fmt.Println(len(payload))
	directory, err := os.MkdirTemp("", "tinystore-size-")
	if err != nil {
		panic(err)
	}
	defer func() { _ = os.RemoveAll(directory) }()
	ctx := context.Background()
	runtime, err := tinystore.Open(ctx, directory, tinystore.Options{})
	if err != nil {
		panic(err)
	}
	defer func() { _ = runtime.Close(ctx) }()
	store, err := metrics.Open(ctx, runtime, metrics.Options{})
	if err != nil {
		panic(err)
	}
	at := time.Now().UnixMilli()
	series := metrics.Series{Labels: []metrics.Label{{Name: "__name__", Value: "probe"}}}
	batch := metrics.Batch{Series: series, Samples: []metrics.Sample{{At: at, Value: 1}}}
	if err = store.Ingest(ctx, []metrics.Batch{batch}); err != nil {
		panic(err)
	}
	if _, err = store.Maintain(ctx); err != nil {
		panic(err)
	}
	result, err := store.Read(ctx, metrics.Range{Matchers: series.Labels, From: at, To: at + 1})
	if err != nil {
		panic(err)
	}
	fmt.Println(len(result), store.Stats())
	probeRecords(ctx, runtime)
	probeKV(ctx, runtime)
	probeJobs(ctx, runtime)
	probeBlobs(ctx, runtime)
	probeSQL(ctx, runtime)
}

// ProbeNote is a row of the probe's own database, declared as an application
// declares one
type ProbeNote struct {
	ID        int64 `db:",generated"`
	Title     string
	Tags      sqldb.JSON[[]string]
	Due       *sqldb.Date
	CreatedAt time.Time
}

var (
	probeNotes  = sqldb.Table[ProbeNote]("notes", sqldb.PrimaryKey("id"), sqldb.Index("created_at"))
	probeSchema = sqldb.Schema(probeNotes)
)

// probeSQL links sqldb the way an application uses it: a schema Open checks
// the file against, Insert, the typed reads, Each, a transaction, a View and
// a constraint's error
func probeSQL(ctx context.Context, runtime *tinystore.Store) {
	db, err := sqldb.Open(ctx, runtime, "app", migrations, probeSchema)
	if err != nil {
		panic(err)
	}
	note, err := sqldb.Insert(ctx, db, probeNotes, ProbeNote{Title: "probe", CreatedAt: time.Now()})
	if err != nil {
		panic(err)
	}
	_, err = db.Exec(ctx, `insert into notes (id, title, tags, created_at) values (?, 'twice', '[]', 0)`, note.ID)
	if broken, ok := errors.AsType[*sqldb.ConstraintError](err); !ok || broken.Kind != sqldb.PrimaryKeyViolation {
		panic(err)
	}
	read, found, err := sqldb.One[ProbeNote](ctx, db, `select * from notes where id = ?`, note.ID)
	if err != nil || !found {
		panic(fmt.Sprint(found, err))
	}
	err = db.Tx(ctx, func(tx *sqldb.Tx) error {
		_, execErr := tx.Exec(ctx, `update notes set title = ? where id = ?`, "edited", note.ID)
		return execErr
	})
	if err == nil {
		err = db.View(ctx, func(tx *sqldb.Tx) error {
			_, viewErr := sqldb.All[ProbeNote](ctx, tx, `select * from notes order by created_at`)
			return viewErr
		})
	}
	if err != nil {
		panic(err)
	}
	for _, eachErr := range sqldb.Each[ProbeNote](ctx, db, `select * from notes`) {
		if eachErr != nil {
			panic(eachErr)
		}
	}
	count, err := sqldb.Scalar[int](ctx, db, `select count(*) from notes`)
	if err != nil {
		panic(err)
	}
	fmt.Println(read.Title, count)
}

// probeRecords links the records engine the way an application uses it: its
// handler, Append, Maintain, Read, Follow and Drop
func probeRecords(ctx context.Context, runtime *tinystore.Store) {
	logs, err := records.Open(ctx, runtime, records.Options{})
	if err != nil {
		panic(err)
	}
	slog.New(logs.Handler("probe")).Info("probe", "n", 1)
	if err = logs.Flush(ctx); err != nil {
		panic(err)
	}
	event := records.Record{At: time.Now(), Stream: "probe", Name: "event", Attrs: []records.Field{records.Int("n", 1)}}
	if err = logs.Append(ctx, event); err != nil {
		panic(err)
	}
	if _, err = logs.Maintain(ctx); err != nil {
		panic(err)
	}
	page, err := logs.Read(ctx, records.Query{Streams: []string{"probe"}})
	if err != nil {
		panic(err)
	}
	batch, err := logs.Follow(ctx, records.Cursor{}, 10)
	if err != nil {
		panic(err)
	}
	for _, damage := range logs.Damaged() {
		if err = logs.Drop(ctx, damage); err != nil {
			panic(err)
		}
	}
	fmt.Println(len(page.Records), len(batch.Records), logs.Stats())
}

// probeKV links the kv engine the way an application uses it: a sliding
// bucket, its writes and reads, a transaction, a walk and a Clear, counters in
// memory, and maintenance
func probeKV(ctx context.Context, runtime *tinystore.Store) {
	state, err := kv.Open(ctx, runtime, kv.Options{})
	if err != nil {
		panic(err)
	}
	value, found, walked := probeBucket(ctx, state)
	attempts := probeCounters(ctx, state)
	done, err := state.Maintain(ctx)
	if err != nil {
		panic(err)
	}
	fmt.Println(value, found, walked, attempts, done)
}

func probeCounters(ctx context.Context, state *kv.Store) int64 {
	attempts, err := kv.OpenCounters(ctx, state, "attempts", kv.DefaultTTL(time.Minute), kv.LoseAtMost(time.Second))
	if err != nil {
		panic(err)
	}
	if _, err = attempts.Of("ip").Add(ctx, "10.0.0.1", 1); err != nil {
		panic(err)
	}
	if _, err = attempts.Of("ip").Max(ctx, "10.0.0.2", 5); err != nil {
		panic(err)
	}
	if err = attempts.Of("ip").Delete(ctx, "10.0.0.2"); err != nil {
		panic(err)
	}
	held, err := attempts.Of("ip").Get(ctx, "10.0.0.1")
	if err != nil {
		panic(err)
	}
	return held
}

func probeBucket(ctx context.Context, state *kv.Store) (value string, found bool, walked int) {
	sessions, err := kv.OpenBucket[string](ctx, state, "sessions", kv.Sliding(time.Hour))
	if err != nil {
		panic(err)
	}
	written, err := sessions.Of(42).SetEntry(ctx, "token", "phone")
	if err != nil {
		panic(err)
	}
	if _, err = sessions.Of(42).SetIfAbsent(ctx, "other", "laptop"); err != nil {
		panic(err)
	}
	err = state.Tx(ctx, func(tx *kv.Tx) error {
		_, _, takeErr := sessions.WithTx(tx).Of(42).Take(ctx, "other")
		return takeErr
	})
	if err != nil {
		panic(err)
	}
	if err = sessions.Of(42).Set(ctx, "token", "tablet", kv.IfVersion(written.Version)); err != nil {
		panic(err)
	}
	value, found, err = sessions.Of(42).Get(ctx, "token")
	if err != nil {
		panic(err)
	}
	for _, walkErr := range sessions.Of(42).All(ctx) {
		if walkErr != nil {
			panic(walkErr)
		}
		walked++
	}
	if err = sessions.Of(43).Clear(ctx); err != nil {
		panic(err)
	}
	return value, found, walked
}

// probeJobs links the jobs engine the way an application uses it: keyed and
// timed enqueues in a transaction, an update, a cancel and reads, a schedule,
// a claim settled by hand, a Work loop and maintenance
func probeJobs(ctx context.Context, runtime *tinystore.Store) {
	queues, err := jobs.Open(ctx, runtime, jobs.Options{})
	if err != nil {
		panic(err)
	}
	later := probeEnqueues(ctx, queues)
	if _, err = jobs.OpenSchedule(ctx, queues, "purge", jobs.Daily("03:10", time.UTC)); err != nil {
		panic(err)
	}
	if job, found, claimErr := later.Claim(ctx, jobs.Lease(time.Minute)); claimErr != nil {
		panic(claimErr)
	} else if found {
		if err = job.Extend(ctx, time.Minute); err == nil {
			err = job.Snooze(ctx, jobs.After(time.Second))
		}
	}
	if err != nil {
		panic(err)
	}
	err = later.Work(ctx, func(ctx context.Context, job jobs.Job[string]) error {
		return job.Retry(ctx, fmt.Errorf("probe %s", job.Value), jobs.After(time.Hour))
	}, jobs.Workers(2), jobs.Timeout(time.Minute), jobs.UntilIdle())
	if err != nil {
		panic(err)
	}
	done, err := queues.Maintain(ctx)
	if err != nil {
		panic(err)
	}
	fmt.Println(done)
}

func probeEnqueues(ctx context.Context, queues *jobs.Store) *jobs.Queue[string] {
	later, err := jobs.OpenQueue[string](ctx, queues, "later", jobs.MaxAttempts(3), jobs.KeepDone(time.Hour))
	if err != nil {
		panic(err)
	}
	err = queues.Tx(ctx, func(tx *jobs.Tx) error {
		repeat := jobs.Cron("*/15 9-18 * * 1-5", time.UTC)
		return later.WithTx(tx).Enqueue(ctx, "digest", jobs.Key("digest"), repeat)
	})
	if err == nil {
		err = later.Enqueue(ctx, "draft", jobs.Key("chat:1"), jobs.At(time.Now()))
	}
	if err == nil {
		err = later.Update(ctx, "chat:1", "edited", jobs.Every(time.Hour))
	}
	if err != nil {
		panic(err)
	}
	if _, err = later.Cancel(ctx, "digest"); err != nil {
		panic(err)
	}
	entry, _, err := later.Get(ctx, "chat:1")
	if err != nil {
		panic(err)
	}
	for _, walkErr := range later.All(ctx, jobs.Query{Prefix: "chat:"}) {
		if walkErr != nil {
			panic(walkErr)
		}
	}
	fmt.Println(entry.State)
	return later
}

// probeBlobs links the blobs engine the way an application uses it: a Put and
// an upload, a reader that seeks and reads a range, Stat, Copy, Move, Delete,
// a walk, Usage, a Clear and maintenance
func probeBlobs(ctx context.Context, runtime *tinystore.Store) {
	objects, err := blobs.Open(ctx, runtime, blobs.Options{})
	if err != nil {
		panic(err)
	}
	media, err := blobs.OpenBucket(ctx, objects, "media", blobs.DefaultTTL(time.Hour), blobs.MaxSize(1<<30))
	if err != nil {
		panic(err)
	}
	mine := media.Of("users", 42)
	if _, err = mine.Put(ctx, "icon.png", strings.NewReader("png"), blobs.ContentType("image/png")); err != nil {
		panic(err)
	}
	upload, err := mine.Create(ctx, "film.mp4", blobs.Meta("name", "film.mp4"), blobs.IfNoneMatch())
	if err != nil {
		panic(err)
	}
	defer upload.Abort()
	if _, err = upload.Write([]byte("mp4")); err != nil {
		panic(err)
	}
	film, err := upload.Commit(ctx)
	if err != nil {
		panic(err)
	}
	fmt.Println(probeReader(ctx, mine), film.ETag)
	probeObjects(ctx, mine)
	done, err := objects.Maintain(ctx)
	if err != nil {
		panic(err)
	}
	fmt.Println(done)
}

func probeReader(ctx context.Context, mine *blobs.Bucket) int {
	reader, found, err := mine.Open(ctx, "film.mp4")
	if err != nil || !found {
		panic(fmt.Sprint(found, err))
	}
	defer func() { _ = reader.Close() }()
	if _, err = reader.Seek(1, io.SeekStart); err != nil {
		panic(err)
	}
	rest, err := io.ReadAll(reader)
	if err != nil {
		panic(err)
	}
	if _, err = reader.ReadAt(make([]byte, 1), 0); err != nil {
		panic(err)
	}
	return len(rest)
}

func probeObjects(ctx context.Context, mine *blobs.Bucket) {
	if _, _, err := mine.Stat(ctx, "icon.png"); err != nil {
		panic(err)
	}
	if _, err := mine.Copy(ctx, "icon.png", "copy.png", blobs.TTL(time.Minute)); err != nil {
		panic(err)
	}
	if _, err := mine.Move(ctx, "copy.png", "moved.png", blobs.ExpireAt(time.Now().Add(time.Hour))); err != nil {
		panic(err)
	}
	if err := mine.Delete(ctx, "moved.png", blobs.IfMatch("*")); err != nil {
		panic(err)
	}
	for _, err := range mine.All(ctx, blobs.Query{Prefix: "f"}) {
		if err != nil {
			panic(err)
		}
	}
	usage, err := mine.Usage(ctx)
	if err != nil {
		panic(err)
	}
	if err = mine.Clear(ctx); err != nil {
		panic(err)
	}
	fmt.Println(usage)
}
