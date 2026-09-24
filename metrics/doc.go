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
// Start with store.go, types.go and options.go; every other public method
// opens the file of its step:
//
//	store.go           Open, Close, Stats: the handle and its lifecycle
//	types.go           samples, series, ranges, results and errors
//	options.go         options, limits and their defaults
//	admission.go       the open/closed gate, slots and the shared work budget
//
//	ingest.go          Ingest: admit → check → register → write each head
//	ingest_input.go    one call's batches, checked and grouped by series
//	series.go          series identity, the label dictionary and postings
//	match.go           matchers to series through the postings
//	head.go            the packed head: parse, decode, encode, merge
//	head_state.go      the head's row in series_state
//
//	read.go            Read and Stream within the query budget
//	snapshot.go        heads and groups taken from one read transaction
//	snapshot_*.go      the batched fetches of heads, groups and payloads
//	aggregate.go       Aggregate: exact buckets over raw samples
//
//	maintain.go        Maintain: expire due series, then seal ready ones
//	seal.go            the safe prefix of a head, encoded as blocks
//	publish.go         blocks, head and frontier written in one transaction
//	merge.go           a new group absorbing the one before it
//	retention.go       expiry and reclamation
//	quarantine.go      series whose maintenance failed
//	drop.go            DropSeries: one series and everything it holds
//
//	group.go           a group of blocks and where their payloads live
//	directory.go       the group directory's bytes
//	clock.go           timestamps shared between series
//	summary.go         a block's exact summary
//	values*.go         value representations: ordinary, changes, grid
//	residuals.go       the grid's residuals
//	binary.go          bounded binary reads and metadata compression
package metrics
