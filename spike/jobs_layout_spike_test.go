package spike

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"slices"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the first time of every fixture, and the burst's moment a minute after it
var jobsFrom = time.Date(2026, 12, 31, 21, 0, 0, 0, time.UTC).UnixMilli()

// a lease's length in every measurement
const jobsLease = 30_000

// TestJobsBurst drains a burst: a million jobs due within one minute, enqueued
// in the order of their times or shuffled, with keys or without, claimed and
// acknowledged a transaction a cycle in batches of 1, 100 and 1000 while the
// batch before is still in its workers' hands, from each layout; and what the
// next open runs to give back a thousand leases a process that died held
func TestJobsBurst(t *testing.T) {
	kvMeasuring(t)
	count := jobsCount()
	for _, run := range []struct {
		layout         string
		inOrder, keyed bool
	}{
		{"arrival", true, false},
		{"arrival", false, false},
		{"time", false, false},
		{"time-mark", false, false},
		{"time-table", false, false},
		{"arrival", false, true},
		{"time", false, true},
	} {
		layout := jobsLayouts[run.layout]
		file, path := jobsOpen(t, layout)
		fixture := jobsFixture{count: count, from: jobsFrom, span: time.Minute, inOrder: run.inOrder, keyed: run.keyed}
		filled := jobsFill(t, file, layout, fixture)
		label := fmt.Sprintf("%-10s %-8s %-7s", layout.name, jobsOrderLabel(run.inOrder), jobsKeyLabel(run.keyed))
		t.Logf("%s  enqueued %d in %s, %8.0f a second, file %6.1f MiB", label, count, jobsMillis(filled),
			float64(count)/filled.Seconds(), float64(kvFileBytes(path))/(1<<20))

		now := jobsFrom + 61_000
		jobsRecovery(t, file, layout, label, now)
		for _, stage := range []struct {
			batch int
			limit time.Duration
		}{{1, jobsSeconds()}, {100, 10 * time.Second}, {1000, 30 * time.Second}} {
			t.Logf("%s  batches of %4d  %s", label, stage.batch, jobsDrain(t, file, layout, now, stage.batch, stage.limit))
		}
	}
}

func jobsOrderLabel(inOrder bool) string {
	if inOrder {
		return "in order"
	}
	return "shuffled"
}

func jobsKeyLabel(keyed bool) string {
	if keyed {
		return "keyed"
	}
	return "no keys"
}

// jobsRecovery leases a thousand jobs, as a process would before it died, and
// times what the next open runs to give them back
func jobsRecovery(t *testing.T, file *sqlite.File, layout jobsLayout, label string, now int64) {
	t.Helper()
	ctx := t.Context()
	err := file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		_, err := layout.claim(ctx, w, now, now+jobsLease, 1000)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if layout.recover == "" {
		t.Logf("%s  a dead process's leases end by themselves, within %d s", label, jobsLease/1000)
		return
	}
	began := time.Now()
	var given int64
	err = file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		result, execErr := w.ExecContext(ctx, layout.recover, now)
		if execErr == nil {
			given, execErr = result.RowsAffected()
		}
		return execErr
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("%s  the next open gives back %d leases in %s", label, given, jobsMillis(time.Since(began)))
}

// jobsDrained is one drain: the jobs acknowledged, how long it took, each
// cycle's transaction, and the commits
type jobsDrained struct {
	acked   int
	elapsed time.Duration
	cycles  []time.Duration
}

func (d jobsDrained) String() string {
	cycles := slices.Clone(d.cycles)
	slices.Sort(cycles)
	p50, p99 := time.Duration(0), time.Duration(0)
	if len(cycles) > 0 {
		p50, p99 = cycles[len(cycles)/2], cycles[len(cycles)*99/100]
	}
	return fmt.Sprintf("%8d jobs in %s  %9.0f a second  a cycle p50 %s p99 %s", d.acked, jobsMillis(d.elapsed),
		float64(d.acked)/d.elapsed.Seconds(), jobsMillis(p50), jobsMillis(p99))
}

// jobsDrain claims and acknowledges the jobs due at now for up to limit, a
// transaction a cycle: the batch claimed two cycles before acknowledged, the
// next claimed, so that one batch is in its workers' hands while the next is
func jobsDrain(t *testing.T, file *sqlite.File, layout jobsLayout, now int64, batch int, limit time.Duration) jobsDrained {
	t.Helper()
	var inHand [2][]jobsClaimed
	drained := jobsDrained{}
	start := time.Now()
	for cycle := 0; time.Since(start) < limit; cycle++ {
		began := time.Now()
		settle := inHand[cycle%2]
		claimed := jobsCycle(t, file, layout, settle, now, batch)
		drained.acked += len(settle)
		inHand[cycle%2] = claimed
		drained.cycles = append(drained.cycles, time.Since(began))
		if len(claimed) == 0 && len(inHand[(cycle+1)%2]) == 0 {
			break
		}
	}
	jobsCycle(t, file, layout, append(inHand[0], inHand[1]...), now, 0)
	drained.acked += len(inHand[0]) + len(inHand[1])
	drained.elapsed = time.Since(start)
	return drained
}

// jobsCycle acknowledges settle and claims up to batch jobs in one transaction
func jobsCycle(t *testing.T, file *sqlite.File, layout jobsLayout, settle []jobsClaimed, now int64, batch int) []jobsClaimed {
	t.Helper()
	ctx := t.Context()
	var claimed []jobsClaimed
	err := file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		for _, job := range settle {
			if err := layout.ack(ctx, w, job); err != nil {
				return err
			}
		}
		if batch == 0 {
			return nil
		}
		var err error
		claimed, err = layout.claim(ctx, w, now, now+jobsLease, batch)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return claimed
}

const (
	jobsObjects = `select name, sum(pgsize), sum(payload), sum(unused) from dbstat group by name order by 2 desc`
	jobsNextDue = `select next from jobs where queue = ?1 order by next limit 1`
	jobsCancel  = `delete from jobs where queue = ?1 and key = ?2 returning id`
)

// TestJobsFile divides the file by object for a million scheduled messages
// with keys, spread over a week and enqueued shuffled, on both layouts, and
// times what a file of them answers most: its next due job, and a key cancelled
func TestJobsFile(t *testing.T) {
	kvMeasuring(t)
	count := jobsCount()
	for _, name := range []string{"arrival", "time"} {
		layout := jobsLayouts[name]
		file, path := jobsOpen(t, layout)
		jobsFill(t, file, layout, jobsFixture{count: count, from: jobsFrom, span: 7 * 24 * time.Hour, keyed: true})
		t.Logf("%-8s file %6.1f MiB, %6.1f bytes a job", name, float64(kvFileBytes(path))/(1<<20),
			float64(kvFileBytes(path))/float64(count))
		for _, object := range jobsDivide(t, file) {
			t.Logf("%-8s   %-10s %6.1f bytes a job, payload %4.1f %%, unused %4.1f %%", name, object.name,
				float64(object.bytes)/float64(count), 100*float64(object.payload)/float64(object.bytes),
				100*float64(object.unused)/float64(object.bytes))
		}
		jobsLookups(t, file, name)
		jobsCancels(t, file, name, count)
	}
}

// jobsObject is one b-tree of a file: its pages' bytes, what its cells hold,
// and what they leave unused
type jobsObject struct {
	name                   string
	bytes, payload, unused int64
}

func jobsDivide(t *testing.T, file *sqlite.File) []jobsObject {
	t.Helper()
	ctx := t.Context()
	var objects []jobsObject
	err := file.View(ctx, func(tx *sql.Tx) error {
		rows, err := tx.QueryContext(ctx, jobsObjects) //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "objects", func(rows *sql.Rows) error {
			var object jobsObject
			if scanErr := rows.Scan(&object.name, &object.bytes, &object.payload, &object.unused); scanErr != nil {
				return scanErr
			}
			objects = append(objects, object)
			return nil
		})
	})
	if err != nil {
		t.Fatal(err)
	}
	return objects
}

// jobsLookups times the next due job read by a point statement without a
// transaction of its own, as a queue's wake-up would read it
func jobsLookups(t *testing.T, file *sqlite.File, name string) {
	t.Helper()
	ctx := t.Context()
	const calls = 20_000
	began := time.Now()
	for range calls {
		err := file.Lookup(ctx, func(r sqlite.Reader) error {
			var next int64
			return sqlite.QueryRow(ctx, r, jobsNextDue, jobsQueue).Scan(&next)
		})
		if err != nil {
			t.Fatal(err)
		}
	}
	t.Logf("%-8s the next due job: %5.1f µs a lookup over %d", name,
		float64(time.Since(began).Microseconds())/calls, calls)
}

// jobsCancels cancels ten thousand keys of the file's jobs in one transaction
func jobsCancels(t *testing.T, file *sqlite.File, name string, count int) {
	t.Helper()
	ctx := t.Context()
	const cancels = 10_000
	found := 0
	began := time.Now()
	err := file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		for i := range cancels {
			var id int64
			err := sqlite.QueryRow(ctx, w, jobsCancel, jobsQueue, jobsKey(i*(count/cancels))).Scan(&id)
			if err == nil {
				found++
			} else if !errors.Is(err, sql.ErrNoRows) {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("%-8s cancelled %d of %d keys, %5.1f µs a cancel in one transaction", name, found, cancels,
		float64(time.Since(began).Microseconds())/cancels)
}

// the claim and acknowledgement of a job whose value lives in a row of its own
const (
	jobsSpillClaim = `update jobs set next = ?3, attempt = attempt + 1, lease = attempt + 1
		where (queue, next, id) in (
			select queue, next, id from jobs where queue = ?1 and next <= ?2
			order by next, id limit cast(?4 as integer))
		returning next, id, attempt, spill`
	jobsSpillRead = `select value from spilled where id = ?1`
	jobsSpillDrop = `delete from spilled where id = ?1`
)

var jobsSpilledLayout = jobsLayout{
	name: "time", schema: jobsTimeSchema, insert: jobsTimeInsert,
	claim: jobsClaimSpilled, ack: jobsAckSpilled,
}

func jobsClaimSpilled(ctx context.Context, w sqlite.Writer, now, until int64, batch int) ([]jobsClaimed, error) {
	rows, err := w.QueryContext(ctx, jobsSpillClaim, jobsQueue, now, until, batch) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return nil, err
	}
	var claimed []jobsClaimed
	var spills []int64
	err = sqlite.EachRow(rows, "claimed jobs", func(rows *sql.Rows) error {
		var job jobsClaimed
		var spill int64
		if scanErr := rows.Scan(&job.next, &job.id, &job.attempt, &spill); scanErr != nil {
			return scanErr
		}
		claimed, spills = append(claimed, job), append(spills, spill)
		return nil
	})
	for i := range claimed {
		if err == nil {
			err = sqlite.QueryRow(ctx, w, jobsSpillRead, spills[i]).Scan(&claimed[i].value)
		}
	}
	return claimed, err
}

func jobsAckSpilled(ctx context.Context, w sqlite.Writer, job jobsClaimed) error {
	if _, err := w.ExecContext(ctx, jobsMoveAck, jobsQueue, job.next, job.id, job.attempt); err != nil {
		return err
	}
	_, err := w.ExecContext(ctx, jobsSpillDrop, job.id)
	return err
}

// TestJobsValues keeps 100,000 jobs due within a minute, enqueued shuffled,
// with values of 64 to 4096 bytes in their rows and in rows of their own,
// and reports the file and a drain in batches of 100 for each
func TestJobsValues(t *testing.T) {
	kvMeasuring(t)
	const count = 100_000
	for _, size := range []int{64, 256, 512, 1024, 4096} {
		for _, spilled := range []bool{false, true} {
			layout := jobsLayouts["time"]
			if spilled {
				layout = jobsSpilledLayout
			}
			file, _ := jobsOpen(t, layout)
			jobsFill(t, file, layout, jobsFixture{
				count: count, from: jobsFrom, span: time.Minute, valueSize: size,
				spilled: spilled,
			})
			kept := "in the row"
			if spilled {
				kept = "spilled"
			}
			var bytes int64
			for _, object := range jobsDivide(t, file) {
				bytes += object.bytes
			}
			drained := jobsDrain(t, file, layout, jobsFrom+61_000, 100, jobsSeconds())
			t.Logf("%4d bytes %-10s  %6.0f bytes a job  drained %s", size, kept, float64(bytes)/count, drained)
		}
	}
}
