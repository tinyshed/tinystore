// Command kv measures the five cases of docs/kv.md under concurrent load, each
// implemented twice and run on identical input in one run: through kv, and
// through the tables an application keeps today without it, in its own
// sql/app.db through sqldb, each write a transaction of its own. Every stage
// prints key=value lines.
//
//	kv -stage sessions -dir <dir>   sessions read and renewed, sign-ins, sign-outs, devices listed
//	kv -stage links -dir <dir>      sign-in codes asked for and clicked, some clicked twice
//	kv -stage attempts -dir <dir>   attempts by address and by email, a few addresses past their limit
//	kv -stage flood -dir <dir>      attempts, each from an address never seen before
//	kv -stage claims -dir <dir>     webhook events claimed, handled and finished, some delivered twice
//	kv -stage drafts -dir <dir>     drafts saved from two tabs, each at the version it read
//	kv -stage writes -dir <dir>     sessions written, and nothing else, as the mechanics round wrote them
//	kv -stage hits -dir <dir>       stored sessions read, and nothing else
//	kv -stage misses -dir <dir>     tokens nobody holds read, and nothing else
//
// -impl table,kv names the implementations, -workers 1,8,64,512 the goroutines
// of each phase, -keys the state a phase starts from and -phase its length. A
// stage first runs its case's script against each implementation, on a clock
// the script moves, and stops when an answer is not the case's.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io/fs"
	"log"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/tinyshed/tinystore"
)

func main() {
	r := parseRun()
	ctx := context.Background()

	measured := r.checkAll(ctx)
	for _, workers := range r.workers {
		for _, impl := range measured {
			r.measure(ctx, impl, workers)
		}
	}

	if r.broken {
		log.Fatal("a promise of the case was broken or a request failed: see the lines above")
	}
}

// stage is one case: its load, the script an implementation of it answers,
// and the query that counts its rows in sql/app.db
type stage struct {
	name  string
	load  func(ctx context.Context, b *backend, keys int) (workload, error)
	check func(ctx context.Context, b *backend, clock *fakeClock) error
	rows  string
}

var stages = []stage{
	{name: "sessions", load: loadSessions, check: checkSessions, rows: countSessions},
	{name: "links", load: loadLinks, check: checkLinks, rows: countCodes},
	{name: "attempts", load: loadAttempts, check: checkAttempts, rows: countAttempts},
	{name: "flood", load: loadFlood, check: checkAttempts, rows: countAttempts},
	{name: "claims", load: loadClaims, check: checkClaims, rows: countEvents},
	{name: "drafts", load: loadDrafts, check: checkDrafts, rows: countDrafts},
	{name: "writes", load: loadWrites, check: checkSessions, rows: countSessions},
	{name: "hits", load: loadHits, check: checkSessions, rows: countSessions},
	{name: "misses", load: loadMisses, check: checkSessions, rows: countSessions},
}

// run is what one invocation measures; broken is a promise a phase saw broken
type run struct {
	stage   stage
	impls   []string
	workers []int
	keys    int
	phase   time.Duration
	dir     string
	broken  bool
}

func parseRun() *run {
	name := flag.String("stage", "sessions", "sessions, links, attempts, flood, claims, drafts, writes, hits or misses")
	impls := flag.String("impl", implTable+","+implKV, "the implementations, each phase in this order")
	workers := flag.String("workers", "1,8,64,512", "the goroutines of each phase")
	keys := flag.Int("keys", 100_000, "the state a phase starts from: sessions, codes, counters, events or notes")
	phase := flag.Duration("phase", 3*time.Second, "how long each phase lasts")
	dir := flag.String("dir", "", "an empty directory for the stores")
	flag.Parse()

	r := &run{keys: *keys, phase: *phase, dir: *dir}
	var err error
	r.stage, err = stageNamed(*name)
	must(err)
	r.impls, err = implementations(*impls)
	must(err)
	r.workers, err = workerCounts(*workers)
	must(err)
	must(r.checkSettings())
	return r
}

func stageNamed(name string) (stage, error) {
	for _, s := range stages {
		if s.name == name {
			return s, nil
		}
	}
	return stage{}, fmt.Errorf("unknown stage %q", name)
}

func implementations(list string) ([]string, error) {
	impls := strings.Split(list, ",")
	for i, impl := range impls {
		if impl != implTable && impl != implKV || slices.Contains(impls[:i], impl) {
			return nil, fmt.Errorf("-impl %q: table, kv or both, each once", list)
		}
	}
	return impls, nil
}

func workerCounts(list string) ([]int, error) {
	var counts []int
	for field := range strings.SplitSeq(list, ",") {
		count, err := strconv.Atoi(field)
		if err != nil || count < 1 {
			return nil, fmt.Errorf("-workers %q: positive counts, comma separated", list)
		}
		counts = append(counts, count)
	}
	return counts, nil
}

// checkSettings refuses a directory that holds anything, since every phase
// removes the store it made there
func (r *run) checkSettings() error {
	if r.keys < 1 || r.phase <= 0 || r.dir == "" {
		return errors.New("-keys and -phase must be positive, and -dir is required")
	}
	entries, err := os.ReadDir(r.dir)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return nil
	case err != nil:
		return err
	case len(entries) > 0:
		return fmt.Errorf("-dir %s is not empty", r.dir)
	}
	return nil
}

// checkAll runs the case's script against each implementation and returns
// those that can be measured; one whose answers are not the case's stops the run
func (r *run) checkAll(ctx context.Context) []string {
	var measured []string
	for _, impl := range r.impls {
		label := fmt.Sprintf("case=%s impl=%s", r.stage.name, impl)
		err := r.check(ctx, impl)
		switch {
		case errors.Is(err, errUnavailable):
			fmt.Printf("check %s available=false reason=%q\n", label, err.Error())
		case err != nil:
			log.Fatalf("check %s: %v", label, err)
		default:
			fmt.Printf("check %s result=ok\n", label)
			measured = append(measured, impl)
		}
	}
	return measured
}

// check runs the script on a Manual store of its own, whose clock it moves
func (r *run) check(ctx context.Context, impl string) (err error) {
	dir := filepath.Join(r.dir, "check-"+impl)
	clock := &fakeClock{now: checkStart}
	b, err := openBackend(ctx, dir, impl, tinystore.Options{Manual: true, Clock: clock.Now})
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, b.close(ctx), os.RemoveAll(dir)) }()

	return r.stage.check(ctx, b, clock)
}

// measure prepares a store for one implementation, runs the case's requests
// from workers goroutines for a phase, and reports them and the file it left
func (r *run) measure(ctx context.Context, impl string, workers int) {
	label := fmt.Sprintf("case=%s impl=%s workers=%d", r.stage.name, impl, workers)
	dir := filepath.Join(r.dir, fmt.Sprintf("%s-%s-%d", r.stage.name, impl, workers))
	b, err := openBackend(ctx, dir, impl, tinystore.Options{Logger: warnings()})
	must(err)
	load, err := r.stage.load(ctx, b, r.keys)
	must(err)

	started := time.Now()
	must(load.prepare(ctx))
	prepared := time.Since(started)
	runtime.GC()

	requests, err := runPhase(ctx, load, workers, r.phase)
	must(err)
	verified := load.verify(ctx)
	must(b.close(ctx))

	fmt.Printf("%s keys=%d prepare=%v phase=%v %s\n", label, r.keys, prepared.Round(time.Millisecond), r.phase,
		requests.summary())
	requests.print(label)
	printVerified(label, verified)
	must(reportFile(ctx, label, b, r.stage.rows))
	must(os.RemoveAll(dir))
	r.broken = r.broken || requests.failed() > 0 || verified != nil
}

func printVerified(label string, err error) {
	if err != nil {
		fmt.Printf("verify %s result=%q\n", label, err.Error())
		return
	}
	fmt.Printf("verify %s result=ok\n", label)
}

func must(err error) {
	if err != nil {
		log.Fatal(err)
	}
}
