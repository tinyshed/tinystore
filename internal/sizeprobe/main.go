// Command sizeprobe links the public API so task size can report what importing it costs.
package main

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/codec"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/metrics"
	"github.com/tinyshed/tinystore/records"
)

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

// probeKV links the kv engine the way an application uses it: a bucket, its
// writes and reads, a transaction and maintenance
func probeKV(ctx context.Context, runtime *tinystore.Store) {
	state, err := kv.Open(ctx, runtime, kv.Options{})
	if err != nil {
		panic(err)
	}
	sessions, err := kv.OpenBucket[string](ctx, state, "sessions", kv.DefaultTTL(time.Hour))
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
	value, found, err := sessions.Of(42).Get(ctx, "token")
	if err != nil {
		panic(err)
	}
	page, err := sessions.Of(42).Scan(ctx, kv.Query{})
	if err != nil {
		panic(err)
	}
	done, err := state.Maintain(ctx)
	if err != nil {
		panic(err)
	}
	fmt.Println(value, found, len(page.Entries), done)
}
