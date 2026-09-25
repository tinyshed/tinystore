// Command records measures the records engine on the corpora of the research
// rounds, so that the figures docs/records.md promises are measured on the
// engine itself: bytes a record in the file, divided by object, and what reads
// fetch. Every stage prints key=value lines.
//
//	records -stage density -dir <dir>                      one million frontend records
//	records -stage late -dir <dir>                         the same, one in a hundred up to ten minutes late
//	records -stage docker -dir <dir> -corpus <corpus>      production container logs, full segments
//	records -stage replay -dir <dir> -corpus <corpus>      the same corpus on its own clock, sealed hourly
package main

import (
	"context"
	"flag"
	"log"
	"os"
	"path/filepath"
	"time"
)

func main() {
	stage := flag.String("stage", "density", "density, late, docker or replay")
	dir := flag.String("dir", "", "an empty directory for the store")
	corpus := flag.String("corpus", "", "a docker json-file corpus, for docker and replay")
	count := flag.Int("records", 1_000_000, "frontend records, for density and late")
	batch := flag.Int("batch", 1024, "records a fixture appends at a time: a flush, or a research round's segment")
	sealAge := flag.Duration("seal-age", time.Hour, "how long a head waits before it seals however small, for replay")
	flag.Parse()
	if *dir == "" {
		log.Fatal("-dir is required")
	}
	if err := os.RemoveAll(*dir); err != nil {
		log.Fatal(err)
	}
	ctx := context.Background()
	path := filepath.Join(*dir, "store")
	switch *stage {
	case "density":
		measureFixture(ctx, path, frontendRecords(*count), "frontend", *batch)
	case "late":
		measureFixture(ctx, path, withLateRecords(frontendRecords(*count)), "frontend, 1% late", *batch)
	case "docker":
		measureDocker(ctx, path, readCorpus(*corpus))
	case "replay":
		replayDocker(ctx, path, readCorpus(*corpus), *sealAge)
	default:
		log.Fatalf("unknown stage %q", *stage)
	}
}
