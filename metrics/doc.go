// Package metrics stores exact numeric time series in one SQLite file.
//
// A sample lives in one of two places, and every read sees both at once:
//
//	Ingest ──► head ──── Maintain ───► groups
//	           mutable,  seals what    immutable blocks
//	           durable   can no longer of up to 240 samples
//	                     change
//
//	Read, Stream, Aggregate ◄── head + groups from one snapshot
//
// Start with the public surface in open.go and types.go, then follow one step
// per file:
//
//	ingest.go        Ingest: admit → check → register → write each head
//	ingest_input.go  one call's batches, checked and grouped by series
//	head.go          the packed head: parse, decode, encode, merge
//	head_state.go    the head's row in series_state
//	registry.go      series identity, labels and postings
//	query.go         Read and Stream from one snapshot
//	aggregate.go     exact buckets over raw samples
//	packing.go       Maintain: seal safe prefixes into groups
//	retention.go     expiry and reclamation
package metrics
