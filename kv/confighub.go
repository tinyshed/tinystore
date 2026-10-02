package kv

import (
	"context"
	"encoding/json"
	"fmt"
	"iter"
	"maps"
	"slices"
	"sync"
	"sync/atomic"
	"unicode/utf8"

	"github.com/tinyshed/tinystore"
)

// the kind of bucket a config keeps its changed fields in
const kindConfig = "config"

// the bounds of what a config keeps: a path, one value, and all of them, which
// travel to a watcher in one message
const (
	maxConfigPath  = 256
	maxConfigValue = 16 << 10
	maxConfigKept  = 256 << 10
)

// configHub is what one config keeps, its changed fields as JSON by path,
// shared by every handle on it in this process. The store's one process sees
// every change, so the hub tells every watcher of it, here and through the
// server, without reading the file again.
type configHub struct {
	fields *Bucket[Raw] // a path a key

	mu      sync.Mutex // one change at a time, and the fields with it
	kept    map[string][]byte
	next    chan struct{} // closed at the next change, so that any number may wait for it
	changes atomic.Int64  // how many changes the hub has made
}

// configHub is the hub of the config name, made and read the first time
func (s *Store) configHub(ctx context.Context, name string) (*configHub, error) {
	s.opened.Lock()
	hub, ok := s.configs[name]
	s.opened.Unlock()
	if ok {
		return hub, nil
	}

	id, err := s.claimBucket(ctx, name, kindConfig)
	if err != nil {
		return nil, err
	}
	fields := &Bucket[Raw]{branch: branch{state: s, id: id, name: name}, codec: codecFor[Raw]()}
	kept := map[string][]byte{}
	for entry, err := range fields.All(ctx) {
		if err != nil {
			return nil, err
		}
		kept[entry.Key] = entry.Value.Bytes
	}

	s.opened.Lock()
	defer s.opened.Unlock()
	if hub, ok = s.configs[name]; ok {
		return hub, nil
	}
	hub = &configHub{fields: fields, kept: kept, next: make(chan struct{})}
	s.configs[name] = hub
	return hub, nil
}

// snapshot is the kept fields and the change that made them; the caller does
// not change the map
func (h *configHub) snapshot() (map[string][]byte, int64) {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.kept, h.changes.Load()
}

// waiting is closed at the next change
func (h *configHub) waiting() <-chan struct{} {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.next
}

// change keeps set and forgets reset in one transaction, then tells every
// watcher. Nothing changes when the transaction fails.
func (h *configHub) change(ctx context.Context, set map[string][]byte, reset []string) error {
	h.mu.Lock()
	defer h.mu.Unlock()

	next := maps.Clone(h.kept)
	maps.Copy(next, set)
	for _, path := range reset {
		delete(next, path)
	}
	if err := checkKept(next); err != nil {
		return fmt.Errorf("kv: config %q: %w", h.fields.name, err)
	}

	err := h.fields.state.Tx(ctx, func(tx *Tx) error {
		fields := h.fields.WithTx(tx)
		for _, path := range slices.Sorted(maps.Keys(set)) {
			if err := fields.Set(ctx, path, Raw{Kind: RawBytes, Bytes: set[path]}); err != nil {
				return err
			}
		}
		for _, path := range reset {
			if err := fields.Delete(ctx, path); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		return err
	}

	h.kept = next
	h.changes.Add(1)
	close(h.next)
	h.next = make(chan struct{})
	return nil
}

// checkKept refuses what a config may not keep: a path that is not text of
// 1 to 256 bytes, a value that is not JSON of 16 KiB at most, and more than
// 256 KiB of them all
func checkKept(kept map[string][]byte) error {
	total := 0
	for path, value := range kept {
		switch {
		case path == "" || len(path) > maxConfigPath || !utf8.ValidString(path):
			return fmt.Errorf("%w: a path of %d bytes, not text of 1 to %d", tinystore.ErrInvalid, len(path),
				maxConfigPath)
		case len(value) > maxConfigValue:
			return fmt.Errorf("%w: %s holds %d bytes, past %d", tinystore.ErrLimit, path, len(value), maxConfigValue)
		case !json.Valid(value):
			return fmt.Errorf("%w: %s holds no JSON", tinystore.ErrInvalid, path)
		}
		total += len(path) + len(value)
	}
	if total > maxConfigKept {
		return fmt.Errorf("%w: %d bytes kept, past %d", tinystore.ErrLimit, total, maxConfigKept)
	}
	return nil
}

// RawConfig is what a config keeps, its changed fields as JSON by path, for a
// program that types them itself, as the server does for its clients. It
// checks that a value is JSON and the config within its bounds, and nothing a
// type would.
type RawConfig struct {
	hub *configHub
}

// OpenRawConfig opens the config name of kv.db, creating it the first time. A
// name that holds values, counters or a limiter is ErrInvalid.
func OpenRawConfig(ctx context.Context, state *Store, name string) (*RawConfig, error) {
	hub, err := state.configHub(ctx, name)
	if err != nil {
		return nil, err
	}
	return &RawConfig{hub: hub}, nil
}

// Kept is the changed fields and how many changes the config has had since
// this process opened it.
func (r *RawConfig) Kept() (map[string]json.RawMessage, int64) {
	kept, changes := r.hub.snapshot()
	return rawFields(kept), changes
}

// Change keeps set and forgets reset in one transaction; every watcher of the
// config sees the change once it commits.
func (r *RawConfig) Change(ctx context.Context, set map[string]json.RawMessage, reset []string) error {
	bytes := make(map[string][]byte, len(set))
	for path, value := range set {
		bytes[path] = value
	}
	return r.hub.change(ctx, bytes, reset)
}

// Watch yields the changed fields now and after each change, until ctx ends.
// A watcher that falls behind skips to the latest: it never sees a state the
// config did not have.
func (r *RawConfig) Watch(ctx context.Context) iter.Seq2[map[string]json.RawMessage, int64] {
	return func(yield func(map[string]json.RawMessage, int64) bool) {
		for {
			next := r.hub.waiting()
			if !yield(r.Kept()) {
				return
			}
			select {
			case <-ctx.Done():
				return
			case <-next:
			}
		}
	}
}

func rawFields(kept map[string][]byte) map[string]json.RawMessage {
	fields := make(map[string]json.RawMessage, len(kept))
	for path, value := range kept {
		fields[path] = value
	}
	return fields
}
