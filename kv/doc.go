// Package kv keeps an application's current state in kv.db inside a
// tinystore.Store: buckets of one value type and counters, keyed by text in
// branches, with expiry by the store's clock and versions that never repeat.
//
//	state, err := kv.Open(ctx, store, kv.Options{})
//	sessions, err := kv.OpenBucket[Session](ctx, state, "sessions")
//	err = sessions.Of(userID).Set(ctx, token, session, kv.TTL(24*time.Hour))
//	session, found, err := sessions.Of(userID).Get(ctx, token)
//
// A write waits for the file's one writer beside the writes of other
// goroutines and commits with them, one fsync for all; a read is one
// statement. Start with kv.go and bucket.go; every other file holds one step:
//
//	kv.go        Open, Close, Snapshot: the handle, its revision and admission
//	bucket.go    OpenBucket, Of, WithTx: a handle on a bucket of values
//	counters.go  OpenCounters, Add, Max, Get, Delete: a handle on counters
//	branch.go    what both handles share: a branch's path and where its calls run
//	options.go   the engine's bounds, the options of buckets, counters and calls
//	keys.go      a key's text and the path owners and a key make
//	values.go    a value's bytes by its type, and where they are kept
//	entry.go     Entry, Version, Query, Page, KeyError
//	read.go      Get, GetEntry, Has, Scan
//	write.go     Set, SetEntry, SetIfAbsent, SetEntryIfAbsent, Take, Delete, Touch
//	tx.go        Tx and View: several calls in one transaction or snapshot
//	expiry.go    Maintain: expired keys and the values they spilled
package kv
