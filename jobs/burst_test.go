package jobs

import (
	"context"
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// measurements of the engine under a burst, skipped unless TINYSTORE_SPIKE=1;
// TINYSTORE_JOBS_DIR puts their files on a volume of their own

// a scheduled message of about 190 bytes, the round's
type burstMessage struct {
	ID, Chat, Author, Text string
}

// TestWorkBurstMeasured drains jobs due at once through Work from 1 to 512
// workers, each handler returning at once, the loop holding as many jobs as it
// has workers or claiming two and four times as many ahead of them, and the
// jobs keyed at random or not
func TestWorkBurstMeasured(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") != "1" {
		t.Skip("a measurement: set TINYSTORE_SPIKE=1")
	}
	for _, keyed := range []bool{false, true} {
		for _, workers := range []int{1, 8, 64, 512} {
			for _, ahead := range []int{1, 2, 4} {
				measureBurst(t, burst{jobs: burstSize(workers), workers: workers, ahead: ahead, keyed: keyed})
			}
		}
	}
}

// burst is one measured drain
type burst struct {
	jobs, workers, ahead int
	keyed                bool
}

// burstSize is enough jobs for a drain of a few seconds at the rates the round
// measured: about 370 a second alone, 50,000 at 512 workers
func burstSize(workers int) int {
	return min(1_000+workers*1_000, 200_000)
}

func measureBurst(t *testing.T, b burst) {
	dir := burstDir(t)
	queues := openTestQueuesWith(t, dir, tinystore.Options{})
	queue := openTestQueue[burstMessage](t, queues, "burst")
	enqueueBurst(t, queues, queue, b)

	before := commits(t, queues)
	started := time.Now()
	settings := workSettings{workers: b.workers, timeout: time.Minute, untilIdle: true}
	err := queue.work(t.Context(), func(context.Context, Job[burstMessage]) error { return nil }, settings,
		b.workers*b.ahead)
	elapsed := time.Since(started)
	if err != nil {
		t.Fatal(err)
	}
	written := commits(t, queues) - before
	nothingDue(t, queue)
	t.Logf("keyed %-5v %3d workers  hold %4d  %6d jobs in %6.2fs  %8.0f a second  %6.1f a commit",
		b.keyed, b.workers, b.workers*b.ahead, b.jobs, elapsed.Seconds(), float64(b.jobs)/elapsed.Seconds(),
		float64(b.jobs)/float64(max(written, 1)))
	if err = queues.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
}

// burstDir is a new directory on TINYSTORE_JOBS_DIR, or the test's own
func burstDir(t *testing.T) string {
	root := os.Getenv("TINYSTORE_JOBS_DIR")
	if root == "" {
		return t.TempDir()
	}
	dir, err := os.MkdirTemp(root, "burst-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return filepath.Clean(dir)
}

// enqueueBurst writes the burst's jobs, all due now, in transactions of ten
// thousand, keyed at random when it says, the enqueues in a shuffled order
func enqueueBurst(t *testing.T, queues *testQueues, queue *Queue[burstMessage], b burst) {
	order := rand.New(rand.NewPCG(uint64(b.jobs), 7)).Perm(b.jobs)
	for first := 0; first < b.jobs; first += 10_000 {
		err := queues.Tx(t.Context(), func(tx *Tx) error {
			for _, n := range order[first:min(first+10_000, b.jobs)] {
				var options []EnqueueOption
				if b.keyed {
					options = append(options, Key(fmt.Sprintf("chat:%08x:%d", rand.Uint32(), n)))
				}
				message := burstMessage{ID: fmt.Sprint("m", n), Chat: "42", Author: "7", Text: burstText}
				if err := queue.WithTx(tx).Enqueue(t.Context(), message, options...); err != nil {
					return err
				}
			}
			return nil
		})
		if err != nil {
			t.Fatal(err)
		}
	}
}

var burstText = strings.Repeat("see you at nine by the station ", 4)
