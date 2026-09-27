// Package tinystore keeps one application's data in one directory: a file per
// engine, and one lifecycle for all of them.
//
//	data/
//	├── LOCK           held while a store is open
//	├── metrics.db     metrics.Open(ctx, store, …)
//	├── records.db     records.Open(ctx, store, …)
//	├── kv.db          kv.Open(ctx, store, …)
//	├── jobs.db        jobs.Open(ctx, store, …)
//	├── blobs/         blobs.Open(ctx, store, …): blobs.db and the files
//	└── sql/
//	    └── app.db     sqldb.Open(ctx, store, "app", migrations)
//
// Open the store, then each engine the program uses against it. Close stops
// background work and closes every engine, the last opened first. The store
// imports no engine, so a program links only the engines it opens.
//
//	tinystore.go   Open, Close: the store and its lifecycle
//	errors.go      the sentinels every engine wraps
//	engine.go      Claim, Attach, Logger, Now: how an engine joins the store
//	every.go       background work, and how its failures are logged
//	memory.go      Reserve: one memory budget for every engine's work
//	snapshot.go    Snapshot: every engine's file copied while it keeps working
//	lock_*.go      one store per directory, per platform
package tinystore
