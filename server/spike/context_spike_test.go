package spike

import (
	"context"
	mrand "math/rand/v2"
	"testing"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/kv"
)

// what a context that can end costs a point read, without a server: the
// server hands kv each call's own context, derived from its connection's, and
// database/sql watches a query's context from a goroutine of its own whenever
// that context can end, which context.Background cannot
func TestGetsWithAContextThatCanEnd(t *testing.T) {
	skip(t)
	dir := storeDir(t)
	fill(t, dir)
	ctx := context.Background()
	store, err := tinystore.Open(ctx, dir, tinystore.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(ctx)
	state, err := kv.Open(ctx, store, kv.Options{})
	if err != nil {
		t.Fatal(err)
	}
	values, err := kv.OpenBucket[[]byte](ctx, state, bucket)
	if err != nil {
		t.Fatal(err)
	}
	connection, cancel := context.WithCancel(ctx)
	defer cancel()
	for _, kind := range []struct {
		name string
		ctx  func() (context.Context, context.CancelFunc)
	}{
		{"context.Background", func() (context.Context, context.CancelFunc) { return ctx, func() {} }},
		{"a call's own, can end", func() (context.Context, context.CancelFunc) { return context.WithCancel(connection) }},
	} {
		for _, depth := range depths {
			ops, err := run(depth, caseTime(), func(r *mrand.Rand) error {
				call, done := kind.ctx()
				defer done()
				_, _, err := values.GetEntry(call, key(r.IntN(keys)))
				return err
			})
			if err != nil {
				t.Fatal(err)
			}
			t.Logf("%-22s get %4d %11.0f ops/s  mean %9.1f µs", kind.name, depth, ops, float64(depth)/ops*1e6)
		}
	}
}
