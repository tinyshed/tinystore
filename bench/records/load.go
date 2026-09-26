package main

import (
	"context"
	"fmt"
	"log"
	"math/rand/v2"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore/records"
)

// measureLoad asks what reads cost while records are appended and sealed
// beside them: a store of count frontend records, then phases of the same
// length, readers alone, a writer alone, and both, each reporting its reads'
// latencies, its appends, and the process's memory and write-ahead log at
// their highest. The writer appends a flush of 1024 at a time, rate records
// a second or as fast as it can when rate is zero, and seals what fills;
// nothing of the fixture is held beyond the flush being written.
func measureLoad(ctx context.Context, dir string, count, readers, rate int, phase time.Duration) {
	load := &loadRun{dir: dir, random: rand.New(rand.NewPCG(17, 23))} //nolint:gosec // the research's seeds
	load.h = openHarness(ctx, dir, time.Unix(0, fixtureBase))
	for load.appended.Load() < int64(count) {
		load.appendFlush(ctx)
	}
	load.seal(ctx)
	fmt.Printf("stage=load records=%d readers=%d rate=%d phase=%v\n", count, readers, rate, phase)
	for _, run := range []struct {
		name            string
		readers, writer bool
	}{{"read", true, false}, {"write", false, true}, {"both", true, true}} {
		load.phase(ctx, run.name, readers*boolInt(run.readers), run.writer, rate, phase)
	}
	load.h.close(ctx)
}

func boolInt(b bool) int {
	if b {
		return 1
	}
	return 0
}

// loadRun is the store under load and how far its writer has come, which
// readers ask while it moves
type loadRun struct {
	h        *harness
	dir      string
	random   *rand.Rand // the writer's alone
	appended atomic.Int64
	sealed   atomic.Int64
}

// appendFlush appends the fixture's next 1024 records at their own time
func (l *loadRun) appendFlush(ctx context.Context) {
	next := int(l.appended.Load())
	batch := make([]records.Record, 1024)
	for i := range batch {
		batch[i] = frontendClick(l.random, next+i)
	}
	l.h.setClock(batch[len(batch)-1].At.Add(time.Minute))
	if err := l.h.logs.Append(ctx, batch...); err != nil {
		log.Fatal(err)
	}
	l.appended.Add(int64(len(batch)))
	if (next+len(batch))%16_384 < len(batch) {
		l.seal(ctx)
	}
}

func (l *loadRun) seal(ctx context.Context) {
	work, err := l.h.logs.Maintain(ctx)
	if err != nil {
		log.Fatal(err)
	}
	l.sealed.Add(int64(work.SealedSegments))
}

// phase runs readers and perhaps the writer for a while, with a watcher
// taking the process's memory and the log's size every tenth of a second
func (l *loadRun) phase(ctx context.Context, name string, readers int, writer bool, rate int, length time.Duration) {
	ctx, cancel := context.WithTimeout(ctx, length)
	defer cancel()
	var work sync.WaitGroup
	var peaks peaks
	work.Go(func() { peaks.watch(ctx, l.dir) })
	appendedBefore, sealedBefore := l.appended.Load(), l.sealed.Load()
	if writer {
		work.Go(func() { l.write(ctx, rate) })
	}
	latencies := make([][]time.Duration, readers)
	for reader := range readers {
		work.Go(func() { latencies[reader] = l.read(ctx, uint64(reader)) }) //nolint:gosec // an index, never negative
	}
	work.Wait()
	all := slices.Concat(latencies...)
	slices.Sort(all)
	fmt.Printf("phase=%s readers=%d writer=%v reads=%d %s appended=%d sealed_segments=%d %s\n", name, readers, writer,
		len(all), percentiles(all), l.appended.Load()-appendedBefore, l.sealed.Load()-sealedBefore, peaks.String())
}

// write appends flushes until the phase ends, rate records a second when rate
// is set; a flush that has begun is finished
func (l *loadRun) write(ctx context.Context, rate int) {
	start, written := time.Now(), 0
	for ctx.Err() == nil {
		if rate > 0 {
			due := start.Add(time.Duration(float64(written) / float64(rate) * float64(time.Second)))
			select {
			case <-ctx.Done():
				return
			case <-time.After(time.Until(due)):
			}
		}
		l.appendFlush(context.WithoutCancel(ctx))
		written += 1024
	}
}

// read reads one second at a random place among what the writer had
// appended when the read began, again and again, and returns how long each took
func (l *loadRun) read(ctx context.Context, seed uint64) []time.Duration {
	random := rand.New(rand.NewPCG(seed, 7)) //nolint:gosec // where to read, not a secret
	var took []time.Duration
	for ctx.Err() == nil {
		from := time.Unix(0, fixtureBase+random.Int64N(l.appended.Load()-1000)*1_234_567).UTC()
		start := time.Now()
		_, err := l.h.logs.Read(ctx, records.Query{From: from, To: from.Add(time.Second), Limit: 10_000})
		if err != nil && ctx.Err() == nil {
			log.Fatal(err)
		}
		if err == nil {
			took = append(took, time.Since(start))
		}
	}
	return took
}

func percentiles(sorted []time.Duration) string {
	if len(sorted) == 0 {
		return "p50=- p90=- p99=- max=-"
	}
	at := func(q float64) time.Duration { return sorted[int(q*float64(len(sorted)-1))] }
	return fmt.Sprintf("p50=%v p90=%v p99=%v max=%v", at(0.5).Round(time.Microsecond), at(0.9).Round(time.Microsecond),
		at(0.99).Round(time.Microsecond), sorted[len(sorted)-1].Round(time.Microsecond))
}

// peaks is the most the process held and the log grew to while it watched
type peaks struct {
	rss, heap, wal int64
}

func (p *peaks) watch(ctx context.Context, dir string) {
	tick := time.NewTicker(100 * time.Millisecond)
	defer tick.Stop()
	for {
		p.take(dir)
		select {
		case <-ctx.Done():
			return
		case <-tick.C:
		}
	}
}

func (p *peaks) take(dir string) {
	var stats runtime.MemStats
	runtime.ReadMemStats(&stats)
	p.heap = max(p.heap, int64(stats.HeapInuse)) //nolint:gosec // a heap's bytes, far below an int64's range
	p.rss = max(p.rss, residentBytes())
	if info, err := os.Stat(filepath.Join(dir, "records.db-wal")); err == nil {
		p.wal = max(p.wal, info.Size())
	}
}

func (p *peaks) String() string {
	return fmt.Sprintf("peak_rss_mib=%.1f peak_heap_mib=%.1f peak_wal_mib=%.1f", mib(p.rss), mib(p.heap), mib(p.wal))
}

func mib(bytes int64) float64 {
	return float64(bytes) / (1 << 20)
}

// residentBytes is the process's resident memory, where Linux says it; zero elsewhere
func residentBytes() int64 {
	status, err := os.ReadFile("/proc/self/status")
	if err != nil {
		return 0
	}
	for _, line := range strings.Split(string(status), "\n") {
		if rest, ok := strings.CutPrefix(line, "VmRSS:"); ok {
			kib, err := strconv.ParseInt(strings.TrimSpace(strings.TrimSuffix(strings.TrimSpace(rest), "kB")), 10, 64)
			if err == nil {
				return kib << 10
			}
		}
	}
	return 0
}
