// Command records measures the records engine on the corpora of the research
// rounds, so that the figures docs/records.md promises are measured on the
// engine itself: bytes a record in the file, divided by object, and what reads
// fetch. Every stage prints key=value lines.
//
//	records -stage density -dir <dir>                      one million frontend records
//	records -stage late -dir <dir>                         the same, one in a hundred up to ten minutes late
//	records -stage docker -dir <dir> -corpus <corpus>      production container logs, full segments
//	records -stage replay -dir <dir> -corpus <corpus>      the same corpus on its own clock, sealed hourly
//	records -stage reach -dir <dir> -records <n>           one-second reads near a large store's start and end
//	records -stage lines -dir <dir> -corpus <corpus>       the corpus through writers of Lines, as a follower of it
//	records -stage load -dir <dir> -records <n>            reads alone, appends and seals alone, and both at once
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
	stage := flag.String("stage", "density", "density, late, docker, replay, reach, lines or load")
	readers := flag.Int("readers", 2, "readers reading one second at a time, for load")
	phase := flag.Duration("phase", 20*time.Second, "how long each phase of load lasts")
	rate := flag.Int("rate", 0, "records a second the writer of load appends; as fast as it can when zero")
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
	case "reach":
		measureReach(ctx, path, *count)
	case "lines":
		measureLines(ctx, path, *corpus)
	case "load":
		measureLoad(ctx, path, *count, *readers, *rate, *phase)
	default:
		log.Fatalf("unknown stage %q", *stage)
	}
}
