// Package spike compares storage prototypes with the implemented adaptive codec.
//
// The small gates run everywhere, because they pin arithmetic
// that is easy to get wrong and expensive to get wrong later — a counter's
// increase across a reset, the codec's round trip, what a late sample would do to
// a sealed block, the strict edge of the watermark, why a partial range needs raw,
// the retention overshoot of a whole block, cutoff reads and direct head expiry.
// The opt-in measurements build a million series
// or write for forty seconds, so they are skipped unless TINYSTORE_SPIKE is set,
// and they belong in a linux container if their numbers are to sit beside the
// others:
//
//	docker run --rm -v <repo>:/src -w /src -e TINYSTORE_SPIKE=1 golang:1.27 \
//	  go test ./internal/tinystore/spike/ -run <name> -v
//
// This package is deleted when the storage engine exists: the gates move onto
// the real implementation, and a harness that measures a prototype nobody runs any
// more is worse than no harness at all.
package spike
