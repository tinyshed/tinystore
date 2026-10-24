package metrics

import (
	"context"
	"errors"
	"math"
	"reflect"
	"sync"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/ways"
)

func TestAValueFallsInTheIntervalOfTheGridWhereTheStorePutsIt(t *testing.T) {
	got := []string{binOf(0), binOf(1), binOf(1.05), binOf(0.99), binOf(math.Sqrt2)}
	if !reflect.DeepEqual(got, []string{"zero", "3:0", "3:1", "3:0", "3:5"}) {
		t.Fatalf("bins %v", got)
	}
	if below := math.Float64frombits(math.Float64bits(math.Sqrt2) - 1); binOf(below) != "3:4" {
		t.Fatalf("below the square root of two: %s", binOf(below))
	}
	if binOf(math.MaxFloat64) != "3:8192" {
		t.Fatalf("the largest float: %s", binOf(math.MaxFloat64))
	}
	if bounds := ExponentialBuckets(1, 2, 4); !reflect.DeepEqual(bounds, []float64{1, 2, 4, 8}) {
		t.Fatalf("bounds %v", bounds)
	}
}

func TestALabelIsWrittenAsEveryLanguageWritesIt(t *testing.T) {
	var got []string
	for _, value := range []any{200, 5.0, 0.25, 1e21, true, "x", 1e-7, 0.000001, math.Copysign(0, -1), int64(-3)} {
		text, err := labelText(value)
		if err != nil {
			t.Fatal(err)
		}
		got = append(got, text)
	}
	if want := []string{"200", "5", "0.25", "1e+21", "true", "x", "1e-7", "0.000001", "0", "-3"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("labels %q, want %q", got, want)
	}
	if Percentile(99.9) != "p99.9" || Percentile(95) != "p95" {
		t.Fatalf("percentiles %s %s", Percentile(99.9), Percentile(95))
	}
}

// cuttable is a link to the core in this process that a test can cut and
// take down, keeping each connection it dialled.
type cuttable struct {
	mu      sync.Mutex
	dialled []*core.Connection
	down    bool
}

func (c *cuttable) store(dir string) *core.Store {
	dial := core.InProcess(dir, ways.Library(), core.Started{})
	return core.NewStore(core.NewLink(func(ctx context.Context) (*core.Connection, error) {
		c.mu.Lock()
		down := c.down
		c.mu.Unlock()
		if down {
			return nil, core.Errorf(core.Unavailable, "the store is down")
		}
		conn, err := dial(ctx)
		if err == nil {
			c.mu.Lock()
			c.dialled = append(c.dialled, conn)
			c.mu.Unlock()
		}
		return conn, err
	}))
}

func (c *cuttable) cut() {
	c.mu.Lock()
	last := c.dialled[len(c.dialled)-1]
	c.mu.Unlock()
	last.Session.End(errors.New("cut"))
}

// closeAll closes every connection it dialled: a cut one's pipe still holds
// the store until it closes.
func (c *cuttable) closeAll() {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, conn := range c.dialled {
		conn.Close()
	}
}

func (c *cuttable) setDown(down bool) {
	c.mu.Lock()
	c.down = down
	c.mu.Unlock()
}

// counted is a counter's total as a store of the directory reads it.
func counted(t *testing.T, keeper *tinystore.Store, name string) float64 {
	t.Helper()
	read := ways.Must(Read(context.Background(), keeper, Query{Name: name, Since: time.Hour}))
	if len(read) != 1 || len(read[0].Samples) == 0 {
		t.Fatalf("%s read %+v", name, read)
	}
	return read[0].Samples[len(read[0].Samples)-1].Value
}

func cutCounter(t *testing.T, in *instruments, name string) *Counter {
	t.Helper()
	d := ways.Must(in.declare(name, formCounter, nil, false))
	return &Counter{in: in, d: d}
}

func TestABatchWhoseAnswerALostConnectionTookCountsOnce(t *testing.T) {
	if ways.Library() == "" {
		t.Skip("the core is not built here")
	}
	defer ways.Recover(t)
	ctx := context.Background()
	dir := t.TempDir()
	// a second handle keeps the store open while the connection under test is cut
	keeper := ways.Must(tinystore.Open(ctx, dir, tinystore.Library(ways.Library())))
	defer keeper.Close()
	link := &cuttable{}
	held := link.store(dir)
	defer link.closeAll()
	defer held.Close()
	in := newInstruments(held)
	jobs := cutCounter(t, in, "cut_jobs_total")
	jobs.Inc()
	ways.Must(in.flush(ctx))
	for cut := range 5 {
		jobs.Add(2)
		flushed := make(chan error, 1)
		go func() { _, err := in.flush(ctx); flushed <- err }()
		time.Sleep(time.Duration(cut) * time.Millisecond)
		link.cut()
		if err := <-flushed; err != nil {
			t.Fatalf("cut %d: %v", cut, err)
		}
	}
	if total := counted(t, keeper, "cut_jobs_total"); total != 11 {
		t.Fatalf("total %v, want 11", total)
	}
}

func TestAFlushTheStoreCannotBeReachedForFailsAndWhatItCarriedGoesOnceItCanBe(t *testing.T) {
	if ways.Library() == "" {
		t.Skip("the core is not built here")
	}
	defer ways.Recover(t)
	ctx := context.Background()
	dir := t.TempDir()
	keeper := ways.Must(tinystore.Open(ctx, dir, tinystore.Library(ways.Library())))
	defer keeper.Close()
	link := &cuttable{}
	held := link.store(dir)
	defer link.closeAll()
	defer held.Close()
	in := newInstruments(held)
	jobs := cutCounter(t, in, "down_jobs_total")
	jobs.Inc()
	ways.Must(in.flush(ctx))
	link.setDown(true)
	link.cut()
	jobs.Add(2)
	if _, err := in.flush(ctx); !errors.Is(err, tinystore.ErrUnavailable) {
		t.Fatalf("a flush of a store that is down: %v", err)
	}
	link.setDown(false)
	// a tick of the background flush may have begun its dial while the store was down
	deadline := time.Now().Add(5 * time.Second)
	for _, err := in.flush(ctx); err != nil; _, err = in.flush(ctx) {
		if !errors.Is(err, tinystore.ErrUnavailable) || time.Now().After(deadline) {
			t.Fatalf("a flush once the store is up again: %v", err)
		}
	}
	if total := counted(t, keeper, "down_jobs_total"); total != 3 {
		t.Fatalf("total %v, want 3", total)
	}
}
