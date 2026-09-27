// Package blobs keeps an application's files in blobs/ inside a
// tinystore.Store: objects under keys that are paths, their rows in blobs.db,
// their bytes inline up to 16 KiB and in a file of their own above it.
//
//	objects, err := blobs.Open(ctx, store, blobs.Options{})
//	avatars, err := blobs.OpenBucket(ctx, objects, "avatars", blobs.MaxSize(20<<20))
//	obj, err := avatars.Of(user.ID).Put(ctx, "original", r.Body, blobs.ContentType("image/jpeg"))
//	photo, found, err := avatars.Of(user.ID).Open(ctx, "original") // Read, ReadAt, Seek, Close
//
// An object appears at its commit, whole, or not at all: its bytes are synced
// and renamed into objects/ before the row that names them commits, and a
// file is never written or renamed again, so a reader keeps what it opened.
// Start with blobs.go and bucket.go; every other file holds one step:
//
//	blobs.go     Open, Close: the directory, the file, admission and the gates' steps
//	bucket.go    OpenBucket, Of: a handle on a bucket or a folder of it
//	options.go   the engine's bounds, a bucket's options and a call's
//	keys.go      what a path is, and the range of a folder or a prefix
//	object.go    Object, Query, Page, Usage, KeyError, ETags and meta
//	upload.go    Put, Create, Write: bytes in memory, then in a file of uploads/
//	publish.go   Commit, Abort: the file synced and renamed, the row committed
//	commit.go    the rows a write changes: its condition, its content and the names it takes
//	read.go      Open, Stat
//	reader.go    Reader: Read, ReadAt, Seek, and a whole read checked
//	write.go     Delete, Copy, Move
//	scan.go      Scan, All, Usage
//	clear.go     Clear: a folder deleted at once, or marked and deleted by Maintain
//	ids.go       content ids reserved in blocks, held until their uploads end
//	files.go     the files' names, their directories' syncs, and their removal
//	recover.go   what Open removes that a process that died left
//	maintain.go  Maintain: expiry, marks, files no key names, the settled mark
//	scrub.go     the scrub: every content read once a pass
//	snapshot.go  Snapshot: blobs.db copied and the files it names linked
package blobs
