package kv_test

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/kv"
)

// contract: SEC-04
func TestAnOlderKeyOpensWhatItSealedAndAResealMovesItToTheNewOne(t *testing.T) {
	opened := map[string][]tinystore.Option{
		"the core in this process": {tinystore.Library(ways.Library())},
		"a private child":          {tinystore.Private(), tinystore.Binary(ways.Binary())},
	}
	ready := map[string]bool{"the core in this process": ways.Library() != "", "a private child": ways.Binary() != ""}
	for name, way := range opened {
		t.Run(name, func(t *testing.T) {
			if !ready[name] {
				t.Skip("not built here")
			}
			defer ways.Recover(t)
			ctx := context.Background()
			dir := t.TempDir()
			old, renewed := filepath.Join(dir, "old.key"), filepath.Join(dir, "new.key")
			check(t, os.WriteFile(old, []byte(strings.Repeat("1a", 32)+"\n"), 0o600))
			check(t, os.WriteFile(renewed, []byte(strings.Repeat("2b", 32)+"\n"), 0o600))
			data := filepath.Join(dir, "data")
			open := func(opts ...tinystore.Option) *tinystore.Store {
				return ways.Must(tinystore.Open(ctx, data, append(append([]tinystore.Option{}, way...), opts...)...))
			}
			passwords := func(store *tinystore.Store) *kv.Bucket[string] {
				return ways.Must(kv.OpenBucket[string](store, "passwords", kv.Encrypted()))
			}

			before := open(tinystore.EncryptionKeyFile(old))
			check(t, passwords(before).Set(ctx, "source-42", "hunter2"))
			check(t, before.Close())

			store := open(tinystore.EncryptionKeyFile(renewed), tinystore.OlderEncryptionKeys(old))
			if got, _ := ways.Must2(passwords(store).Get(ctx, "source-42")); got != "hunter2" {
				t.Fatalf("an older key opens what it sealed: got %q", got)
			}
			if moved := ways.Must(passwords(store).Reseal(ctx)); moved != 1 {
				t.Fatalf("resealed %d, want 1", moved)
			}
			check(t, store.Close())

			after := open(tinystore.EncryptionKeyFile(renewed))
			defer after.Close()
			if got, _ := ways.Must2(passwords(after).Get(ctx, "source-42")); got != "hunter2" {
				t.Fatalf("the new key alone opens it: got %q", got)
			}
		})
	}
}
